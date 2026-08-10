//! Production Ubuntu entry point.
//!
//! The process requires a durable owner-only registry.  In the default build
//! it serves the pinned-TLS REMOTE CONTROL endpoint and a metadata-only health
//! endpoint; enabling the `iroh-server` feature additionally starts iroh 1.0.3
//! after validating explicit ACME/bind configuration. No public relay or
//! ephemeral registry is selected by this binary.

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;

use kaleido_relay::{
    ControlConnection, ControlService, FcmSendError, FcmSender, Registry, RegistryConfig,
    REMOTE_CONTROL_VERSION,
};
use kaleido_transport::remote::{RemoteControlFrame, RemoteErrorCode};
use kaleido_transport::remote_client::{read_frame, write_frame};
use kaleido_transport::tls::{server_config, TlsIdentityStore};
use rustls::{ServerConnection, StreamOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_CONTROL_CONNECTIONS: usize = 1_024;
const MAX_CONTROL_CONNECTIONS_PER_SOURCE: usize = 4;
const MAX_CONTROL_FRAMES_PER_CONNECTION: usize = 16;

#[derive(Debug, Default)]
struct ControlAdmission {
    active: Mutex<ControlAdmissionState>,
}

#[derive(Debug, Default)]
struct ControlAdmissionState {
    global: usize,
    sources: HashMap<IpAddr, usize>,
}

struct ControlLease {
    source: IpAddr,
    admission: Arc<ControlAdmission>,
}

impl std::fmt::Debug for ControlLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ControlLease([redacted source])")
    }
}

impl ControlAdmission {
    fn acquire(self: &Arc<Self>, source: IpAddr) -> Option<ControlLease> {
        let mut state = self.active.lock().ok()?;
        let source_count = state.sources.get(&source).copied().unwrap_or(0);
        if state.global >= MAX_CONTROL_CONNECTIONS
            || source_count >= MAX_CONTROL_CONNECTIONS_PER_SOURCE
        {
            return None;
        }
        state.global = state.global.saturating_add(1);
        state.sources.insert(source, source_count.saturating_add(1));
        Some(ControlLease {
            source,
            admission: Arc::clone(self),
        })
    }
}

impl Drop for ControlLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.admission.active.lock() {
            state.global = state.global.saturating_sub(1);
            if let Some(count) = state.sources.get_mut(&self.source) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.sources.remove(&self.source);
                }
            }
        }
    }
}

struct RuntimeConfig {
    registry_path: PathBuf,
    identity_path: PathBuf,
    health_addr: SocketAddr,
    control_addr: SocketAddr,
    fcm_project_id: String,
    #[cfg(feature = "iroh-server")]
    relay_http_addr: SocketAddr,
    #[cfg(feature = "iroh-server")]
    relay_https_addr: SocketAddr,
    #[cfg(feature = "iroh-server")]
    relay_quic_addr: SocketAddr,
    #[cfg(feature = "iroh-server")]
    acme_domain: String,
    #[cfg(feature = "iroh-server")]
    acme_contact: String,
}

impl fmt::Debug for RuntimeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeConfig")
            .field("registry_path", &"<configured>")
            .field("identity_path", &"<configured>")
            .field("health_addr", &self.health_addr)
            .field("control_addr", &"<configured>")
            .field("fcm_project_id", &"<configured>")
            .finish()
    }
}

impl RuntimeConfig {
    #[allow(clippy::needless_return)]
    fn from_env() -> Result<Self, &'static str> {
        let registry_path = env::var_os("KALEIDO_RELAY_REGISTRY")
            .map(PathBuf::from)
            .ok_or("KALEIDO_RELAY_REGISTRY is required")?;
        let identity_path = env::var_os("KALEIDO_RELAY_IDENTITY")
            .map(PathBuf::from)
            .ok_or("KALEIDO_RELAY_IDENTITY is required")?;
        let health_addr = parse_addr("KALEIDO_RELAY_HEALTH_ADDR", "127.0.0.1:8787")?;
        let control_addr = parse_addr("KALEIDO_RELAY_CONTROL_ADDR", "0.0.0.0:7443")?;
        let fcm_project_id =
            env::var("KALEIDO_FCM_PROJECT_ID").map_err(|_| "KALEIDO_FCM_PROJECT_ID is required")?;
        if fcm_project_id.is_empty() {
            return Err("KALEIDO_FCM_PROJECT_ID is required");
        }
        #[cfg(feature = "iroh-server")]
        {
            let relay_http_addr = parse_addr("KALEIDO_RELAY_HTTP_ADDR", "0.0.0.0:80")?;
            let relay_https_addr = parse_addr("KALEIDO_RELAY_HTTPS_ADDR", "0.0.0.0:443")?;
            let relay_quic_addr = parse_addr("KALEIDO_RELAY_QUIC_ADDR", "0.0.0.0:7842")?;
            let acme_domain = env::var("KALEIDO_RELAY_ACME_DOMAIN")
                .map_err(|_| "KALEIDO_RELAY_ACME_DOMAIN is required")?;
            let acme_contact = env::var("KALEIDO_RELAY_ACME_CONTACT")
                .map_err(|_| "KALEIDO_RELAY_ACME_CONTACT is required")?;
            if acme_domain.is_empty() || acme_contact.is_empty() {
                return Err("ACME domain and contact are required");
            }
            return Ok(Self {
                registry_path,
                identity_path,
                health_addr,
                control_addr,
                fcm_project_id,
                relay_http_addr,
                relay_https_addr,
                relay_quic_addr,
                acme_domain,
                acme_contact,
            });
        }
        #[cfg(not(feature = "iroh-server"))]
        Ok(Self {
            registry_path,
            identity_path,
            health_addr,
            control_addr,
            fcm_project_id,
        })
    }
}

fn parse_addr(name: &str, default: &str) -> Result<SocketAddr, &'static str> {
    let value = env::var(name).unwrap_or_else(|_| default.to_owned());
    value.parse().map_err(|_| "invalid socket address")
}

#[tokio::main]
async fn main() -> ExitCode {
    if env::args().any(|arg| arg == "--print-service-pin") {
        return match RuntimeConfig::from_env().and_then(|config| {
            let identity = TlsIdentityStore::new(config.identity_path)
                .and_then(|store| store.load_or_generate())
                .map_err(|_| "service identity unavailable or unsafe")?;
            identity
                .leaf_pin()
                .map(|pin| pin.encode())
                .map_err(|_| "service identity unavailable or unsafe")
        }) {
            Ok(pin) => {
                println!("{pin}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("configuration rejected: {error}");
                ExitCode::from(2)
            }
        };
    }
    if env::args().any(|arg| arg == "--print-config") {
        return match RuntimeConfig::from_env() {
            Ok(config) => {
                println!(
                    "version={} relay_mode=custom-only registry=<configured> identity=<configured> health_addr={}",
                    REMOTE_CONTROL_VERSION, config.health_addr
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("configuration rejected: {error}");
                ExitCode::from(2)
            }
        };
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("relay startup failed: {error}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<(), &'static str> {
    let config = RuntimeConfig::from_env()?;
    let registry = Registry::open(RegistryConfig::durable(config.registry_path.clone()))
        .map_err(|_| "durable registry unavailable or unsafe")?;
    let identity = TlsIdentityStore::new(config.identity_path.clone())
        .and_then(|store| store.load_or_generate())
        .map_err(|_| "service identity unavailable or unsafe")?;
    let tls = server_config(identity).map_err(|_| "service TLS unavailable")?;
    let fcm = Arc::new(
        FcmSender::from_adc(config.fcm_project_id.clone())
            .map_err(|_| "FCM credentials unavailable")?,
    );
    let control_listener =
        StdTcpListener::bind(config.control_addr).map_err(|_| "control listener unavailable")?;
    let control = Arc::new(ControlService::new(registry.clone()));
    let runtime = tokio::runtime::Handle::current();
    let admission = Arc::new(ControlAdmission::default());
    let control_task = tokio::task::spawn_blocking(move || {
        serve_control(control_listener, tls, control, fcm, runtime, admission)
    });
    let registry = Arc::new(registry);
    let health_listener = TcpListener::bind(config.health_addr)
        .await
        .map_err(|_| "health listener unavailable")?;
    let health_registry = Arc::clone(&registry);
    let health_task =
        tokio::spawn(async move { serve_health(health_listener, health_registry).await });

    #[cfg(feature = "iroh-server")]
    {
        run_iroh(config, registry).await?;
        health_task.abort();
        control_task.abort();
        Ok(())
    }

    #[cfg(not(feature = "iroh-server"))]
    {
        let _ = registry;
        tokio::select! {
            result = health_task => result.map_err(|_| "health task failed")??,
            result = control_task => result.map_err(|_| "control task failed")??,
            signal = tokio::signal::ctrl_c() => signal.map_err(|_| "shutdown signal failed")?,
        }
        Ok(())
    }
}

fn serve_control(
    listener: StdTcpListener,
    tls: Arc<rustls::ServerConfig>,
    control: Arc<ControlService>,
    fcm: Arc<FcmSender>,
    runtime: tokio::runtime::Handle,
    admission: Arc<ControlAdmission>,
) -> Result<(), &'static str> {
    for accepted in listener.incoming() {
        let socket = accepted.map_err(|_| "control accept failed")?;
        let source = socket
            .peer_addr()
            .map_err(|_| "control peer unavailable")?
            .ip();
        let Some(lease) = admission.acquire(source) else {
            continue;
        };
        let connection_tls = Arc::clone(&tls);
        let connection_control = Arc::clone(&control);
        let connection_fcm = Arc::clone(&fcm);
        let connection_runtime = runtime.clone();
        thread::Builder::new()
            .name("kaleido-remote-control".to_owned())
            .spawn(move || {
                let _ = handle_control_connection(
                    socket,
                    connection_tls,
                    connection_control,
                    connection_fcm,
                    connection_runtime,
                    lease,
                );
            })
            .map_err(|_| "control worker unavailable")?;
    }
    Ok(())
}

fn handle_control_connection(
    socket: std::net::TcpStream,
    tls: Arc<rustls::ServerConfig>,
    control: Arc<ControlService>,
    fcm: Arc<FcmSender>,
    runtime: tokio::runtime::Handle,
    _lease: ControlLease,
) -> Result<(), &'static str> {
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .and_then(|()| socket.set_write_timeout(Some(std::time::Duration::from_secs(10))))
        .map_err(|_| "control socket setup failed")?;
    let connection = ServerConnection::new(tls).map_err(|_| "control TLS failed")?;
    let mut stream = StreamOwned::new(connection, socket);
    let mut state = ControlConnection::default();
    for _ in 0..MAX_CONTROL_FRAMES_PER_CONNECTION {
        let frame = read_frame(&mut stream).map_err(|_| "control frame rejected")?;
        let mut outcome = control.handle(&mut state, frame);
        if let Some(dispatch) = outcome.wake.as_ref() {
            if let Err(error) =
                runtime.block_on(fcm.send_wake(&dispatch.address, &dispatch.payload))
            {
                if error == FcmSendError::DeleteAddress {
                    let _ = control.delete_unregistered_push(dispatch);
                }
                let code = match error {
                    FcmSendError::Retryable | FcmSendError::Transport => RemoteErrorCode::Internal,
                    FcmSendError::Credentials | FcmSendError::AuthRejected => {
                        RemoteErrorCode::AuthenticationFailed
                    }
                    FcmSendError::DeleteAddress | FcmSendError::InvalidAddress => {
                        RemoteErrorCode::RouteUnavailable
                    }
                    FcmSendError::InvalidPayload | FcmSendError::Rejected => {
                        RemoteErrorCode::Internal
                    }
                };
                outcome.response = RemoteControlFrame::RemoteError {
                    request_id: outcome.response.request_id(),
                    code,
                    retriable: matches!(error, FcmSendError::Retryable | FcmSendError::Transport),
                };
            }
        }
        write_frame(&mut stream, &outcome.response, false)
            .map_err(|_| "control response failed")?;
    }
    Ok(())
}

async fn serve_health(listener: TcpListener, _registry: Arc<Registry>) -> Result<(), &'static str> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|_| "health accept failed")?;
        tokio::spawn(async move {
            let _ = handle_health(stream).await;
        });
    }
}

async fn handle_health(mut stream: TcpStream) -> Result<(), &'static str> {
    let mut request = [0_u8; 2_048];
    let size = stream
        .read(&mut request)
        .await
        .map_err(|_| "health read failed")?;
    let request_bytes = request.get(..size).ok_or("malformed health request")?;
    let request = std::str::from_utf8(request_bytes).map_err(|_| "malformed health request")?;
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("malformed health request")?;
    let (status, body) = if path == "/healthz" {
        (
            "200 OK",
            format!(
                "{{\"status\":\"ok\",\"version\":\"{}\",\"relay_mode\":\"custom-only\"}}",
                REMOTE_CONTROL_VERSION
            ),
        )
    } else {
        ("404 Not Found", "{\"status\":\"not_found\"}".to_owned())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|_| "health write failed")
}

#[cfg(feature = "iroh-server")]
async fn run_iroh(config: RuntimeConfig, registry: Arc<Registry>) -> Result<(), &'static str> {
    use std::num::NonZeroU32;

    use iroh_relay::server::{
        AcmeConfig, CertConfig, ClientRateLimit, Limits, QuicConfig, RelayConfig, Server,
        ServerConfig, TlsConfig,
    };
    use kaleido_relay::{AdmissionLimits, IrohAccessControl, RelayAdmission};
    use rustls::crypto::ring::default_provider;

    let acme = AcmeConfig::letsencrypt(true)
        .domains(vec![config.acme_domain])
        .contact(vec![format!("mailto:{}", config.acme_contact)]);
    let provider = Arc::new(default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| "TLS 1.3 provider unavailable")?
        .with_no_client_auth();
    let cert = CertConfig::LetsEncrypt {
        acme_config: acme,
        server_config_builder: builder,
    };
    let mut relay = RelayConfig::new(config.relay_http_addr);
    relay.tls = Some(TlsConfig::new(config.relay_https_addr, cert));
    let mut limits = Limits::default();
    let bytes_per_second = NonZeroU32::new(1_048_576).ok_or("invalid relay byte rate")?;
    let mut client_rate = ClientRateLimit::new(bytes_per_second);
    client_rate.max_burst_bytes = NonZeroU32::new(262_144);
    limits.client_rx = Some(client_rate);
    relay.limits = limits;
    relay.access = Arc::new(IrohAccessControl::new(Arc::new(RelayAdmission::new(
        registry,
        AdmissionLimits::default(),
    ))));
    let mut server_config = ServerConfig::default();
    server_config.relay = Some(relay);
    server_config.quic = Some(QuicConfig::new(config.relay_quic_addr));
    let mut server = Server::spawn(server_config)
        .await
        .map_err(|_| "iroh relay failed to bind")?;
    tokio::select! {
        result = server.join() => {
            let joined = result.map_err(|_| "iroh relay supervisor failed")?;
            joined.map_err(|_| "iroh relay stopped unexpectedly")?;
        },
        signal = tokio::signal::ctrl_c() => signal.map_err(|_| "shutdown signal failed")?,
    }
    server
        .shutdown()
        .await
        .map_err(|_| "iroh relay shutdown failed")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    use super::{ControlAdmission, MAX_CONTROL_CONNECTIONS_PER_SOURCE};

    #[test]
    fn control_admission_limits_each_unauthenticated_source_and_releases_leases() {
        let admission = Arc::new(ControlAdmission::default());
        let source = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let mut leases = Vec::new();
        for _ in 0..MAX_CONTROL_CONNECTIONS_PER_SOURCE {
            leases.push(admission.acquire(source).expect("within source limit"));
        }
        assert!(admission.acquire(source).is_none());
        leases.pop();
        assert!(admission.acquire(source).is_some());
    }
}
