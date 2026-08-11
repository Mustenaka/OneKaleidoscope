//! Executable composition for a logged-in Codex runtime and the LAN Broker.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
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
use kaleido_transport::private_file::PrivateFileStore;
use rand_core::{OsRng, RngCore};

use crate::remote_control::{
    PinnedRemoteControlSession, RemoteControlConfig, RemoteControlPlane, RemotePairingUri,
    PRESENCE_REFRESH_INTERVAL,
};
use crate::{Broker, LanServer, RuntimeSupervisor};

const IDENTITY_SALT_FILE: &str = "host-identity-salt";
const REMOTE_CONTROL_STATE_FILE: &str = "remote-control.json";
const REMOTE_BOOTSTRAP_LIFETIME: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct CodexLanConfig {
    pub executable: PathBuf,
    pub project_root: PathBuf,
    pub data_directory: PathBuf,
    pub bind_address: SocketAddr,
    pub sandbox: CodexSandboxMode,
    pub request_timeout: Duration,
}

#[derive(Clone)]
pub struct StructuredRemoteConfig {
    pub service_endpoint: String,
    pub service_public_key_pin: String,
    pub relay_url: String,
}

/// Compatibility name retained for callers of the R4 Codex-only wrapper.
pub type CodexRemoteConfig = StructuredRemoteConfig;

impl std::fmt::Debug for StructuredRemoteConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StructuredRemoteConfig")
            .field("service_endpoint", &"[redacted]")
            .field("service_public_key_pin", &"[redacted]")
            .field("relay_url", &"[redacted]")
            .finish()
    }
}

impl std::fmt::Debug for CodexLanConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexLanConfig")
            .field("executable", &"<PATH>")
            .field("project_root", &"<SANDBOX>")
            .field("data_directory", &"<PATH>")
            .field("bind_address", &"[redacted]")
            .field("sandbox", &self.sandbox)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
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
    #[error("the self-hosted remote control plane is unavailable")]
    RemoteControl,
}

struct RemoteRuntime {
    plane: RemoteControlPlane,
    session: Option<PinnedRemoteControlSession>,
    refresh_after: Instant,
}

impl std::fmt::Debug for RemoteRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteRuntime")
            .field("plane", &self.plane)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl RemoteRuntime {
    fn new(plane: RemoteControlPlane, session: PinnedRemoteControlSession) -> Self {
        Self {
            plane,
            session: Some(session),
            refresh_after: Instant::now(),
        }
    }

    fn ensure_session(&mut self) -> Result<(), CodexLanError> {
        if self.session.is_none() {
            self.session = Some(
                self.plane
                    .connect()
                    .map_err(|_| CodexLanError::RemoteControl)?,
            );
        }
        Ok(())
    }

    fn refresh_presence_if_due(&mut self, force: bool) -> Result<(), CodexLanError> {
        self.flush_pending_revokes()?;
        if !force && Instant::now() < self.refresh_after {
            return Ok(());
        }
        self.ensure_session()?;
        let result = match self.session.as_mut() {
            Some(session) => self.plane.refresh_presence(session, now_ms()),
            None => return Err(CodexLanError::RemoteControl),
        };
        if result.is_err() {
            self.session = None;
            self.refresh_after = Instant::now()
                .checked_add(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            return Err(CodexLanError::RemoteControl);
        }
        self.refresh_after = Instant::now()
            .checked_add(PRESENCE_REFRESH_INTERVAL)
            .unwrap_or_else(Instant::now);
        Ok(())
    }

    fn flush_pending_revokes(&mut self) -> Result<(), CodexLanError> {
        while self.plane.pending_revoke_count() > 0 {
            self.ensure_session()?;
            let result = match self.session.as_mut() {
                Some(session) => self.plane.flush_next_revoke(session, now_ms()),
                None => return Err(CodexLanError::RemoteControl),
            };
            if result.is_err() {
                self.session = None;
                return Err(CodexLanError::RemoteControl);
            }
        }
        Ok(())
    }
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
    remote_control: Option<Mutex<RemoteRuntime>>,
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
        Self::start_inner(config, None)
    }

    /// Starts the provider-neutral product host through the same persisted,
    /// exact-SPKI-pinned R4 control plane used by the Codex-only wrapper.
    pub fn start_remote_controlled(
        config: StructuredLanConfig,
        remote: &CodexRemoteConfig,
    ) -> Result<Self, CodexLanError> {
        let security_root = config.data_directory.join("security");
        let endpoint_id = crate::server::persistent_remote_endpoint_id(&security_root)
            .map_err(|_| CodexLanError::RemoteControl)?;
        let mut plane = RemoteControlPlane::open(RemoteControlConfig {
            state_file: security_root.join(REMOTE_CONTROL_STATE_FILE),
            service_endpoint: remote.service_endpoint.clone(),
            service_public_key_pin: remote.service_public_key_pin.clone(),
            host_endpoint_id: endpoint_id.clone(),
            relay_url: remote.relay_url.clone(),
        })
        .map_err(|_| CodexLanError::RemoteControl)?;
        let route = plane.host_route();
        if route.host_endpoint_id() != endpoint_id || route.relay_url() != remote.relay_url {
            return Err(CodexLanError::RemoteControl);
        }
        let tunnel = crate::server::RemoteTunnelConfig {
            relay_url: route.relay_url().to_owned(),
            relay_auth_token: route.admin_token().to_owned(),
        };
        let mut initial_session = plane.connect().map_err(|_| CodexLanError::RemoteControl)?;
        plane
            .register_route(&mut initial_session, now_ms())
            .map_err(|_| CodexLanError::RemoteControl)?;
        let mut host = Self::start_inner(config, Some(tunnel))?;
        if host.remote_endpoint_id().as_deref() != Some(endpoint_id.as_str()) {
            let _ = host.shutdown();
            return Err(CodexLanError::RemoteControl);
        }
        let revoked_devices = host
            .server
            .as_ref()
            .ok_or(CodexLanError::Listener)?
            .revoked_device_ids();
        if plane
            .reconcile_local_revocations(&revoked_devices, now_ms())
            .is_err()
        {
            let _ = host.shutdown();
            return Err(CodexLanError::RemoteControl);
        }
        let mut runtime = RemoteRuntime::new(plane, initial_session);
        if runtime.refresh_presence_if_due(true).is_err() {
            let _ = host.shutdown();
            return Err(CodexLanError::RemoteControl);
        }
        host.remote_control = Some(Mutex::new(runtime));
        Ok(host)
    }

    fn start_inner(
        config: StructuredLanConfig,
        remote: Option<crate::server::RemoteTunnelConfig>,
    ) -> Result<Self, CodexLanError> {
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
            let bootstrap = match factory(context.clone()) {
                Ok(bootstrap) => bootstrap,
                Err(error) => {
                    stop_sessions(&supervisor, &session_ids);
                    return Err(error);
                }
            };
            let start = SessionStartRequest {
                project_id: bootstrap.project_id,
                project_binding_id: bootstrap.project_binding_id,
                runtime_id: bootstrap.runtime_id,
                project_root_ref: root_ref.clone(),
            };
            match supervisor.start_runtime(start, bootstrap.runtime) {
                Ok(session_id) => session_ids.push(session_id),
                Err(_) => {
                    stop_sessions(&supervisor, &session_ids);
                    return Err(CodexLanError::Runtime);
                }
            }
        }
        let recovered = supervisor.recover_all_ready();
        if recovered.iter().any(|(_, result)| result.is_err()) {
            stop_sessions(&supervisor, &session_ids);
            return Err(CodexLanError::Runtime);
        }
        let security_root = config.data_directory.join("security");
        let server_result = match remote {
            Some(remote) => LanServer::bind_with_remote(
                config.bind_address,
                &security_root,
                broker,
                Some(Arc::clone(&supervisor)),
                remote,
            ),
            None => LanServer::bind(
                config.bind_address,
                &security_root,
                broker,
                Some(Arc::clone(&supervisor)),
            ),
        };
        let server = match server_result {
            Ok(server) => server,
            Err(_) => {
                stop_sessions(&supervisor, &session_ids);
                return Err(CodexLanError::Listener);
            }
        };
        let bootstrap = match server.issue_pairing(now_ms()) {
            Ok(bootstrap) => bootstrap,
            Err(_) => {
                let _ = server.shutdown();
                stop_sessions(&supervisor, &session_ids);
                return Err(CodexLanError::Pairing);
            }
        };
        let pairing_uri = match encode_uri(&bootstrap) {
            Ok(uri) => uri,
            Err(_) => {
                let _ = server.shutdown();
                stop_sessions(&supervisor, &session_ids);
                return Err(CodexLanError::Pairing);
            }
        };
        Ok(Self {
            server: Some(server),
            supervisor,
            session_ids,
            pairing_uri,
            remote_control: None,
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
        let server = self.server.as_ref().ok_or(CodexLanError::Listener)?;
        server
            .revoke_device_durable(device_id, now_ms())
            .map_err(|_| CodexLanError::Listener)?;
        let remote_result = if let Some(remote) = &self.remote_control {
            let mut runtime = lock(remote);
            let queued = runtime
                .plane
                .enqueue_revoke_after_local_registry(device_id, now_ms())
                .map_err(|_| CodexLanError::RemoteControl);
            queued.and_then(|()| runtime.flush_pending_revokes())
        } else {
            Ok(())
        };
        server.disconnect_revoked_device(device_id);
        remote_result
    }

    pub fn issue_remote_pairing_uri(
        &self,
        device_id: &DeviceId,
    ) -> Result<RemotePairingUri, CodexLanError> {
        self.server
            .as_ref()
            .ok_or(CodexLanError::Listener)?
            .require_active_device(device_id)
            .map_err(|_| CodexLanError::Pairing)?;
        let remote = self
            .remote_control
            .as_ref()
            .ok_or(CodexLanError::RemoteControl)?;
        let mut runtime = lock(remote);
        runtime.ensure_session()?;
        let expires_at_ms = i64::try_from(REMOTE_BOOTSTRAP_LIFETIME.as_millis())
            .ok()
            .and_then(|lifetime| now_ms().checked_add(lifetime))
            .ok_or(CodexLanError::RemoteControl)?;
        let RemoteRuntime { plane, session, .. } = &mut *runtime;
        let session = session.as_mut().ok_or(CodexLanError::RemoteControl)?;
        plane
            .register_device_and_issue_pairing(session, device_id, now_ms(), expires_at_ms)
            .map_err(|_| CodexLanError::RemoteControl)
    }

    pub fn wake_remote_device(&self, device_id: &DeviceId) -> Result<(), CodexLanError> {
        let remote = self
            .remote_control
            .as_ref()
            .ok_or(CodexLanError::RemoteControl)?;
        let mut runtime = lock(remote);
        runtime.ensure_session()?;
        let RemoteRuntime { plane, session, .. } = &mut *runtime;
        let session = session.as_mut().ok_or(CodexLanError::RemoteControl)?;
        plane
            .wake_device(session, device_id, now_ms())
            .map_err(|_| CodexLanError::RemoteControl)
    }

    pub fn session_ids(&self) -> &[SessionId] {
        &self.session_ids
    }

    pub fn remote_endpoint_id(&self) -> Option<String> {
        self.server.as_ref().and_then(LanServer::remote_endpoint_id)
    }

    pub fn selected_remote_path(&self) -> Option<crate::server::SelectedRemotePath> {
        self.server
            .as_ref()
            .and_then(LanServer::selected_remote_path)
    }

    pub fn run_for(&self, duration: Duration) {
        let deadline = Instant::now().checked_add(duration);
        while deadline.is_some_and(|deadline| Instant::now() < deadline) {
            if let Some(remote) = &self.remote_control {
                let _ = lock(remote).refresh_presence_if_due(false);
            }
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
        stop_sessions(&self.supervisor, &self.session_ids);
        if let Some(server) = self.server.take() {
            let _ = server.shutdown();
        }
        self.pairing_uri.clear();
    }
}

pub struct CodexLanHost {
    server: Option<LanServer>,
    supervisor: Arc<RuntimeSupervisor>,
    session_id: SessionId,
    pairing_uri: String,
    remote_control: Option<Mutex<RemoteRuntime>>,
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
        Self::start_inner(config, None)
    }

    pub fn start_remote(
        config: &CodexLanConfig,
        remote: crate::server::RemoteTunnelConfig,
    ) -> Result<Self, CodexLanError> {
        Self::start_inner(config, Some(remote))
    }

    /// Starts the product remote path using one persisted Host endpoint, one
    /// exact-SPKI-pinned control service and one custom self-hosted relay.
    pub fn start_remote_controlled(
        config: &CodexLanConfig,
        remote: &CodexRemoteConfig,
    ) -> Result<Self, CodexLanError> {
        let security_root = config.data_directory.join("security");
        let endpoint_id = crate::server::persistent_remote_endpoint_id(&security_root)
            .map_err(|_| CodexLanError::RemoteControl)?;
        let mut plane = RemoteControlPlane::open(RemoteControlConfig {
            state_file: security_root.join(REMOTE_CONTROL_STATE_FILE),
            service_endpoint: remote.service_endpoint.clone(),
            service_public_key_pin: remote.service_public_key_pin.clone(),
            host_endpoint_id: endpoint_id.clone(),
            relay_url: remote.relay_url.clone(),
        })
        .map_err(|_| CodexLanError::RemoteControl)?;
        let route = plane.host_route();
        if route.host_endpoint_id() != endpoint_id || route.relay_url() != remote.relay_url {
            return Err(CodexLanError::RemoteControl);
        }
        let tunnel = crate::server::RemoteTunnelConfig {
            relay_url: route.relay_url().to_owned(),
            relay_auth_token: route.admin_token().to_owned(),
        };
        let mut initial_session = plane.connect().map_err(|_| CodexLanError::RemoteControl)?;
        plane
            .register_route(&mut initial_session, now_ms())
            .map_err(|_| CodexLanError::RemoteControl)?;
        let mut host = Self::start_inner(config, Some(tunnel))?;
        if host.remote_endpoint_id().as_deref() != Some(endpoint_id.as_str()) {
            let _ = host.shutdown();
            return Err(CodexLanError::RemoteControl);
        }
        let revoked_devices = host
            .server
            .as_ref()
            .ok_or(CodexLanError::Listener)?
            .revoked_device_ids();
        if plane
            .reconcile_local_revocations(&revoked_devices, now_ms())
            .is_err()
        {
            let _ = host.shutdown();
            return Err(CodexLanError::RemoteControl);
        }
        let mut runtime = RemoteRuntime::new(plane, initial_session);
        if runtime.refresh_presence_if_due(true).is_err() {
            let _ = host.shutdown();
            return Err(CodexLanError::RemoteControl);
        }
        host.remote_control = Some(Mutex::new(runtime));
        Ok(host)
    }

    fn start_inner(
        config: &CodexLanConfig,
        remote: Option<crate::server::RemoteTunnelConfig>,
    ) -> Result<Self, CodexLanError> {
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
        let security_root = config.data_directory.join("security");
        let server = match remote {
            Some(remote) => LanServer::bind_with_remote(
                config.bind_address,
                &security_root,
                broker,
                Some(Arc::clone(&supervisor)),
                remote,
            ),
            None => LanServer::bind(
                config.bind_address,
                &security_root,
                broker,
                Some(Arc::clone(&supervisor)),
            ),
        }
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
            remote_control: None,
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
        let server = self.server.as_ref().ok_or(CodexLanError::Listener)?;
        server
            .revoke_device_durable(device_id, now_ms())
            .map_err(|_| CodexLanError::Listener)?;
        let remote_result = if let Some(remote) = &self.remote_control {
            let mut runtime = lock(remote);
            let queued = runtime
                .plane
                .enqueue_revoke_after_local_registry(device_id, now_ms())
                .map_err(|_| CodexLanError::RemoteControl);
            queued.and_then(|()| runtime.flush_pending_revokes())
        } else {
            Ok(())
        };
        // Normal success reaches this point only after the Ubuntu ack, whose
        // service handler has already disconnected the outer relay endpoint.
        // On an unavailable service or outbox persistence error we still close
        // the inner connection after durably suppressing local authentication;
        // startup reconciliation repairs any remaining cross-store window.
        server.disconnect_revoked_device(device_id);
        remote_result
    }

    /// Registers an already LAN-paired DeviceId and returns a short-lived
    /// sensitive bootstrap for direct display to the local operator.
    pub fn issue_remote_pairing_uri(
        &self,
        device_id: &DeviceId,
    ) -> Result<RemotePairingUri, CodexLanError> {
        self.server
            .as_ref()
            .ok_or(CodexLanError::Listener)?
            .require_active_device(device_id)
            .map_err(|_| CodexLanError::Pairing)?;
        let remote = self
            .remote_control
            .as_ref()
            .ok_or(CodexLanError::RemoteControl)?;
        let mut runtime = lock(remote);
        runtime.ensure_session()?;
        let expires_at_ms = i64::try_from(REMOTE_BOOTSTRAP_LIFETIME.as_millis())
            .ok()
            .and_then(|lifetime| now_ms().checked_add(lifetime))
            .ok_or(CodexLanError::RemoteControl)?;
        let RemoteRuntime { plane, session, .. } = &mut *runtime;
        let session = session.as_mut().ok_or(CodexLanError::RemoteControl)?;
        plane
            .register_device_and_issue_pairing(session, device_id, now_ms(), expires_at_ms)
            .map_err(|_| CodexLanError::RemoteControl)
    }

    pub fn wake_remote_device(&self, device_id: &DeviceId) -> Result<(), CodexLanError> {
        let remote = self
            .remote_control
            .as_ref()
            .ok_or(CodexLanError::RemoteControl)?;
        let mut runtime = lock(remote);
        runtime.ensure_session()?;
        let RemoteRuntime { plane, session, .. } = &mut *runtime;
        let session = session.as_mut().ok_or(CodexLanError::RemoteControl)?;
        plane
            .wake_device(session, device_id, now_ms())
            .map_err(|_| CodexLanError::RemoteControl)
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn remote_endpoint_id(&self) -> Option<String> {
        self.server.as_ref().and_then(LanServer::remote_endpoint_id)
    }

    pub fn selected_remote_path(&self) -> Option<crate::server::SelectedRemotePath> {
        self.server
            .as_ref()
            .and_then(LanServer::selected_remote_path)
    }

    pub fn run_for(&self, duration: Duration) {
        let deadline = Instant::now().checked_add(duration);
        while deadline.is_some_and(|deadline| Instant::now() < deadline) {
            if let Some(remote) = &self.remote_control {
                let _ = lock(remote).refresh_presence_if_due(false);
            }
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
        let _ = self.supervisor.stop_session(&self.session_id);
        if let Some(server) = self.server.take() {
            let _ = server.shutdown();
        }
        self.pairing_uri.clear();
    }
}

fn stop_sessions(supervisor: &RuntimeSupervisor, session_ids: &[SessionId]) {
    for session_id in session_ids {
        let _ = supervisor.stop_session(session_id);
    }
}

fn load_or_create_identity_salt(root: &Path) -> Result<String, CodexLanError> {
    let store = PrivateFileStore::new(root.join("security").join(IDENTITY_SALT_FILE))
        .map_err(|_| CodexLanError::HostIdentity)?;
    if let Some(bytes) = store.load().map_err(|_| CodexLanError::HostIdentity)? {
        return parse_identity_salt(&bytes);
    }
    let legacy_path = root.join(IDENTITY_SALT_FILE);
    if legacy_path.exists() {
        let legacy = fs::read(&legacy_path).map_err(|_| CodexLanError::HostIdentity)?;
        let salt = parse_identity_salt(&legacy)?;
        store
            .store(salt.as_bytes())
            .map_err(|_| CodexLanError::HostIdentity)?;
        fs::remove_file(legacy_path).map_err(|_| CodexLanError::HostIdentity)?;
        return Ok(salt);
    }
    let mut random = [0_u8; 32];
    OsRng.fill_bytes(&mut random);
    let salt = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    random.fill(0);
    store
        .store(salt.as_bytes())
        .map_err(|_| CodexLanError::HostIdentity)?;
    let committed = store
        .load()
        .map_err(|_| CodexLanError::HostIdentity)?
        .ok_or(CodexLanError::HostIdentity)?;
    parse_identity_salt(&committed)
}

fn parse_identity_salt(bytes: &[u8]) -> Result<String, CodexLanError> {
    if bytes.len() != 64
        || !bytes
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(CodexLanError::HostIdentity);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| CodexLanError::HostIdentity)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::PathBuf;
    use std::time::Duration;

    use kaleido_adapter_codex::CodexSandboxMode;

    use super::{load_or_create_identity_salt, CodexLanConfig, CodexLanError, IDENTITY_SALT_FILE};

    #[test]
    fn product_runtime_diagnostics_keep_sandbox_specific_without_path_leaks() {
        let config = CodexLanConfig {
            executable: PathBuf::from(r"C:\Users\private-user\bin\codex.exe"),
            project_root: PathBuf::from(r"C:\Users\private-user\secret-project"),
            data_directory: PathBuf::from(r"C:\Users\private-user\private-data"),
            bind_address: "127.0.0.1:7443".parse().expect("loopback address"),
            sandbox: CodexSandboxMode::WorkspaceWrite,
            request_timeout: Duration::from_secs(10),
        };
        let diagnostic = format!("{config:?}");
        assert!(diagnostic.contains("project_root: \"<SANDBOX>\""));
        assert!(diagnostic.contains("executable: \"<PATH>\""));
        for forbidden in [
            "private-user",
            "secret-project",
            "private-data",
            "127.0.0.1",
        ] {
            assert!(!diagnostic.contains(forbidden));
        }
    }

    #[test]
    fn host_identity_is_owner_only_stable_and_corruption_is_fail_loud() {
        let directory = tempfile::tempdir().expect("directory");
        let first = load_or_create_identity_salt(directory.path()).expect("first identity");
        let second = load_or_create_identity_salt(directory.path()).expect("stable identity");
        assert_eq!(first, second);
        let path = directory.path().join("security").join(IDENTITY_SALT_FILE);
        std::fs::write(path, b"corrupt").expect("corrupt identity");
        assert_eq!(
            load_or_create_identity_salt(directory.path()),
            Err(CodexLanError::HostIdentity)
        );
    }

    #[test]
    fn legacy_identity_is_migrated_without_changing_the_host() {
        let directory = tempfile::tempdir().expect("directory");
        let legacy = "ab".repeat(32);
        let legacy_path = directory.path().join(IDENTITY_SALT_FILE);
        std::fs::write(&legacy_path, &legacy).expect("legacy identity");
        assert_eq!(
            load_or_create_identity_salt(directory.path()).expect("migration"),
            legacy
        );
        assert!(!legacy_path.exists());
        assert!(directory
            .path()
            .join("security")
            .join(IDENTITY_SALT_FILE)
            .exists());
    }
}
