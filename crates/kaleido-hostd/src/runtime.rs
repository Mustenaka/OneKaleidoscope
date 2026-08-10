//! Persistent provider workers owned by the Broker composition root.
//!
//! Each provider conversation lives on one blocking worker thread. LAN tasks
//! only enqueue canonical commands; they never touch provider transports or
//! mutate the canonical store directly.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::{mpsc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use kaleido_adapter::{ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest};
use kaleido_proto::capability::CapabilityUnavailableReason;
use kaleido_proto::command::Command;
use kaleido_proto::effect::StateEffect;
use kaleido_proto::ids::{CommandId, QueueEntryId, SessionId};
use kaleido_state::{DispatchClaim, DispatchTicket, PendingDispatch, QueueDeliveryClaim};

use crate::broker::{Broker, BrokerError};
use crate::content::StoreContentAccess;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeSupervisorError {
    #[error("the session already has a runtime worker")]
    AlreadyRegistered,
    #[error("no ready runtime worker owns the command session")]
    RuntimeUnavailable,
    #[error("the runtime worker stopped")]
    WorkerStopped,
    #[error("the provider runtime rejected or lost the operation")]
    RuntimeFailed,
    #[error("the canonical broker rejected a runtime transition")]
    BrokerRejected,
    #[error("the command has no provider runtime route")]
    UnsupportedRoute,
    #[error("the provider bootstrap did not identify exactly one session")]
    SessionIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDispatchReport {
    pub command_id: CommandId,
    pub result: Result<(), RuntimeSupervisorError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLifecycleStage {
    Drain,
    ApplyDrain,
    Reconnect,
    ApplyReconnect,
    QueueDelivery,
    ApplyQueueDelivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFailureClass {
    NotConnected,
    AlreadyStarted,
    ConnectionFault,
    ProtocolViolation,
    CapabilityUnavailable,
    Content,
    Contract,
}

/// A provider worker lifecycle result that is not correlated to a command.
///
/// This channel is diagnostic only and deliberately separate from canonical
/// command acknowledgements. In particular, it cannot turn a local queue send
/// into evidence that a provider accepted the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLifecycleReport {
    pub session_id: SessionId,
    pub stage: RuntimeLifecycleStage,
    pub result: Result<(), RuntimeSupervisorError>,
    pub failure_class: Option<RuntimeFailureClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyRecoveryOutcome {
    Dispatched,
    RejectedRuntimeUnavailable,
}

enum WorkerMessage {
    Dispatch(Box<DispatchClaim>),
    DeliverQueue(Box<QueueDeliveryClaim>),
    Drain,
    Close,
}

#[derive(Debug)]
struct WorkerHandle {
    sender: mpsc::Sender<WorkerMessage>,
    join: Option<JoinHandle<()>>,
    drain_pending: Arc<AtomicBool>,
}

struct WorkerChannels {
    dispatch_reports: mpsc::Sender<RuntimeDispatchReport>,
    lifecycle_reports: mpsc::SyncSender<RuntimeLifecycleReport>,
    ready: mpsc::SyncSender<Result<WorkerReady, RuntimeSupervisorError>>,
}

#[derive(Debug, Clone)]
struct WorkerReady {
    primary_session_id: SessionId,
    routed_session_ids: Vec<SessionId>,
}

#[derive(Debug)]
pub struct RuntimeSupervisor {
    broker: Broker,
    workers: Mutex<BTreeMap<SessionId, WorkerHandle>>,
    session_routes: Mutex<BTreeMap<SessionId, SessionId>>,
    reports_tx: mpsc::Sender<RuntimeDispatchReport>,
    reports_rx: Mutex<mpsc::Receiver<RuntimeDispatchReport>>,
    lifecycle_tx: mpsc::SyncSender<RuntimeLifecycleReport>,
    lifecycle_rx: Mutex<mpsc::Receiver<RuntimeLifecycleReport>>,
}

impl RuntimeSupervisor {
    pub fn new(broker: Broker) -> Self {
        let (reports_tx, reports_rx) = mpsc::channel();
        let (lifecycle_tx, lifecycle_rx) = mpsc::sync_channel(64);
        Self {
            broker,
            workers: Mutex::new(BTreeMap::new()),
            session_routes: Mutex::new(BTreeMap::new()),
            reports_tx,
            reports_rx: Mutex::new(reports_rx),
            lifecycle_tx,
            lifecycle_rx: Mutex::new(lifecycle_rx),
        }
    }

    /// Starts one provider conversation and only publishes it as ready after
    /// its structured bootstrap effects have reached the Broker.
    pub fn start_runtime(
        &self,
        request: SessionStartRequest,
        runtime: Box<dyn ProviderRuntimeSession + Send>,
    ) -> Result<SessionId, RuntimeSupervisorError> {
        let (sender, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let broker = self.broker.clone();
        let reports = self.reports_tx.clone();
        let lifecycle = self.lifecycle_tx.clone();
        let drain_pending = Arc::new(AtomicBool::new(false));
        let worker_drain_pending = Arc::clone(&drain_pending);
        let join = thread::Builder::new()
            .name("kaleido-runtime".to_owned())
            .spawn(move || {
                run_worker(
                    runtime,
                    request,
                    broker,
                    receiver,
                    WorkerChannels {
                        dispatch_reports: reports,
                        lifecycle_reports: lifecycle,
                        ready: ready_tx,
                    },
                    worker_drain_pending,
                );
            })
            .map_err(|_| RuntimeSupervisorError::WorkerStopped)?;
        match ready_rx.recv() {
            Ok(Ok(ready)) => {
                let session_id = ready.primary_session_id;
                let mut workers = lock(&self.workers);
                let routes = lock(&self.session_routes);
                if workers.contains_key(&session_id)
                    || ready
                        .routed_session_ids
                        .iter()
                        .any(|candidate| routes.contains_key(candidate))
                {
                    drop(routes);
                    let _ = sender.send(WorkerMessage::Close);
                    let _ = join.join();
                    return Err(RuntimeSupervisorError::AlreadyRegistered);
                }
                drop(routes);
                workers.insert(
                    session_id.clone(),
                    WorkerHandle {
                        sender,
                        join: Some(join),
                        drain_pending,
                    },
                );
                drop(workers);
                let mut routes = lock(&self.session_routes);
                for routed_session_id in ready.routed_session_ids {
                    routes.insert(routed_session_id, session_id.clone());
                }
                Ok(session_id)
            }
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(RuntimeSupervisorError::WorkerStopped)
            }
        }
    }

    /// Dispatches a durable Ready outbox entry. Runtime readiness is resolved
    /// before the durable claim; after the claim, any provider failure is
    /// intentionally uncertain and the command is never auto-replayed.
    pub fn dispatch_ticket(&self, ticket: &DispatchTicket) -> Result<(), RuntimeSupervisorError> {
        let pending = self
            .broker
            .pending_dispatches()
            .into_iter()
            .find(|pending| &pending.ticket == ticket)
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        let session_id = route_session(&pending)?;
        let primary_session_id = lock(&self.session_routes)
            .get(&session_id)
            .cloned()
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        let sender = lock(&self.workers)
            .get(&primary_session_id)
            .map(|worker| worker.sender.clone())
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        let claim = self
            .broker
            .claim_dispatch(ticket, now_ms())
            .map_err(map_broker)?;
        sender
            .send(WorkerMessage::Dispatch(Box::new(claim)))
            .map_err(|_| RuntimeSupervisorError::WorkerStopped)
    }

    pub fn dispatch_all_ready(&self) -> Vec<(CommandId, Result<(), RuntimeSupervisorError>)> {
        self.broker
            .pending_dispatches()
            .into_iter()
            .map(|pending| {
                let command_id = pending.envelope.command_id.clone();
                let result = self.dispatch_ticket(&pending.ticket);
                (command_id, result)
            })
            .collect()
    }

    pub fn pump_pending_queue(&self) -> Vec<(QueueEntryId, Result<(), RuntimeSupervisorError>)> {
        self.broker
            .pending_queue_deliveries()
            .into_iter()
            .map(|(entry, _)| {
                let entry_id = entry.id.clone();
                let result = (|| {
                    let primary_session_id = lock(&self.session_routes)
                        .get(&entry.session_id)
                        .cloned()
                        .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
                    let sender = lock(&self.workers)
                        .get(&primary_session_id)
                        .map(|worker| worker.sender.clone())
                        .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
                    let claim = self
                        .broker
                        .claim_queue_delivery(&entry_id, now_ms())
                        .map_err(map_broker)?;
                    sender
                        .send(WorkerMessage::DeliverQueue(Box::new(claim)))
                        .map_err(|_| RuntimeSupervisorError::WorkerStopped)
                })();
                (entry_id, result)
            })
            .collect()
    }

    /// Recovers every durable Ready entry before the listener becomes live.
    /// Entries whose original session has no worker are durably rejected;
    /// claimed/uncertain entries are absent from `pending_dispatches` and can
    /// therefore never be sent a second time.
    pub fn recover_all_ready(
        &self,
    ) -> Vec<(
        CommandId,
        Result<ReadyRecoveryOutcome, RuntimeSupervisorError>,
    )> {
        self.broker
            .pending_dispatches()
            .into_iter()
            .map(|pending| {
                let command_id = pending.envelope.command_id.clone();
                let outcome = match self.dispatch_ticket(&pending.ticket) {
                    Ok(()) => Ok(ReadyRecoveryOutcome::Dispatched),
                    Err(
                        RuntimeSupervisorError::RuntimeUnavailable
                        | RuntimeSupervisorError::UnsupportedRoute,
                    ) => self
                        .broker
                        .reject_ready_dispatch(&pending.ticket, now_ms())
                        .map(|_| ReadyRecoveryOutcome::RejectedRuntimeUnavailable)
                        .map_err(map_broker),
                    Err(error) => Err(error),
                };
                (command_id, outcome)
            })
            .collect()
    }

    pub fn drain_session(&self, session_id: &SessionId) -> Result<(), RuntimeSupervisorError> {
        let primary_session_id = lock(&self.session_routes)
            .get(session_id)
            .cloned()
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        let workers = lock(&self.workers);
        let worker = workers
            .get(&primary_session_id)
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        request_drain(worker)
    }

    pub fn drain_all(&self) {
        for worker in lock(&self.workers).values() {
            let _ = request_drain(worker);
        }
    }

    pub fn try_report(&self) -> Option<RuntimeDispatchReport> {
        lock(&self.reports_rx).try_recv().ok()
    }

    pub fn try_lifecycle_report(&self) -> Option<RuntimeLifecycleReport> {
        lock(&self.lifecycle_rx).try_recv().ok()
    }

    pub fn stop_session(&self, session_id: &SessionId) -> Result<(), RuntimeSupervisorError> {
        let primary_session_id = lock(&self.session_routes)
            .get(session_id)
            .cloned()
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        let mut worker = lock(&self.workers)
            .remove(&primary_session_id)
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        lock(&self.session_routes).retain(|_, primary| primary != &primary_session_id);
        worker
            .sender
            .send(WorkerMessage::Close)
            .map_err(|_| RuntimeSupervisorError::WorkerStopped)?;
        if let Some(join) = worker.join.take() {
            join.join()
                .map_err(|_| RuntimeSupervisorError::WorkerStopped)?;
        }
        Ok(())
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        let workers = match self.workers.get_mut() {
            Ok(workers) => workers,
            Err(poisoned) => poisoned.into_inner(),
        };
        for worker in workers.values() {
            let _ = worker.sender.send(WorkerMessage::Close);
        }
        for worker in workers.values_mut() {
            if let Some(join) = worker.join.take() {
                let _ = join.join();
            }
        }
    }
}

fn run_worker(
    mut runtime: Box<dyn ProviderRuntimeSession + Send>,
    request: SessionStartRequest,
    broker: Broker,
    receiver: mpsc::Receiver<WorkerMessage>,
    channels: WorkerChannels,
    drain_pending: Arc<AtomicBool>,
) {
    let mut content = StoreContentAccess::new(broker.content_store());
    let start = (|| {
        let discovery = runtime.discover(&request, &mut content).map_err(|error| {
            tracing::warn!(
                stage = "discover",
                class = runtime_error_class(&error),
                "provider runtime bootstrap failed"
            );
            RuntimeSupervisorError::RuntimeFailed
        })?;
        let mut routed_session_ids = session_ids(&discovery);
        broker
            .apply_effects(&discovery, now_ms())
            .map_err(|error| {
                tracing::warn!(
                    stage = "apply_discovery",
                    error = ?error,
                    "provider runtime bootstrap failed"
                );
                map_broker(error)
            })?;
        let effects = runtime.start(&request, &mut content).map_err(|error| {
            tracing::warn!(
                stage = "start",
                class = runtime_error_class(&error),
                "provider runtime bootstrap failed"
            );
            RuntimeSupervisorError::RuntimeFailed
        })?;
        let session_id = unique_session_id(&effects).map_err(|error| {
            tracing::warn!(
                stage = "session_identity",
                effect_count = effects.len(),
                error = ?error,
                "provider runtime bootstrap failed"
            );
            error
        })?;
        broker.apply_effects(&effects, now_ms()).map_err(|error| {
            tracing::warn!(
                stage = "apply_start",
                error = ?error,
                "provider runtime bootstrap failed"
            );
            map_broker(error)
        })?;
        if !routed_session_ids.contains(&session_id) {
            routed_session_ids.push(session_id.clone());
        }
        Ok(WorkerReady {
            primary_session_id: session_id,
            routed_session_ids,
        })
    })();
    if channels.ready.send(start.clone()).is_err() || start.is_err() {
        return;
    }
    let mut session_id = match start {
        Ok(ready) => ready.primary_session_id,
        Err(_) => return,
    };
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Dispatch(claim) => {
                let command_id = claim.envelope.command_id.clone();
                let resumed_session = match &claim.envelope.body {
                    Command::ResumeSession { session_id } => Some(session_id.clone()),
                    _ => None,
                };
                let result = dispatch_claim(&mut *runtime, &request, &mut content, &broker, &claim);
                if result.is_ok() {
                    if let Some(resumed_session) = resumed_session {
                        session_id = resumed_session;
                    }
                }
                let _ = channels
                    .dispatch_reports
                    .send(RuntimeDispatchReport { command_id, result });
            }
            WorkerMessage::DeliverQueue(claim) => {
                let entry_id = claim.entry.id.clone();
                let queue_session_id = claim.entry.session_id.clone();
                match runtime.deliver_queue_entry(&claim.command_id, &claim.entry, &mut content) {
                    Ok(effects) => {
                        if let Err(error) =
                            broker.finish_queue_delivery(&entry_id, &effects, now_ms())
                        {
                            tracing::warn!(
                                stage = "apply_queue_delivery",
                                error = ?error,
                                "provider queue delivery effects were rejected"
                            );
                            let _ = channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                                session_id: queue_session_id,
                                stage: RuntimeLifecycleStage::ApplyQueueDelivery,
                                result: Err(RuntimeSupervisorError::BrokerRejected),
                                failure_class: None,
                            });
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            stage = "queue_delivery",
                            class = runtime_error_class(&error),
                            "provider queue delivery failed"
                        );
                        let _ = channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                            session_id: queue_session_id,
                            stage: RuntimeLifecycleStage::QueueDelivery,
                            result: Err(RuntimeSupervisorError::RuntimeFailed),
                            failure_class: Some(runtime_failure_class(&error)),
                        });
                    }
                }
            }
            WorkerMessage::Drain => {
                match runtime.drain_effects(&mut content) {
                    Ok(effects) => {
                        if let Err(error) = broker.apply_effects(&effects, now_ms()) {
                            tracing::warn!(
                                stage = "apply_drain",
                                error = ?error,
                                "provider runtime lifecycle effects were rejected"
                            );
                            let _ = channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                                session_id: session_id.clone(),
                                stage: RuntimeLifecycleStage::ApplyDrain,
                                result: Err(RuntimeSupervisorError::BrokerRejected),
                                failure_class: None,
                            });
                        }
                    }
                    Err(error) if error.ends_the_connection() => {
                        let _ = channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                            session_id: session_id.clone(),
                            stage: RuntimeLifecycleStage::Drain,
                            result: Err(RuntimeSupervisorError::RuntimeFailed),
                            failure_class: Some(runtime_failure_class(&error)),
                        });
                        let at_ms = now_ms();
                        let loss_effects = runtime.connection_lost_effects(
                            CapabilityUnavailableReason::SubscriptionLost,
                            at_ms,
                        );
                        if !loss_effects.is_empty() {
                            if let Err(apply_error) = broker.apply_effects(&loss_effects, at_ms) {
                                tracing::warn!(
                                    stage = "apply_connection_loss",
                                    error = ?apply_error,
                                    "provider connection-loss effects were rejected"
                                );
                                let _ =
                                    channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                                        session_id: session_id.clone(),
                                        stage: RuntimeLifecycleStage::ApplyDrain,
                                        result: Err(RuntimeSupervisorError::BrokerRejected),
                                        failure_class: None,
                                    });
                            }
                        }
                        match runtime.reconnect(&request, &mut content) {
                            Ok(effects) => {
                                let result = broker
                                    .apply_effects(&effects, now_ms())
                                    .map(|_| ())
                                    .map_err(|error| {
                                        tracing::warn!(
                                            stage = "apply_reconnect",
                                            error = ?error,
                                            "provider runtime lifecycle effects were rejected"
                                        );
                                        map_broker(error)
                                    });
                                let stage = if result.is_ok() {
                                    RuntimeLifecycleStage::Reconnect
                                } else {
                                    RuntimeLifecycleStage::ApplyReconnect
                                };
                                let _ =
                                    channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                                        session_id: session_id.clone(),
                                        stage,
                                        result,
                                        failure_class: None,
                                    });
                            }
                            Err(reconnect_error) => {
                                tracing::warn!(
                                    stage = "reconnect",
                                    class = runtime_error_class(&reconnect_error),
                                    "provider runtime lifecycle failed"
                                );
                                let _ =
                                    channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                                        session_id: session_id.clone(),
                                        stage: RuntimeLifecycleStage::Reconnect,
                                        result: Err(RuntimeSupervisorError::RuntimeFailed),
                                        failure_class: Some(runtime_failure_class(
                                            &reconnect_error,
                                        )),
                                    });
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            stage = "drain",
                            class = runtime_error_class(&error),
                            "provider runtime lifecycle failed"
                        );
                        let _ = channels.lifecycle_reports.try_send(RuntimeLifecycleReport {
                            session_id: session_id.clone(),
                            stage: RuntimeLifecycleStage::Drain,
                            result: Err(RuntimeSupervisorError::RuntimeFailed),
                            failure_class: Some(runtime_failure_class(&error)),
                        });
                    }
                }
                drain_pending.store(false, Ordering::Release);
            }
            WorkerMessage::Close => {
                if let Ok(effects) = runtime.close() {
                    let _ = broker.apply_effects(&effects, now_ms());
                }
                break;
            }
        }
    }
}

fn dispatch_claim(
    runtime: &mut dyn ProviderRuntimeSession,
    request: &SessionStartRequest,
    content: &mut StoreContentAccess,
    broker: &Broker,
    claim: &DispatchClaim,
) -> Result<(), RuntimeSupervisorError> {
    let effects = match &claim.envelope.body {
        Command::SubmitPrompt { body, .. } => {
            runtime.submit_prompt(&claim.envelope.command_id, body, content)
        }
        Command::RespondAttention { response } => {
            runtime.respond_attention(&claim.envelope.command_id, response, content)
        }
        Command::InterruptTurn { turn_id, .. } => {
            runtime.interrupt_turn(&claim.envelope.command_id, turn_id, content)
        }
        Command::ResumeSession { session_id } => {
            runtime.resume_session(session_id, request, content)
        }
        _ => return Err(RuntimeSupervisorError::UnsupportedRoute),
    }
    .map_err(|_| RuntimeSupervisorError::RuntimeFailed)?;
    broker
        .finish_dispatch(&claim.ticket, &effects, now_ms())
        .map(|_| ())
        .map_err(|error| {
            tracing::warn!(
                stage = "finish_dispatch",
                error = ?error,
                "provider runtime dispatch result was rejected"
            );
            map_broker(error)
        })
}

fn route_session(pending: &PendingDispatch) -> Result<SessionId, RuntimeSupervisorError> {
    match &pending.envelope.body {
        Command::SubmitPrompt { session_id, .. } => Ok(session_id.clone()),
        Command::RespondAttention { response } => response
            .session_id
            .clone()
            .ok_or(RuntimeSupervisorError::UnsupportedRoute),
        Command::InterruptTurn { session_id, .. } => Ok(session_id.clone()),
        Command::ResumeSession { session_id } => Ok(session_id.clone()),
        _ => Err(RuntimeSupervisorError::UnsupportedRoute),
    }
}

fn unique_session_id(effects: &[StateEffect]) -> Result<SessionId, RuntimeSupervisorError> {
    let ids = session_ids(effects)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() != 1 {
        return Err(RuntimeSupervisorError::SessionIdentity);
    }
    ids.into_iter()
        .next()
        .ok_or(RuntimeSupervisorError::SessionIdentity)
}

fn session_ids(effects: &[StateEffect]) -> Vec<SessionId> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            StateEffect::SessionUpserted { session } => Some(session.id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn map_broker(_error: BrokerError) -> RuntimeSupervisorError {
    RuntimeSupervisorError::BrokerRejected
}

fn runtime_error_class(error: &RuntimeSessionError) -> &'static str {
    match error {
        RuntimeSessionError::NotConnected => "not_connected",
        RuntimeSessionError::AlreadyStarted => "already_started",
        RuntimeSessionError::ConnectionFault { .. } => "connection_fault",
        RuntimeSessionError::ProtocolViolation { .. } => "protocol_violation",
        RuntimeSessionError::CapabilityUnavailable => "capability_unavailable",
        RuntimeSessionError::Content(_) => "content",
        RuntimeSessionError::Contract(_) => "contract",
    }
}

fn runtime_failure_class(error: &RuntimeSessionError) -> RuntimeFailureClass {
    match error {
        RuntimeSessionError::NotConnected => RuntimeFailureClass::NotConnected,
        RuntimeSessionError::AlreadyStarted => RuntimeFailureClass::AlreadyStarted,
        RuntimeSessionError::ConnectionFault { .. } => RuntimeFailureClass::ConnectionFault,
        RuntimeSessionError::ProtocolViolation { .. } => RuntimeFailureClass::ProtocolViolation,
        RuntimeSessionError::CapabilityUnavailable => RuntimeFailureClass::CapabilityUnavailable,
        RuntimeSessionError::Content(_) => RuntimeFailureClass::Content,
        RuntimeSessionError::Contract(_) => RuntimeFailureClass::Contract,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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

fn request_drain(worker: &WorkerHandle) -> Result<(), RuntimeSupervisorError> {
    if worker
        .drain_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    if worker.sender.send(WorkerMessage::Drain).is_err() {
        worker.drain_pending.store(false, Ordering::Release);
        return Err(RuntimeSupervisorError::WorkerStopped);
    }
    Ok(())
}
