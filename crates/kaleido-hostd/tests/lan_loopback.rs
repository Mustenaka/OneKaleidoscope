#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use kaleido_core::{DeviceSigner, DeviceSignerError, MobileClient, ProjectionCallback};
use kaleido_hostd::slice::{self, ReplayRequest, REPLAY_BASE_AT_MS};
use kaleido_hostd::{Broker, LanServer};
use kaleido_proto::command::{Command, CommandOutcome, DeviceCommandRequest};
use kaleido_proto::content::{
    ContentAvailability, ContentKind, ContentReadRequest, ContentReadResponse, ContentWriteRequest,
    ContentWriteResponse, Sensitivity,
};
use kaleido_proto::effect::Cursor;
use kaleido_proto::host::HostReachability;
use kaleido_proto::projection::{
    ProjectionKey, ProjectionPayload, ProjectionSubscribe, ProjectionSubscribeOutcome,
};
use kaleido_proto::queue::QueueIntent;
use kaleido_state::ClockSource;
use kaleido_transport::auth::{build_transcript, sign_transcript, ChallengeTranscript};
use kaleido_transport::bootstrap::encode_uri;
use kaleido_transport::control::{ControlFrame, PairRequest, TransportErrorCode};
use kaleido_transport::frame::{encode_content, encode_control, FrameDecoder};
use kaleido_transport::tls::{client_config, export_client_device_auth_binding, SpkiPin};
use kaleido_transport::{MAX_FRAME_LENGTH, TRANSPORT_VERSION};
use p256::ecdsa::SigningKey;
use p256::pkcs8::EncodePublicKey;
use rand_core::OsRng;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use sha2::{Digest, Sha256};

mod support;

type ClientStream = StreamOwned<ClientConnection, TcpStream>;

fn replayed_broker() -> (tempfile::TempDir, Broker, kaleido_proto::ids::SessionId) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let log_dir = directory.path().join("log");
    let outcome = slice::replay(&ReplayRequest::new(
        support::fixture("01-simple-turn.jsonl"),
        &log_dir,
    ))
    .expect("replay real fixture");
    let broker = Broker::load(
        &log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS + 10_000,
        },
        "kaleido-host",
        "kaleido-host",
    )
    .expect("load broker");
    (directory, broker, outcome.session_id)
}

#[derive(Clone)]
struct TestSigner {
    key: Arc<SigningKey>,
}

impl DeviceSigner for TestSigner {
    fn public_key_spki_der(&self) -> Result<Vec<u8>, DeviceSignerError> {
        p256::PublicKey::from(self.key.verifying_key())
            .to_public_key_der()
            .map(|document| document.as_bytes().to_vec())
            .map_err(|_| DeviceSignerError::InvalidPublicKey)
    }

    fn sign_p256_sha256(&self, transcript: Vec<u8>) -> Result<Vec<u8>, DeviceSignerError> {
        sign_transcript(&self.key, &transcript).map_err(|_| DeviceSignerError::SigningFailed)
    }
}

struct RecordingProjection {
    projections: mpsc::Sender<kaleido_proto::projection::ProjectionEnvelope>,
    errors: Arc<AtomicUsize>,
}

impl ProjectionCallback for RecordingProjection {
    fn on_projection(&self, projection: kaleido_proto::projection::ProjectionEnvelope) {
        let _ = self.projections.send(projection);
    }

    fn on_error(&self, _error: kaleido_proto::error::CanonicalError) {
        self.errors.fetch_add(1, Ordering::SeqCst);
    }

    fn on_closed(&self, _error: Option<kaleido_proto::error::CanonicalError>) {}
}

struct BlockingProjection {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl ProjectionCallback for BlockingProjection {
    fn on_projection(&self, _projection: kaleido_proto::projection::ProjectionEnvelope) {
        let _ = self.entered.send(());
        let _ = self
            .release
            .lock()
            .expect("blocking callback lock")
            .recv_timeout(Duration::from_secs(2));
    }

    fn on_error(&self, _error: kaleido_proto::error::CanonicalError) {}

    fn on_closed(&self, _error: Option<kaleido_proto::error::CanonicalError>) {}
}

#[test]
fn mobile_subscribe_returns_only_after_the_initial_projection_is_applied() {
    let (directory, broker, _) = replayed_broker();
    let server = LanServer::bind(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        &directory.path().join("security"),
        broker.clone(),
        None,
    )
    .expect("bind product LAN server");
    let bootstrap = server.issue_pairing(current_ms()).expect("pairing QR");
    let uri = encode_uri(&bootstrap).expect("canonical QR URI");
    let client = Arc::new(
        MobileClient::new(
            directory
                .path()
                .join("mobile")
                .to_string_lossy()
                .into_owned(),
            Box::new(TestSigner {
                key: Arc::new(SigningKey::random(&mut OsRng)),
            }),
        )
        .expect("mobile client"),
    );
    client
        .pair(uri, "ordered subscription mobile".to_owned())
        .expect("pair through MobileClient");
    client.connect().expect("challenge connect");

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let subscribing_client = Arc::clone(&client);
    let host_id = broker.host_id();
    let join = std::thread::spawn(move || {
        let result = subscribing_client.subscribe(
            ProjectionKey::ProjectIndex { host_id },
            Box::new(BlockingProjection {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
        );
        let _ = finished_tx.send(result);
    });

    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("initial projection entered callback");
    assert!(
        finished_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "subscribe returned before its initial projection callback completed"
    );
    release_tx.send(()).expect("release callback");
    let subscription = finished_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("subscribe completion")
        .expect("synchronized subscription");
    join.join().expect("subscribe thread");

    subscription.unsubscribe().expect("unsubscribe");
    client.disconnect().expect("disconnect");
    server.shutdown().expect("shutdown server");
}

#[test]
fn mobile_client_cold_reconnect_uses_the_exact_cached_projection_cursor() {
    let (directory, broker, session_id) = replayed_broker();
    let server = LanServer::bind(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        &directory.path().join("security"),
        broker.clone(),
        None,
    )
    .expect("bind product LAN server");
    let bootstrap = server.issue_pairing(current_ms()).expect("pairing QR");
    let uri = encode_uri(&bootstrap).expect("canonical QR URI");
    let key = Arc::new(SigningKey::random(&mut OsRng));
    let signer = TestSigner {
        key: Arc::clone(&key),
    };
    let mobile_root = directory.path().join("mobile");
    let mobile_root_text = mobile_root.to_string_lossy().into_owned();
    let client = MobileClient::new(mobile_root_text.clone(), Box::new(signer.clone()))
        .expect("mobile client");
    client
        .pair(uri, "product mobile loopback".to_owned())
        .expect("pair through MobileClient");
    client.connect().expect("challenge connect");

    let project_key = ProjectionKey::ProjectIndex {
        host_id: broker.host_id(),
    };
    let (projection_tx, projection_rx) = mpsc::channel();
    let errors = Arc::new(AtomicUsize::new(0));
    let subscription = client
        .subscribe(
            project_key.clone(),
            Box::new(RecordingProjection {
                projections: projection_tx,
                errors: Arc::clone(&errors),
            }),
        )
        .expect("subscribe through MobileClient");
    let first = projection_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("current projection callback");
    assert!(matches!(
        first.payload,
        ProjectionPayload::ProjectIndex { view }
            if view.reachability == HostReachability::LanDirect
    ));

    let body = b"mobile core content and command";
    let digest = format!("sha256:{:x}", Sha256::digest(body));
    let content_ref = match client
        .write_content(
            ContentWriteRequest {
                content_kind: ContentKind::PlainText,
                byte_len: body.len() as u64,
                digest,
            },
            body.to_vec(),
        )
        .expect("authenticated content write")
    {
        ContentWriteResponse::Stored { content_ref } => Some(content_ref),
        ContentWriteResponse::Rejected { .. } => None,
    }
    .expect("stored content");
    assert!(matches!(
        client
            .read_content(ContentReadRequest {
                content_id: content_ref.content_id.clone(),
                offset: 0,
                max_bytes: 65_536,
            })
            .expect("authenticated content read"),
        ContentReadResponse::Chunk { chunk } if chunk.bytes == body
    ));
    assert!(matches!(
        client
            .submit_command(DeviceCommandRequest {
                idempotency_key: "mobile-product-enqueue".to_owned(),
                ttl_ms: Some(30_000),
                body: Command::EnqueueInput {
                    session_id,
                    body: content_ref.clone(),
                    intent: QueueIntent::NewTurn,
                },
            })
            .expect("authenticated command"),
        kaleido_proto::command::CommandAck {
            outcome: CommandOutcome::Enqueued { .. },
            ..
        }
    ));
    let changed = projection_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("derived ProjectIndex update");
    assert!(changed.cursor > first.cursor);
    let exact_cursor = changed.cursor;
    subscription.unsubscribe().expect("unsubscribe");
    client.disconnect().expect("disconnect");
    drop(client);

    let cold = MobileClient::new(mobile_root_text, Box::new(signer)).expect("cold mobile client");
    cold.connect().expect("cold challenge reconnect");
    let (cold_tx, cold_rx) = mpsc::channel();
    let cold_subscription = cold
        .subscribe(
            project_key.clone(),
            Box::new(RecordingProjection {
                projections: cold_tx,
                errors: Arc::clone(&errors),
            }),
        )
        .expect("resume cached subscription");
    assert!(cold_rx.recv_timeout(Duration::from_millis(250)).is_err());
    assert_eq!(
        cold.cached_projection(project_key)
            .expect("cold cache")
            .cursor,
        exact_cursor
    );
    assert!(matches!(
        cold.read_content(ContentReadRequest {
            content_id: content_ref.content_id,
            offset: 0,
            max_bytes: 65_536,
        })
        .expect("read after cold reconnect"),
        ContentReadResponse::Chunk { chunk } if chunk.bytes == body
    ));
    assert_eq!(errors.load(Ordering::SeqCst), 0);
    cold_subscription.unsubscribe().expect("cold unsubscribe");
    cold.disconnect().expect("cold disconnect");
    server.shutdown().expect("shutdown server");
}

#[test]
fn tls_loopback_pairs_projects_writes_commands_reconnects_and_revokes() {
    let (directory, broker, session_id) = replayed_broker();
    let server = LanServer::bind(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        &directory.path().join("security"),
        broker.clone(),
        None,
    )
    .expect("bind TLS listener");
    let wall_now = current_ms();
    let bootstrap = server
        .issue_pairing(wall_now)
        .expect("issue one-time pairing");
    let signing_key = SigningKey::random(&mut OsRng);
    let public_key = p256::PublicKey::from(signing_key.verifying_key())
        .to_public_key_der()
        .expect("encode P-256 SPKI")
        .as_bytes()
        .to_vec();
    let pin = SpkiPin::parse(&bootstrap.host_public_key_pin).expect("parse host pin");
    let mut client = connect(server.local_addr(), pin.clone());
    negotiate(&mut client);
    send(
        &mut client,
        &ControlFrame::PairRequest {
            request: PairRequest {
                request_id: 3,
                secret: bootstrap.secret,
                device_public_key_spki: public_key,
                device_label: "loopback phone".to_owned(),
            },
        },
    );
    let paired = receive(&mut client);
    let response = match paired {
        ControlFrame::PairResponse { response } => Some(response),
        _ => None,
    }
    .expect("pair response");
    let device_id = response.device_id;

    let project_key = ProjectionKey::ProjectIndex {
        host_id: broker.host_id(),
    };
    send(
        &mut client,
        &ControlFrame::ProjectionSubscribeFrame {
            request_id: 4,
            subscription_id: 1,
            subscribe: ProjectionSubscribe {
                key: project_key.clone(),
                since: None,
            },
        },
    );
    assert!(matches!(
        receive(&mut client),
        ControlFrame::ProjectionSubscribeAckFrame {
            request_id: 4,
            subscription_id: 1,
            ..
        }
    ));
    let project = receive(&mut client);
    let envelope = match project {
        ControlFrame::ProjectionEnvelopeFrame {
            subscription_id: 1,
            envelope,
        } => Some(envelope),
        _ => None,
    }
    .expect("project envelope");
    let project_cursor = envelope.cursor;
    assert!(matches!(
        envelope.payload,
        ProjectionPayload::ProjectIndex { view }
            if view.reachability == HostReachability::LanDirect
    ));

    let body = b"queue this from the authenticated phone";
    let digest = format!("sha256:{:x}", Sha256::digest(body));
    send(
        &mut client,
        &ControlFrame::ContentWriteHeader {
            request_id: 5,
            request: ContentWriteRequest {
                content_kind: ContentKind::PlainText,
                byte_len: body.len() as u64,
                digest: digest.clone(),
            },
        },
    );
    client
        .write_all(&encode_content(5, body).expect("encode content"))
        .expect("send content");
    client.flush().expect("flush content");
    let written = receive(&mut client);
    let content_ref = match written {
        ControlFrame::ContentWriteResult {
            response: ContentWriteResponse::Stored { content_ref },
            ..
        } => Some(content_ref),
        _ => None,
    }
    .expect("stored content");
    assert_eq!(content_ref.sensitivity, Sensitivity::Sensitive);
    assert_eq!(content_ref.availability, ContentAvailability::Stored);
    assert_eq!(content_ref.preview, None);
    assert_eq!(content_ref.digest, digest);

    send(
        &mut client,
        &ControlFrame::ContentReadFrame {
            request_id: 6,
            request: ContentReadRequest {
                content_id: content_ref.content_id.clone(),
                offset: 0,
                max_bytes: 65_536,
            },
        },
    );
    assert!(matches!(
        receive_until(&mut client, |frame| matches!(
            frame,
            ControlFrame::ContentReadResult { request_id: 6, .. }
        )),
        ControlFrame::ContentReadResult {
            response: ContentReadResponse::Chunk { chunk },
            ..
        } if chunk.bytes == body
    ));

    send(
        &mut client,
        &ControlFrame::DeviceCommandFrame {
            request_id: 7,
            request: DeviceCommandRequest {
                idempotency_key: "loopback-enqueue".to_owned(),
                ttl_ms: Some(30_000),
                body: Command::EnqueueInput {
                    session_id: session_id.clone(),
                    body: content_ref,
                    intent: QueueIntent::NewTurn,
                },
            },
        },
    );
    assert!(matches!(
        receive(&mut client),
        ControlFrame::DeviceCommandAck {
            ack: kaleido_proto::command::CommandAck {
                outcome: CommandOutcome::Enqueued { .. },
                ..
            },
            ..
        }
    ));

    send(
        &mut client,
        &ControlFrame::ProjectionSubscribeFrame {
            request_id: 8,
            subscription_id: 2,
            subscribe: ProjectionSubscribe {
                key: ProjectionKey::InputQueue {
                    session_id: session_id.clone(),
                },
                since: None,
            },
        },
    );
    assert!(matches!(
        receive_until(&mut client, |frame| matches!(
            frame,
            ControlFrame::ProjectionSubscribeAckFrame {
                request_id: 8,
                subscription_id: 2,
                ..
            }
        )),
        ControlFrame::ProjectionSubscribeAckFrame {
            request_id: 8,
            subscription_id: 2,
            ..
        }
    ));
    assert!(matches!(
        receive_until(&mut client, |frame| matches!(
            frame,
            ControlFrame::ProjectionEnvelopeFrame {
                subscription_id: 2,
                ..
            }
        )),
        ControlFrame::ProjectionEnvelopeFrame {
            subscription_id: 2,
            envelope: kaleido_proto::projection::ProjectionEnvelope {
                payload: ProjectionPayload::InputQueue { view },
                ..
            }
        } if view.entries.len() == 1
    ));
    drop(client);

    let mut reconnected = connect(server.local_addr(), pin.clone());
    negotiate(&mut reconnected);
    send(
        &mut reconnected,
        &ControlFrame::ChallengeRequest {
            request_id: 3,
            device_id: device_id.clone(),
        },
    );
    let challenge = receive(&mut reconnected);
    let (request_id, challenge_id, nonce, expires_at_ms) = match challenge {
        ControlFrame::DeviceChallenge {
            request_id,
            challenge_id,
            nonce,
            expires_at_ms,
        } => Some((request_id, challenge_id, nonce, expires_at_ms)),
        _ => None,
    }
    .expect("device challenge");
    let exporter =
        export_client_device_auth_binding(&reconnected.conn).expect("export TLS channel binding");
    let transcript = build_transcript(&ChallengeTranscript {
        transport_version: TRANSPORT_VERSION,
        protocol_version: kaleido_proto::PROTOCOL_VERSION,
        host_id: &broker.host_id(),
        device_id: &device_id,
        tls_exporter: &exporter,
        challenge_id: &challenge_id,
        nonce: &nonce,
        expires_at_ms,
    })
    .expect("build exact challenge transcript");
    let signature_der = sign_transcript(&signing_key, &transcript).expect("sign challenge");
    send(
        &mut reconnected,
        &ControlFrame::ChallengeProof {
            request_id,
            challenge_id,
            signature_der,
        },
    );
    assert!(matches!(
        receive(&mut reconnected),
        ControlFrame::AuthAccepted { request_id: 3, .. }
    ));
    send(
        &mut reconnected,
        &ControlFrame::ProjectionSubscribeFrame {
            request_id: 4,
            subscription_id: 1,
            subscribe: ProjectionSubscribe {
                key: project_key,
                since: Some(project_cursor),
            },
        },
    );
    let resumed = receive(&mut reconnected);
    assert!(matches!(
        resumed,
        ControlFrame::ProjectionSubscribeAckFrame {
            ack: kaleido_proto::projection::ProjectionSubscribeAck {
                outcome: ProjectionSubscribeOutcome::Resumed { from_cursor },
                ..
            },
            ..
        } if from_cursor == project_cursor.next().expect("next cursor")
    ));

    server
        .revoke_device(&device_id, wall_now + 1)
        .expect("durably revoke");
    assert!(matches!(
        receive_until(&mut reconnected, |frame| matches!(
            frame,
            ControlFrame::TransportError {
                code: TransportErrorCode::DeviceRevoked,
                ..
            }
        )),
        ControlFrame::TransportError {
            code: TransportErrorCode::DeviceRevoked,
            ..
        }
    ));
    drop(reconnected);

    let mut denied = connect(server.local_addr(), pin);
    negotiate(&mut denied);
    send(
        &mut denied,
        &ControlFrame::ChallengeRequest {
            request_id: 3,
            device_id,
        },
    );
    let revoked_challenge = receive(&mut denied);
    let revoked_challenge_id = match revoked_challenge {
        ControlFrame::DeviceChallenge {
            request_id: 3,
            challenge_id,
            ..
        } => Some(challenge_id),
        _ => None,
    }
    .expect("revoked devices receive the indistinguishable challenge shape");
    send(
        &mut denied,
        &ControlFrame::ChallengeProof {
            request_id: 3,
            challenge_id: revoked_challenge_id,
            signature_der: vec![1, 2, 3],
        },
    );
    assert!(matches!(
        receive(&mut denied),
        ControlFrame::TransportError {
            code: TransportErrorCode::AuthenticationFailed,
            ..
        }
    ));
    drop(denied);
    server.shutdown().expect("shutdown listener");
}

#[test]
fn business_frames_before_device_auth_are_closed_without_reaching_broker() {
    let (directory, broker, _) = replayed_broker();
    let server = LanServer::bind(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        &directory.path().join("security"),
        broker.clone(),
        None,
    )
    .expect("bind TLS listener");
    let bootstrap = server.issue_pairing(current_ms()).expect("bootstrap");
    let pin = SpkiPin::parse(&bootstrap.host_public_key_pin).expect("pin");
    let mut client = connect(server.local_addr(), pin);
    negotiate(&mut client);
    let unauthenticated = ControlFrame::ProjectionSubscribeFrame {
        request_id: 3,
        subscription_id: 1,
        subscribe: ProjectionSubscribe {
            key: ProjectionKey::ProjectIndex {
                host_id: broker.host_id(),
            },
            since: Some(Cursor::START),
        },
    };
    let encoded = encode_control(&unauthenticated).expect("encode unauthenticated frame");
    let _ = client.write_all(&encoded);
    let _ = client.flush();
    client
        .sock
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("read timeout");
    let mut byte = [0_u8; 1];
    let closed = client.read(&mut byte);
    assert!(closed.is_err() || closed.ok() == Some(0));
    drop(client);
    server.shutdown().expect("shutdown listener");
}

#[test]
fn invalid_expired_or_used_pairing_shape_is_rate_limited_per_source() {
    let (directory, broker, _) = replayed_broker();
    let server = LanServer::bind(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        &directory.path().join("security"),
        broker,
        None,
    )
    .expect("bind TLS listener");
    let bootstrap = server.issue_pairing(current_ms()).expect("bootstrap pin");
    let pin = SpkiPin::parse(&bootstrap.host_public_key_pin).expect("pin");
    let signing_key = SigningKey::random(&mut OsRng);
    let public_key = p256::PublicKey::from(signing_key.verifying_key())
        .to_public_key_der()
        .expect("P-256 SPKI")
        .as_bytes()
        .to_vec();

    for attempt in 0..=4 {
        let mut client = connect(server.local_addr(), pin.clone());
        negotiate(&mut client);
        send(
            &mut client,
            &ControlFrame::PairRequest {
                request: PairRequest {
                    request_id: 3,
                    secret: vec![0; 32],
                    device_public_key_spki: public_key.clone(),
                    device_label: "invalid pair".to_owned(),
                },
            },
        );
        let error = receive(&mut client);
        if attempt < 4 {
            assert!(matches!(
                error,
                ControlFrame::TransportError {
                    request_id: Some(3),
                    code: TransportErrorCode::PairingInvalid,
                    ..
                }
            ));
        } else {
            assert!(matches!(
                error,
                ControlFrame::TransportError {
                    request_id: Some(3),
                    code: TransportErrorCode::RateLimited,
                    ..
                }
            ));
        }
        drop(client);
        std::thread::sleep(Duration::from_millis(25));
    }
    server.shutdown().expect("shutdown listener");
}

#[test]
fn a_dripped_valid_tls_client_hello_hits_the_absolute_deadline_and_releases_slots() {
    let (directory, broker, _) = replayed_broker();
    let server = LanServer::bind(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        &directory.path().join("security"),
        broker,
        None,
    )
    .expect("bind TLS listener");
    let bootstrap = server.issue_pairing(current_ms()).expect("bootstrap pin");
    let pin = SpkiPin::parse(&bootstrap.host_public_key_pin).expect("pin");
    let name = ServerName::try_from("onekaleidoscope.invalid".to_owned()).expect("server name");
    let mut hello_connection =
        ClientConnection::new(client_config(pin.clone()).expect("config"), name)
            .expect("client hello");
    let mut client_hello = Vec::new();
    while hello_connection.wants_write() {
        hello_connection
            .write_tls(&mut client_hello)
            .expect("serialize client hello");
    }
    assert!(client_hello.len() > 6);
    let mut slow = (0..4)
        .map(|_| TcpStream::connect(server.local_addr()).expect("slow connection"))
        .collect::<Vec<_>>();
    for offset in 0..6 {
        for socket in &mut slow {
            if let Some(byte) = client_hello.get(offset) {
                let _ = socket.write_all(std::slice::from_ref(byte));
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    drop(slow);
    std::thread::sleep(Duration::from_millis(200));

    let mut healthy = connect(server.local_addr(), pin);
    negotiate(&mut healthy);
    drop(healthy);
    server.shutdown().expect("shutdown listener");
}

#[test]
fn challenge_failures_are_indistinguishable_and_device_limit_precedes_auth_accepted() {
    let (directory, broker, _) = replayed_broker();
    let server = LanServer::bind(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        &directory.path().join("security"),
        broker.clone(),
        None,
    )
    .expect("bind TLS listener");
    let bootstrap = server.issue_pairing(current_ms()).expect("pairing");
    let pin = SpkiPin::parse(&bootstrap.host_public_key_pin).expect("pin");
    let signing_key = SigningKey::random(&mut OsRng);
    let public_key = p256::PublicKey::from(signing_key.verifying_key())
        .to_public_key_der()
        .expect("P-256 SPKI")
        .as_bytes()
        .to_vec();
    let mut paired = connect(server.local_addr(), pin.clone());
    negotiate(&mut paired);
    send(
        &mut paired,
        &ControlFrame::PairRequest {
            request: PairRequest {
                request_id: 3,
                secret: bootstrap.secret,
                device_public_key_spki: public_key,
                device_label: "limited phone".to_owned(),
            },
        },
    );
    let device_id = match receive(&mut paired) {
        ControlFrame::PairResponse { response } => Some(response.device_id),
        _ => None,
    }
    .expect("paired device");

    let mut second = connect(server.local_addr(), pin.clone());
    negotiate(&mut second);
    assert!(matches!(
        authenticate_device(&mut second, &broker.host_id(), &device_id, &signing_key),
        ControlFrame::AuthAccepted { .. }
    ));

    let mut third = connect(server.local_addr(), pin.clone());
    negotiate(&mut third);
    assert!(matches!(
        authenticate_device(&mut third, &broker.host_id(), &device_id, &signing_key),
        ControlFrame::TransportError {
            code: TransportErrorCode::TooManyConnections,
            ..
        }
    ));
    drop(third);

    let mut wrong_signature = connect(server.local_addr(), pin.clone());
    negotiate(&mut wrong_signature);
    let wrong_challenge = request_challenge(&mut wrong_signature, &device_id);
    send(
        &mut wrong_signature,
        &ControlFrame::ChallengeProof {
            request_id: 3,
            challenge_id: wrong_challenge.0,
            signature_der: vec![1, 2, 3],
        },
    );
    assert!(matches!(
        receive(&mut wrong_signature),
        ControlFrame::TransportError {
            request_id: Some(3),
            code: TransportErrorCode::AuthenticationFailed,
            ..
        }
    ));
    drop(wrong_signature);

    let mut unknown = connect(server.local_addr(), pin);
    negotiate(&mut unknown);
    let unknown_challenge = request_challenge(
        &mut unknown,
        &kaleido_proto::ids::DeviceId::new("unknown-device"),
    );
    send(
        &mut unknown,
        &ControlFrame::ChallengeProof {
            request_id: 3,
            challenge_id: unknown_challenge.0,
            signature_der: vec![1, 2, 3],
        },
    );
    assert!(matches!(
        receive(&mut unknown),
        ControlFrame::TransportError {
            request_id: Some(3),
            code: TransportErrorCode::AuthenticationFailed,
            ..
        }
    ));

    drop(unknown);
    drop(second);
    drop(paired);
    server.shutdown().expect("shutdown listener");
}

fn request_challenge(
    client: &mut ClientStream,
    device_id: &kaleido_proto::ids::DeviceId,
) -> (Vec<u8>, Vec<u8>, i64) {
    send(
        client,
        &ControlFrame::ChallengeRequest {
            request_id: 3,
            device_id: device_id.clone(),
        },
    );
    match receive(client) {
        ControlFrame::DeviceChallenge {
            request_id: 3,
            challenge_id,
            nonce,
            expires_at_ms,
        } => Some((challenge_id, nonce, expires_at_ms)),
        _ => None,
    }
    .expect("fixed challenge exchange")
}

fn authenticate_device(
    client: &mut ClientStream,
    host_id: &kaleido_proto::ids::HostId,
    device_id: &kaleido_proto::ids::DeviceId,
    signing_key: &SigningKey,
) -> ControlFrame {
    let (challenge_id, nonce, expires_at_ms) = request_challenge(client, device_id);
    let exporter = export_client_device_auth_binding(&client.conn).expect("TLS exporter");
    let transcript = build_transcript(&ChallengeTranscript {
        transport_version: TRANSPORT_VERSION,
        protocol_version: kaleido_proto::PROTOCOL_VERSION,
        host_id,
        device_id,
        tls_exporter: &exporter,
        challenge_id: &challenge_id,
        nonce: &nonce,
        expires_at_ms,
    })
    .expect("challenge transcript");
    let signature_der = sign_transcript(signing_key, &transcript).expect("challenge signature");
    send(
        client,
        &ControlFrame::ChallengeProof {
            request_id: 3,
            challenge_id,
            signature_der,
        },
    );
    receive(client)
}

fn connect(address: SocketAddr, pin: SpkiPin) -> ClientStream {
    let socket = TcpStream::connect(address).expect("connect loopback");
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set read timeout");
    let name = ServerName::try_from("onekaleidoscope.invalid".to_owned()).expect("server name");
    let connection = ClientConnection::new(client_config(pin).expect("client config"), name)
        .expect("TLS client");
    StreamOwned::new(connection, socket)
}

fn negotiate(client: &mut ClientStream) {
    send(
        client,
        &ControlFrame::TransportHello {
            request_id: 1,
            transport_version: TRANSPORT_VERSION.to_owned(),
            max_frame_length: MAX_FRAME_LENGTH,
        },
    );
    assert!(matches!(
        receive(client),
        ControlFrame::TransportHelloAck { request_id: 1, .. }
    ));
    send(
        client,
        &ControlFrame::UacpHello {
            request_id: 2,
            protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
        },
    );
    assert!(matches!(
        receive(client),
        ControlFrame::UacpHelloAck { request_id: 2, .. }
    ));
}

fn send(client: &mut ClientStream, frame: &ControlFrame) {
    client
        .write_all(&encode_control(frame).expect("encode control"))
        .expect("write control");
    client.flush().expect("flush control");
}

fn receive(client: &mut ClientStream) -> ControlFrame {
    let mut header = [0_u8; 5];
    client.read_exact(&mut header).expect("read frame header");
    let prefix: [u8; 4] = header
        .get(..4)
        .expect("length prefix")
        .try_into()
        .expect("four bytes");
    let body_len = u32::from_be_bytes(prefix)
        .checked_sub(1)
        .and_then(|length| usize::try_from(length).ok())
        .expect("body length");
    let mut body = vec![0_u8; body_len];
    client.read_exact(&mut body).expect("read frame body");
    let mut decoder = FrameDecoder::new();
    assert!(decoder.push(&header).expect("header").is_empty());
    let frame = decoder.push(&body).expect("body").pop().expect("one frame");
    decoder.finish().expect("complete frame");
    frame.decode_control().expect("control frame")
}

fn receive_until(
    client: &mut ClientStream,
    predicate: impl Fn(&ControlFrame) -> bool,
) -> ControlFrame {
    (0..8)
        .find_map(|_| {
            let frame = receive(client);
            predicate(&frame).then_some(frame)
        })
        .expect("matching multiplexed frame")
}

fn current_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .expect("system clock")
}
