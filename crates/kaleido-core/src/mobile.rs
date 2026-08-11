//! Product mobile client surface and its single authenticated connection worker.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use kaleido_transport::auth::{
    build_transcript, validate_p256_spki, verify_transcript_signature, ChallengeTranscript,
};
use kaleido_transport::bootstrap::decode_uri;
use kaleido_transport::control::{ControlFrame, PairRequest};
use kaleido_transport::frame::Frame;
use kaleido_transport::remote::{generate_remote_id, ExpectedRemoteResponse, RemoteControlFrame};
use kaleido_transport::remote_client::RemoteControlClient;
use kaleido_transport::{FRAME_IO_TIMEOUT_MS, MAX_FRAME_LENGTH, TRANSPORT_VERSION};
use zeroize::Zeroize;

use crate::cache::{CacheApply, ProjectionCache};
use crate::connection::{SelectedRemotePath, WireConnection};
use crate::credential::{
    CredentialStore, PairedHost, PairedHostInfo, RemoteAccess, SecureCredentialVault,
};
use crate::signer::DeviceSigner;

#[path = "path.rs"]
mod path;
#[path = "reconnect.rs"]
mod reconnect;

use path::{PathAttempt, PathMachine};
use reconnect::ReconnectBackoff;

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
    #[error("mobile remote control service is unavailable")]
    RemoteUnavailable,
    #[error("mobile connection worker stopped")]
    WorkerStopped,
    #[error("mobile request identifier space is exhausted")]
    IdentifierExhausted,
}

/// A connection path is online only after the inner TLS/device-authenticated
/// wire has completed.  Connecting deliberately has no online reachability.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MobileConnectionPath {
    Offline,
    Connecting,
    LanDirect,
    PeerToPeer,
    Relayed,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MobileConnectionStatus {
    pub path: MobileConnectionPath,
    pub at_ms: i64,
}

#[uniffi::export(callback_interface)]
pub trait ConnectionStatusCallback: Send + Sync {
    fn on_status(&self, status: MobileConnectionStatus);
}

struct ConnectionStatusSink {
    current: Mutex<MobileConnectionStatus>,
    callback: Mutex<Option<Arc<dyn ConnectionStatusCallback>>>,
}

impl ConnectionStatusSink {
    fn new(now_ms: i64) -> Self {
        Self {
            current: Mutex::new(MobileConnectionStatus {
                path: MobileConnectionPath::Offline,
                at_ms: now_ms,
            }),
            callback: Mutex::new(None),
        }
    }

    fn current(&self) -> MobileConnectionStatus {
        lock(&self.current).clone()
    }

    fn set_callback(&self, callback: Box<dyn ConnectionStatusCallback>) {
        let callback: Arc<dyn ConnectionStatusCallback> = Arc::from(callback);
        *lock(&self.callback) = Some(Arc::clone(&callback));
    }

    fn clear_callback(&self) {
        *lock(&self.callback) = None;
    }

    fn publish(&self, path: MobileConnectionPath, at_ms: i64) {
        let status = MobileConnectionStatus { path, at_ms };
        *lock(&self.current) = status.clone();
        let callback = lock(&self.callback).as_ref().map(Arc::clone);
        if let Some(callback) = callback {
            callback.on_status(status);
        }
    }
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
    network_epoch: Arc<AtomicU64>,
    status: Arc<ConnectionStatusSink>,
}

impl std::fmt::Debug for MobileClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MobileClient([redacted storage])")
    }
}

impl MobileClient {
    /// Native-only constructor retained for hostd loopback integration tests.
    /// Mobile bindings expose only `new_with_secure_vault`, so Android cannot
    /// accidentally persist endpoint, pin or DeviceId in plaintext storage.
    pub fn new(
        storage_directory: String,
        signer: Box<dyn DeviceSigner>,
    ) -> Result<Arc<Self>, MobileClientError> {
        let credentials =
            CredentialStore::open(&storage_directory).map_err(|_| MobileClientError::Storage)?;
        Self::from_stores(storage_directory, signer, credentials)
    }

    fn from_stores(
        cache_directory: String,
        signer: Box<dyn DeviceSigner>,
        credentials: CredentialStore,
    ) -> Result<Arc<Self>, MobileClientError> {
        let paired = credentials.load().map_err(|_| MobileClientError::Storage)?;
        let cache =
            ProjectionCache::open(&cache_directory).map_err(|_| MobileClientError::Storage)?;
        Ok(Arc::new(Self {
            signer: Arc::from(signer),
            credentials,
            paired: Mutex::new(paired),
            cache: Arc::new(Mutex::new(cache)),
            worker: Mutex::new(None),
            next_subscription_id: AtomicU64::new(1),
            network_epoch: Arc::new(AtomicU64::new(0)),
            status: Arc::new(ConnectionStatusSink::new(system_time_ms())),
        }))
    }
}

#[uniffi::export]
impl MobileClient {
    #[uniffi::constructor]
    pub fn new_with_secure_vault(
        cache_directory: String,
        signer: Box<dyn DeviceSigner>,
        credential_vault: Box<dyn SecureCredentialVault>,
    ) -> Result<Arc<Self>, MobileClientError> {
        let credentials = CredentialStore::secure(Arc::from(credential_vault));
        Self::from_stores(cache_directory, signer, credentials)
    }

    pub fn paired_host_info(&self) -> Option<PairedHostInfo> {
        lock(&self.paired).as_ref().map(PairedHostInfo::from)
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
        validate_p256_spki(&public_key).map_err(|_| MobileClientError::Authentication)?;
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
            remote: None,
        };
        self.credentials
            .store(&paired)
            .map_err(|_| MobileClientError::Storage)?;
        *lock(&self.paired) = Some(paired);
        Ok(response.device_id)
    }

    pub fn configure_remote(&self, mut bootstrap_uri: String) -> Result<(), MobileClientError> {
        if lock(&self.worker).is_some() {
            return Err(MobileClientError::AlreadyConnected);
        }
        let decoded = kaleido_transport::remote::decode_remote_bootstrap(&bootstrap_uri);
        bootstrap_uri.zeroize();
        let mut bootstrap = decoded.map_err(|_| MobileClientError::Contract)?;
        if system_time_ms() >= bootstrap.expires_at_ms {
            bootstrap.access_token.zeroize();
            return Err(MobileClientError::Authentication);
        }
        let mut paired = lock(&self.paired);
        let host = paired.as_mut().ok_or(MobileClientError::NotPaired)?;
        host.remote = Some(RemoteAccess {
            route_id: bootstrap.route_id.clone(),
            route_hint: bootstrap.route_hint.clone(),
            device_slot_id: bootstrap.device_slot_id.clone(),
            access_token: std::mem::take(&mut bootstrap.access_token),
            host_endpoint_id: bootstrap.host_endpoint_id.clone(),
            relay_url: bootstrap.relay_url.clone(),
            service_endpoint: bootstrap.service_endpoint.clone(),
            service_public_key_pin: bootstrap.service_public_key_pin.clone(),
            pending_push: None,
        });
        if self.credentials.store(host).is_err() {
            host.remote = None;
            return Err(MobileClientError::Storage);
        }
        Ok(())
    }

    pub fn remote_is_configured(&self) -> bool {
        lock(&self.paired)
            .as_ref()
            .is_some_and(|host| host.remote.is_some())
    }

    /// Durably replaces the device's opaque FCM address, then waits for the
    /// pinned self-hosted control service to acknowledge it. A network or
    /// service failure leaves the operation in the secure-vault outbox.
    pub fn replace_push_address(
        &self,
        opaque_address: String,
        registered_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<(), MobileClientError> {
        let mut paired = lock(&self.paired);
        let host = paired.as_mut().ok_or(MobileClientError::NotPaired)?;
        crate::remote_push::queue_replace(
            &self.credentials,
            host,
            opaque_address,
            registered_at_ms,
            expires_at_ms,
        )
        .map_err(map_remote_push_error)?;
        crate::remote_push::flush(&self.credentials, host, system_time_ms())
            .map(|_| ())
            .map_err(map_remote_push_error)
    }

    /// Durably tombstones the current opaque push address, then waits for the
    /// pinned service acknowledgement before reporting success.
    pub fn delete_push_address(&self) -> Result<(), MobileClientError> {
        let mut paired = lock(&self.paired);
        let host = paired.as_mut().ok_or(MobileClientError::NotPaired)?;
        crate::remote_push::queue_delete(&self.credentials, host).map_err(map_remote_push_error)?;
        crate::remote_push::flush(&self.credentials, host, system_time_ms())
            .map(|_| ())
            .map_err(map_remote_push_error)
    }

    /// Retries the secure-vault push outbox without changing its operation ID.
    pub fn flush_remote_push_outbox(&self) -> Result<(), MobileClientError> {
        let mut paired = lock(&self.paired);
        let host = paired.as_mut().ok_or(MobileClientError::NotPaired)?;
        crate::remote_push::flush(&self.credentials, host, system_time_ms())
            .map(|_| ())
            .map_err(map_remote_push_error)
    }

    /// Notifies the worker that the platform network generation changed.
    ///
    /// The generation is deliberately supplied by the platform instead of
    /// inferred from a socket error: a stale dial result must never publish
    /// an online path after Wi-Fi/cellular handover.  Existing subscription
    /// intents remain owned by the worker and are resumed from the durable
    /// projection cache after the new authenticated connection is ready.
    pub fn network_epoch_changed(&self, epoch: u64) -> Result<(), MobileClientError> {
        self.network_epoch.store(epoch, Ordering::SeqCst);
        let commands = worker_sender(&self.worker)?;
        commands
            .send(WorkerCommand::NetworkEpoch { epoch })
            .map_err(|_| MobileClientError::WorkerStopped)
    }

    pub fn connection_status(&self) -> MobileConnectionStatus {
        self.status.current()
    }

    pub fn set_connection_status_callback(&self, callback: Box<dyn ConnectionStatusCallback>) {
        self.status.set_callback(callback);
    }

    pub fn clear_connection_status_callback(&self) {
        self.status.clear_callback();
    }

    pub fn connect(&self) -> Result<(), MobileClientError> {
        let mut worker = lock(&self.worker);
        if worker
            .as_ref()
            .is_some_and(|existing| existing.join.is_finished())
        {
            if let Some(stale) = worker.take() {
                drop(stale.join.join());
            }
        }
        if worker.is_some() {
            return Err(MobileClientError::AlreadyConnected);
        }
        self.status
            .publish(MobileConnectionPath::Connecting, system_time_ms());
        let Some(paired) = lock(&self.paired).clone() else {
            self.status
                .publish(MobileConnectionPath::Offline, system_time_ms());
            return Err(MobileClientError::NotPaired);
        };
        let (commands, receiver) = mpsc::channel();
        let (ready, readiness) = mpsc::sync_channel(1);
        let signer = Arc::clone(&self.signer);
        let cache = Arc::clone(&self.cache);
        let network_epoch = self.network_epoch.load(Ordering::SeqCst);
        let epoch_source = Arc::clone(&self.network_epoch);
        let status = Arc::clone(&self.status);
        let join = std::thread::Builder::new()
            .name("kaleido-mobile-connection".to_owned())
            .spawn(move || {
                run_worker(
                    paired,
                    signer,
                    cache,
                    receiver,
                    ready,
                    network_epoch,
                    epoch_source,
                    status,
                )
            })
            .map_err(|_| {
                self.status
                    .publish(MobileConnectionPath::Offline, system_time_ms());
                MobileClientError::WorkerStopped
            })?;
        match readiness.recv() {
            Ok(Ok(())) => {
                *worker = Some(WorkerHandle { commands, join });
                Ok(())
            }
            Ok(Err(error)) => {
                drop(join.join());
                self.status
                    .publish(MobileConnectionPath::Offline, system_time_ms());
                Err(error)
            }
            Err(_) => {
                drop(join.join());
                self.status
                    .publish(MobileConnectionPath::Offline, system_time_ms());
                Err(MobileClientError::WorkerStopped)
            }
        }
    }

    pub fn reconnect(&self) -> Result<(), MobileClientError> {
        if lock(&self.worker).is_some() {
            let epoch = self
                .network_epoch
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                    current.checked_add(1)
                })
                .map_err(|_| MobileClientError::IdentifierExhausted)?;
            self.network_epoch_changed(epoch.saturating_add(1))
        } else {
            self.connect()
        }
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

fn map_remote_push_error(error: crate::remote_push::RemotePushError) -> MobileClientError {
    match error {
        crate::remote_push::RemotePushError::NotConfigured => MobileClientError::NotPaired,
        crate::remote_push::RemotePushError::Storage => MobileClientError::Storage,
        crate::remote_push::RemotePushError::Unavailable => MobileClientError::RemoteUnavailable,
        crate::remote_push::RemotePushError::Rejected => MobileClientError::RemoteRejected,
        crate::remote_push::RemotePushError::Contract => MobileClientError::Contract,
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
    NetworkEpoch {
        epoch: u64,
    },
    Shutdown,
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    paired: PairedHost,
    signer: Arc<dyn DeviceSigner>,
    cache: Arc<Mutex<ProjectionCache>>,
    commands: Receiver<WorkerCommand>,
    ready: SyncSender<Result<(), MobileClientError>>,
    initial_epoch: u64,
    epoch_source: Arc<AtomicU64>,
    status: Arc<ConnectionStatusSink>,
) {
    let mut path = PathMachine::new(system_time_ms());
    path.start(initial_epoch, system_time_ms());
    let connected = match authenticated_wire(&paired, signer.as_ref()) {
        Ok(connected) => connected,
        Err(error) => {
            status.publish(MobileConnectionPath::Offline, system_time_ms());
            let _send_result = ready.send(Err(error));
            return;
        }
    };
    if epoch_source.load(Ordering::SeqCst) != initial_epoch
        || connected.wire.set_poll_timeout(WORKER_POLL).is_err()
        || !publish_authenticated_path(connected.attempt, initial_epoch, &mut path, &status)
    {
        status.publish(MobileConnectionPath::Offline, system_time_ms());
        let _send_result = ready.send(Err(MobileClientError::Authentication));
        return;
    }
    let mut wire = Some(connected.wire);
    let _send_result = ready.send(Ok(()));
    let mut next_request_id = 4_u64;
    let mut callbacks: BTreeMap<u64, ActiveSubscription> = BTreeMap::new();
    let mut current_snapshots: BTreeMap<u64, Cursor> = BTreeMap::new();
    let mut backoff = ReconnectBackoff::default();
    loop {
        let observed_epoch = epoch_source.load(Ordering::SeqCst);
        if observed_epoch != path.epoch() {
            if let Some(active_wire) = wire.as_ref() {
                let _ = active_wire.notify_network_change();
            }
            path.network_changed(observed_epoch, system_time_ms());
            status.publish(MobileConnectionPath::Connecting, system_time_ms());
            current_snapshots.clear();
            wire = None;
            backoff.reset();
            continue;
        }
        if let Some(active_wire) = wire.as_ref() {
            publish_remote_path_if_changed(active_wire, &mut path, &status);
        }
        if wire.is_none() {
            match reconnect_worker(
                &paired,
                signer.as_ref(),
                &cache,
                &commands,
                &mut callbacks,
                &mut current_snapshots,
                &mut next_request_id,
                &mut path,
                &mut backoff,
                &epoch_source,
                &status,
            ) {
                ReconnectResult::Connected(reconnected) => {
                    wire = Some(*reconnected);
                    continue;
                }
                ReconnectResult::Shutdown => break,
            }
        }

        match commands.try_recv() {
            Ok(WorkerCommand::Shutdown) => break,
            Ok(WorkerCommand::NetworkEpoch { epoch }) => {
                if epoch != path.epoch() {
                    if let Some(active_wire) = wire.as_ref() {
                        let _ = active_wire.notify_network_change();
                    }
                    path.network_changed(epoch, system_time_ms());
                    status.publish(MobileConnectionPath::Connecting, system_time_ms());
                    current_snapshots.clear();
                    wire = None;
                    backoff.reset();
                }
            }
            Ok(command) => {
                let Some(active_wire) = wire.as_mut() else {
                    continue;
                };
                if execute_command(
                    active_wire,
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
            Err(mpsc::TryRecvError::Empty) => {
                let result = match wire.as_mut() {
                    Some(active_wire) => active_wire.try_receive(),
                    None => continue,
                };
                match result {
                    Ok(Some(frame)) => {
                        let Some(active_wire) = wire.as_mut() else {
                            continue;
                        };
                        if handle_unsolicited(
                            active_wire,
                            frame,
                            &mut callbacks,
                            &mut current_snapshots,
                            &cache,
                        )
                        .is_err()
                        {
                            path.offline(system_time_ms());
                            status.publish(MobileConnectionPath::Offline, system_time_ms());
                            current_snapshots.clear();
                            wire = None;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        path.offline(system_time_ms());
                        status.publish(MobileConnectionPath::Offline, system_time_ms());
                        current_snapshots.clear();
                        wire = None;
                    }
                }
            }
        }
    }
    close_callbacks(&mut callbacks, None);
}

enum ReconnectResult {
    Connected(Box<WireConnection>),
    Shutdown,
}

/// Reconnects LAN or iroh without dropping subscription intents or cached
/// cursors.  A path is published only after fresh inner TLS/device auth and
/// cursor-aware resubscription complete for the current network epoch.
#[allow(
    clippy::too_many_arguments,
    reason = "the reconnect loop must retain the authenticated worker state and each subscription map"
)]
fn reconnect_worker(
    paired: &PairedHost,
    signer: &dyn DeviceSigner,
    cache: &Arc<Mutex<ProjectionCache>>,
    commands: &Receiver<WorkerCommand>,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
    next_request_id: &mut u64,
    path: &mut PathMachine,
    backoff: &mut ReconnectBackoff,
    epoch_source: &Arc<AtomicU64>,
    status: &Arc<ConnectionStatusSink>,
) -> ReconnectResult {
    loop {
        let observed_epoch = epoch_source.load(Ordering::SeqCst);
        if observed_epoch != path.epoch() {
            path.network_changed(observed_epoch, system_time_ms());
        }
        status.publish(MobileConnectionPath::Connecting, system_time_ms());
        let delay = backoff.next_delay();
        let deadline = Instant::now()
            .checked_add(delay)
            .unwrap_or_else(Instant::now);
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(WORKER_POLL);
            match commands.recv_timeout(wait) {
                Ok(WorkerCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return ReconnectResult::Shutdown;
                }
                Ok(WorkerCommand::NetworkEpoch { epoch }) => {
                    if epoch != path.epoch() {
                        path.network_changed(epoch, system_time_ms());
                        backoff.reset();
                    }
                }
                Ok(command) => handle_disconnected_command(command, callbacks, current_snapshots),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }

        let attempt_epoch = epoch_source.load(Ordering::SeqCst);
        let mut candidate = match authenticated_wire(paired, signer) {
            Ok(candidate) => candidate,
            Err(_) => {
                path.offline(system_time_ms());
                status.publish(MobileConnectionPath::Offline, system_time_ms());
                continue;
            }
        };
        if epoch_source.load(Ordering::SeqCst) != attempt_epoch {
            continue;
        }
        if candidate.wire.set_poll_timeout(WORKER_POLL).is_err()
            || resubscribe_all(
                &mut candidate.wire,
                callbacks,
                current_snapshots,
                cache,
                next_request_id,
            )
            .is_err()
        {
            path.offline(system_time_ms());
            status.publish(MobileConnectionPath::Offline, system_time_ms());
            continue;
        }
        if epoch_source.load(Ordering::SeqCst) != attempt_epoch
            || !publish_authenticated_path(candidate.attempt, attempt_epoch, path, status)
        {
            continue;
        }
        backoff.reset();
        return ReconnectResult::Connected(Box::new(candidate.wire));
    }
}

fn handle_disconnected_command(
    command: WorkerCommand,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
) {
    match command {
        WorkerCommand::Subscribe {
            subscription_id,
            reply,
            ..
        } => {
            // The caller can retry the subscription once the public worker is
            // connected.  Existing intents are deliberately left untouched.
            let _ = reply.send(Err(MobileClientError::NotConnected));
            let _ = subscription_id;
        }
        WorkerCommand::Unsubscribe {
            subscription_id,
            reply,
        } => {
            if let Some(active) = callbacks.remove(&subscription_id) {
                active.callback.on_closed(None);
            }
            current_snapshots.remove(&subscription_id);
            let _ = reply.send(Ok(()));
        }
        WorkerCommand::SubmitCommand { reply, .. } => {
            let _ = reply.send(Err(MobileClientError::NotConnected));
        }
        WorkerCommand::WriteContent {
            mut bytes, reply, ..
        } => {
            bytes.fill(0);
            let _ = reply.send(Err(MobileClientError::NotConnected));
        }
        WorkerCommand::ReadContent { reply, .. } => {
            let _ = reply.send(Err(MobileClientError::NotConnected));
        }
        WorkerCommand::NetworkEpoch { .. } | WorkerCommand::Shutdown => {}
    }
}

fn resubscribe_all(
    wire: &mut WireConnection,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
    cache: &Arc<Mutex<ProjectionCache>>,
    next_request_id: &mut u64,
) -> Result<(), MobileClientError> {
    let subscriptions = callbacks.keys().copied().collect::<Vec<_>>();
    for subscription_id in subscriptions {
        let Some(key) = callbacks
            .get(&subscription_id)
            .map(|active| active.key.clone())
        else {
            continue;
        };
        let subscribe = resume_subscription(key, cache);
        let request_id = take_request_id(next_request_id)?;
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
        if let ProjectionSubscribeOutcome::Rejected { error } = ack.outcome.clone() {
            if let Some(active) = callbacks.remove(&subscription_id) {
                active.callback.on_error(error.clone());
                active.callback.on_closed(Some(error));
            }
            current_snapshots.remove(&subscription_id);
            continue;
        }
        if let ProjectionSubscribeOutcome::CurrentFollows { current_cursor } = ack.outcome {
            current_snapshots.insert(subscription_id, current_cursor);
        }
        let barrier_request_id = take_request_id(next_request_id)?;
        wire.send_control(&ControlFrame::Ping {
            request_id: barrier_request_id,
            nonce: barrier_request_id,
        })
        .map_err(|_| MobileClientError::WorkerStopped)?;
        wait_for(
            wire,
            callbacks,
            current_snapshots,
            cache,
            |frame| match frame {
                ControlFrame::Pong { request_id, nonce }
                    if *request_id == barrier_request_id && *nonce == barrier_request_id =>
                {
                    Some(())
                }
                _ => None,
            },
        )?;
        let synchronized_cursor = lock(cache)
            .since(&subscribe.key)
            .ok_or(MobileClientError::Contract)?;
        validate_synchronized_cursor(&ack.outcome, subscribe.since, synchronized_cursor)?;
    }
    Ok(())
}

fn resume_subscription(
    key: ProjectionKey,
    cache: &Arc<Mutex<ProjectionCache>>,
) -> ProjectionSubscribe {
    let since = lock(cache).since(&key);
    ProjectionSubscribe { key, since }
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

                // The subscribe ack deliberately precedes the initial replay/current envelope.
                // A request/response after that ack is therefore an ordered wire barrier: hostd
                // cannot read this Ping or write its Pong until it has written the complete
                // initial projection sequence. Waiting for the matching Pong lets callers treat
                // the core cache as synchronized when `subscribe` returns, including the
                // `since == head` case where Resumed carries no projection envelope.
                let barrier_request_id = take_request_id(next_request_id)?;
                wire.send_control(&ControlFrame::Ping {
                    request_id: barrier_request_id,
                    nonce: barrier_request_id,
                })
                .map_err(|_| MobileClientError::WorkerStopped)?;
                wait_for(
                    wire,
                    callbacks,
                    current_snapshots,
                    cache,
                    |frame| match frame {
                        ControlFrame::Pong { request_id, nonce }
                            if *request_id == barrier_request_id
                                && *nonce == barrier_request_id =>
                        {
                            Some(())
                        }
                        _ => None,
                    },
                )?;

                let synchronized_cursor = lock(cache)
                    .since(&subscribe.key)
                    .ok_or(MobileClientError::Contract)?;
                validate_synchronized_cursor(&ack.outcome, subscribe.since, synchronized_cursor)?;
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
        WorkerCommand::NetworkEpoch { .. } => {}
        WorkerCommand::Shutdown => return Err(MobileClientError::WorkerStopped),
    }
    Ok(())
}

fn validate_synchronized_cursor(
    outcome: &ProjectionSubscribeOutcome,
    since: Option<Cursor>,
    synchronized_cursor: Cursor,
) -> Result<(), MobileClientError> {
    match outcome {
        ProjectionSubscribeOutcome::CurrentFollows { current_cursor }
            if synchronized_cursor < *current_cursor =>
        {
            Err(MobileClientError::Contract)
        }
        ProjectionSubscribeOutcome::Resumed { .. }
            if since.is_none_or(|cursor| synchronized_cursor < cursor) =>
        {
            Err(MobileClientError::Contract)
        }
        ProjectionSubscribeOutcome::Rejected { .. } => Err(MobileClientError::Contract),
        ProjectionSubscribeOutcome::CurrentFollows { .. }
        | ProjectionSubscribeOutcome::Resumed { .. } => Ok(()),
    }
}

fn wait_for<T>(
    wire: &mut WireConnection,
    callbacks: &mut BTreeMap<u64, ActiveSubscription>,
    current_snapshots: &mut BTreeMap<u64, Cursor>,
    cache: &Arc<Mutex<ProjectionCache>>,
    mut select: impl FnMut(&ControlFrame) -> Option<T>,
) -> Result<T, MobileClientError> {
    let timeout_ms = u64::try_from(FRAME_IO_TIMEOUT_MS).map_err(|_| MobileClientError::Contract)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or(MobileClientError::Contract)?;
    loop {
        let frame = poll_until(deadline, || {
            wire.try_receive()
                .map_err(|_| MobileClientError::WorkerStopped)
        })?;
        if let Frame::Control(bytes) = &frame {
            let control = ControlFrame::decode(bytes).map_err(|_| MobileClientError::Contract)?;
            if let Some(value) = select(&control) {
                return Ok(value);
            }
        }
        handle_unsolicited(wire, frame, callbacks, current_snapshots, cache)?;
    }
}

fn poll_until<T>(
    deadline: Instant,
    mut poll: impl FnMut() -> Result<Option<T>, MobileClientError>,
) -> Result<T, MobileClientError> {
    loop {
        if Instant::now() >= deadline {
            return Err(MobileClientError::WorkerStopped);
        }
        if let Some(value) = poll()? {
            return Ok(value);
        }
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

struct AuthenticatedWire {
    wire: WireConnection,
    attempt: PathAttempt,
}

fn publish_authenticated_path(
    attempt: PathAttempt,
    epoch: u64,
    path: &mut PathMachine,
    status: &ConnectionStatusSink,
) -> bool {
    path.start(epoch, system_time_ms());
    match attempt {
        PathAttempt::Lan => {}
        PathAttempt::PeerToPeer => {
            if path.failed(epoch, system_time_ms()) != Some(PathAttempt::PeerToPeer) {
                return false;
            }
        }
        PathAttempt::Relay => {
            if path.failed(epoch, system_time_ms()) != Some(PathAttempt::PeerToPeer)
                || path.failed(epoch, system_time_ms()) != Some(PathAttempt::Relay)
            {
                return false;
            }
        }
    }
    if !path.established(attempt, epoch, system_time_ms()) {
        return false;
    }
    let mobile_path = match attempt {
        PathAttempt::Lan => MobileConnectionPath::LanDirect,
        PathAttempt::PeerToPeer => MobileConnectionPath::PeerToPeer,
        PathAttempt::Relay => MobileConnectionPath::Relayed,
    };
    if status.current().path != mobile_path {
        status.publish(mobile_path, system_time_ms());
    }
    true
}

fn publish_remote_path_if_changed(
    wire: &WireConnection,
    path: &mut PathMachine,
    status: &ConnectionStatusSink,
) {
    let attempt = match wire.selected_remote_path() {
        Ok(Some(SelectedRemotePath::PeerToPeer)) => PathAttempt::PeerToPeer,
        Ok(Some(SelectedRemotePath::Relayed)) => PathAttempt::Relay,
        Ok(None) | Err(_) => return,
    };
    let current = match attempt {
        PathAttempt::PeerToPeer => MobileConnectionPath::PeerToPeer,
        PathAttempt::Relay => MobileConnectionPath::Relayed,
        PathAttempt::Lan => MobileConnectionPath::LanDirect,
    };
    if status.current().path != current {
        let _ = publish_authenticated_path(attempt, path.epoch(), path, status);
    }
}

fn authenticated_wire(
    paired: &PairedHost,
    signer: &dyn DeviceSigner,
) -> Result<AuthenticatedWire, MobileClientError> {
    if let Ok(wire) = WireConnection::connect(&paired.endpoint, &paired.host_public_key_pin) {
        if let Ok(wire) = authenticate_over_wire(wire, paired, signer) {
            return Ok(AuthenticatedWire {
                wire,
                attempt: PathAttempt::Lan,
            });
        }
    }
    let remote = paired
        .remote
        .as_ref()
        .ok_or(MobileClientError::Authentication)?;
    let resolved = resolve_remote_route(remote)?;
    let wire = WireConnection::connect_remote(
        &resolved.host_endpoint_id,
        &resolved.relay_url,
        &remote.access_token,
        &paired.host_public_key_pin,
    )
    .map_err(|_| MobileClientError::Authentication)?;
    let wire = authenticate_over_wire(wire, paired, signer)?;
    let attempt = match wire
        .selected_remote_path()
        .map_err(|_| MobileClientError::Authentication)?
    {
        Some(SelectedRemotePath::PeerToPeer) => PathAttempt::PeerToPeer,
        Some(SelectedRemotePath::Relayed) => PathAttempt::Relay,
        None => return Err(MobileClientError::Authentication),
    };
    Ok(AuthenticatedWire { wire, attempt })
}

struct ResolvedRemoteRoute {
    host_endpoint_id: String,
    relay_url: String,
}

fn resolve_remote_route(remote: &RemoteAccess) -> Result<ResolvedRemoteRoute, MobileClientError> {
    let issued_at_ms = system_time_ms();
    let mut client =
        RemoteControlClient::connect(&remote.service_endpoint, &remote.service_public_key_pin)
            .map_err(|_| MobileClientError::Authentication)?;
    let mut request = RemoteControlFrame::ResolveRoute {
        request_id: 2,
        operation_id: generate_remote_id(),
        issued_at_ms,
        route_id: remote.route_id.clone(),
        device_slot_id: remote.device_slot_id.clone(),
        access_token: remote.access_token.clone(),
    };
    let response = client.request(&request, ExpectedRemoteResponse::RouteResolved);
    if let RemoteControlFrame::ResolveRoute { access_token, .. } = &mut request {
        access_token.zeroize();
    }
    validate_resolved_route(
        remote,
        response.map_err(|_| MobileClientError::Authentication)?,
        system_time_ms(),
    )
}

fn validate_resolved_route(
    remote: &RemoteAccess,
    response: RemoteControlFrame,
    now_ms: i64,
) -> Result<ResolvedRemoteRoute, MobileClientError> {
    let RemoteControlFrame::RouteResolved {
        host_endpoint_id,
        relay_url,
        expires_at_ms,
        ..
    } = response
    else {
        return Err(MobileClientError::Authentication);
    };
    if expires_at_ms <= now_ms
        || host_endpoint_id != remote.host_endpoint_id
        || relay_url != remote.relay_url
    {
        return Err(MobileClientError::Authentication);
    }
    Ok(ResolvedRemoteRoute {
        host_endpoint_id,
        relay_url,
    })
}

fn authenticate_over_wire(
    mut wire: WireConnection,
    paired: &PairedHost,
    signer: &dyn DeviceSigner,
) -> Result<WireConnection, MobileClientError> {
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
    let public_key = signer
        .public_key_spki_der()
        .map_err(|_| MobileClientError::Authentication)?;
    validate_p256_spki(&public_key).map_err(|_| MobileClientError::Authentication)?;
    let signature = signer
        .sign_p256_sha256(transcript.to_vec())
        .map_err(|_| MobileClientError::Authentication)?;
    verify_transcript_signature(&public_key, &transcript, &signature)
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
    let mut guard = lock(worker);
    if guard
        .as_ref()
        .is_some_and(|existing| existing.join.is_finished())
    {
        if let Some(stale) = guard.take() {
            drop(stale.join.join());
        }
        return Err(MobileClientError::WorkerStopped);
    }
    guard
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
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use kaleido_proto::effect::Cursor;
    use kaleido_proto::error::CanonicalError;
    use kaleido_proto::host::HostReachability;
    use kaleido_proto::ids::HostId;
    use kaleido_proto::projection::{
        ProjectIndexView, ProjectionEnvelope, ProjectionKey, ProjectionPayload,
        ProjectionSubscribeOutcome, PROJECTION_VERSION,
    };
    use kaleido_transport::remote::RemoteControlFrame;
    use kaleido_transport::remote_client::{read_frame, write_frame};
    use kaleido_transport::tls::{server_config, TlsIdentityStore};
    use rustls::{ServerConnection, StreamOwned};

    use super::{
        apply_projection, close_callbacks, poll_until, resolve_remote_route, resume_subscription,
        validate_resolved_route, validate_synchronized_cursor, worker_sender, ActiveSubscription,
        MobileClient, MobileClientError, ProjectionCallback, WorkerHandle,
    };
    use crate::cache::ProjectionCache;
    use crate::credential::{
        CredentialStore, PairedHost, RemoteAccess, SecureCredentialVault,
        SecureCredentialVaultError,
    };
    use crate::signer::{DeviceSigner, DeviceSignerError};

    #[derive(Default)]
    struct CallbackEvents {
        projections: AtomicUsize,
        errors: AtomicUsize,
        closed: AtomicUsize,
    }

    struct TestCallback(Arc<CallbackEvents>);

    #[derive(Default)]
    struct StatusEvents(Mutex<Vec<super::MobileConnectionStatus>>);

    struct TestStatusCallback(Arc<StatusEvents>);

    #[derive(Clone, Default)]
    struct TestVault {
        bytes: Arc<Mutex<Option<Vec<u8>>>>,
    }

    impl SecureCredentialVault for TestVault {
        fn load_paired_host(&self) -> Result<Option<Vec<u8>>, SecureCredentialVaultError> {
            Ok(self.bytes.lock().expect("vault lock").clone())
        }

        fn store_paired_host(&self, credential: Vec<u8>) -> Result<(), SecureCredentialVaultError> {
            *self.bytes.lock().expect("vault lock") = Some(credential);
            Ok(())
        }
    }

    struct UnusedSigner;

    impl DeviceSigner for UnusedSigner {
        fn public_key_spki_der(&self) -> Result<Vec<u8>, DeviceSignerError> {
            Err(DeviceSignerError::KeyUnavailable)
        }

        fn sign_p256_sha256(&self, _transcript: Vec<u8>) -> Result<Vec<u8>, DeviceSignerError> {
            Err(DeviceSignerError::SigningFailed)
        }
    }

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

    impl super::ConnectionStatusCallback for TestStatusCallback {
        fn on_status(&self, status: super::MobileConnectionStatus) {
            self.0 .0.lock().expect("status lock").push(status);
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

    fn remote_access(service_endpoint: String, service_public_key_pin: String) -> RemoteAccess {
        RemoteAccess {
            route_id: "AQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
            route_hint: "AgICAgICAgICAgICAgICAg".to_owned(),
            device_slot_id: "AwMDAwMDAwMDAwMDAwMDAw".to_owned(),
            access_token: "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
            host_endpoint_id: iroh::SecretKey::generate().public().to_string(),
            relay_url: "https://relay.example.test".to_owned(),
            service_endpoint,
            service_public_key_pin,
            pending_push: None,
        }
    }

    #[test]
    fn remote_route_is_resolved_over_pinned_tls_before_iroh_dial() {
        let directory = tempfile::tempdir().expect("TLS identity directory");
        let identity = TlsIdentityStore::new(directory.path().join("private").join("service.json"))
            .expect("identity store")
            .load_or_generate()
            .expect("service identity");
        let pin = identity.leaf_pin().expect("service pin").encode();
        let tls = server_config(identity).expect("server TLS");
        let listener = TcpListener::bind("127.0.0.1:0").expect("control listener");
        let endpoint = listener.local_addr().expect("control address").to_string();
        let remote = remote_access(endpoint, pin);
        let expected_endpoint = remote.host_endpoint_id.clone();
        let expected_relay = remote.relay_url.clone();
        let response_endpoint = expected_endpoint.clone();
        let response_relay = expected_relay.clone();
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().expect("control connection");
            let connection = ServerConnection::new(tls).expect("control TLS");
            let mut stream = StreamOwned::new(connection, socket);
            assert!(matches!(
                read_frame(&mut stream).expect("remote hello"),
                RemoteControlFrame::RemoteHello { request_id: 1, .. }
            ));
            write_frame(
                &mut stream,
                &RemoteControlFrame::RemoteHelloAck {
                    request_id: 1,
                    remote_control_version: "0.1.0".to_owned(),
                    max_frame_length: 4_096,
                },
                false,
            )
            .expect("hello ack");
            assert!(matches!(
                read_frame(&mut stream).expect("resolve request"),
                RemoteControlFrame::ResolveRoute {
                    request_id: 2,
                    ref route_id,
                    ref device_slot_id,
                    ref access_token,
                    ..
                } if route_id == "AQEBAQEBAQEBAQEBAQEBAQ"
                    && device_slot_id == "AwMDAwMDAwMDAwMDAwMDAw"
                    && access_token == "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ"
            ));
            write_frame(
                &mut stream,
                &RemoteControlFrame::RouteResolved {
                    request_id: 2,
                    host_endpoint_id: response_endpoint,
                    relay_url: response_relay,
                    expires_at_ms: i64::MAX,
                },
                false,
            )
            .expect("route response");
        });

        let resolved = resolve_remote_route(&remote).expect("pinned route resolution");
        assert_eq!(resolved.host_endpoint_id, expected_endpoint);
        assert_eq!(resolved.relay_url, expected_relay);
        server.join().expect("control server");
    }

    #[test]
    fn route_resolution_rejects_cache_mismatch_and_expiry() {
        let remote = remote_access(
            "127.0.0.1:7444".to_owned(),
            format!("sha256:{}", "A".repeat(43)),
        );
        let response = |host_endpoint_id: String, relay_url: String, expires_at_ms| {
            RemoteControlFrame::RouteResolved {
                request_id: 2,
                host_endpoint_id,
                relay_url,
                expires_at_ms,
            }
        };
        assert!(validate_resolved_route(
            &remote,
            response(
                remote.host_endpoint_id.clone(),
                remote.relay_url.clone(),
                101,
            ),
            100,
        )
        .is_ok());
        assert!(validate_resolved_route(
            &remote,
            response(
                iroh::SecretKey::generate().public().to_string(),
                remote.relay_url.clone(),
                101,
            ),
            100,
        )
        .is_err());
        assert!(validate_resolved_route(
            &remote,
            response(
                remote.host_endpoint_id.clone(),
                "https://different-relay.example.test".to_owned(),
                101,
            ),
            100,
        )
        .is_err());
        assert!(validate_resolved_route(
            &remote,
            response(
                remote.host_endpoint_id.clone(),
                remote.relay_url.clone(),
                100,
            ),
            100,
        )
        .is_err());
    }

    #[test]
    fn secure_constructor_cold_loads_only_paired_host_identity() {
        let directory = tempfile::tempdir().expect("cache directory");
        let vault = TestVault::default();
        let host = PairedHost {
            host_id: HostId::new("host-secure-cold"),
            device_id: kaleido_proto::ids::DeviceId::new("device-secure-cold"),
            endpoint: "127.0.0.1:7443".to_owned(),
            host_public_key_pin: format!("sha256:{}", "A".repeat(43)),
            remote: None,
        };
        CredentialStore::secure(Arc::new(vault.clone()))
            .store(&host)
            .expect("seed secure vault");

        let client = MobileClient::new_with_secure_vault(
            directory.path().to_string_lossy().into_owned(),
            Box::new(UnusedSigner),
            Box::new(vault),
        )
        .expect("cold client");

        let info = client.paired_host_info().expect("paired identity");
        assert_eq!(info.host_id, host.host_id);
        assert_eq!(info.device_id, host.device_id);
    }

    #[test]
    fn a_live_projection_before_the_barrier_pong_may_advance_past_the_current_ack() {
        let outcome = ProjectionSubscribeOutcome::CurrentFollows {
            current_cursor: Cursor { seq: 10 },
        };

        assert!(validate_synchronized_cursor(&outcome, None, Cursor { seq: 10 }).is_ok());
        assert!(
            validate_synchronized_cursor(&outcome, None, Cursor { seq: 11 }).is_ok(),
            "a contiguous live projection may be published before hostd reads the barrier Ping"
        );
        assert!(validate_synchronized_cursor(&outcome, None, Cursor { seq: 9 }).is_err());
    }

    #[test]
    fn a_request_wait_tolerates_empty_worker_polls_before_the_response() {
        let mut polls = 0_u8;
        let response = poll_until(Instant::now() + Duration::from_secs(1), || {
            polls = polls.saturating_add(1);
            Ok((polls == 3).then_some("response"))
        })
        .expect("response after bounded empty polls");

        assert_eq!(response, "response");
        assert_eq!(polls, 3);
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

    #[test]
    fn a_finished_worker_handle_is_reaped_instead_of_reported_as_connected() {
        let (commands, _receiver) = mpsc::channel();
        let join = std::thread::spawn(|| {});
        while !join.is_finished() {
            std::thread::yield_now();
        }
        // A finished JoinHandle still occupies the public slot until the
        // sender path observes it.  The next operation must clear it so a
        // subsequent connect can create a fresh worker.
        let worker = Mutex::new(Some(WorkerHandle { commands, join }));
        assert!(matches!(
            worker_sender(&worker),
            Err(MobileClientError::WorkerStopped)
        ));
        assert!(worker.lock().expect("worker lock").is_none());
    }

    #[test]
    fn status_callback_reports_connecting_and_offline_without_claiming_online() {
        let events = Arc::new(StatusEvents::default());
        let sink = super::ConnectionStatusSink::new(10);
        sink.set_callback(Box::new(TestStatusCallback(Arc::clone(&events))));
        sink.publish(super::MobileConnectionPath::Connecting, 11);
        sink.publish(super::MobileConnectionPath::Offline, 12);

        let statuses = events.0.lock().expect("status lock").clone();
        assert_eq!(statuses.len(), 2);
        let mut statuses = statuses.into_iter();
        assert_eq!(
            statuses.next().expect("initial status").path,
            super::MobileConnectionPath::Connecting
        );
        assert_eq!(
            statuses.next().expect("offline status").path,
            super::MobileConnectionPath::Offline
        );
        assert_eq!(sink.current().at_ms, 12);
    }

    #[test]
    fn reconnect_resume_uses_each_projection_keys_last_good_cursor() {
        let directory = tempfile::tempdir().expect("cache directory");
        let mut cache = ProjectionCache::open(directory.path()).expect("cache");
        cache.apply(envelope("host-a", 9)).expect("host a");
        cache.apply(envelope("host-b", 4)).expect("host b");
        let cache = Arc::new(Mutex::new(cache));

        let first = resume_subscription(key("host-a"), &cache);
        let second = resume_subscription(key("host-b"), &cache);
        assert_eq!(first.since, Some(Cursor { seq: 9 }));
        assert_eq!(second.since, Some(Cursor { seq: 4 }));
    }
}
