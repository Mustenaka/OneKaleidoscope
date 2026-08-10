//! Executable composition for a logged-in Codex runtime and the LAN Broker.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kaleido_adapter::{ProviderRuntimeSession, SessionStartRequest};
use kaleido_adapter_codex::{
    CodexRuntimeConfig, CodexRuntimeSession, CodexSandboxMode, ReducerConfig,
};
use kaleido_proto::capability::EvidenceSource;
use kaleido_proto::content::{ContentKind, Sensitivity};
use kaleido_proto::host::LaunchSurface;
use kaleido_proto::ids::{DeviceId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId};
use kaleido_proto::turn::TurnOrigin;
use kaleido_state::ClockSource;
use kaleido_transport::bootstrap::encode_uri;
use rand_core::{OsRng, RngCore};

use crate::{Broker, LanServer, RuntimeSupervisor};

const IDENTITY_SALT_FILE: &str = "host-identity-salt";

#[derive(Debug, Clone)]
pub struct CodexLanConfig {
    pub executable: PathBuf,
    pub project_root: PathBuf,
    pub data_directory: PathBuf,
    pub bind_address: SocketAddr,
    pub sandbox: CodexSandboxMode,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodexLanError {
    #[error("persistent host identity is unavailable")]
    HostIdentity,
    #[error("the project root is unavailable")]
    ProjectRoot,
    #[error("the canonical broker could not start")]
    Broker,
    #[error("the Codex runtime could not start")]
    Runtime,
    #[error("the TLS LAN listener could not start")]
    Listener,
    #[error("the pairing URI could not be encoded")]
    Pairing,
}

/// Provider-neutral bootstrap data for one runtime worker. Provider-private
/// session identifiers remain inside the boxed adapter.
pub struct RuntimeBootstrap {
    pub project_id: ProjectId,
    pub project_binding_id: ProjectBindingId,
    pub runtime_id: ProviderRuntimeId,
    pub runtime: Box<dyn ProviderRuntimeSession + Send>,
}

impl std::fmt::Debug for RuntimeBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeBootstrap")
            .field("project_id", &self.project_id)
            .field("project_binding_id", &self.project_binding_id)
            .field("runtime_id", &self.runtime_id)
            .field("runtime", &"[provider adapter]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeBootstrapContext {
    pub identity_salt: String,
    pub project_root: PathBuf,
    pub host_platform: kaleido_proto::host::HostPlatform,
}

pub type RuntimeBootstrapFactory =
    Box<dyn FnOnce(RuntimeBootstrapContext) -> Result<RuntimeBootstrap, CodexLanError> + Send>;

pub struct StructuredLanConfig {
    pub project_root: PathBuf,
    pub data_directory: PathBuf,
    pub bind_address: SocketAddr,
    pub runtimes: Vec<RuntimeBootstrapFactory>,
}

impl std::fmt::Debug for StructuredLanConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StructuredLanConfig")
            .field("project_root", &self.project_root)
            .field("data_directory", &self.data_directory)
            .field("bind_address", &self.bind_address)
            .field("runtime_count", &self.runtimes.len())
            .finish()
    }
}

/// One LAN broker hosting any number of structured provider runtimes over the
/// same durable store and projection journal.
pub struct StructuredLanHost {
    server: Option<LanServer>,
    supervisor: Arc<RuntimeSupervisor>,
    session_ids: Vec<SessionId>,
    pairing_uri: String,
}

impl std::fmt::Debug for StructuredLanHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StructuredLanHost")
            .field("session_ids", &self.session_ids)
            .field("pairing_uri", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl StructuredLanHost {
    pub fn start(config: StructuredLanConfig) -> Result<Self, CodexLanError> {
        if config.runtimes.is_empty() {
            return Err(CodexLanError::Runtime);
        }
        let identity_salt = load_or_create_identity_salt(&config.data_directory)?;
        let broker = Broker::load(
            config.data_directory.join("canonical"),
            ClockSource::System,
            identity_salt.clone(),
            "kaleido-host",
        )
        .map_err(|_| CodexLanError::Broker)?;
        let canonical_root =
            fs::canonicalize(&config.project_root).map_err(|_| CodexLanError::ProjectRoot)?;
        let provider_root = crate::platform::provider_path(&canonical_root);
        let root_text = provider_root.to_string_lossy();
        let root_ref = broker
            .content_store()
            .store(
                ContentKind::FilePath,
                Sensitivity::Sensitive,
                root_text.as_bytes(),
            )
            .map_err(|_| CodexLanError::Broker)?;
        drop(root_text);
        let supervisor = Arc::new(RuntimeSupervisor::new(broker.clone()));
        let mut session_ids = Vec::with_capacity(config.runtimes.len());
        let context = RuntimeBootstrapContext {
            identity_salt,
            project_root: provider_root,
            host_platform: crate::platform::host_platform().ok_or(CodexLanError::Listener)?,
        };
        for factory in config.runtimes {
            let bootstrap = factory(context.clone())?;
            let start = SessionStartRequest {
                project_id: bootstrap.project_id,
                project_binding_id: bootstrap.project_binding_id,
                runtime_id: bootstrap.runtime_id,
                project_root_ref: root_ref.clone(),
            };
            match supervisor.start_runtime(start, bootstrap.runtime) {
                Ok(session_id) => session_ids.push(session_id),
                Err(_) => {
                    for session_id in &session_ids {
                        let _ = supervisor.stop_session(session_id);
                    }
                    return Err(CodexLanError::Runtime);
                }
            }
        }
        let recovered = supervisor.recover_all_ready();
        if recovered.iter().any(|(_, result)| result.is_err()) {
            for session_id in &session_ids {
                let _ = supervisor.stop_session(session_id);
            }
            return Err(CodexLanError::Runtime);
        }
        let server = LanServer::bind(
            config.bind_address,
            &config.data_directory.join("security"),
            broker,
            Some(Arc::clone(&supervisor)),
        )
        .map_err(|_| CodexLanError::Listener)?;
        let bootstrap = server
            .issue_pairing(now_ms())
            .map_err(|_| CodexLanError::Pairing)?;
        let pairing_uri = encode_uri(&bootstrap).map_err(|_| CodexLanError::Pairing)?;
        Ok(Self {
            server: Some(server),
            supervisor,
            session_ids,
            pairing_uri,
        })
    }

    pub fn pairing_uri(&self) -> &str {
        &self.pairing_uri
    }

    pub fn issue_pairing_uri(&self) -> Result<String, CodexLanError> {
        let server = self.server.as_ref().ok_or(CodexLanError::Listener)?;
        let bootstrap = server
            .issue_pairing(now_ms())
            .map_err(|_| CodexLanError::Pairing)?;
        encode_uri(&bootstrap).map_err(|_| CodexLanError::Pairing)
    }

    pub fn revoke_device(&self, device_id: &DeviceId) -> Result<(), CodexLanError> {
        self.server
            .as_ref()
            .ok_or(CodexLanError::Listener)?
            .revoke_device(device_id, now_ms())
            .map_err(|_| CodexLanError::Listener)
    }

    pub fn session_ids(&self) -> &[SessionId] {
        &self.session_ids
    }

    pub fn run_for(&self, duration: Duration) {
        let deadline = Instant::now().checked_add(duration);
        while deadline.is_some_and(|deadline| Instant::now() < deadline) {
            let _ = self.supervisor.pump_pending_queue();
            self.supervisor.drain_all();
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn shutdown(mut self) -> Result<(), CodexLanError> {
        for session_id in &self.session_ids {
            let _ = self.supervisor.stop_session(session_id);
        }
        if let Some(server) = self.server.take() {
            server.shutdown().map_err(|_| CodexLanError::Listener)?;
        }
        self.pairing_uri.clear();
        Ok(())
    }
}

impl Drop for StructuredLanHost {
    fn drop(&mut self) {
        self.pairing_uri.clear();
    }
}

pub struct CodexLanHost {
    server: Option<LanServer>,
    supervisor: Arc<RuntimeSupervisor>,
    session_id: SessionId,
    pairing_uri: String,
}

impl std::fmt::Debug for CodexLanHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexLanHost")
            .field("session_id", &self.session_id)
            .field("pairing_uri", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl CodexLanHost {
    pub fn start(config: &CodexLanConfig) -> Result<Self, CodexLanError> {
        let identity_salt = load_or_create_identity_salt(&config.data_directory)?;
        let broker = Broker::load(
            config.data_directory.join("canonical"),
            ClockSource::System,
            identity_salt.clone(),
            "kaleido-host",
        )
        .map_err(|_| CodexLanError::Broker)?;
        let canonical_root =
            fs::canonicalize(&config.project_root).map_err(|_| CodexLanError::ProjectRoot)?;
        let canonical_root = crate::platform::provider_path(&canonical_root);
        let root_text = canonical_root.to_string_lossy();
        let root_ref = broker
            .content_store()
            .store(
                ContentKind::FilePath,
                Sensitivity::Sensitive,
                root_text.as_bytes(),
            )
            .map_err(|_| CodexLanError::Broker)?;
        drop(root_text);

        let reducer = ReducerConfig {
            host_display_name: "kaleido-host".to_owned(),
            host_platform: crate::platform::host_platform().ok_or(CodexLanError::Listener)?,
            project_display_name: "kaleido-project".to_owned(),
            identity_salt: identity_salt.clone(),
            evidence: EvidenceSource::ObservedInTraffic,
            launch_surface: LaunchSurface::BrokerLaunched,
            turn_origin: TurnOrigin::LocalSurface,
            base_at_ms: now_ms(),
            runtime_version_label: None,
        };
        let runtime = CodexRuntimeSession::new(CodexRuntimeConfig {
            executable: config.executable.clone(),
            reducer,
            sandbox: config.sandbox,
            request_timeout: config.request_timeout,
        });
        let start = SessionStartRequest {
            project_id: runtime.project_id().clone(),
            project_binding_id: runtime.project_binding_id().clone(),
            runtime_id: runtime.runtime_id().clone(),
            project_root_ref: root_ref,
        };
        let supervisor = Arc::new(RuntimeSupervisor::new(broker.clone()));
        let session_id = supervisor
            .start_runtime(start, Box::new(runtime))
            .map_err(|_| CodexLanError::Runtime)?;
        let recovered = supervisor.recover_all_ready();
        if recovered.iter().any(|(_, result)| result.is_err()) {
            let _ = supervisor.stop_session(&session_id);
            return Err(CodexLanError::Runtime);
        }
        let server = LanServer::bind(
            config.bind_address,
            &config.data_directory.join("security"),
            broker,
            Some(Arc::clone(&supervisor)),
        )
        .map_err(|_| CodexLanError::Listener)?;
        let bootstrap = server
            .issue_pairing(now_ms())
            .map_err(|_| CodexLanError::Pairing)?;
        let pairing_uri = encode_uri(&bootstrap).map_err(|_| CodexLanError::Pairing)?;
        Ok(Self {
            server: Some(server),
            supervisor,
            session_id,
            pairing_uri,
        })
    }

    /// This value contains the one-time secret and is intentionally returned
    /// only to the local operator. Callers must print it directly, never send
    /// it to `tracing` or ordinary analytics.
    pub fn pairing_uri(&self) -> &str {
        &self.pairing_uri
    }

    /// Issues another one-time pairing credential for an operator-initiated
    /// device enrollment. The returned secret must be shown directly and must
    /// never enter tracing or durable state.
    pub fn issue_pairing_uri(&self) -> Result<String, CodexLanError> {
        let server = self.server.as_ref().ok_or(CodexLanError::Listener)?;
        let bootstrap = server
            .issue_pairing(now_ms())
            .map_err(|_| CodexLanError::Pairing)?;
        encode_uri(&bootstrap).map_err(|_| CodexLanError::Pairing)
    }

    /// Durably revokes a paired Android identity before active connections are
    /// notified and closed by the LAN server.
    pub fn revoke_device(&self, device_id: &DeviceId) -> Result<(), CodexLanError> {
        self.server
            .as_ref()
            .ok_or(CodexLanError::Listener)?
            .revoke_device(device_id, now_ms())
            .map_err(|_| CodexLanError::Listener)
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn run_for(&self, duration: Duration) {
        let deadline = Instant::now().checked_add(duration);
        while deadline.is_some_and(|deadline| Instant::now() < deadline) {
            let _ = self.supervisor.drain_session(&self.session_id);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn shutdown(mut self) -> Result<(), CodexLanError> {
        let _ = self.supervisor.stop_session(&self.session_id);
        if let Some(server) = self.server.take() {
            server.shutdown().map_err(|_| CodexLanError::Listener)?;
        }
        self.pairing_uri.clear();
        Ok(())
    }
}

impl Drop for CodexLanHost {
    fn drop(&mut self) {
        self.pairing_uri.clear();
    }
}

fn load_or_create_identity_salt(root: &Path) -> Result<String, CodexLanError> {
    fs::create_dir_all(root).map_err(|_| CodexLanError::HostIdentity)?;
    let path = root.join(IDENTITY_SALT_FILE);
    match read_identity_salt(&path) {
        Ok(salt) => return Ok(salt),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(CodexLanError::HostIdentity);
        }
        Err(_) => {}
    }
    let mut random = [0_u8; 32];
    OsRng.fill_bytes(&mut random);
    let salt = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    random.fill(0);
    let temporary = root.join(format!(".{IDENTITY_SALT_FILE}-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| CodexLanError::HostIdentity)?;
    let result = file
        .write_all(salt.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, &path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return read_identity_salt(&path).map_err(|_| CodexLanError::HostIdentity);
    }
    Ok(salt)
}

fn read_identity_salt(path: &Path) -> Result<String, std::io::Error> {
    let file = fs::File::open(path)?;
    let mut salt = String::new();
    file.take(65).read_to_string(&mut salt)?;
    if salt.len() != 64
        || !salt
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid host identity",
        ));
    }
    Ok(salt)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
