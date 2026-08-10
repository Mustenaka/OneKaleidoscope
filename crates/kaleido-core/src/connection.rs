//! Blocking TLS wire connection used by the mobile worker thread.
//!
//! No platform UI thread calls this type directly. `MobileClient` owns it on a
//! dedicated worker, while Kotlin/Swift communicate through UniFFI methods and
//! callbacks.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use kaleido_transport::control::ControlFrame;
use kaleido_transport::frame::{encode_content, encode_control, Frame, FrameDecoder};
use kaleido_transport::tls::{client_config, export_client_device_auth_binding, SpkiPin};
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use zeroize::Zeroizing;

#[path = "remote_tunnel.rs"]
pub(crate) mod remote_tunnel;

use remote_tunnel::RemoteTunnelStream;
pub use remote_tunnel::SelectedRemotePath;

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const READ_BUFFER_BYTES: usize = 8_192;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("mobile transport connection failed")]
    Connect,
    #[error("mobile transport I/O failed")]
    Io,
    #[error("mobile transport security validation failed")]
    Security,
    #[error("mobile transport frame validation failed")]
    Frame,
    #[error("mobile transport closed unexpectedly")]
    Closed,
}

enum WireSocket {
    Tcp(TcpStream),
    Remote(Box<RemoteTunnelStream>),
}

impl Read for WireSocket {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.read(buffer),
            Self::Remote(stream) => stream.read(buffer),
        }
    }
}

impl Write for WireSocket {
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

pub struct WireConnection {
    stream: StreamOwned<ClientConnection, WireSocket>,
    decoder: FrameDecoder,
    pending: VecDeque<Frame>,
}

impl std::fmt::Debug for WireConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WireConnection")
            .field("pending_frames", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl WireConnection {
    pub fn connect(endpoint: &str, encoded_pin: &str) -> Result<Self, ConnectionError> {
        kaleido_transport::bootstrap::validate_endpoint(endpoint)
            .map_err(|_| ConnectionError::Connect)?;
        let address = endpoint
            .to_socket_addrs()
            .map_err(|_| ConnectionError::Connect)?
            .next()
            .ok_or(ConnectionError::Connect)?;
        let socket = TcpStream::connect_timeout(&address, IO_TIMEOUT)
            .map_err(|_| ConnectionError::Connect)?;
        socket
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| socket.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(|_| ConnectionError::Io)?;
        Self::from_socket(WireSocket::Tcp(socket), encoded_pin)
    }

    pub fn connect_remote(
        host_endpoint_id: &str,
        relay_url: &str,
        relay_auth_token: &str,
        encoded_pin: &str,
    ) -> Result<Self, ConnectionError> {
        let stream = RemoteTunnelStream::connect(host_endpoint_id, relay_url, relay_auth_token)
            .map_err(|_| ConnectionError::Connect)?;
        Self::from_socket(WireSocket::Remote(Box::new(stream)), encoded_pin)
    }

    fn from_socket(socket: WireSocket, encoded_pin: &str) -> Result<Self, ConnectionError> {
        let pin = SpkiPin::parse(encoded_pin).map_err(|_| ConnectionError::Security)?;
        let config = client_config(pin).map_err(|_| ConnectionError::Security)?;
        let server_name =
            ServerName::try_from("onekaleidoscope.local").map_err(|_| ConnectionError::Security)?;
        let connection =
            ClientConnection::new(config, server_name).map_err(|_| ConnectionError::Security)?;
        let mut wire = Self {
            stream: StreamOwned::new(connection, socket),
            decoder: FrameDecoder::new(),
            pending: VecDeque::new(),
        };
        wire.stream.flush().map_err(|_| ConnectionError::Io)?;
        Ok(wire)
    }

    pub fn send_control(&mut self, frame: &ControlFrame) -> Result<(), ConnectionError> {
        let encoded = encode_control(frame).map_err(|_| ConnectionError::Frame)?;
        self.stream
            .write_all(&encoded)
            .and_then(|()| self.stream.flush())
            .map_err(|_| ConnectionError::Io)
    }

    pub fn send_sensitive_control(&mut self, frame: &ControlFrame) -> Result<(), ConnectionError> {
        let encoded = Zeroizing::new(encode_control(frame).map_err(|_| ConnectionError::Frame)?);
        self.stream
            .write_all(&encoded)
            .and_then(|()| self.stream.flush())
            .map_err(|_| ConnectionError::Io)
    }

    pub fn send_content(&mut self, request_id: u64, body: &[u8]) -> Result<(), ConnectionError> {
        let encoded = encode_content(request_id, body).map_err(|_| ConnectionError::Frame)?;
        self.stream
            .write_all(&encoded)
            .and_then(|()| self.stream.flush())
            .map_err(|_| ConnectionError::Io)
    }

    pub fn receive(&mut self) -> Result<Frame, ConnectionError> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(frame);
        }
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        loop {
            let read = self
                .stream
                .read(&mut buffer)
                .map_err(|_| ConnectionError::Io)?;
            if read == 0 {
                return Err(ConnectionError::Closed);
            }
            let bytes = buffer.get(..read).ok_or(ConnectionError::Frame)?;
            let frames = self
                .decoder
                .push(bytes)
                .map_err(|_| ConnectionError::Frame)?;
            self.pending.extend(frames);
            if let Some(frame) = self.pending.pop_front() {
                return Ok(frame);
            }
        }
    }

    pub fn try_receive(&mut self) -> Result<Option<Frame>, ConnectionError> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(Some(frame));
        }
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        match self.stream.read(&mut buffer) {
            Ok(0) => Err(ConnectionError::Closed),
            Ok(read) => {
                let bytes = buffer.get(..read).ok_or(ConnectionError::Frame)?;
                self.pending.extend(
                    self.decoder
                        .push(bytes)
                        .map_err(|_| ConnectionError::Frame)?,
                );
                Ok(self.pending.pop_front())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(_) => Err(ConnectionError::Io),
        }
    }

    pub fn receive_control(&mut self) -> Result<ControlFrame, ConnectionError> {
        self.receive()?
            .decode_control()
            .map_err(|_| ConnectionError::Frame)
    }

    pub fn exporter(&self) -> Result<[u8; 32], ConnectionError> {
        export_client_device_auth_binding(&self.stream.conn).map_err(|_| ConnectionError::Security)
    }

    pub fn set_poll_timeout(&self, timeout: Duration) -> Result<(), ConnectionError> {
        match &self.stream.sock {
            WireSocket::Tcp(socket) => socket.set_read_timeout(Some(timeout)),
            WireSocket::Remote(stream) => stream.set_read_timeout(Some(timeout)),
        }
        .map_err(|_| ConnectionError::Io)
    }

    pub fn selected_remote_path(&self) -> Result<Option<SelectedRemotePath>, ConnectionError> {
        match &self.stream.sock {
            WireSocket::Tcp(_) => Ok(None),
            WireSocket::Remote(stream) => stream
                .selected_path()
                .map(Some)
                .map_err(|_| ConnectionError::Connect),
        }
    }

    pub fn notify_network_change(&self) -> Result<bool, ConnectionError> {
        match &self.stream.sock {
            WireSocket::Tcp(_) => Ok(false),
            WireSocket::Remote(stream) => {
                stream.notify_network_change();
                Ok(true)
            }
        }
    }
}
