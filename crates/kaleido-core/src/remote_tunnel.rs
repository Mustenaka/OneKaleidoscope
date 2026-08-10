//! Blocking adapter for one authenticated iroh bidirectional stream.
//!
//! The adapter deliberately exposes only `Read`/`Write` semantics to the
//! existing inner rustls client.  Product construction always uses one
//! configured self-hosted relay map and never invokes iroh's 0-RTT APIs.

use std::io::{self, Read, Write};
#[cfg(target_os = "android")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{presets, QuicTransportConfig, TransportAddrUsage, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl};
use tokio::io::AsyncWriteExt;

pub const KALEIDO_REMOTE_ALPN: &[u8] = b"onekaleidoscope/transport/0.1";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const PATH_WAIT: Duration = Duration::from_secs(5);
const PATH_POLL: Duration = Duration::from_millis(20);

#[cfg(target_os = "android")]
static ANDROID_JNI_CONTEXT_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Marks the process-lifetime Android JNI context as installed.
///
/// The Android JNI adapter must call `iroh::dns::install_android_jni_context`
/// with a valid `JavaVM` and application-context global reference before
/// invoking this function.  Keeping the unsafe pointer boundary out of this
/// module lets remote endpoint construction fail closed instead of using
/// iroh's public fallback DNS servers.
#[cfg(target_os = "android")]
pub(crate) fn mark_android_jni_context_installed() {
    ANDROID_JNI_CONTEXT_INSTALLED.store(true, Ordering::Release);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedRemotePath {
    PeerToPeer,
    Relayed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RemoteTunnelError {
    #[error("remote tunnel configuration was rejected")]
    Configuration,
    #[error("remote tunnel runtime is unavailable")]
    Runtime,
    #[error("remote tunnel connection failed")]
    Connection,
    #[error("remote tunnel selected path is unavailable")]
    PathUnavailable,
}

#[derive(Debug, Clone, Copy)]
struct Timeouts {
    read: Option<Duration>,
    write: Option<Duration>,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            read: Some(IO_TIMEOUT),
            write: Some(IO_TIMEOUT),
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
    // Declared last so every iroh handle is dropped before the runtime.
    runtime: Arc<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for RemoteTunnelStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteTunnelStream(..)")
    }
}

impl RemoteTunnelStream {
    pub fn connect(
        host_endpoint_id: &str,
        relay_url: &str,
        relay_auth_token: &str,
    ) -> Result<Self, RemoteTunnelError> {
        #[cfg(target_os = "android")]
        if !ANDROID_JNI_CONTEXT_INSTALLED.load(Ordering::Acquire) {
            return Err(RemoteTunnelError::Runtime);
        }

        let remote_id = host_endpoint_id
            .parse::<EndpointId>()
            .map_err(|_| RemoteTunnelError::Configuration)?;
        let (relay_url, relay_map) = custom_relay_map(relay_url, relay_auth_token)?;
        let runtime = Arc::new(build_runtime()?);
        let endpoint = runtime
            .block_on(
                Endpoint::builder(presets::Minimal)
                    .clear_address_lookup()
                    .relay_mode(RelayMode::Custom(relay_map))
                    .max_tls_tickets(0)
                    .transport_config(client_transport_config())
                    .bind(),
            )
            .map_err(|_| RemoteTunnelError::Connection)?;
        let address = EndpointAddr::new(remote_id).with_relay_url(relay_url);
        let connection = runtime
            .block_on(tokio::time::timeout(
                CONNECT_TIMEOUT,
                endpoint.connect(address, KALEIDO_REMOTE_ALPN),
            ))
            .map_err(|_| RemoteTunnelError::Connection)?
            .map_err(|_| RemoteTunnelError::Connection)?;
        let (send, recv) = runtime
            .block_on(tokio::time::timeout(CONNECT_TIMEOUT, connection.open_bi()))
            .map_err(|_| RemoteTunnelError::Connection)?
            .map_err(|_| RemoteTunnelError::Connection)?;
        runtime.block_on(wait_for_selected_path(&endpoint, remote_id))?;
        Ok(Self {
            send,
            recv,
            connection,
            endpoint,
            remote_id,
            timeouts: Mutex::new(Timeouts::default()),
            runtime,
        })
    }

    pub fn selected_path(&self) -> Result<SelectedRemotePath, RemoteTunnelError> {
        self.runtime
            .block_on(selected_path(&self.endpoint, self.remote_id))
            .ok_or(RemoteTunnelError::PathUnavailable)
    }

    pub fn notify_network_change(&self) {
        self.runtime.block_on(self.endpoint.network_change());
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let mut timeouts = self
            .timeouts
            .lock()
            .map_err(|_| io::Error::other("remote tunnel timeout state unavailable"))?;
        timeouts.read = timeout;
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
        let timeout = timeouts.read;
        let read = async { self.recv.read(buffer).await };
        match timeout {
            Some(timeout) => match self.runtime.block_on(tokio::time::timeout(timeout, read)) {
                Ok(Ok(Some(count))) => Ok(count),
                Ok(Ok(None)) => Ok(0),
                Ok(Err(_)) => Err(io::Error::other("remote tunnel read failed")),
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
        let timeout = timeouts.write;
        let write = async { self.send.write(buffer).await };
        match timeout {
            Some(timeout) => match self.runtime.block_on(tokio::time::timeout(timeout, write)) {
                Ok(Ok(count)) => Ok(count),
                Ok(Err(_)) => Err(io::Error::other("remote tunnel write failed")),
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

fn client_transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(0))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .build()
}

fn custom_relay_map(
    relay_url: &str,
    relay_auth_token: &str,
) -> Result<(RelayUrl, RelayMap), RemoteTunnelError> {
    validate_self_hosted_relay(relay_url)?;
    validate_auth_token(relay_auth_token)?;
    let relay_url = relay_url
        .parse::<RelayUrl>()
        .map_err(|_| RemoteTunnelError::Configuration)?;
    let relay_map = RelayMap::from(relay_url.clone()).with_auth_token(relay_auth_token.to_owned());
    Ok((relay_url, relay_map))
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

async fn wait_for_selected_path(
    endpoint: &Endpoint,
    remote_id: EndpointId,
) -> Result<SelectedRemotePath, RemoteTunnelError> {
    let deadline = tokio::time::Instant::now() + PATH_WAIT;
    loop {
        if let Some(path) = selected_path(endpoint, remote_id).await {
            return Ok(path);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RemoteTunnelError::PathUnavailable);
        }
        tokio::time::sleep(PATH_POLL).await;
    }
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

#[cfg(test)]
mod tests {
    use super::{custom_relay_map, RemoteTunnelError};

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
}
