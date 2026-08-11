//! The canonical Broker and its projection subscription hub.
//!
//! The mutex is intentional: it is the single ordering point between a
//! canonical append, the projection journal entry derived from that append,
//! and live fanout. Provider workers and LAN connection tasks never mutate a
//! [`CanonicalStore`] directly.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use kaleido_adapter::identity::IdentityMint;
use kaleido_proto::command::{Actor, CommandEnvelope, DeviceCommandRequest};
use kaleido_proto::content::{
    ContentReadRequest, ContentReadResponse, ContentWriteRequest, ContentWriteResponse,
};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::host::HostReachability;
use kaleido_proto::ids::{CommandId, DeviceId, HostId, QueueEntryId};
use kaleido_proto::projection::{
    ProjectionEnvelope, ProjectionKey, ProjectionSubscribe, ProjectionSubscribeOutcome,
};
use kaleido_proto::queue::QueueEntry;
use kaleido_state::{
    CanonicalStore, ClockSource, ContentStore, DeviceCommandAdmission, DispatchClaim,
    DispatchTicket, PendingDispatch, ProjectionReplay, QueueDeliveryClaim, StateError,
};

const DEFAULT_SUBSCRIPTION_CAPACITY: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionEvent {
    Projection(ProjectionEnvelope),
    Closed(CanonicalError),
}

#[derive(Debug)]
struct MailboxState {
    events: VecDeque<SubscriptionEvent>,
    capacity: usize,
    closed: bool,
}

#[derive(Debug)]
struct SubscriptionMailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

impl SubscriptionMailbox {
    fn new(capacity: usize) -> Result<Self, BrokerError> {
        if capacity == 0 {
            return Err(BrokerError::InvalidSubscriptionCapacity);
        }
        Ok(Self {
            state: Mutex::new(MailboxState {
                events: VecDeque::with_capacity(capacity),
                capacity,
                closed: false,
            }),
            ready: Condvar::new(),
        })
    }

    fn push(&self, envelope: ProjectionEnvelope, at_ms: i64) -> bool {
        let mut state = lock(&self.state);
        if state.closed {
            return false;
        }
        if state.events.len() >= state.capacity {
            state.events.clear();
            state
                .events
                .push_back(SubscriptionEvent::Closed(CanonicalError {
                    code: ErrorCode::CursorGap,
                    retriable: true,
                    detail_ref: None,
                    at_ms,
                }));
            state.closed = true;
            self.ready.notify_all();
            return false;
        }
        state
            .events
            .push_back(SubscriptionEvent::Projection(envelope));
        self.ready.notify_one();
        true
    }

    fn recv_timeout(&self, timeout: Duration) -> Option<SubscriptionEvent> {
        let state = lock(&self.state);
        let mut state = if state.events.is_empty() && !state.closed {
            match self.ready.wait_timeout(state, timeout) {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            }
        } else {
            state
        };
        state.events.pop_front()
    }
}

#[derive(Debug)]
struct Subscriber {
    key: ProjectionKey,
    mailbox: Arc<SubscriptionMailbox>,
}

#[derive(Debug)]
struct BrokerState {
    store: CanonicalStore,
    subscribers: BTreeMap<u64, Subscriber>,
    next_subscription_token: u64,
    host_id: HostId,
    authenticated_reachability: HostReachability,
}

#[derive(Debug, Clone)]
pub struct Broker {
    inner: Arc<Mutex<BrokerState>>,
    mint: IdentityMint,
}

#[derive(Debug)]
pub struct BrokerSubscription {
    token: u64,
    replay: ProjectionReplay,
    mailbox: Arc<SubscriptionMailbox>,
    broker: Arc<Mutex<BrokerState>>,
}

impl BrokerSubscription {
    pub fn replay(&self) -> &ProjectionReplay {
        &self.replay
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<SubscriptionEvent> {
        self.mailbox.recv_timeout(timeout)
    }

    pub fn unsubscribe(&self) {
        lock(&self.broker).subscribers.remove(&self.token);
    }
}

impl Drop for BrokerSubscription {
    fn drop(&mut self) {
        lock(&self.broker).subscribers.remove(&self.token);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("subscription capacity must be positive")]
    InvalidSubscriptionCapacity,
    #[error("the broker exhausted its subscription identifier space")]
    SubscriptionIdExhausted,
    #[error("the host clock cannot represent the requested command lifetime")]
    CommandExpiryOverflow,
    #[error("a provider effect referenced a different host identity")]
    HostIdentityMismatch,
}

impl Broker {
    pub fn open(
        root: impl AsRef<std::path::Path>,
        clock: ClockSource,
        identity_salt: impl Into<String>,
        host_display_name: &str,
    ) -> Result<Self, BrokerError> {
        let mint = IdentityMint::new(identity_salt);
        let host_id = mint.host_id(host_display_name);
        Ok(Self {
            inner: Arc::new(Mutex::new(BrokerState {
                store: CanonicalStore::open(root, clock)?,
                subscribers: BTreeMap::new(),
                next_subscription_token: 1,
                host_id,
                authenticated_reachability: HostReachability::Offline,
            })),
            mint,
        })
    }

    pub fn load(
        root: impl AsRef<std::path::Path>,
        clock: ClockSource,
        identity_salt: impl Into<String>,
        host_display_name: &str,
    ) -> Result<Self, BrokerError> {
        let mint = IdentityMint::new(identity_salt);
        let host_id = mint.host_id(host_display_name);
        let store = CanonicalStore::load(root, clock)?;
        if store.state().hosts().any(|host| host.id != host_id) {
            return Err(BrokerError::HostIdentityMismatch);
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(BrokerState {
                store,
                subscribers: BTreeMap::new(),
                next_subscription_token: 1,
                host_id,
                authenticated_reachability: HostReachability::Offline,
            })),
            mint,
        })
    }

    pub fn apply_effect(
        &self,
        effect: &StateEffect,
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, BrokerError> {
        let mut state = lock(&self.inner);
        let owned;
        let effect = match effect {
            StateEffect::HostUpserted { host } => {
                if host.id != state.host_id {
                    return Err(BrokerError::HostIdentityMismatch);
                }
                let mut host = host.clone();
                host.reachability = state.authenticated_reachability.clone();
                owned = StateEffect::HostUpserted { host };
                &owned
            }
            other => other,
        };
        let commit = state.store.apply_commit(effect)?;
        publish(&mut state, &commit.projections, at_ms);
        Ok(commit.projections)
    }

    pub fn apply_effects(
        &self,
        effects: &[StateEffect],
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, BrokerError> {
        let mut published = Vec::new();
        for effect in effects {
            published.extend(self.apply_effect(effect, at_ms)?);
        }
        Ok(published)
    }

    pub fn subscribe(
        &self,
        request: &ProjectionSubscribe,
        at_ms: i64,
    ) -> Result<BrokerSubscription, BrokerError> {
        self.subscribe_with_capacity(request, at_ms, DEFAULT_SUBSCRIPTION_CAPACITY)
    }

    pub fn subscribe_with_capacity(
        &self,
        request: &ProjectionSubscribe,
        at_ms: i64,
        capacity: usize,
    ) -> Result<BrokerSubscription, BrokerError> {
        let mailbox = Arc::new(SubscriptionMailbox::new(capacity)?);
        let mut state = lock(&self.inner);
        let token = state.next_subscription_token;
        state.next_subscription_token = token
            .checked_add(1)
            .ok_or(BrokerError::SubscriptionIdExhausted)?;

        // Register before reading replay state. The same lock orders canonical
        // writes and fanout, so nothing can be appended between the captured
        // head and this subscriber becoming live.
        state.subscribers.insert(
            token,
            Subscriber {
                key: request.key.clone(),
                mailbox: Arc::clone(&mailbox),
            },
        );
        let replay = state.store.projection_replay(request, at_ms)?;
        if matches!(
            replay.ack.outcome,
            ProjectionSubscribeOutcome::Rejected { .. }
        ) {
            state.subscribers.remove(&token);
        }
        drop(state);
        Ok(BrokerSubscription {
            token,
            replay,
            mailbox,
            broker: Arc::clone(&self.inner),
        })
    }

    pub fn admit_device_command(
        &self,
        device_id: &DeviceId,
        request: &DeviceCommandRequest,
        now_ms: i64,
    ) -> Result<DeviceCommandAdmission, BrokerError> {
        let envelope = self.device_envelope(device_id, request, now_ms)?;
        let mut state = lock(&self.inner);
        let admission = state
            .store
            .admit_device_command(device_id, &envelope, request, now_ms)?;
        publish(&mut state, &admission.projections, now_ms);
        Ok(admission)
    }

    pub fn pending_dispatches(&self) -> Vec<PendingDispatch> {
        lock(&self.inner).store.pending_dispatches()
    }

    pub fn pending_queue_deliveries(&self) -> Vec<(QueueEntry, CommandId)> {
        lock(&self.inner).store.pending_queue_deliveries()
    }

    pub fn claim_queue_delivery(
        &self,
        entry_id: &QueueEntryId,
        at_ms: i64,
    ) -> Result<QueueDeliveryClaim, BrokerError> {
        let mut state = lock(&self.inner);
        let claim = state.store.claim_queue_delivery(entry_id, at_ms)?;
        publish(&mut state, &claim.projections, at_ms);
        Ok(claim)
    }

    pub fn finish_queue_delivery(
        &self,
        entry_id: &QueueEntryId,
        effects: &[StateEffect],
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, BrokerError> {
        let mut state = lock(&self.inner);
        let projections = state.store.finish_queue_delivery(entry_id, effects)?;
        publish(&mut state, &projections, at_ms);
        Ok(projections)
    }

    pub fn claim_dispatch(
        &self,
        ticket: &DispatchTicket,
        at_ms: i64,
    ) -> Result<DispatchClaim, BrokerError> {
        let mut state = lock(&self.inner);
        let claim = state.store.claim_dispatch(ticket)?;
        publish(&mut state, &claim.projections, at_ms);
        Ok(claim)
    }

    pub fn reject_ready_dispatch(
        &self,
        ticket: &DispatchTicket,
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, BrokerError> {
        let mut state = lock(&self.inner);
        let projections = state.store.reject_ready_dispatch(ticket, at_ms)?;
        publish(&mut state, &projections, at_ms);
        Ok(projections)
    }

    pub fn finish_dispatch(
        &self,
        ticket: &DispatchTicket,
        effects: &[StateEffect],
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, BrokerError> {
        let mut state = lock(&self.inner);
        let projections = state.store.finish_dispatch(ticket, effects)?;
        publish(&mut state, &projections, at_ms);
        Ok(projections)
    }

    /// Latches authenticated-path readiness and publishes the matching
    /// canonical host reachability if the provider has bootstrapped the host.
    /// Merely binding or accepting a socket must never call this with `true`.
    pub fn set_lan_ready(
        &self,
        ready: bool,
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, BrokerError> {
        self.set_authenticated_reachability(
            if ready {
                HostReachability::LanDirect
            } else {
                HostReachability::Offline
            },
            at_ms,
        )
    }

    /// Publishes the path selected by a fully device-authenticated connection.
    /// Callers must publish `Offline` after the last authenticated path closes;
    /// listener, QUIC and inner-TLS handshakes are not sufficient.
    pub fn set_authenticated_reachability(
        &self,
        reachability: HostReachability,
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, BrokerError> {
        let mut state = lock(&self.inner);
        if state.authenticated_reachability == reachability {
            return Ok(Vec::new());
        }
        state.authenticated_reachability = reachability.clone();
        let host = state
            .store
            .state()
            .hosts()
            .find(|host| host.id == state.host_id)
            .cloned();
        let Some(mut host) = host else {
            return Ok(Vec::new());
        };
        host.reachability = reachability;
        host.last_seen_at_ms = at_ms;
        let commit = state
            .store
            .apply_commit(&StateEffect::HostUpserted { host })?;
        publish(&mut state, &commit.projections, at_ms);
        Ok(commit.projections)
    }

    pub fn host_id(&self) -> HostId {
        lock(&self.inner).host_id.clone()
    }

    /// Returns a cloneable adapter-facing handle to the same content-addressed
    /// store. The handle contains no body and its `Debug` implementation
    /// redacts the backing path.
    pub fn content_store(&self) -> ContentStore {
        lock(&self.inner).store.content().clone()
    }

    pub fn write_content(
        &self,
        device_id: &DeviceId,
        request: &ContentWriteRequest,
        bytes: &[u8],
        now_ms: i64,
    ) -> Result<ContentWriteResponse, BrokerError> {
        Ok(lock(&self.inner)
            .store
            .write_content_for_device(device_id, request, bytes, now_ms)?)
    }

    pub fn read_content(
        &self,
        device_id: &DeviceId,
        request: &ContentReadRequest,
        now_ms: i64,
    ) -> Result<ContentReadResponse, BrokerError> {
        Ok(lock(&self.inner)
            .store
            .read_content_for_device(device_id, request, now_ms)?)
    }

    fn device_envelope(
        &self,
        device_id: &DeviceId,
        request: &DeviceCommandRequest,
        now_ms: i64,
    ) -> Result<CommandEnvelope, BrokerError> {
        let seed = format!(
            "{}:{}|{}:{}",
            device_id.as_str().len(),
            device_id,
            request.idempotency_key.len(),
            request.idempotency_key
        );
        let expires_at_ms = request
            .ttl_ms
            .map(|ttl| {
                let ttl = i64::try_from(ttl).map_err(|_| BrokerError::CommandExpiryOverflow)?;
                now_ms
                    .checked_add(ttl)
                    .ok_or(BrokerError::CommandExpiryOverflow)
            })
            .transpose()?;
        Ok(CommandEnvelope {
            command_id: self.mint.command_id(&seed),
            idempotency_key: request.idempotency_key.clone(),
            actor: Actor::Human {
                device_id: device_id.clone(),
            },
            issued_at_ms: now_ms,
            expires_at_ms,
            body: request.body.clone(),
        })
    }
}

fn publish(state: &mut BrokerState, envelopes: &[ProjectionEnvelope], at_ms: i64) {
    let mut closed = Vec::new();
    for envelope in envelopes {
        for (token, subscriber) in &state.subscribers {
            if subscriber.key == envelope.key && !subscriber.mailbox.push(envelope.clone(), at_ms) {
                closed.push(*token);
            }
        }
    }
    closed.sort_unstable();
    closed.dedup();
    for token in closed {
        state.subscribers.remove(&token);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
