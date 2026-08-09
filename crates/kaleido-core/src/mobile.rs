//! Product mobile client surface and its single authenticated connection worker.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kaleido_proto::command::{CommandAck, DeviceCommandRequest};
use kaleido_proto::content::{
    ContentReadRequest, ContentReadResponse, ContentWriteRequest, ContentWriteResponse,
};
use kaleido_proto::effect::Cursor;
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::ids::DeviceId;
use kaleido_proto::projection::{
    ProjectionEnvelope, ProjectionKey, ProjectionSubscribe, ProjectionSubscribeOutcome,
};
use kaleido_transport::auth::{build_transcript, ChallengeTranscript};
use kaleido_transport::bootstrap::decode_uri;
use kaleido_transport::control::{ControlFrame, PairRequest};
use kaleido_transport::frame::Frame;
use kaleido_transport::{MAX_FRAME_LENGTH, TRANSPORT_VERSION};
use zeroize::Zeroize;

use crate::cache::{CacheApply, ProjectionCache};
use crate::connection::WireConnection;
use crate::credential::{CredentialStore, PairedHost};
use crate::signer::DeviceSigner;

const WORKER_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum MobileClientError {
    #[error("mobile client storage is unavailable")]
    Storage,
    #[error("mobile client is not paired")]
    NotPaired,
    #[error("mobile client is not connected")]
    NotConnected,
    #[error("mobile client is already connected")]
    AlreadyConnected,
    #[error("mobile transport authentication failed")]
    Authentication,
    #[error("mobile transport contract failed")]
    Contract,
    #[error("mobile request was rejected")]
    RemoteRejected,
    #[error("mobile connection worker stopped")]
    WorkerStopped,
    #[error("mobile request identifier space is exhausted")]
    IdentifierExhausted,
}

#[uniffi::export(callback_interface)]
pub trait ProjectionCallback: Send + Sync {
    fn on_projection(&self, projection: ProjectionEnvelope);
    fn on_error(&self, error: CanonicalError);
    fn on_closed(&self, error: Option<CanonicalError>);
}

#[derive(uniffi::Object)]
pub struct ProjectionSubscription {
    subscription_id: u64,
    commands: mpsc::Sender<WorkerCommand>,
}

impl std::fmt::Debug for ProjectionSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectionSubscription")
            .field("subscription_id", &self.subscription_id)
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl ProjectionSubscription {
    pub fn unsubscribe(&self) -> Result<(), MobileClientError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .send(WorkerCommand::Unsubscribe {
                subscription_id: self.subscription_id,
                reply,
            })
            .map_err(|_| MobileClientError::WorkerStopped)?;
        receive_reply(response)
    }
}

struct WorkerHandle {
    commands: mpsc::Sender<WorkerCommand>,
    join: JoinHandle<()>,
}

struct ActiveSubscription {
    key: ProjectionKey,
    callback: Box<dyn ProjectionCallback>,
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorkerHandle(..)")
    }
}

#[derive(uniffi::Object)]
pub struct MobileClient {
    signer: Arc<dyn DeviceSigner>,
    credentials: CredentialStore,
    paired: Mutex<Option<PairedHost>>,
    cache: Arc<Mutex<ProjectionCache>>,
    worker: Mutex<Option<WorkerHandle>>,
    next_subscription_id: AtomicU64,
}

impl std::fmt::Debug for MobileClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MobileClient([redacted storage])")
    }
}

#[uniffi::export]
impl MobileClient {
    #[uniffi::constructor]
    pub fn new(
        storage_directory: String,
        signer: Box<dyn DeviceSigner>,
    ) -> Result<Arc<Self>, MobileClientError> {
        let credentials =
            CredentialStore::open(&storage_directory).map_err(|_| MobileClientError::Storage)?;
        let paired = credentials.load().map_err(|_| MobileClientError::Storage)?;
        let cache =
            ProjectionCache::open(&storage_directory).map_err(|_| MobileClientError::Storage)?;
        Ok(Arc::new(Self {
            signer: Arc::from(signer),
            credentials,
            paired: Mutex::new(paired),
            cache: Arc::new(Mutex::new(cache)),
            worker: Mutex::new(None),
            next_subscription_id: AtomicU64::new(1),
        }))
    }

    pub fn pair(
        &self,
        mut bootstrap_uri: String,
        device_label: String,
    ) -> Result<DeviceId, MobileClientError> {
        if lock(&self.worker).is_some() {
            return Err(MobileClientError::AlreadyConnected);
        }
        let decoded = decode_uri(&bootstrap_uri);
        bootstrap_uri.zeroize();
        let mut bootstrap = decoded.map_err(|_| MobileClientError::Contract)?;
        if system_time_ms() >= bootstrap.expires_at_ms {
            bootstrap.secret.fill(0);
            return Err(MobileClientError::Authentication);
        }
        let public_key = self
            .signer
            .public_key_spki_der()
            .map_err(|_| MobileClientError::Authentication)?;
        let mut wire = WireConnection::connect(&bootstrap.endpoint, &bootstrap.host_public_key_pin)
            .map_err(|_| MobileClientError::Authentication)?;
        hello(&mut wire)?;
        let mut frame = ControlFrame::PairRequest {
            request: PairRequest {
                request_id: 3,
                secret: std::mem::take(&mut bootstrap.secret),
                device_public_key_spki: public_key,
                device_label,
            },
        };
        let sent = wire.send_sensitive_control(&frame);
        if let ControlFrame::PairRequest { request } = &mut frame {
            request.secret.fill(0);
        }
        sent.map_err(|_| MobileClientError::Authentication)?;
        let response = wire
            .receive_control()
            .map_err(|_| MobileClientError::Authentication)?;
        let ControlFrame::PairResponse { response } = response else {
            return Err(MobileClientError::Authentication);
        };
        if response.request_id != 3
            || response.host_id != bootstrap.host_id
            || !kaleido_transport::version_is_compatible(&response.transport_version)
            || !kaleido_proto::version_is_compatible(&response.protocol_version)
        {
            return Err(MobileClientError::Authentication);
        }
        let paired = PairedHost {
            host_id: response.host_id,
            device_id: response.device_id.clone(),
            endpoint: bootstrap.endpoint,
            host_public_key_pin: bootstrap.host_public_key_pin,
        };
        self.credentials
            .store(&paired)
            .map_err(|_| MobileClientError::Storage)?;
        *lock(&self.paired) = Some(paired);
        Ok(response.device_id)
    }

    pub fn connect(&self) -> Result<(), MobileClientError> {
        let mut worker = lock(&self.worker);
        if worker.is_some() {
            return Err(MobileClientError::AlreadyConnected);
        }
        let paired = lock(&self.paired)
            .clone()
            .ok_or(MobileClientError::NotPaired)?;
        let (commands, receiver) = mpsc::channel();
        let (ready, readiness) = mpsc::sync_channel(1);
        let signer = Arc::clone(&self.signer);
        let cache = Arc::clone(&self.cache);
        let join = std::thread::Builder::new()
            .name("kaleido-mobile-connection".to_owned())
            .spawn(move || run_worker(paired, signer, cache, receiver, ready))
            .map_err(|_| MobileClientError::WorkerStopped)?;
        match readiness.recv() {
            Ok(Ok(())) => {
                *worker = Some(WorkerHandle { commands, join });
                Ok(())
            }
            Ok(Err(error)) => {
                drop(join.join());
                Err(error)
            }
            Err(_) => {
                drop(join.join());
                Err(MobileClientError::WorkerStopped)
            }
        }
    }

    pub fn reconnect(&self) -> Result<(), MobileClientError> {
        self.disconnect()?;
        self.connect()
    }

    pub fn disconnect(&self) -> Result<(), MobileClientError> {
        let handle = lock(&self.worker).take();
        let Some(handle) = handle else {
            return Ok(());
        };
        drop(handle.commands.send(WorkerCommand::Shutdown));
        handle
            .join
            .join()
            .map_err(|_| MobileClientError::WorkerStopped)
    }

    pub fn subscribe(
        &self,
        key: ProjectionKey,
        callback: Box<dyn ProjectionCallback>,
    ) -> Result<Arc<ProjectionSubscription>, MobileClientError> {
        let id = next_atomic(&self.next_subscription_id)?;
        let commands = worker_sender(&self.worker)?;
        let since = lock(&self.cache).since(&key);
        let subscribe = ProjectionSubscribe { key, since };
        let (reply, response) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Subscribe {
                subscription_id: id,
                subscribe,
                callback,
                reply,
            })
            .map_err(|_| MobileClientError::WorkerStopped)?;
        receive_reply(response)?;
        Ok(Arc::new(ProjectionSubscription {
            subscription_id: id,
            commands,
        }))
    }

    pub fn submit_command(
        &self,
        request: DeviceCommandRequest,
    ) -> Result<CommandAck, MobileClientError> {
        let commands = worker_sender(&self.worker)?;
        let (reply, response) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::SubmitCommand { request, reply })
            .map_err(|_| MobileClientError::WorkerStopped)?;
        receive_reply(response)
    }

    pub fn write_content(
        &self,
        request: ContentWriteRequest,
        bytes: Vec<u8>,
    ) -> Result<ContentWriteResponse, MobileClientError> {
        let commands = worker_sender(&self.worker)?;
        let (reply, response) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::WriteContent {
                request,
                bytes,
                reply,
            })
            .map_err(|_| MobileClientError::WorkerStopped)?;
        receive_reply(response)
    }

    pub fn read_content(
        &self,
        request: ContentReadRequest,
    ) -> Result<ContentReadResponse, MobileClientError> {
        let commands = worker_sender(&self.worker)?;
        let (reply, response) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::ReadContent { request, reply })
            .map_err(|_| MobileClientError::WorkerStopped)?;
        receive_reply(response)
    }

    pub fn cached_projection(&self, key: ProjectionKey) -> Option<ProjectionEnvelope> {
        lock(&self.cache).cached(&key).cloned()
    }
}

impl Drop for MobileClient {
    fn drop(&mut self) {
        if let Some(handle) = lock(&self.worker).take() {
            drop(handle.commands.send(WorkerCommand::Shutdown));
            drop(handle.join.join());
        }
    }
}

enum WorkerCommand {
    Subscribe {
        subscription_id: u64,
        subscribe: ProjectionSubscribe,
        callback: Box<dyn ProjectionCallback>,
        reply: SyncSender<Result<(), MobileClientError>>,
    },
    Unsubscribe {
        subscription_id: u64,
        reply: SyncSender<Result<(), MobileClientError>>,
    },
    SubmitCommand {
        request: DeviceCommandRequest,
        reply: SyncSender<Result<CommandAck, MobileClientError>>,
    },
    WriteContent {
        request: ContentWriteRequest,
        bytes: Vec<u8>,
        reply: SyncSender<Result<ContentWriteResponse, MobileClientError>>,
    },
    ReadContent {
        request: ContentReadRequest,
        reply: SyncSender<Result<ContentReadResponse, MobileClientError>>,
    },
    Shutdown,
}

fn run_worker(
    paired: PairedHost,
    signer: Arc<dyn DeviceSigner>,
    cache: Arc<Mutex<ProjectionCache>>,
    commands: Receiver<WorkerCommand>,
    ready: SyncSender<Result<(), MobileClientError>>,
) {
    let mut wire = match authenticated_wire(&paired, signer.as_ref()) {
        Ok(wire) => wire,
        Err(error) => {
            let _send_result = ready.send(Err(error));
            return;
        }
    };
    if wire.set_poll_timeout(WORKER_POLL).is_err() {
        let _send_result = ready.send(Err(MobileClientError::Authentication));
        return;
    }
    let _send_result = ready.send(Ok(()));
    let mut next_request_id = 4_u64;
    let mut callbacks: BTreeMap<u64, ActiveSubscription> = BTreeMap::new();
    let mut current_snapshots: BTreeMap<u64, Cursor> = BTreeMap::new();
    loop {
        match commands.try_recv() {
            Ok(WorkerCommand::Shutdown) => break,
            Ok(command) => {
                if execute_command(
                    &mut wire,
                    &mut next_request_id,
                    &mut callbacks,
                    &mut current_snapshots,
                    &cache,
                    command,
                )
                .is_err()
                {
                    close_callbacks(&mut callbacks, None);
                    break;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => match wire.try_receive() {
                Ok(Some(frame)) => {
                    if handle_unsolicited(
                        &mut wire,
                        frame,
                        &mut callbacks,
                        &mut current_snapshots,
                        &cache,
                    )
                    .is_err()
                    {
                        close_callbacks(&mut callbacks, None);
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    close_callbacks(&mut callbacks, None);
                    break;
                }
            },
        }
    }
    close_callbacks(&mut callbacks, None);
}

fn execute_command(
    wire: &mut WireConnection,
    next_request_id: &mut u64,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
    cache: &Arc<Mutex<ProjectionCache>>,
    command: WorkerCommand,
) -> Result<(), MobileClientError> {
    match command {
        WorkerCommand::Subscribe {
            subscription_id,
            subscribe,
            callback,
            reply,
        } => {
            let request_id = take_request_id(next_request_id)?;
            callbacks.insert(
                subscription_id,
                ActiveSubscription {
                    key: subscribe.key.clone(),
                    callback,
                },
            );
            let result = (|| {
                wire.send_control(&ControlFrame::ProjectionSubscribeFrame {
                    request_id,
                    subscription_id,
                    subscribe: subscribe.clone(),
                })
                .map_err(|_| MobileClientError::WorkerStopped)?;
                let ack = wait_for(
                    wire,
                    callbacks,
                    current_snapshots,
                    cache,
                    |frame| match frame {
                        ControlFrame::ProjectionSubscribeAckFrame {
                            request_id: found,
                            subscription_id: found_subscription,
                            ack,
                        } if *found == request_id && *found_subscription == subscription_id => {
                            Some(ack.clone())
                        }
                        _ => None,
                    },
                )?;
                ack.validate_for(&subscribe)
                    .map_err(|_| MobileClientError::Contract)?;
                if let ProjectionSubscribeOutcome::CurrentFollows { current_cursor } = &ack.outcome
                {
                    current_snapshots.insert(subscription_id, *current_cursor);
                }
                if let ProjectionSubscribeOutcome::Rejected { error } = ack.outcome {
                    if let Some(active) = callbacks.remove(&subscription_id) {
                        active.callback.on_error(error.clone());
                        active.callback.on_closed(Some(error));
                    }
                    return Err(MobileClientError::RemoteRejected);
                }
                Ok(())
            })();
            if result.is_err() {
                callbacks.remove(&subscription_id);
                current_snapshots.remove(&subscription_id);
            }
            let _send_result = reply.send(result);
        }
        WorkerCommand::Unsubscribe {
            subscription_id,
            reply,
        } => {
            let result = (|| {
                let request_id = take_request_id(next_request_id)?;
                wire.send_control(&ControlFrame::UnsubscribeRequest {
                    request_id,
                    subscription_id,
                })
                .map_err(|_| MobileClientError::WorkerStopped)?;
                wait_for(
                    wire,
                    callbacks,
                    current_snapshots,
                    cache,
                    |frame| match frame {
                        ControlFrame::UnsubscribeAck {
                            request_id: found,
                            subscription_id: found_subscription,
                        } if *found == request_id && *found_subscription == subscription_id => {
                            Some(())
                        }
                        _ => None,
                    },
                )?;
                if let Some(active) = callbacks.remove(&subscription_id) {
                    active.callback.on_closed(None);
                }
                current_snapshots.remove(&subscription_id);
                Ok(())
            })();
            let _send_result = reply.send(result);
        }
        WorkerCommand::SubmitCommand { request, reply } => {
            let result = (|| {
                let request_id = take_request_id(next_request_id)?;
                wire.send_control(&ControlFrame::DeviceCommandFrame {
                    request_id,
                    request,
                })
                .map_err(|_| MobileClientError::WorkerStopped)?;
                wait_for(
                    wire,
                    callbacks,
                    current_snapshots,
                    cache,
                    |frame| match frame {
                        ControlFrame::DeviceCommandAck {
                            request_id: found,
                            ack,
                        } if *found == request_id => Some(ack.clone()),
                        _ => None,
                    },
                )
            })();
            let _send_result = reply.send(result);
        }
        WorkerCommand::WriteContent {
            request,
            mut bytes,
            reply,
        } => {
            let result = (|| {
                request
                    .validate()
                    .map_err(|_| MobileClientError::Contract)?;
                let request_id = take_request_id(next_request_id)?;
                wire.send_control(&ControlFrame::ContentWriteHeader {
                    request_id,
                    request: request.clone(),
                })
                .and_then(|()| wire.send_content(request_id, &bytes))
                .map_err(|_| MobileClientError::WorkerStopped)?;
                wait_for(
                    wire,
                    callbacks,
                    current_snapshots,
                    cache,
                    |frame| match frame {
                        ControlFrame::ContentWriteResult {
                            request_id: found,
                            response,
                        } if *found == request_id => Some(response.clone()),
                        _ => None,
                    },
                )
            })();
            bytes.fill(0);
            let _send_result = reply.send(result);
        }
        WorkerCommand::ReadContent { request, reply } => {
            let result = (|| {
                request
                    .validate()
                    .map_err(|_| MobileClientError::Contract)?;
                let request_id = take_request_id(next_request_id)?;
                wire.send_control(&ControlFrame::ContentReadFrame {
                    request_id,
                    request,
                })
                .map_err(|_| MobileClientError::WorkerStopped)?;
                wait_for(
                    wire,
                    callbacks,
                    current_snapshots,
                    cache,
                    |frame| match frame {
                        ControlFrame::ContentReadResult {
                            request_id: found,
                            response,
                        } if *found == request_id => Some(response.clone()),
                        _ => None,
                    },
                )
            })();
            let _send_result = reply.send(result);
        }
        WorkerCommand::Shutdown => return Err(MobileClientError::WorkerStopped),
    }
    Ok(())
}

fn wait_for<T>(
    wire: &mut WireConnection,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
    cache: &Arc<Mutex<ProjectionCache>>,
    mut select: impl FnMut(&ControlFrame) -> Option<T>,
) -> Result<T, MobileClientError> {
    loop {
        let frame = wire
            .receive()
            .map_err(|_| MobileClientError::WorkerStopped)?;
        if let Frame::Control(bytes) = &frame {
            let control = ControlFrame::decode(bytes).map_err(|_| MobileClientError::Contract)?;
            if let Some(value) = select(&control) {
                return Ok(value);
            }
        }
        handle_unsolicited(wire, frame, callbacks, current_snapshots, cache)?;
    }
}

fn handle_unsolicited(
    wire: &mut WireConnection,
    frame: Frame,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
    cache: &Arc<Mutex<ProjectionCache>>,
) -> Result<(), MobileClientError> {
    let control = frame
        .decode_control()
        .map_err(|_| MobileClientError::Contract)?;
    match control {
        ControlFrame::ProjectionEnvelopeFrame {
            subscription_id,
            envelope,
        } => apply_projection(
            subscription_id,
            envelope,
            callbacks,
            current_snapshots,
            cache,
        )?,
        ControlFrame::ProjectionSubscriptionClosed {
            subscription_id,
            error,
        } => {
            if error.code != ErrorCode::CursorGap || !error.retriable || error.detail_ref.is_some()
            {
                return Err(MobileClientError::Contract);
            }
            let active = callbacks
                .remove(&subscription_id)
                .ok_or(MobileClientError::Contract)?;
            current_snapshots.remove(&subscription_id);
            active.callback.on_error(error.clone());
            active.callback.on_closed(Some(error));
        }
        ControlFrame::Ping { request_id, nonce } => wire
            .send_control(&ControlFrame::Pong { request_id, nonce })
            .map_err(|_| MobileClientError::WorkerStopped)?,
        ControlFrame::TransportError { .. } => return Err(MobileClientError::RemoteRejected),
        _ => return Err(MobileClientError::Contract),
    }
    Ok(())
}

fn apply_projection(
    subscription_id: u64,
    envelope: ProjectionEnvelope,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
    cache: &Arc<Mutex<ProjectionCache>>,
) -> Result<(), MobileClientError> {
    let expected_key = callbacks
        .get(&subscription_id)
        .map(|active| &active.key)
        .ok_or(MobileClientError::Contract)?;
    if expected_key != &envelope.key {
        return Err(MobileClientError::Contract);
    }
    let applied = if let Some(expected_cursor) = current_snapshots.remove(&subscription_id) {
        lock(cache).apply_current(envelope.clone(), expected_cursor)
    } else {
        lock(cache).apply(envelope.clone())
    }
    .map_err(|_| MobileClientError::Contract)?;
    match applied {
        CacheApply::Applied => callbacks
            .get(&subscription_id)
            .ok_or(MobileClientError::Contract)?
            .callback
            .on_projection(envelope),
        CacheApply::RefreshRequired => {
            let error = cursor_gap();
            let active = callbacks
                .remove(&subscription_id)
                .ok_or(MobileClientError::Contract)?;
            active.callback.on_error(error.clone());
            active.callback.on_closed(Some(error));
            // The wire subscription is still active. Close the whole
            // authenticated connection now so a later push cannot be
            // mistaken for a new stream; reconnecting subscribes with
            // `since=None` because the incompatible key was removed.
            return Err(MobileClientError::Contract);
        }
    }
    Ok(())
}

fn authenticated_wire(
    paired: &PairedHost,
    signer: &dyn DeviceSigner,
) -> Result<WireConnection, MobileClientError> {
    let mut wire = WireConnection::connect(&paired.endpoint, &paired.host_public_key_pin)
        .map_err(|_| MobileClientError::Authentication)?;
    hello(&mut wire)?;
    wire.send_control(&ControlFrame::ChallengeRequest {
        request_id: 3,
        device_id: paired.device_id.clone(),
    })
    .map_err(|_| MobileClientError::Authentication)?;
    let challenge = wire
        .receive_control()
        .map_err(|_| MobileClientError::Authentication)?;
    let ControlFrame::DeviceChallenge {
        request_id,
        challenge_id,
        nonce,
        expires_at_ms,
    } = challenge
    else {
        return Err(MobileClientError::Authentication);
    };
    if request_id != 3 {
        return Err(MobileClientError::Authentication);
    }
    let exporter = wire
        .exporter()
        .map_err(|_| MobileClientError::Authentication)?;
    let transcript = build_transcript(&ChallengeTranscript {
        transport_version: TRANSPORT_VERSION,
        protocol_version: kaleido_proto::PROTOCOL_VERSION,
        host_id: &paired.host_id,
        device_id: &paired.device_id,
        tls_exporter: &exporter,
        challenge_id: &challenge_id,
        nonce: &nonce,
        expires_at_ms,
    })
    .map_err(|_| MobileClientError::Authentication)?;
    let signature = signer
        .sign_p256_sha256(transcript.to_vec())
        .map_err(|_| MobileClientError::Authentication)?;
    let mut proof = ControlFrame::ChallengeProof {
        request_id,
        challenge_id,
        signature_der: signature,
    };
    let sent = wire.send_sensitive_control(&proof);
    if let ControlFrame::ChallengeProof {
        challenge_id,
        signature_der,
        ..
    } = &mut proof
    {
        challenge_id.fill(0);
        signature_der.fill(0);
    }
    sent.map_err(|_| MobileClientError::Authentication)?;
    match wire
        .receive_control()
        .map_err(|_| MobileClientError::Authentication)?
    {
        ControlFrame::AuthAccepted { request_id: 3, .. } => Ok(wire),
        _ => Err(MobileClientError::Authentication),
    }
}

fn hello(wire: &mut WireConnection) -> Result<(), MobileClientError> {
    wire.send_control(&ControlFrame::TransportHello {
        request_id: 1,
        transport_version: TRANSPORT_VERSION.to_owned(),
        max_frame_length: MAX_FRAME_LENGTH,
    })
    .map_err(|_| MobileClientError::Authentication)?;
    match wire
        .receive_control()
        .map_err(|_| MobileClientError::Authentication)?
    {
        ControlFrame::TransportHelloAck {
            request_id: 1,
            transport_version,
            max_frame_length: MAX_FRAME_LENGTH,
        } if kaleido_transport::version_is_compatible(&transport_version) => {}
        _ => return Err(MobileClientError::Authentication),
    }
    wire.send_control(&ControlFrame::UacpHello {
        request_id: 2,
        protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
    })
    .map_err(|_| MobileClientError::Authentication)?;
    match wire
        .receive_control()
        .map_err(|_| MobileClientError::Authentication)?
    {
        ControlFrame::UacpHelloAck {
            request_id: 2,
            protocol_version,
        } if kaleido_proto::version_is_compatible(&protocol_version) => Ok(()),
        _ => Err(MobileClientError::Authentication),
    }
}

fn worker_sender(
    worker: &Mutex<Option<WorkerHandle>>,
) -> Result<mpsc::Sender<WorkerCommand>, MobileClientError> {
    lock(worker)
        .as_ref()
        .map(|worker| worker.commands.clone())
        .ok_or(MobileClientError::NotConnected)
}

fn receive_reply<T>(
    receiver: Receiver<Result<T, MobileClientError>>,
) -> Result<T, MobileClientError> {
    receiver
        .recv()
        .map_err(|_| MobileClientError::WorkerStopped)?
}

fn take_request_id(next: &mut u64) -> Result<u64, MobileClientError> {
    let current = *next;
    *next = next
        .checked_add(1)
        .ok_or(MobileClientError::IdentifierExhausted)?;
    Ok(current)
}

fn next_atomic(next: &AtomicU64) -> Result<u64, MobileClientError> {
    next.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
        current.checked_add(1)
    })
    .map_err(|_| MobileClientError::IdentifierExhausted)
}

fn close_callbacks(
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    error: Option<CanonicalError>,
) {
    for (_, active) in std::mem::take(callbacks) {
        active.callback.on_closed(error.clone());
    }
}

fn cursor_gap() -> CanonicalError {
    CanonicalError {
        code: ErrorCode::CursorGap,
        retriable: true,
        detail_ref: None,
        at_ms: system_time_ms(),
    }
}

fn system_time_ms() -> i64 {
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

    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use kaleido_proto::effect::Cursor;
    use kaleido_proto::error::CanonicalError;
    use kaleido_proto::host::HostReachability;
    use kaleido_proto::ids::HostId;
    use kaleido_proto::projection::{
        ProjectIndexView, ProjectionEnvelope, ProjectionKey, ProjectionPayload, PROJECTION_VERSION,
    };

    use super::{
        apply_projection, close_callbacks, ActiveSubscription, MobileClientError,
        ProjectionCallback,
    };
    use crate::cache::ProjectionCache;

    #[derive(Default)]
    struct CallbackEvents {
        projections: AtomicUsize,
        errors: AtomicUsize,
        closed: AtomicUsize,
    }

    struct TestCallback(Arc<CallbackEvents>);

    impl ProjectionCallback for TestCallback {
        fn on_projection(&self, _projection: ProjectionEnvelope) {
            self.0.projections.fetch_add(1, Ordering::SeqCst);
        }

        fn on_error(&self, _error: CanonicalError) {
            self.0.errors.fetch_add(1, Ordering::SeqCst);
        }

        fn on_closed(&self, _error: Option<CanonicalError>) {
            self.0.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn key(host: &str) -> ProjectionKey {
        ProjectionKey::ProjectIndex {
            host_id: HostId::new(host),
        }
    }

    fn envelope(host: &str, cursor: u64) -> ProjectionEnvelope {
        let host_id = HostId::new(host);
        ProjectionEnvelope {
            projection_version: PROJECTION_VERSION,
            key: ProjectionKey::ProjectIndex {
                host_id: host_id.clone(),
            },
            cursor: Cursor { seq: cursor },
            payload: ProjectionPayload::ProjectIndex {
                view: ProjectIndexView {
                    host_id,
                    reachability: HostReachability::Offline,
                    groups: Vec::new(),
                },
            },
        }
    }

    fn active(key: ProjectionKey, events: Arc<CallbackEvents>) -> ActiveSubscription {
        ActiveSubscription {
            key,
            callback: Box::new(TestCallback(events)),
        }
    }

    #[test]
    fn a_subscription_rejects_an_envelope_for_a_different_projection_key() {
        let directory = tempfile::tempdir().expect("cache directory");
        let cache = Arc::new(Mutex::new(
            ProjectionCache::open(directory.path()).expect("cache"),
        ));
        let events = Arc::new(CallbackEvents::default());
        let mut subscriptions = BTreeMap::from([(1, active(key("host-a"), events.clone()))]);

        assert_eq!(
            apply_projection(
                1,
                envelope("host-b", 0),
                &mut subscriptions,
                &mut BTreeMap::new(),
                &cache,
            ),
            Err(MobileClientError::Contract)
        );
        assert!(cache
            .lock()
            .expect("cache lock")
            .cached(&key("host-b"))
            .is_none());
        assert_eq!(events.projections.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_incompatible_projection_invalidates_only_its_key_and_closes_the_worker() {
        let directory = tempfile::tempdir().expect("cache directory");
        let mut initial = ProjectionCache::open(directory.path()).expect("cache");
        initial.apply(envelope("host-a", 0)).expect("target");
        initial.apply(envelope("host-b", 0)).expect("other");
        let cache = Arc::new(Mutex::new(initial));
        let target_events = Arc::new(CallbackEvents::default());
        let other_events = Arc::new(CallbackEvents::default());
        let mut subscriptions = BTreeMap::from([
            (1, active(key("host-a"), target_events.clone())),
            (2, active(key("host-b"), other_events.clone())),
        ]);
        let mut incompatible = envelope("host-a", 1);
        incompatible.projection_version = PROJECTION_VERSION.saturating_add(1);

        assert_eq!(
            apply_projection(
                1,
                incompatible,
                &mut subscriptions,
                &mut BTreeMap::new(),
                &cache,
            ),
            Err(MobileClientError::Contract)
        );
        close_callbacks(&mut subscriptions, None);

        let cache = cache.lock().expect("cache lock");
        assert!(cache.cached(&key("host-a")).is_none());
        assert!(cache.cached(&key("host-b")).is_some());
        assert_eq!(target_events.errors.load(Ordering::SeqCst), 1);
        assert_eq!(target_events.closed.load(Ordering::SeqCst), 1);
        assert_eq!(other_events.closed.load(Ordering::SeqCst), 1);
        drop(cache);
        let cold = ProjectionCache::open(directory.path()).expect("cold cache");
        assert_eq!(cold.since(&key("host-a")), None);
    }
}
