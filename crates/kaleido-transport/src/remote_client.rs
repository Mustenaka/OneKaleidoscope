//! Blocking, pinned-TLS client for the closed REMOTE CONTROL protocol.
//!
//! The control plane is deliberately separate from the iroh data path.  It
//! carries only rendezvous and opaque push metadata, and authenticates the
//! Ubuntu service with the exact SPKI pin distributed during pairing.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use zeroize::Zeroizing;

use crate::remote::{
    ExpectedRemoteResponse, RemoteControlFrame, RemoteCorrelationState, RemoteErrorCode,
    MAX_REMOTE_CONTROL_FRAME_BYTES, REMOTE_CONTROL_VERSION,
};
use crate::tls::{client_config, SpkiPin};

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const LENGTH_PREFIX_BYTES: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum RemoteClientError {
    #[error("remote control connection failed")]
    Connect,
    #[error("remote control security validation failed")]
    Security,
    #[error("remote control I/O failed")]
    Io,
    #[error("remote control contract validation failed")]
    Contract,
    #[error("remote control request was rejected: {0:?}")]
    Rejected(RemoteErrorCode),
}

pub struct RemoteControlClient {
    stream: StreamOwned<ClientConnection, TcpStream>,
    correlation: RemoteCorrelationState,
}

impl std::fmt::Debug for RemoteControlClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteControlClient([redacted endpoint])")
    }
}

impl RemoteControlClient {
    pub fn connect(endpoint: &str, encoded_pin: &str) -> Result<Self, RemoteClientError> {
        crate::bootstrap::validate_endpoint(endpoint).map_err(|_| RemoteClientError::Connect)?;
        let address = endpoint
            .to_socket_addrs()
            .map_err(|_| RemoteClientError::Connect)?
            .next()
            .ok_or(RemoteClientError::Connect)?;
        let socket = TcpStream::connect_timeout(&address, IO_TIMEOUT)
            .map_err(|_| RemoteClientError::Connect)?;
        socket
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| socket.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(|_| RemoteClientError::Io)?;
        let pin = SpkiPin::parse(encoded_pin).map_err(|_| RemoteClientError::Security)?;
        let config = client_config(pin).map_err(|_| RemoteClientError::Security)?;
        let server_name = ServerName::try_from("remote.onekaleidoscope.local")
            .map_err(|_| RemoteClientError::Security)?;
        let connection =
            ClientConnection::new(config, server_name).map_err(|_| RemoteClientError::Security)?;
        let mut client = Self {
            stream: StreamOwned::new(connection, socket),
            correlation: RemoteCorrelationState::default(),
        };
        client.hello()?;
        Ok(client)
    }

    pub fn request(
        &mut self,
        frame: &RemoteControlFrame,
        expected: ExpectedRemoteResponse,
    ) -> Result<RemoteControlFrame, RemoteClientError> {
        let request_id = frame.request_id().ok_or(RemoteClientError::Contract)?;
        self.correlation
            .register_outgoing_request(request_id, expected)
            .map_err(|_| RemoteClientError::Contract)?;
        write_frame(&mut self.stream, frame, true)?;
        let response = read_frame(&mut self.stream)?;
        self.correlation
            .accept_response(&response)
            .map_err(|_| RemoteClientError::Contract)?;
        if let RemoteControlFrame::RemoteError { code, .. } = response {
            return Err(RemoteClientError::Rejected(code));
        }
        Ok(response)
    }

    fn hello(&mut self) -> Result<(), RemoteClientError> {
        let request = RemoteControlFrame::RemoteHello {
            request_id: 1,
            remote_control_version: REMOTE_CONTROL_VERSION.to_owned(),
            max_frame_length: u32::try_from(MAX_REMOTE_CONTROL_FRAME_BYTES)
                .map_err(|_| RemoteClientError::Contract)?,
        };
        let response = self.request(&request, ExpectedRemoteResponse::RemoteHelloAck)?;
        if !matches!(
            response,
            RemoteControlFrame::RemoteHelloAck { request_id: 1, .. }
        ) {
            return Err(RemoteClientError::Contract);
        }
        Ok(())
    }
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<RemoteControlFrame, RemoteClientError> {
    let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
    reader
        .read_exact(&mut prefix)
        .map_err(|_| RemoteClientError::Io)?;
    let length =
        usize::try_from(u32::from_be_bytes(prefix)).map_err(|_| RemoteClientError::Contract)?;
    if length == 0 || length > MAX_REMOTE_CONTROL_FRAME_BYTES {
        return Err(RemoteClientError::Contract);
    }
    let mut encoded = Zeroizing::new(vec![0_u8; length]);
    reader
        .read_exact(&mut encoded)
        .map_err(|_| RemoteClientError::Io)?;
    RemoteControlFrame::decode(&encoded).map_err(|_| RemoteClientError::Contract)
}

pub fn write_frame<W: Write>(
    writer: &mut W,
    frame: &RemoteControlFrame,
    sensitive: bool,
) -> Result<(), RemoteClientError> {
    let encoded = frame.encode().map_err(|_| RemoteClientError::Contract)?;
    let length = u32::try_from(encoded.len()).map_err(|_| RemoteClientError::Contract)?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(|_| RemoteClientError::Io)?;
    if sensitive {
        let encoded = Zeroizing::new(encoded);
        writer
            .write_all(&encoded)
            .and_then(|()| writer.flush())
            .map_err(|_| RemoteClientError::Io)
    } else {
        writer
            .write_all(&encoded)
            .and_then(|()| writer.flush())
            .map_err(|_| RemoteClientError::Io)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    use rustls::{ServerConnection, StreamOwned};

    use super::{read_frame, write_frame, RemoteClientError, RemoteControlClient};
    use crate::remote::{
        ExpectedRemoteResponse, RemoteControlFrame, MAX_REMOTE_CONTROL_FRAME_BYTES,
    };
    use crate::tls::{server_config, TlsIdentityStore};

    #[test]
    fn framing_round_trips_and_rejects_oversized_prefix_before_allocation() {
        let frame = RemoteControlFrame::RemoteHello {
            request_id: 1,
            remote_control_version: "0.1.0".to_owned(),
            max_frame_length: u32::try_from(MAX_REMOTE_CONTROL_FRAME_BYTES).unwrap(),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame, false).unwrap();
        assert_eq!(read_frame(&mut bytes.as_slice()).unwrap(), frame);

        let oversized = u32::try_from(MAX_REMOTE_CONTROL_FRAME_BYTES + 1)
            .unwrap()
            .to_be_bytes();
        assert!(matches!(
            read_frame(&mut oversized.as_slice()),
            Err(RemoteClientError::Contract)
        ));
    }

    #[test]
    fn pinned_tls_client_completes_hello_and_correlates_response() {
        let directory = tempfile::tempdir().unwrap();
        let identity = TlsIdentityStore::new(directory.path().join("private").join("service.json"))
            .unwrap()
            .load_or_generate()
            .unwrap();
        let pin = identity.leaf_pin().unwrap().encode();
        let tls = server_config(identity).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || serve_two_frames(listener, tls));

        let mut client = RemoteControlClient::connect(&endpoint, &pin).unwrap();
        let response = client
            .request(
                &RemoteControlFrame::DeletePushAddress {
                    request_id: 2,
                    operation_id: "AQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
                    issued_at_ms: 1,
                    route_id: "AgICAgICAgICAgICAgICAg".to_owned(),
                    device_slot_id: "AwMDAwMDAwMDAwMDAwMDAw".to_owned(),
                    access_token: "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
                },
                ExpectedRemoteResponse::PushAddressDeleted,
            )
            .unwrap();
        assert!(matches!(
            response,
            RemoteControlFrame::PushAddressDeleted { request_id: 2 }
        ));
        server.join().unwrap();
    }

    #[test]
    fn wrong_service_identity_is_rejected_before_remote_hello() {
        let directory = tempfile::tempdir().unwrap();
        let server_identity =
            TlsIdentityStore::new(directory.path().join("server-private").join("server.json"))
                .unwrap()
                .load_or_generate()
                .unwrap();
        let wrong_pin =
            TlsIdentityStore::new(directory.path().join("other-private").join("other.json"))
                .unwrap()
                .load_or_generate()
                .unwrap()
                .leaf_pin()
                .unwrap()
                .encode();
        let tls = server_config(server_identity).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let connection = ServerConnection::new(tls).unwrap();
            let mut stream = StreamOwned::new(connection, socket);
            assert!(read_frame(&mut stream).is_err());
        });
        assert!(RemoteControlClient::connect(&endpoint, &wrong_pin).is_err());
        server.join().unwrap();
    }

    fn serve_two_frames(listener: TcpListener, tls: Arc<rustls::ServerConfig>) {
        let (socket, _) = listener.accept().unwrap();
        let connection = ServerConnection::new(tls).unwrap();
        let mut stream = StreamOwned::new(connection, socket);
        assert!(matches!(
            read_frame(&mut stream).unwrap(),
            RemoteControlFrame::RemoteHello { request_id: 1, .. }
        ));
        write_frame(
            &mut stream,
            &RemoteControlFrame::RemoteHelloAck {
                request_id: 1,
                remote_control_version: "0.1.0".to_owned(),
                max_frame_length: u32::try_from(MAX_REMOTE_CONTROL_FRAME_BYTES).unwrap(),
            },
            false,
        )
        .unwrap();
        assert!(matches!(
            read_frame(&mut stream).unwrap(),
            RemoteControlFrame::DeletePushAddress { request_id: 2, .. }
        ));
        write_frame(
            &mut stream,
            &RemoteControlFrame::PushAddressDeleted { request_id: 2 },
            false,
        )
        .unwrap();
    }
}
