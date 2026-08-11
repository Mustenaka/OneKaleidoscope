//! Persistent host iroh endpoint and blocking accepted-stream adapter.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use iroh::endpoint::{presets, QuicTransportConfig, TransportAddrUsage, VarInt};
use iroh::{Endpoint, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey};
use kaleido_transport::private_file::PrivateFileStore;
use tokio::io::AsyncWriteExt;
use zeroize::Zeroize;

pub const KALEIDO_REMOTE_ALPN: &[u8] = b"onekaleidoscope/transport/0.1";

const IDENTITY_FILE: &str = "iroh-endpoint.key";
const ACCEPT_POLL: Duration = Duration::from_millis(25);
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);
const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_QUEUE: usize = 16;

#[derive(PartialEq, Eq)]
pub struct RemoteTunnelConfig {
    pub relay_url: String,
    pub relay_auth_token: String,
}

impl std::fmt::Debug for RemoteTunnelConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteTunnelConfig([redacted])")
    }
}

impl Drop for RemoteTunnelConfig {
    fn drop(&mut self) {
        self.relay_auth_token.zeroize();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RemoteTunnelError {
    #[error("remote tunnel configuration was rejected")]
    Configuration,
    #[error("persistent remote endpoint identity is unavailable")]
    Identity,
    #[error("remote tunnel runtime is unavailable")]
    Runtime,
    #[error("remote tunnel endpoint failed")]
    Endpoint,
    #[error("remote tunnel listener stopped")]
    Listener,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedRemotePath {
    PeerToPeer,
    Relayed,
}

pub struct RemoteAccepted {
    pub stream: RemoteTunnelStream,
    pub source_ip: IpAddr,
}

impl std::fmt::Debug for RemoteAccepted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteAccepted([redacted endpoint])")
    }
}

pub struct RemoteTunnelServer {
    endpoint_id: String,
    endpoint: Endpoint,
    shutdown: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for RemoteTunnelServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteTunnelServer")
            .field("endpoint_id", &"[redacted]")
            .field("listener_running", &self.listener.is_some())
            .finish()
    }
}

impl RemoteTunnelServer {
    pub fn start(
        storage_root: PathBuf,
        config: RemoteTunnelConfig,
    ) -> Result<(Self, Receiver<RemoteAccepted>), RemoteTunnelError> {
        let secret = persistent_secret(&storage_root)?;
        let endpoint_id = secret.public().to_string();
        let relay_map = custom_relay_map(&config.relay_url, &config.relay_auth_token)?;
        let runtime = Arc::new(build_runtime()?);
        let endpoint = runtime
            .block_on(
                Endpoint::builder(presets::Minimal)
                    .secret_key(secret)
                    .alpns(vec![KALEIDO_REMOTE_ALPN.to_vec()])
                    .clear_address_lookup()
                    .relay_mode(RelayMode::Custom(relay_map))
                    .max_tls_tickets(0)
                    .transport_config(server_transport_config())
                    .bind(),
            )
            .map_err(|_| RemoteTunnelError::Endpoint)?;
        if runtime
            .block_on(tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online()))
            .is_err()
        {
            runtime.block_on(endpoint.close());
            return Err(RemoteTunnelError::Endpoint);
        }
        let (sender, receiver) = mpsc::sync_channel(ACCEPT_QUEUE);
        let shutdown = Arc::new(AtomicBool::new(false));
        let listener_endpoint = endpoint.clone();
        let listener_shutdown = Arc::clone(&shutdown);
        let listener_runtime = Arc::clone(&runtime);
        let listener = thread::Builder::new()
            .name("kaleido-iroh-listener".to_owned())
            .spawn(move || {
                listener_runtime.block_on(accept_loop(
                    listener_endpoint,
                    listener_shutdown,
                    sender,
                    listener_runtime.clone(),
                ));
            })
            .map_err(|_| RemoteTunnelError::Listener)?;
        Ok((
            Self {
                endpoint_id,
                endpoint,
                shutdown,
                listener: Some(listener),
                runtime,
            },
            receiver,
        ))
    }

    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    pub fn stop(&mut self) -> Result<(), RemoteTunnelError> {
        self.shutdown.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            listener.join().map_err(|_| RemoteTunnelError::Listener)?;
        }
        self.runtime.block_on(self.endpoint.close());
        Ok(())
    }
}

/// Loads or creates the exact persistent endpoint identity used by
/// `RemoteTunnelServer::start`, without opening a socket. This lets the host
/// register a route bound to that identity before publishing presence.
pub(crate) fn persistent_endpoint_id(storage_root: &Path) -> Result<String, RemoteTunnelError> {
    persistent_secret(storage_root).map(|secret| secret.public().to_string())
}

fn persistent_secret(storage_root: &Path) -> Result<SecretKey, RemoteTunnelError> {
    load_or_create_secret(storage_root.join(IDENTITY_FILE))
}

impl Drop for RemoteTunnelServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

async fn accept_loop(
    endpoint: Endpoint,
    shutdown: Arc<AtomicBool>,
    sender: SyncSender<RemoteAccepted>,
    runtime: Arc<tokio::runtime::Runtime>,
) {
    while !shutdown.load(Ordering::Acquire) {
        let incoming = match tokio::time::timeout(ACCEPT_POLL, endpoint.accept()).await {
            Ok(Some(incoming)) => incoming,
            Ok(None) => break,
            Err(_) => continue,
        };
        let accepted_sender = sender.clone();
        let accepted_runtime = Arc::clone(&runtime);
        let accepted_endpoint = endpoint.clone();
        tokio::spawn(async move {
            let Ok(accepting) = incoming.accept() else {
                return;
            };
            // Awaiting the full handshake is intentional: never call into_0rtt.
            let Ok(Ok(connection)) = tokio::time::timeout(STREAM_TIMEOUT, accepting).await else {
                return;
            };
            let remote_id = connection.remote_id();
            let Ok(Ok((send, recv))) =
                tokio::time::timeout(STREAM_TIMEOUT, connection.accept_bi()).await
            else {
                return;
            };
            let stream = RemoteTunnelStream::new(
                send,
                recv,
                connection,
                accepted_endpoint,
                remote_id,
                Arc::clone(&accepted_runtime),
            );
            let accepted = RemoteAccepted {
                stream,
                source_ip: endpoint_source_bucket(remote_id),
            };
            let _ = accepted_sender.try_send(accepted);
        });
    }
}

#[derive(Debug, Clone, Copy)]
struct Timeouts {
    read: Option<Duration>,
    write: Option<Duration>,
    nonblocking: bool,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            read: Some(STREAM_TIMEOUT),
            write: Some(STREAM_TIMEOUT),
            nonblocking: false,
        }
    }
}

pub struct RemoteTunnelStream {
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    connection: iroh::endpoint::Connection,
    endpoint: Endpoint,
    remote_id: EndpointId,
    timeouts: Mutex<Timeouts>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for RemoteTunnelStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteTunnelStream(..)")
    }
}

impl RemoteTunnelStream {
    fn new(
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
        connection: iroh::endpoint::Connection,
        endpoint: Endpoint,
        remote_id: EndpointId,
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        Self {
            send,
            recv,
            connection,
            endpoint,
            remote_id,
            timeouts: Mutex::new(Timeouts::default()),
            runtime,
        }
    }

    pub fn selected_path(&self) -> Option<SelectedRemotePath> {
        self.runtime
            .block_on(selected_path(&self.endpoint, self.remote_id))
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let mut timeouts = self
            .timeouts
            .lock()
            .map_err(|_| io::Error::other("remote tunnel timeout state unavailable"))?;
        timeouts.nonblocking = nonblocking;
        Ok(())
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let mut timeouts = self
            .timeouts
            .lock()
            .map_err(|_| io::Error::other("remote tunnel timeout state unavailable"))?;
        timeouts.read = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let mut timeouts = self
            .timeouts
            .lock()
            .map_err(|_| io::Error::other("remote tunnel timeout state unavailable"))?;
        timeouts.write = timeout;
        Ok(())
    }

    fn timeouts(&self) -> io::Result<Timeouts> {
        self.timeouts
            .lock()
            .map(|timeouts| *timeouts)
            .map_err(|_| io::Error::other("remote tunnel timeout state unavailable"))
    }
}

impl Read for RemoteTunnelStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let timeouts = self.timeouts()?;
        let timeout = if timeouts.nonblocking {
            Some(Duration::from_millis(1))
        } else {
            timeouts.read
        };
        let read = async { self.recv.read(buffer).await };
        match timeout {
            Some(timeout) => match self.runtime.block_on(tokio::time::timeout(timeout, read)) {
                Ok(Ok(Some(count))) => Ok(count),
                Ok(Ok(None)) => Ok(0),
                Ok(Err(_)) => Err(io::Error::other("remote tunnel read failed")),
                Err(_) if timeouts.nonblocking => Err(io::ErrorKind::WouldBlock.into()),
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            },
            None => match self.runtime.block_on(read) {
                Ok(Some(count)) => Ok(count),
                Ok(None) => Ok(0),
                Err(_) => Err(io::Error::other("remote tunnel read failed")),
            },
        }
    }
}

impl Write for RemoteTunnelStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let timeouts = self.timeouts()?;
        let timeout = if timeouts.nonblocking {
            Some(Duration::from_millis(1))
        } else {
            timeouts.write
        };
        let write = async { self.send.write(buffer).await };
        match timeout {
            Some(timeout) => match self.runtime.block_on(tokio::time::timeout(timeout, write)) {
                Ok(Ok(count)) => Ok(count),
                Ok(Err(_)) => Err(io::Error::other("remote tunnel write failed")),
                Err(_) if timeouts.nonblocking => Err(io::ErrorKind::WouldBlock.into()),
                Err(_) => Err(io::ErrorKind::TimedOut.into()),
            },
            None => self
                .runtime
                .block_on(write)
                .map_err(|_| io::Error::other("remote tunnel write failed")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let timeouts = self.timeouts()?;
        let flush = async { self.send.flush().await };
        match timeouts.write {
            Some(timeout) => self
                .runtime
                .block_on(tokio::time::timeout(timeout, flush))
                .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
                .map_err(|_| io::Error::other("remote tunnel flush failed")),
            None => self
                .runtime
                .block_on(flush)
                .map_err(|_| io::Error::other("remote tunnel flush failed")),
        }
    }
}

impl Drop for RemoteTunnelStream {
    fn drop(&mut self) {
        let _ = self.send.finish();
        self.connection.close(0_u32.into(), b"");
    }
}

fn build_runtime() -> Result<tokio::runtime::Runtime, RemoteTunnelError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| RemoteTunnelError::Runtime)
}

fn server_transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .build()
}

fn custom_relay_map(
    relay_url: &str,
    relay_auth_token: &str,
) -> Result<RelayMap, RemoteTunnelError> {
    validate_self_hosted_relay(relay_url)?;
    validate_auth_token(relay_auth_token)?;
    let relay_url = relay_url
        .parse::<RelayUrl>()
        .map_err(|_| RemoteTunnelError::Configuration)?;
    Ok(RelayMap::from(relay_url).with_auth_token(relay_auth_token.to_owned()))
}

fn validate_self_hosted_relay(value: &str) -> Result<(), RemoteTunnelError> {
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("https://")
        || lower.contains("staging")
        || lower.contains("n0.computer")
        || lower.contains("n0.iroh")
        || lower.contains("iroh.link")
        || value.bytes().any(|byte| {
            byte.is_ascii_whitespace() || byte.is_ascii_control() || matches!(byte, b'@' | b'#')
        })
    {
        return Err(RemoteTunnelError::Configuration);
    }
    Ok(())
}

fn validate_auth_token(value: &str) -> Result<(), RemoteTunnelError> {
    if value.len() != 43
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RemoteTunnelError::Configuration);
    }
    Ok(())
}

fn load_or_create_secret(path: PathBuf) -> Result<SecretKey, RemoteTunnelError> {
    let store = PrivateFileStore::new(path).map_err(|_| RemoteTunnelError::Identity)?;
    if let Some(bytes) = store.load().map_err(|_| RemoteTunnelError::Identity)? {
        let secret: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| RemoteTunnelError::Identity)?;
        return Ok(SecretKey::from_bytes(&secret));
    }
    let generated = SecretKey::generate();
    let mut bytes = generated.to_bytes();
    let stored = store.store(&bytes);
    bytes.zeroize();
    stored.map_err(|_| RemoteTunnelError::Identity)?;
    let committed = store
        .load()
        .map_err(|_| RemoteTunnelError::Identity)?
        .ok_or(RemoteTunnelError::Identity)?;
    let secret: [u8; 32] = committed
        .as_slice()
        .try_into()
        .map_err(|_| RemoteTunnelError::Identity)?;
    Ok(SecretKey::from_bytes(&secret))
}

async fn selected_path(endpoint: &Endpoint, remote_id: EndpointId) -> Option<SelectedRemotePath> {
    let info = endpoint.remote_info(remote_id).await?;
    let mut relay_active = false;
    for address in info.addrs() {
        if !matches!(address.usage(), TransportAddrUsage::Active) {
            continue;
        }
        if address.addr().is_ip() {
            return Some(SelectedRemotePath::PeerToPeer);
        }
        if address.addr().is_relay() {
            relay_active = true;
        }
    }
    relay_active.then_some(SelectedRemotePath::Relayed)
}

fn endpoint_source_bucket(endpoint_id: EndpointId) -> IpAddr {
    let octets = endpoint_id
        .as_bytes()
        .get(..16)
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or([0_u8; 16]);
    IpAddr::V6(Ipv6Addr::from(octets))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::time::Duration;

    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMode, SecretKey};

    use super::{
        custom_relay_map, persistent_endpoint_id, selected_path, server_transport_config,
        RemoteTunnelError, SelectedRemotePath, IDENTITY_FILE, KALEIDO_REMOTE_ALPN,
    };

    #[test]
    fn host_identity_is_durable_and_corruption_is_fail_loud() {
        let directory = tempfile::tempdir().expect("identity directory");
        let root = directory.path().join("remote");
        let path = root.join(IDENTITY_FILE);
        let first = persistent_endpoint_id(&root).expect("first identity");
        let second = persistent_endpoint_id(&root).expect("stable identity");
        assert_eq!(first, second);
        std::fs::write(path, b"corrupt").expect("corrupt identity");
        assert_eq!(
            persistent_endpoint_id(&root).err(),
            Some(RemoteTunnelError::Identity)
        );
    }

    #[test]
    fn product_relay_configuration_rejects_public_and_missing_auth() {
        let token = "A".repeat(43);
        for public in [
            "https://use1.relay.n0.iroh.link",
            "https://relay.n0.computer",
            "https://staging.relay.example.test",
        ] {
            assert_eq!(
                custom_relay_map(public, &token).err(),
                Some(RemoteTunnelError::Configuration)
            );
        }
        assert_eq!(
            custom_relay_map("https://relay.example.test", "").err(),
            Some(RemoteTunnelError::Configuration)
        );
        assert!(custom_relay_map("https://relay.example.test", &token).is_ok());
    }

    #[test]
    fn real_loopback_tunnel_is_bidirectional_and_binds_identity_and_alpn() {
        let runtime = super::build_runtime().expect("runtime");
        runtime.block_on(async {
            let host = Endpoint::builder(presets::Minimal)
                .secret_key(SecretKey::generate())
                .alpns(vec![KALEIDO_REMOTE_ALPN.to_vec()])
                .relay_mode(RelayMode::Disabled)
                .max_tls_tickets(0)
                .transport_config(server_transport_config())
                .bind()
                .await
                .expect("host endpoint");
            let client = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .max_tls_tickets(0)
                .bind()
                .await
                .expect("client endpoint");

            let host_addr = host.addr();
            let accepting_host = host.clone();
            let accept = tokio::spawn(async move {
                let incoming = accepting_host.accept().await.expect("incoming");
                let connection = incoming
                    .accept()
                    .expect("accept handshake")
                    .await
                    .expect("full handshake");
                let (mut send, mut recv) = connection.accept_bi().await.expect("one stream");
                let mut request = [0_u8; 4];
                recv.read_exact(&mut request).await.expect("request");
                assert_eq!(&request, b"ping");
                send.write_all(b"pong").await.expect("response");
                send.finish().expect("finish response");
                send.stopped().await.expect("response acknowledged");
            });
            let connection = client
                .connect(host_addr.clone(), KALEIDO_REMOTE_ALPN)
                .await
                .expect("pinned endpoint connection");
            let (mut send, mut recv) = connection.open_bi().await.expect("one stream");
            send.write_all(b"ping").await.expect("request");
            send.finish().expect("finish request");
            let mut response = [0_u8; 4];
            recv.read_exact(&mut response).await.expect("response");
            assert_eq!(&response, b"pong");
            assert_eq!(
                selected_path(&host, client.id()).await,
                Some(SelectedRemotePath::PeerToPeer)
            );
            tokio::time::timeout(Duration::from_secs(2), accept)
                .await
                .expect("bounded accept")
                .expect("accept task");

            let mut wrong_identity = host_addr.clone();
            wrong_identity.id = SecretKey::generate().public();
            let rejecting_host = host.clone();
            let reject = tokio::spawn(async move {
                let incoming = rejecting_host
                    .accept()
                    .await
                    .expect("wrong identity incoming");
                let accepting = incoming.accept().expect("start wrong identity handshake");
                assert!(accepting.await.is_err());
            });
            assert!(client
                .connect(wrong_identity, KALEIDO_REMOTE_ALPN)
                .await
                .is_err());
            tokio::time::timeout(Duration::from_secs(2), reject)
                .await
                .expect("bounded identity rejection")
                .expect("identity rejection task");

            let rejecting_host = host.clone();
            let reject = tokio::spawn(async move {
                let incoming = rejecting_host.accept().await.expect("wrong ALPN incoming");
                if let Ok(accepting) = incoming.accept() {
                    assert!(accepting.await.is_err());
                }
            });
            assert!(client.connect(host_addr, b"wrong/alpn").await.is_err());
            tokio::time::timeout(Duration::from_secs(2), reject)
                .await
                .expect("bounded ALPN rejection")
                .expect("ALPN rejection task");

            client.close().await;
            host.close().await;
        });
    }
}
