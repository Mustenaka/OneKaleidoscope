//! Persistent provider workers owned by the Broker composition root.
//!
//! Each provider conversation lives on one blocking worker thread. LAN tasks
//! only enqueue canonical commands; they never touch provider transports or
//! mutate the canonical store directly.

use std::collections::BTreeMap;
use std::sync::{mpsc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use kaleido_adapter::{ProviderRuntimeSession, SessionStartRequest};
use kaleido_proto::command::Command;
use kaleido_proto::effect::StateEffect;
use kaleido_proto::ids::{CommandId, SessionId};
use kaleido_state::{DispatchClaim, DispatchTicket, PendingDispatch};

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
pub enum ReadyRecoveryOutcome {
    Dispatched,
    RejectedRuntimeUnavailable,
}

enum WorkerMessage {
    Dispatch(Box<DispatchClaim>),
    Drain,
    Close,
}

#[derive(Debug)]
struct WorkerHandle {
    sender: mpsc::Sender<WorkerMessage>,
    join: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub struct RuntimeSupervisor {
    broker: Broker,
    workers: Mutex<BTreeMap<SessionId, WorkerHandle>>,
    reports_tx: mpsc::Sender<RuntimeDispatchReport>,
    reports_rx: Mutex<mpsc::Receiver<RuntimeDispatchReport>>,
}

impl RuntimeSupervisor {
    pub fn new(broker: Broker) -> Self {
        let (reports_tx, reports_rx) = mpsc::channel();
        Self {
            broker,
            workers: Mutex::new(BTreeMap::new()),
            reports_tx,
            reports_rx: Mutex::new(reports_rx),
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
        let join = thread::Builder::new()
            .name("kaleido-runtime".to_owned())
            .spawn(move || {
                run_worker(runtime, request, broker, receiver, reports, ready_tx);
            })
            .map_err(|_| RuntimeSupervisorError::WorkerStopped)?;
        match ready_rx.recv() {
            Ok(Ok(session_id)) => {
                let mut workers = lock(&self.workers);
                if workers.contains_key(&session_id) {
                    let _ = sender.send(WorkerMessage::Close);
                    let _ = join.join();
                    return Err(RuntimeSupervisorError::AlreadyRegistered);
                }
                workers.insert(
                    session_id.clone(),
                    WorkerHandle {
                        sender,
                        join: Some(join),
                    },
                );
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
        let sender = lock(&self.workers)
            .get(&session_id)
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
        let sender = lock(&self.workers)
            .get(session_id)
            .map(|worker| worker.sender.clone())
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
        sender
            .send(WorkerMessage::Drain)
            .map_err(|_| RuntimeSupervisorError::WorkerStopped)
    }

    pub fn try_report(&self) -> Option<RuntimeDispatchReport> {
        lock(&self.reports_rx).try_recv().ok()
    }

    pub fn stop_session(&self, session_id: &SessionId) -> Result<(), RuntimeSupervisorError> {
        let mut worker = lock(&self.workers)
            .remove(session_id)
            .ok_or(RuntimeSupervisorError::RuntimeUnavailable)?;
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
    reports: mpsc::Sender<RuntimeDispatchReport>,
    ready: mpsc::SyncSender<Result<SessionId, RuntimeSupervisorError>>,
) {
    let mut content = StoreContentAccess::new(broker.content_store());
    let start = runtime
        .start(&request, &mut content)
        .map_err(|_| RuntimeSupervisorError::RuntimeFailed)
        .and_then(|effects| {
            let session_id = unique_session_id(&effects)?;
            broker
                .apply_effects(&effects, now_ms())
                .map_err(map_broker)?;
            Ok(session_id)
        });
    if ready.send(start.clone()).is_err() || start.is_err() {
        return;
    }
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Dispatch(claim) => {
                let command_id = claim.envelope.command_id.clone();
                let result = dispatch_claim(&mut *runtime, &mut content, &broker, &claim);
                let _ = reports.send(RuntimeDispatchReport { command_id, result });
            }
            WorkerMessage::Drain => {
                if let Ok(effects) = runtime.drain_effects(&mut content) {
                    let _ = broker.apply_effects(&effects, now_ms());
                }
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
        _ => return Err(RuntimeSupervisorError::UnsupportedRoute),
    }
    .map_err(|_| RuntimeSupervisorError::RuntimeFailed)?;
    broker
        .finish_dispatch(&claim.ticket, &effects, now_ms())
        .map(|_| ())
        .map_err(map_broker)
}

fn route_session(pending: &PendingDispatch) -> Result<SessionId, RuntimeSupervisorError> {
    match &pending.envelope.body {
        Command::SubmitPrompt { session_id, .. } => Ok(session_id.clone()),
        Command::RespondAttention { response } => response
            .session_id
            .clone()
            .ok_or(RuntimeSupervisorError::UnsupportedRoute),
        _ => Err(RuntimeSupervisorError::UnsupportedRoute),
    }
}

fn unique_session_id(effects: &[StateEffect]) -> Result<SessionId, RuntimeSupervisorError> {
    let ids = effects
        .iter()
        .filter_map(|effect| match effect {
            StateEffect::SessionUpserted { session } => Some(session.id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() != 1 {
        return Err(RuntimeSupervisorError::SessionIdentity);
    }
    ids.into_iter()
        .next()
        .ok_or(RuntimeSupervisorError::SessionIdentity)
}

fn map_broker(_error: BrokerError) -> RuntimeSupervisorError {
    RuntimeSupervisorError::BrokerRejected
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
