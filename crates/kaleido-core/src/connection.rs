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

pub struct WireConnection {
    stream: StreamOwned<ClientConnection, TcpStream>,
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
        self.stream
            .sock
            .set_read_timeout(Some(timeout))
            .map_err(|_| ConnectionError::Io)
    }
}
