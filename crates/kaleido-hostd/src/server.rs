//! TLS 1.3 TCP/iroh listener and authenticated TRANSPORT 0.1 state machine.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kaleido_proto::content::ContentWriteRequest;
use kaleido_proto::host::HostReachability;
use kaleido_proto::ids::DeviceId;
use kaleido_proto::projection::ProjectionSubscribeOutcome;
use kaleido_transport::auth::{ChallengeProof, ChallengeStore, IssueChallenge};
use kaleido_transport::bootstrap::PairingBootstrap;
use kaleido_transport::control::{
    ControlFrame, ExpectedResponse, PairResponse, TransportErrorCode,
};
use kaleido_transport::error::TransportError;
use kaleido_transport::frame::{encode_control, Frame, FrameDecoder};
use kaleido_transport::limits::{ConnectionAction, ConnectionLimiter, ConnectionSession};
use kaleido_transport::registry::{AtomicFileBackend, IssuePairing, SecurityStore};
use kaleido_transport::tls::{export_server_device_auth_binding, server_config, TlsIdentityStore};
use kaleido_transport::{
    AUTH_TIMEOUT_MS, FRAME_IO_TIMEOUT_MS, HELLO_TIMEOUT_MS, MAX_FRAME_LENGTH,
    TLS_HANDSHAKE_TIMEOUT_MS, TRANSPORT_VERSION,
};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::broker::{Broker, BrokerSubscription, SubscriptionEvent};
use crate::gateway::{AuthenticatedGateway, GatewayError};
use crate::runtime::RuntimeSupervisor;

#[path = "remote_tunnel.rs"]
mod remote_tunnel;

use remote_tunnel::{RemoteAccepted, RemoteTunnelServer, RemoteTunnelStream};
pub use remote_tunnel::{RemoteTunnelConfig, RemoteTunnelError, SelectedRemotePath};

const BUSINESS_POLL: Duration = Duration::from_millis(50);
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
const AUTH_FAILURE_LIMIT: usize = 4;
const GLOBAL_AUTH_FAILURE_LIMIT: usize = 32;
const AUTH_FAILURE_RESPONSE_DELAY: Duration = Duration::from_millis(50);

type Registry = SecurityStore<AtomicFileBackend>;

#[derive(Debug, Default)]
struct AuthFailureLimiter {
    failures: BTreeMap<IpAddr, VecDeque<Instant>>,
    global_failures: VecDeque<Instant>,
}

impl AuthFailureLimiter {
    fn check(&mut self, source: IpAddr, now: Instant) -> Result<(), TransportError> {
        self.prune(source, now);
        self.prune_global(now);
        if self.global_failures.len() >= GLOBAL_AUTH_FAILURE_LIMIT
            || self
                .failures
                .get(&source)
                .is_some_and(|failures| failures.len() >= AUTH_FAILURE_LIMIT)
        {
            Err(TransportError::RateLimited)
        } else {
            Ok(())
        }
    }

    fn record_failure(&mut self, source: IpAddr, now: Instant) {
        self.prune(source, now);
        self.prune_global(now);
        self.failures.entry(source).or_default().push_back(now);
        self.global_failures.push_back(now);
    }

    fn clear(&mut self, source: IpAddr) {
        self.failures.remove(&source);
    }

    fn prune(&mut self, source: IpAddr, now: Instant) {
        let Some(failures) = self.failures.get_mut(&source) else {
            return;
        };
        while failures
            .front()
            .is_some_and(|failure| now.duration_since(*failure) >= AUTH_FAILURE_WINDOW)
        {
            failures.pop_front();
        }
        if failures.is_empty() {
            self.failures.remove(&source);
        }
    }

    fn prune_global(&mut self, now: Instant) {
        while self
            .global_failures
            .front()
            .is_some_and(|failure| now.duration_since(*failure) >= AUTH_FAILURE_WINDOW)
        {
            self.global_failures.pop_front();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LanServerError {
    #[error("the LAN socket operation failed")]
    Socket,
    #[error("the TLS connection failed")]
    Tls,
    #[error("the transport contract rejected the operation")]
    Transport,
    #[error("the canonical broker rejected the operation")]
    Broker,
    #[error("the listener worker stopped")]
    WorkerStopped,
    #[error("the remote tunnel could not start")]
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionProvenance {
    Lan,
    Remote,
}

enum ServerSocket {
    Tcp(TcpStream),
    Remote(Box<RemoteTunnelStream>),
}

impl ServerSocket {
    fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()> {
        match self {
            Self::Tcp(socket) => socket.set_nonblocking(nonblocking),
            Self::Remote(stream) => stream.set_nonblocking(nonblocking),
        }
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Tcp(socket) => socket.set_read_timeout(timeout),
            Self::Remote(stream) => stream.set_read_timeout(timeout),
        }
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Tcp(socket) => socket.set_write_timeout(timeout),
            Self::Remote(stream) => stream.set_write_timeout(timeout),
        }
    }

    fn selected_remote_path(&self) -> Option<SelectedRemotePath> {
        match self {
            Self::Tcp(_) => None,
            Self::Remote(stream) => stream.selected_path(),
        }
    }
}

impl Read for ServerSocket {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.read(buffer),
            Self::Remote(stream) => stream.read(buffer),
        }
    }
}

impl Write for ServerSocket {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.write(buffer),
            Self::Remote(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(socket) => socket.flush(),
            Self::Remote(stream) => stream.flush(),
        }
    }
}

#[derive(Debug)]
struct ServerShared {
    broker: Broker,
    runtime: Option<Arc<RuntimeSupervisor>>,
    tls: Arc<ServerConfig>,
    registry: Arc<Mutex<Registry>>,
    challenges: Arc<Mutex<ChallengeStore>>,
    limiter: Arc<Mutex<ConnectionLimiter>>,
    revoked: Arc<Mutex<BTreeSet<DeviceId>>>,
    auth_failures: Arc<Mutex<AuthFailureLimiter>>,
    shutdown: Arc<AtomicBool>,
    next_connection: Arc<AtomicU64>,
    authenticated_connections: AtomicUsize,
    authenticated_remote_paths: Arc<Mutex<BTreeMap<String, SelectedRemotePath>>>,
}

pub struct LanServer {
    local_addr: SocketAddr,
    endpoint: String,
    host_pin: String,
    broker: Broker,
    registry: Arc<Mutex<Registry>>,
    revoked: Arc<Mutex<BTreeSet<DeviceId>>>,
    shutdown: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    remote: Option<RemoteTunnelServer>,
    authenticated_remote_paths: Arc<Mutex<BTreeMap<String, SelectedRemotePath>>>,
}

impl std::fmt::Debug for LanServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LanServer")
            .field("local_addr", &"[redacted]")
            .field("endpoint", &"[redacted]")
            .field("host_pin", &"[redacted]")
            .field("listener_running", &self.listener.is_some())
            .field("remote_running", &self.remote.is_some())
            .finish_non_exhaustive()
    }
}

impl LanServer {
    pub fn bind(
        address: SocketAddr,
        storage_root: &Path,
        broker: Broker,
        runtime: Option<Arc<RuntimeSupervisor>>,
    ) -> Result<Self, LanServerError> {
        Self::bind_inner(address, storage_root, broker, runtime, None)
    }

    pub fn bind_with_remote(
        address: SocketAddr,
        storage_root: &Path,
        broker: Broker,
        runtime: Option<Arc<RuntimeSupervisor>>,
        remote_config: RemoteTunnelConfig,
    ) -> Result<Self, LanServerError> {
        Self::bind_inner(address, storage_root, broker, runtime, Some(remote_config))
    }

    fn bind_inner(
        address: SocketAddr,
        storage_root: &Path,
        broker: Broker,
        runtime: Option<Arc<RuntimeSupervisor>>,
        remote_config: Option<RemoteTunnelConfig>,
    ) -> Result<Self, LanServerError> {
        let identity = TlsIdentityStore::new(storage_root.join("tls-identity.json"))
            .map_err(map_transport)?
            .load_or_generate()
            .map_err(map_transport)?;
        let host_pin = identity.leaf_pin().map_err(map_transport)?.encode();
        let tls = server_config(identity).map_err(map_transport)?;
        let backend = AtomicFileBackend::new(storage_root.join("device-registry.json"))
            .map_err(map_transport)?;
        let registry = Arc::new(Mutex::new(
            SecurityStore::open(backend).map_err(map_transport)?,
        ));
        let listener = TcpListener::bind(address).map_err(|_| LanServerError::Socket)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| LanServerError::Socket)?;
        let local_addr = listener.local_addr().map_err(|_| LanServerError::Socket)?;
        let endpoint = endpoint_for(local_addr);
        let shutdown = Arc::new(AtomicBool::new(false));
        let revoked = Arc::new(Mutex::new(BTreeSet::new()));
        let authenticated_remote_paths = Arc::new(Mutex::new(BTreeMap::new()));
        let shared = Arc::new(ServerShared {
            broker: broker.clone(),
            runtime,
            tls,
            registry: Arc::clone(&registry),
            challenges: Arc::new(Mutex::new(ChallengeStore::default())),
            limiter: Arc::new(Mutex::new(ConnectionLimiter::default())),
            revoked: Arc::clone(&revoked),
            auth_failures: Arc::new(Mutex::new(AuthFailureLimiter::default())),
            shutdown: Arc::clone(&shutdown),
            next_connection: Arc::new(AtomicU64::new(1)),
            authenticated_connections: AtomicUsize::new(0),
            authenticated_remote_paths: Arc::clone(&authenticated_remote_paths),
        });
        let (remote, remote_receiver) = match remote_config {
            Some(config) => {
                let (server, receiver) =
                    RemoteTunnelServer::start(storage_root.join("remote"), config)
                        .map_err(|_| LanServerError::Remote)?;
                (Some(server), Some(receiver))
            }
            None => (None, None),
        };
        let listener_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("kaleido-lan-listener".to_owned())
            .spawn(move || listener_loop(listener, remote_receiver, listener_shared))
            .map_err(|_| LanServerError::WorkerStopped)?;
        Ok(Self {
            local_addr,
            endpoint,
            host_pin,
            broker,
            registry,
            revoked,
            shutdown,
            listener: Some(worker),
            remote,
            authenticated_remote_paths,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn remote_endpoint_id(&self) -> Option<String> {
        self.remote
            .as_ref()
            .map(|remote| remote.endpoint_id().to_owned())
    }

    /// Returns the best currently authenticated iroh path. Unauthenticated
    /// QUIC/TLS connections are deliberately absent from this view.
    pub fn selected_remote_path(&self) -> Option<SelectedRemotePath> {
        let paths = lock(&self.authenticated_remote_paths);
        if paths
            .values()
            .any(|path| matches!(path, SelectedRemotePath::PeerToPeer))
        {
            Some(SelectedRemotePath::PeerToPeer)
        } else if paths
            .values()
            .any(|path| matches!(path, SelectedRemotePath::Relayed))
        {
            Some(SelectedRemotePath::Relayed)
        } else {
            None
        }
    }

    pub fn issue_pairing(&self, at_ms: i64) -> Result<PairingBootstrap, LanServerError> {
        lock(&self.registry)
            .issue_pairing(IssuePairing {
                host_id: &self.broker.host_id(),
                endpoint: &self.endpoint,
                host_public_key_pin: &self.host_pin,
                now_ms: at_ms,
            })
            .map_err(map_transport)
    }

    /// Persists revocation first; live connections observe the durable marker
    /// and emit `DeviceRevoked` before closing on their next bounded poll.
    pub fn revoke_device(&self, device_id: &DeviceId, at_ms: i64) -> Result<(), LanServerError> {
        self.revoke_device_durable(device_id, at_ms)?;
        self.disconnect_revoked_device(device_id);
        Ok(())
    }

    pub(crate) fn revoke_device_durable(
        &self,
        device_id: &DeviceId,
        at_ms: i64,
    ) -> Result<(), LanServerError> {
        match lock(&self.registry).revoke_and_then(device_id, at_ms, |_| {}) {
            Ok(()) | Err(TransportError::DeviceRevoked) => Ok(()),
            Err(error) => Err(map_transport(error)),
        }
    }

    pub(crate) fn disconnect_revoked_device(&self, device_id: &DeviceId) {
        lock(&self.revoked).insert(device_id.clone());
    }

    pub(crate) fn revoked_device_ids(&self) -> Vec<DeviceId> {
        lock(&self.registry).revoked_device_ids()
    }

    pub(crate) fn require_active_device(&self, device_id: &DeviceId) -> Result<(), LanServerError> {
        lock(&self.registry)
            .device_for_auth(device_id)
            .map(|_| ())
            .map_err(map_transport)
    }

    pub fn shutdown(mut self) -> Result<(), LanServerError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), LanServerError> {
        if self.listener.is_none() {
            return Ok(());
        }
        self.shutdown.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            listener.join().map_err(|_| LanServerError::WorkerStopped)?;
        }
        if let Some(mut remote) = self.remote.take() {
            remote.stop().map_err(|_| LanServerError::Remote)?;
        }
        self.broker
            .set_lan_ready(false, now_ms())
            .map_err(|_| LanServerError::Broker)?;
        Ok(())
    }
}

pub(crate) fn persistent_remote_endpoint_id(
    security_root: &Path,
) -> Result<String, RemoteTunnelError> {
    remote_tunnel::persistent_endpoint_id(&security_root.join("remote"))
}

impl Drop for LanServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn listener_loop(
    listener: TcpListener,
    remote: Option<std::sync::mpsc::Receiver<RemoteAccepted>>,
    shared: Arc<ServerShared>,
) {
    let mut workers = Vec::new();
    while !shared.shutdown.load(Ordering::Acquire) {
        if let Some(receiver) = &remote {
            loop {
                match receiver.try_recv() {
                    Ok(accepted) => spawn_connection(
                        ServerSocket::Remote(Box::new(accepted.stream)),
                        accepted.source_ip,
                        ConnectionProvenance::Remote,
                        &shared,
                        &mut workers,
                    ),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        match listener.accept() {
            Ok((socket, peer)) => {
                spawn_connection(
                    ServerSocket::Tcp(socket),
                    peer.ip(),
                    ConnectionProvenance::Lan,
                    &shared,
                    &mut workers,
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
        let mut index = 0;
        while index < workers.len() {
            if workers.get(index).is_some_and(JoinHandle::is_finished) {
                let worker = workers.swap_remove(index);
                let _ = worker.join();
            } else {
                index += 1;
            }
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

fn spawn_connection(
    socket: ServerSocket,
    source_ip: IpAddr,
    provenance: ConnectionProvenance,
    shared: &Arc<ServerShared>,
    workers: &mut Vec<JoinHandle<()>>,
) {
    let connection = next_connection_id(&shared.next_connection);
    if lock(&shared.limiter)
        .accept(&connection, source_ip)
        .is_err()
    {
        return;
    }
    let connection_shared = Arc::clone(shared);
    let worker_connection = connection.clone();
    let worker_name = match provenance {
        ConnectionProvenance::Lan => "kaleido-lan-connection",
        ConnectionProvenance::Remote => "kaleido-remote-connection",
    };
    if let Ok(worker) = thread::Builder::new()
        .name(worker_name.to_owned())
        .spawn(move || {
            if let Err(error) = handle_connection(
                socket,
                source_ip,
                provenance,
                &worker_connection,
                &connection_shared,
            ) {
                tracing::debug!(?error, "transport connection closed");
            }
            lock(&connection_shared.challenges).cancel_connection(&worker_connection);
            let _ = lock(&connection_shared.limiter).close(&worker_connection);
        })
    {
        workers.push(worker);
    } else {
        let _ = lock(&shared.limiter).close(&connection);
    }
}

fn handle_connection(
    socket: ServerSocket,
    source_ip: IpAddr,
    provenance: ConnectionProvenance,
    connection_scope: &str,
    shared: &ServerShared,
) -> Result<(), LanServerError> {
    let mut session = ConnectionSession::accepted(now_ms()).map_err(map_transport)?;
    socket
        .set_nonblocking(true)
        .map_err(|_| LanServerError::Socket)?;
    let connection =
        ServerConnection::new(Arc::clone(&shared.tls)).map_err(|_| LanServerError::Tls)?;
    let mut stream = StreamOwned::new(connection, socket);
    let tls_deadline = phase_deadline(TLS_HANDSHAKE_TIMEOUT_MS)?;
    while stream.conn.is_handshaking() {
        ensure_before(tls_deadline)?;
        match stream.conn.complete_io(&mut stream.sock) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(2))
            }
            Err(_) => return Err(LanServerError::Tls),
        }
    }
    let tls_exporter = export_server_device_auth_binding(&stream.conn).map_err(map_transport)?;
    session.tls_established(now_ms()).map_err(map_transport)?;
    let mut last_request_id = 0;

    let transport_deadline = phase_deadline(HELLO_TIMEOUT_MS)?;
    let transport_hello = read_control_until(&mut stream, transport_deadline)?;
    let request_id = match session.accept_transport_hello(&transport_hello, now_ms()) {
        Ok(request_id) => request_id,
        Err(error) => {
            write_transport_error_until(
                &mut stream,
                hello_request_id(&transport_hello),
                &error,
                false,
                transport_deadline,
            )?;
            return Err(map_transport(error));
        }
    };
    require_monotonic_request(&mut last_request_id, request_id)?;
    write_control_until(
        &mut stream,
        &ControlFrame::TransportHelloAck {
            request_id,
            transport_version: TRANSPORT_VERSION.to_owned(),
            max_frame_length: MAX_FRAME_LENGTH,
        },
        transport_deadline,
    )?;

    let uacp_deadline = phase_deadline(HELLO_TIMEOUT_MS)?;
    let uacp_hello = read_control_until(&mut stream, uacp_deadline)?;
    let request_id = match session.accept_uacp_hello(&uacp_hello, now_ms()) {
        Ok(request_id) => request_id,
        Err(error) => {
            write_transport_error_until(
                &mut stream,
                hello_request_id(&uacp_hello),
                &error,
                false,
                uacp_deadline,
            )?;
            return Err(map_transport(error));
        }
    };
    require_monotonic_request(&mut last_request_id, request_id)?;
    write_control_until(
        &mut stream,
        &ControlFrame::UacpHelloAck {
            request_id,
            protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
        },
        uacp_deadline,
    )?;

    let auth_deadline = phase_deadline(AUTH_TIMEOUT_MS)?;
    let auth = read_control_until(&mut stream, auth_deadline)?;
    session.ensure_auth_frame(&auth).map_err(map_transport)?;
    let device_id = authenticate(
        &mut stream,
        &mut session,
        auth,
        &mut last_request_id,
        connection_scope,
        &tls_exporter,
        source_ip,
        auth_deadline,
        shared,
    )?;
    let _lan_reachability = if provenance == ConnectionProvenance::Lan {
        Some(AuthenticatedReachability::acquire(shared)?)
    } else {
        None
    };
    let remote_path = if provenance == ConnectionProvenance::Remote {
        Some(AuthenticatedRemotePath::acquire(
            shared,
            connection_scope,
            &stream.sock,
        )?)
    } else {
        None
    };
    stream
        .sock
        .set_nonblocking(false)
        .map_err(|_| LanServerError::Socket)?;
    stream
        .sock
        .set_read_timeout(Some(BUSINESS_POLL))
        .map_err(|_| LanServerError::Socket)?;
    stream
        .sock
        .set_write_timeout(Some(Duration::from_millis(
            u64::try_from(FRAME_IO_TIMEOUT_MS).map_err(|_| LanServerError::Transport)?,
        )))
        .map_err(|_| LanServerError::Socket)?;
    business_loop(
        &mut stream,
        &mut session,
        AuthenticatedGateway::new(
            device_id,
            shared.broker.clone(),
            Arc::clone(&shared.registry),
        ),
        shared,
        &mut last_request_id,
        remote_path.as_ref(),
    )
}

struct AuthenticatedReachability<'a> {
    shared: &'a ServerShared,
}

impl<'a> AuthenticatedReachability<'a> {
    fn acquire(shared: &'a ServerShared) -> Result<Self, LanServerError> {
        shared
            .authenticated_connections
            .fetch_add(1, Ordering::AcqRel);
        if publish_authenticated_reachability(shared).is_err() {
            shared
                .authenticated_connections
                .fetch_sub(1, Ordering::AcqRel);
            return Err(LanServerError::Broker);
        }
        Ok(Self { shared })
    }
}

impl Drop for AuthenticatedReachability<'_> {
    fn drop(&mut self) {
        let previous = self
            .shared
            .authenticated_connections
            .fetch_sub(1, Ordering::AcqRel);
        if previous == 1 {
            let _ = publish_authenticated_reachability(self.shared);
        }
    }
}

struct AuthenticatedRemotePath<'a> {
    shared: &'a ServerShared,
    connection_scope: &'a str,
}

impl<'a> AuthenticatedRemotePath<'a> {
    fn acquire(
        shared: &'a ServerShared,
        connection_scope: &'a str,
        socket: &ServerSocket,
    ) -> Result<Self, LanServerError> {
        let guard = Self {
            shared,
            connection_scope,
        };
        guard.refresh(socket)?;
        Ok(guard)
    }

    fn refresh(&self, socket: &ServerSocket) -> Result<(), LanServerError> {
        let selected = socket.selected_remote_path();
        let changed = {
            let mut paths = lock(&self.shared.authenticated_remote_paths);
            match selected {
                Some(path) => paths.insert(self.connection_scope.to_owned(), path) != Some(path),
                None => paths.remove(self.connection_scope).is_some(),
            }
        };
        if changed {
            publish_authenticated_reachability(self.shared)?;
        }
        Ok(())
    }
}

impl Drop for AuthenticatedRemotePath<'_> {
    fn drop(&mut self) {
        let removed = lock(&self.shared.authenticated_remote_paths).remove(self.connection_scope);
        if removed.is_some() {
            let _ = publish_authenticated_reachability(self.shared);
        }
    }
}

fn authenticated_reachability(
    lan_connections: usize,
    remote_paths: &BTreeMap<String, SelectedRemotePath>,
) -> HostReachability {
    if lan_connections > 0 {
        HostReachability::LanDirect
    } else if remote_paths
        .values()
        .any(|path| matches!(path, SelectedRemotePath::PeerToPeer))
    {
        HostReachability::PeerToPeer
    } else if remote_paths
        .values()
        .any(|path| matches!(path, SelectedRemotePath::Relayed))
    {
        HostReachability::Relayed
    } else {
        HostReachability::Offline
    }
}

fn publish_authenticated_reachability(shared: &ServerShared) -> Result<(), LanServerError> {
    let reachability = authenticated_reachability(
        shared.authenticated_connections.load(Ordering::Acquire),
        &lock(&shared.authenticated_remote_paths),
    );
    shared
        .broker
        .set_authenticated_reachability(reachability, now_ms())
        .map(|_| ())
        .map_err(|_| LanServerError::Broker)
}

#[allow(clippy::too_many_arguments)]
fn authenticate(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    session: &mut ConnectionSession,
    auth: ControlFrame,
    last_request_id: &mut u64,
    connection_scope: &str,
    tls_exporter: &[u8; 32],
    source_ip: IpAddr,
    deadline: Instant,
    shared: &ServerShared,
) -> Result<DeviceId, LanServerError> {
    if let Err(error) = lock(&shared.auth_failures).check(source_ip, Instant::now()) {
        write_transport_error_until(stream, control_request_id(&auth), &error, false, deadline)?;
        return Err(map_transport(error));
    }
    match auth {
        ControlFrame::PairRequest { request } => {
            require_monotonic_request(last_request_id, request.request_id)?;
            let device = match lock(&shared.registry).pair_device(&request, now_ms()) {
                Ok(device) => device,
                Err(error) => {
                    lock(&shared.auth_failures).record_failure(source_ip, Instant::now());
                    write_transport_error_until(
                        stream,
                        Some(request.request_id),
                        &error,
                        false,
                        deadline,
                    )?;
                    return Err(map_transport(error));
                }
            };
            if let Err(error) =
                lock(&shared.limiter).authenticate(connection_scope, &device.device_id)
            {
                write_transport_error_until(
                    stream,
                    Some(request.request_id),
                    &error,
                    true,
                    deadline,
                )?;
                return Err(map_transport(error));
            }
            let expires_at_ms = session
                .authenticate(device.device_id.clone(), now_ms())
                .map_err(map_transport)?;
            write_control_until(
                stream,
                &ControlFrame::PairResponse {
                    response: PairResponse {
                        request_id: request.request_id,
                        device_id: device.device_id.clone(),
                        host_id: shared.broker.host_id(),
                        transport_version: TRANSPORT_VERSION.to_owned(),
                        protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
                        connection_id: connection_scope.to_owned(),
                        session_expires_at_ms: expires_at_ms,
                    },
                },
                deadline,
            )?;
            lock(&shared.auth_failures).clear(source_ip);
            Ok(device.device_id)
        }
        ControlFrame::ChallengeRequest {
            request_id,
            device_id,
        } => {
            require_monotonic_request(last_request_id, request_id)?;
            // Unknown and revoked identifiers deliberately follow the same
            // challenge exchange as a known device with a bad signature.
            let device = lock(&shared.registry)
                .device_for_auth(&device_id)
                .cloned()
                .ok();
            let challenge = lock(&shared.challenges)
                .issue(IssueChallenge {
                    connection_scope,
                    request_id,
                    transport_version: TRANSPORT_VERSION,
                    protocol_version: kaleido_proto::PROTOCOL_VERSION,
                    host_id: &shared.broker.host_id(),
                    device_id: &device_id,
                    tls_exporter,
                    now_ms: now_ms(),
                })
                .map_err(map_transport)?;
            write_control_until(
                stream,
                &ControlFrame::DeviceChallenge {
                    request_id,
                    challenge_id: challenge.challenge_id.clone(),
                    nonce: challenge.nonce,
                    expires_at_ms: challenge.expires_at_ms,
                },
                deadline,
            )?;
            let proof = read_control_until(stream, deadline)?;
            let failure_not_before = Instant::now()
                .checked_add(AUTH_FAILURE_RESPONSE_DELAY)
                .ok_or(LanServerError::Transport)?;
            let ControlFrame::ChallengeProof {
                request_id: proof_request_id,
                challenge_id,
                signature_der,
            } = proof
            else {
                return authentication_failed(
                    stream,
                    source_ip,
                    request_id,
                    failure_not_before,
                    deadline,
                    shared,
                );
            };
            if proof_request_id != request_id {
                return authentication_failed(
                    stream,
                    source_ip,
                    request_id,
                    failure_not_before,
                    deadline,
                    shared,
                );
            }
            let proof = ChallengeProof {
                request_id: proof_request_id,
                challenge_id,
                signature_der,
            };
            let authenticated = device.as_ref().and_then(|device| {
                lock(&shared.challenges)
                    .verify(
                        connection_scope,
                        &proof,
                        device.public_key_spki(),
                        false,
                        now_ms(),
                    )
                    .ok()
            });
            let Some(authenticated) = authenticated else {
                // If the device was unknown the stored dummy challenge remains;
                // consume it with a deliberately invalid key before returning
                // the same external error as a bad known-device signature.
                if device.is_none() {
                    let _ = lock(&shared.challenges).verify(
                        connection_scope,
                        &proof,
                        &[],
                        false,
                        now_ms(),
                    );
                }
                return authentication_failed(
                    stream,
                    source_ip,
                    request_id,
                    failure_not_before,
                    deadline,
                    shared,
                );
            };
            if let Err(error) = lock(&shared.limiter).authenticate(connection_scope, &authenticated)
            {
                write_transport_error_until(stream, Some(request_id), &error, true, deadline)?;
                return Err(map_transport(error));
            }
            let expires_at_ms = session
                .authenticate(authenticated.clone(), now_ms())
                .map_err(map_transport)?;
            write_control_until(
                stream,
                &ControlFrame::AuthAccepted {
                    request_id,
                    connection_id: connection_scope.to_owned(),
                    expires_at_ms,
                },
                deadline,
            )?;
            lock(&shared.auth_failures).clear(source_ip);
            Ok(authenticated)
        }
        _ => {
            lock(&shared.auth_failures).record_failure(source_ip, Instant::now());
            Err(LanServerError::Transport)
        }
    }
}

fn authentication_failed(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    source_ip: IpAddr,
    request_id: u64,
    not_before: Instant,
    deadline: Instant,
    shared: &ServerShared,
) -> Result<DeviceId, LanServerError> {
    if let Some(delay) = not_before.checked_duration_since(Instant::now()) {
        thread::sleep(delay);
    }
    lock(&shared.auth_failures).record_failure(source_ip, Instant::now());
    let error = TransportError::AuthenticationFailed;
    write_transport_error_until(stream, Some(request_id), &error, false, deadline)?;
    Err(map_transport(error))
}

fn business_loop(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    session: &mut ConnectionSession,
    gateway: AuthenticatedGateway,
    shared: &ServerShared,
    last_request_id: &mut u64,
    remote_path: Option<&AuthenticatedRemotePath<'_>>,
) -> Result<(), LanServerError> {
    let mut decoder = FrameDecoder::new();
    let mut boundaries = FrameBoundaryTracker::default();
    let mut business = BusinessState {
        subscriptions: BTreeMap::new(),
        content_headers: BTreeMap::new(),
        last_request_id: *last_request_id,
        last_subscription_id: 0,
    };
    let mut partial_since: Option<Instant> = None;
    let mut next_server_request_id = 1_u64;
    loop {
        if let Some(remote_path) = remote_path {
            remote_path.refresh(&stream.sock)?;
        }
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        if lock(&shared.revoked).contains(gateway.device_id()) {
            write_control(
                stream,
                &ControlFrame::TransportError {
                    request_id: None,
                    code: TransportErrorCode::DeviceRevoked,
                    retriable: false,
                },
            )?;
            break;
        }
        publish_subscription_events(stream, session, &mut business.subscriptions)?;
        match session.poll(now_ms()) {
            Some(ConnectionAction::SendPing) => {
                let request_id = next_server_request_id;
                next_server_request_id = next_server_request_id
                    .checked_add(1)
                    .ok_or(LanServerError::Transport)?;
                session
                    .correlation_mut()
                    .begin_outgoing_request(
                        request_id,
                        ExpectedResponse::Pong { nonce: request_id },
                    )
                    .map_err(map_transport)?;
                write_control(
                    stream,
                    &ControlFrame::Ping {
                        request_id,
                        nonce: request_id,
                    },
                )?;
            }
            Some(ConnectionAction::CloseSessionExpired) => {
                write_control(
                    stream,
                    &ControlFrame::TransportError {
                        request_id: None,
                        code: TransportErrorCode::AuthenticationFailed,
                        retriable: true,
                    },
                )?;
                break;
            }
            Some(ConnectionAction::CloseTimeout) => break,
            None => {}
        }
        let mut buffer = [0_u8; 8_192];
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let bytes = buffer.get(..count).ok_or(LanServerError::Transport)?;
                let incomplete = boundaries.push(bytes)?;
                if incomplete {
                    partial_since.get_or_insert_with(Instant::now);
                } else {
                    partial_since = None;
                }
                let frames = decoder.push(bytes).map_err(map_transport)?;
                for frame in frames {
                    handle_business_frame(stream, session, &gateway, shared, &mut business, frame)?;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return Err(LanServerError::Socket),
        }
        if partial_since.is_some_and(|started| {
            started.elapsed()
                >= Duration::from_millis(u64::try_from(FRAME_IO_TIMEOUT_MS).unwrap_or(10_000))
        }) {
            return Err(LanServerError::Transport);
        }
    }
    session.close();
    stream.conn.send_close_notify();
    let _ = stream.flush();
    Ok(())
}

#[derive(Debug, Default)]
struct FrameBoundaryTracker {
    prefix: [u8; 4],
    prefix_filled: usize,
    frame_remaining: usize,
}

impl FrameBoundaryTracker {
    fn push(&mut self, mut bytes: &[u8]) -> Result<bool, LanServerError> {
        while !bytes.is_empty() {
            if self.frame_remaining > 0 {
                let take = self.frame_remaining.min(bytes.len());
                self.frame_remaining -= take;
                bytes = bytes.get(take..).ok_or(LanServerError::Transport)?;
                continue;
            }
            let needed = 4_usize.saturating_sub(self.prefix_filled);
            let take = needed.min(bytes.len());
            let target = self
                .prefix
                .get_mut(self.prefix_filled..self.prefix_filled + take)
                .ok_or(LanServerError::Transport)?;
            let source = bytes.get(..take).ok_or(LanServerError::Transport)?;
            target.copy_from_slice(source);
            self.prefix_filled += take;
            bytes = bytes.get(take..).ok_or(LanServerError::Transport)?;
            if self.prefix_filled == 4 {
                let length = u32::from_be_bytes(self.prefix);
                if length == 0 || length > MAX_FRAME_LENGTH {
                    return Err(LanServerError::Transport);
                }
                self.frame_remaining =
                    usize::try_from(length).map_err(|_| LanServerError::Transport)?;
                self.prefix = [0; 4];
                self.prefix_filled = 0;
            }
        }
        Ok(self.prefix_filled != 0 || self.frame_remaining != 0)
    }
}

struct BusinessState {
    subscriptions: BTreeMap<u64, BrokerSubscription>,
    content_headers: BTreeMap<u64, ContentWriteRequest>,
    last_request_id: u64,
    last_subscription_id: u64,
}

fn handle_business_frame(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    session: &mut ConnectionSession,
    gateway: &AuthenticatedGateway,
    shared: &ServerShared,
    business: &mut BusinessState,
    frame: Frame,
) -> Result<(), LanServerError> {
    if matches!(frame, Frame::Content { .. }) {
        let (request_id, body) = session
            .correlation_mut()
            .bind_content(frame)
            .map_err(map_transport)?;
        let request = business
            .content_headers
            .remove(&request_id)
            .ok_or(LanServerError::Transport)?;
        let response = gateway
            .write_content(&request, &body, now_ms())
            .map_err(|error| map_gateway(stream, error))?;
        session
            .correlation_mut()
            .complete_incoming_request(request_id)
            .map_err(map_transport)?;
        return write_control(
            stream,
            &ControlFrame::ContentWriteResult {
                request_id,
                response,
            },
        );
    }
    let control = frame.decode_control().map_err(map_transport)?;
    session
        .ensure_business_frame(&control, now_ms())
        .map_err(map_transport)?;
    match control {
        ControlFrame::ProjectionSubscribeFrame {
            request_id,
            subscription_id,
            subscribe,
        } => {
            begin_request(stream, session, &mut business.last_request_id, request_id)?;
            require_monotonic_request(&mut business.last_subscription_id, subscription_id)?;
            session
                .correlation_mut()
                .open_subscription(subscription_id)
                .map_err(|error| business_transport_error(stream, Some(request_id), error))?;
            let subscription = gateway
                .subscribe(&subscribe, now_ms())
                .map_err(|error| map_gateway(stream, error))?;
            let replay = subscription.replay().clone();
            write_control(
                stream,
                &ControlFrame::ProjectionSubscribeAckFrame {
                    request_id,
                    subscription_id,
                    ack: replay.ack.clone(),
                },
            )?;
            for envelope in replay.envelopes {
                write_control(
                    stream,
                    &ControlFrame::ProjectionEnvelopeFrame {
                        subscription_id,
                        envelope,
                    },
                )?;
            }
            complete_request(session, request_id)?;
            if matches!(
                replay.ack.outcome,
                ProjectionSubscribeOutcome::Rejected { .. }
            ) {
                session
                    .correlation_mut()
                    .close_subscription(subscription_id)
                    .map_err(map_transport)?;
            } else {
                business.subscriptions.insert(subscription_id, subscription);
            }
        }
        ControlFrame::UnsubscribeRequest {
            request_id,
            subscription_id,
        } => {
            begin_request(stream, session, &mut business.last_request_id, request_id)?;
            session
                .correlation_mut()
                .unsubscribe(subscription_id)
                .map_err(map_transport)?;
            if let Some(subscription) = business.subscriptions.remove(&subscription_id) {
                subscription.unsubscribe();
            }
            write_control(
                stream,
                &ControlFrame::UnsubscribeAck {
                    request_id,
                    subscription_id,
                },
            )?;
            complete_request(session, request_id)?;
        }
        ControlFrame::ContentWriteHeader {
            request_id,
            request,
        } => {
            begin_request(stream, session, &mut business.last_request_id, request_id)?;
            session
                .correlation_mut()
                .expect_content(request_id, &request)
                .map_err(map_transport)?;
            if business
                .content_headers
                .insert(request_id, request)
                .is_some()
            {
                return Err(LanServerError::Transport);
            }
        }
        ControlFrame::ContentReadFrame {
            request_id,
            request,
        } => {
            begin_request(stream, session, &mut business.last_request_id, request_id)?;
            let response = gateway
                .read_content(&request, now_ms())
                .map_err(|error| map_gateway(stream, error))?;
            write_control(
                stream,
                &ControlFrame::ContentReadResult {
                    request_id,
                    response,
                },
            )?;
            complete_request(session, request_id)?;
        }
        ControlFrame::DeviceCommandFrame {
            request_id,
            request,
        } => {
            begin_request(stream, session, &mut business.last_request_id, request_id)?;
            let admission = gateway
                .admit_command(&request, now_ms())
                .map_err(|error| map_gateway(stream, error))?;
            write_control(
                stream,
                &ControlFrame::DeviceCommandAck {
                    request_id,
                    ack: admission.ack,
                },
            )?;
            complete_request(session, request_id)?;
            if let (Some(runtime), Some(ticket)) = (&shared.runtime, admission.dispatch_ticket) {
                let _ = runtime.dispatch_ticket(&ticket);
            }
        }
        ControlFrame::Ping { request_id, nonce } => {
            begin_request(stream, session, &mut business.last_request_id, request_id)?;
            write_control(stream, &ControlFrame::Pong { request_id, nonce })?;
            complete_request(session, request_id)?;
        }
        ControlFrame::Pong { request_id, nonce } => session
            .correlation_mut()
            .accept_response(&ControlFrame::Pong { request_id, nonce })
            .map_err(map_transport)?,
        _ => return Err(LanServerError::Transport),
    }
    Ok(())
}

fn publish_subscription_events(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    session: &mut ConnectionSession,
    subscriptions: &mut BTreeMap<u64, BrokerSubscription>,
) -> Result<(), LanServerError> {
    let ids = subscriptions.keys().copied().collect::<Vec<_>>();
    for subscription_id in ids {
        loop {
            let event = subscriptions
                .get(&subscription_id)
                .and_then(|subscription| subscription.recv_timeout(Duration::ZERO));
            match event {
                Some(SubscriptionEvent::Projection(envelope)) => write_control(
                    stream,
                    &ControlFrame::ProjectionEnvelopeFrame {
                        subscription_id,
                        envelope,
                    },
                )?,
                Some(SubscriptionEvent::Closed(error)) => {
                    session
                        .correlation_mut()
                        .close_subscription_for_gap(subscription_id, &error)
                        .map_err(map_transport)?;
                    write_control(
                        stream,
                        &ControlFrame::ProjectionSubscriptionClosed {
                            subscription_id,
                            error,
                        },
                    )?;
                    subscriptions.remove(&subscription_id);
                    break;
                }
                None => break,
            }
        }
    }
    Ok(())
}

fn begin_request(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    session: &mut ConnectionSession,
    last_request_id: &mut u64,
    request_id: u64,
) -> Result<(), LanServerError> {
    require_monotonic_request(last_request_id, request_id)?;
    session
        .correlation_mut()
        .begin_incoming_request(request_id)
        .map_err(|error| business_transport_error(stream, Some(request_id), error))
}

fn business_transport_error(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    request_id: Option<u64>,
    error: TransportError,
) -> LanServerError {
    if matches!(
        error,
        TransportError::RateLimited | TransportError::TooManySubscriptions
    ) {
        let _ = write_transport_error(stream, request_id, &TransportError::RateLimited, true);
    }
    LanServerError::Transport
}

fn complete_request(
    session: &mut ConnectionSession,
    request_id: u64,
) -> Result<(), LanServerError> {
    session
        .correlation_mut()
        .complete_incoming_request(request_id)
        .map_err(map_transport)
}

fn read_control_until(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    deadline: Instant,
) -> Result<ControlFrame, LanServerError> {
    let mut header = [0_u8; 5];
    read_exact_until(stream, &mut header, deadline)?;
    let mut decoder = FrameDecoder::new();
    let initial = decoder.push(&header).map_err(map_transport)?;
    if !initial.is_empty() {
        return Err(LanServerError::Transport);
    }
    let prefix = header
        .get(..4)
        .ok_or(LanServerError::Transport)?
        .try_into()
        .map_err(|_| LanServerError::Transport)?;
    let length = u32::from_be_bytes(prefix);
    let body_length = length.checked_sub(1).ok_or(LanServerError::Transport)?;
    let body_length = usize::try_from(body_length).map_err(|_| LanServerError::Transport)?;
    let mut body = vec![0_u8; body_length];
    read_exact_until(stream, &mut body, deadline)?;
    let mut frames = decoder.push(&body).map_err(map_transport)?;
    decoder.finish().map_err(map_transport)?;
    if frames.len() != 1 {
        return Err(LanServerError::Transport);
    }
    frames
        .pop()
        .ok_or(LanServerError::Transport)?
        .decode_control()
        .map_err(map_transport)
}

fn read_exact_until(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), LanServerError> {
    let mut read = 0;
    while read < bytes.len() {
        ensure_before(deadline)?;
        let remaining = bytes.get_mut(read..).ok_or(LanServerError::Transport)?;
        match stream.read(remaining) {
            Ok(0) => return Err(LanServerError::Socket),
            Ok(count) => {
                read = read.checked_add(count).ok_or(LanServerError::Transport)?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(2))
            }
            Err(_) => return Err(LanServerError::Socket),
        }
    }
    Ok(())
}

fn write_control_until(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    frame: &ControlFrame,
    deadline: Instant,
) -> Result<(), LanServerError> {
    let bytes = encode_control(frame).map_err(map_transport)?;
    let mut written = 0;
    while written < bytes.len() {
        ensure_before(deadline)?;
        stream
            .sock
            .set_write_timeout(Some(remaining_duration(deadline)?))
            .map_err(|_| LanServerError::Socket)?;
        let remaining = bytes.get(written..).ok_or(LanServerError::Transport)?;
        match stream.write(remaining) {
            Ok(0) => return Err(LanServerError::Socket),
            Ok(count) => {
                written = written
                    .checked_add(count)
                    .ok_or(LanServerError::Transport)?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(2))
            }
            Err(_) => return Err(LanServerError::Socket),
        }
    }
    loop {
        ensure_before(deadline)?;
        stream
            .sock
            .set_write_timeout(Some(remaining_duration(deadline)?))
            .map_err(|_| LanServerError::Socket)?;
        match stream.flush() {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(2))
            }
            Err(_) => return Err(LanServerError::Socket),
        }
    }
}

fn phase_deadline(timeout_ms: i64) -> Result<Instant, LanServerError> {
    let timeout = u64::try_from(timeout_ms).map_err(|_| LanServerError::Transport)?;
    Instant::now()
        .checked_add(Duration::from_millis(timeout))
        .ok_or(LanServerError::Transport)
}

fn ensure_before(deadline: Instant) -> Result<(), LanServerError> {
    if Instant::now() < deadline {
        Ok(())
    } else {
        Err(LanServerError::Transport)
    }
}

fn remaining_duration(deadline: Instant) -> Result<Duration, LanServerError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(LanServerError::Transport)
}

fn write_control(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    frame: &ControlFrame,
) -> Result<(), LanServerError> {
    write_control_until(stream, frame, phase_deadline(FRAME_IO_TIMEOUT_MS)?)
}

fn write_transport_error(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    request_id: Option<u64>,
    error: &TransportError,
    retriable: bool,
) -> Result<(), LanServerError> {
    write_control(
        stream,
        &ControlFrame::TransportError {
            request_id,
            code: error_code(error),
            retriable,
        },
    )
}

fn write_transport_error_until(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    request_id: Option<u64>,
    error: &TransportError,
    retriable: bool,
    deadline: Instant,
) -> Result<(), LanServerError> {
    write_control_until(
        stream,
        &ControlFrame::TransportError {
            request_id,
            code: error_code(error),
            retriable,
        },
        deadline,
    )
}

fn error_code(error: &TransportError) -> TransportErrorCode {
    match error {
        TransportError::VersionMismatch => TransportErrorCode::VersionMismatch,
        TransportError::MalformedFrame => TransportErrorCode::MalformedFrame,
        TransportError::FrameTooLarge => TransportErrorCode::FrameTooLarge,
        TransportError::RateLimited => TransportErrorCode::RateLimited,
        TransportError::PairingInvalid => TransportErrorCode::PairingInvalid,
        TransportError::AuthenticationFailed => TransportErrorCode::AuthenticationFailed,
        TransportError::ChallengeExpired => TransportErrorCode::ChallengeExpired,
        TransportError::ChallengeReplayed => TransportErrorCode::ChallengeReplayed,
        TransportError::DeviceRevoked => TransportErrorCode::DeviceRevoked,
        TransportError::TooManyConnections => TransportErrorCode::TooManyConnections,
        TransportError::TooManySubscriptions => TransportErrorCode::RateLimited,
        TransportError::Persistence
        | TransportError::InvalidKeyMaterial
        | TransportError::InsecurePermissions
        | TransportError::Internal
        | TransportError::TimeOverflow => TransportErrorCode::Internal,
    }
}

fn require_monotonic_request(last: &mut u64, request_id: u64) -> Result<(), LanServerError> {
    if request_id == 0 || request_id <= *last {
        return Err(LanServerError::Transport);
    }
    *last = request_id;
    Ok(())
}

fn control_request_id(frame: &ControlFrame) -> Option<u64> {
    match frame {
        ControlFrame::PairRequest { request } => Some(request.request_id),
        ControlFrame::ChallengeRequest { request_id, .. }
        | ControlFrame::ChallengeProof { request_id, .. } => Some(*request_id),
        _ => None,
    }
}

fn hello_request_id(frame: &ControlFrame) -> Option<u64> {
    match frame {
        ControlFrame::TransportHello { request_id, .. }
        | ControlFrame::UacpHello { request_id, .. } => Some(*request_id),
        _ => None,
    }
}

fn next_connection_id(counter: &AtomicU64) -> String {
    format!("conn-{}", counter.fetch_add(1, Ordering::Relaxed))
}

fn endpoint_for(address: SocketAddr) -> String {
    match address {
        SocketAddr::V4(address) => address.to_string(),
        SocketAddr::V6(address) => format!("[{}]:{}", address.ip(), address.port()),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn map_transport(_error: TransportError) -> LanServerError {
    LanServerError::Transport
}

fn map_gateway(
    stream: &mut StreamOwned<ServerConnection, ServerSocket>,
    error: GatewayError,
) -> LanServerError {
    match error {
        GatewayError::DeviceRevoked => {
            let _ = write_control(
                stream,
                &ControlFrame::TransportError {
                    request_id: None,
                    code: TransportErrorCode::DeviceRevoked,
                    retriable: false,
                },
            );
            LanServerError::Transport
        }
        GatewayError::Broker => LanServerError::Broker,
    }
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

    use kaleido_transport::control::ControlFrame;
    use kaleido_transport::frame::encode_control;

    use kaleido_proto::host::HostReachability;

    use super::{authenticated_reachability, FrameBoundaryTracker, SelectedRemotePath};

    #[test]
    fn a_complete_frame_followed_by_a_partial_frame_remains_timed() {
        let first = encode_control(&ControlFrame::Ping {
            request_id: 1,
            nonce: 1,
        })
        .expect("first frame");
        let second = encode_control(&ControlFrame::Ping {
            request_id: 2,
            nonce: 2,
        })
        .expect("second frame");
        let mut combined = first;
        combined.extend_from_slice(second.get(..2).expect("partial prefix"));
        let mut boundaries = FrameBoundaryTracker::default();
        assert!(boundaries.push(&combined).expect("valid boundaries"));
        assert!(!boundaries
            .push(second.get(2..).expect("rest of frame"))
            .expect("complete second frame"));
    }

    #[test]
    fn authenticated_path_priority_is_lan_then_peer_then_relay() {
        let mut paths = std::collections::BTreeMap::new();
        assert_eq!(
            authenticated_reachability(0, &paths),
            HostReachability::Offline
        );
        paths.insert("relay".to_owned(), SelectedRemotePath::Relayed);
        assert_eq!(
            authenticated_reachability(0, &paths),
            HostReachability::Relayed
        );
        paths.insert("peer".to_owned(), SelectedRemotePath::PeerToPeer);
        assert_eq!(
            authenticated_reachability(0, &paths),
            HostReachability::PeerToPeer
        );
        assert_eq!(
            authenticated_reachability(1, &paths),
            HostReachability::LanDirect
        );
    }
}
