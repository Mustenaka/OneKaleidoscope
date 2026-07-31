//! In-memory canonical state and the rules for changing it.
//!
//! Two responsibilities live here that a provider adapter deliberately does not
//! own:
//!
//! * **Accumulating `Turn.item_ids`.** Section 4.4 forbids rebuilding a turn's
//!   item list from the summary a provider attaches to its completion message,
//!   because that summary has been observed to contain only the final message
//!   of a six-item turn. The list therefore grows here, one observed item at a
//!   time, and a later turn upsert can only extend it.
//! * **Deriving session status, queue depth and open-attention counts.** These
//!   are functions of the four independent state families (ADR-0010 D-3), so
//!   letting an effect carry them would allow two producers to disagree.

use std::collections::BTreeMap;

use kaleido_proto::attention::{AttentionItem, AttentionState, AttentionSubject};
use kaleido_proto::capability::RuntimeCapabilities;
use kaleido_proto::command::CommandAck;
use kaleido_proto::effect::{DiagnosticRecord, SessionSnapshot, StateEffect};
use kaleido_proto::host::{Host, Project, ProviderRuntime, SessionCounts};
use kaleido_proto::ids::{
    AttentionId, HostId, ItemId, ProjectId, ProviderRuntimeId, QueueEntryId, SessionId, TurnId,
};
use kaleido_proto::queue::QueueEntry;
use kaleido_proto::session::{derive_session_status, Session, SessionStatus, StatusInputs};
use kaleido_proto::turn::{Item, Turn};

use crate::error::StateError;

/// Everything the broker knows, rebuilt purely from applied effects.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CanonicalState {
    hosts: BTreeMap<HostId, Host>,
    runtimes: BTreeMap<ProviderRuntimeId, ProviderRuntime>,
    projects: BTreeMap<ProjectId, Project>,
    sessions: BTreeMap<SessionId, Session>,
    /// What the runtime last said about a session, kept apart from the derived
    /// status so the two can never be silently conflated.
    reported_status: BTreeMap<SessionId, SessionStatus>,
    turns: BTreeMap<TurnId, Turn>,
    turn_order: Vec<TurnId>,
    items: BTreeMap<ItemId, Item>,
    queue: BTreeMap<QueueEntryId, QueueEntry>,
    attention: BTreeMap<AttentionId, AttentionItem>,
    attention_order: Vec<AttentionId>,
    diagnostics: BTreeMap<String, DiagnosticRecord>,
    acks: Vec<CommandAck>,
}

impl CanonicalState {
    pub fn hosts(&self) -> impl Iterator<Item = &Host> {
        self.hosts.values()
    }

    pub fn runtimes(&self) -> impl Iterator<Item = &ProviderRuntime> {
        self.runtimes.values()
    }

    pub fn runtime(&self, runtime_id: &ProviderRuntimeId) -> Option<&ProviderRuntime> {
        self.runtimes.get(runtime_id)
    }

    pub fn projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.values()
    }

    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    pub fn session(&self, session_id: &SessionId) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    pub fn attention_entries(&self) -> Vec<&AttentionItem> {
        self.attention_order
            .iter()
            .filter_map(|id| self.attention.get(id))
            .collect()
    }

    pub fn attention(&self, attention_id: &AttentionId) -> Option<&AttentionItem> {
        self.attention.get(attention_id)
    }

    pub fn diagnostics(&self) -> impl Iterator<Item = &DiagnosticRecord> {
        self.diagnostics.values()
    }

    pub fn acknowledgements(&self) -> &[CommandAck] {
        &self.acks
    }

    pub fn item(&self, item_id: &ItemId) -> Option<&Item> {
        self.items.get(item_id)
    }

    /// Turns of one session, in the order they were first observed.
    pub fn turns_of(&self, session_id: &SessionId) -> Vec<&Turn> {
        self.turn_order
            .iter()
            .filter_map(|id| self.turns.get(id))
            .filter(|turn| &turn.session_id == session_id)
            .collect()
    }

    /// Items of one session, ordered by their session-scoped sequence.
    pub fn items_of(&self, session_id: &SessionId) -> Vec<&Item> {
        let mut items = self
            .items
            .values()
            .filter(|item| &item.session_id == session_id)
            .collect::<Vec<_>>();
        items.sort_by_key(|item| item.sequence);
        items
    }

    /// Queue entries of one session, ordered by position.
    pub fn queue_of(&self, session_id: &SessionId) -> Vec<&QueueEntry> {
        let mut entries = self
            .queue
            .values()
            .filter(|entry| &entry.session_id == session_id)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.position);
        entries
    }

    /// Attention entries of one session, in observation order.
    pub fn attention_of(&self, session_id: &SessionId) -> Vec<&AttentionItem> {
        self.attention_order
            .iter()
            .filter_map(|id| self.attention.get(id))
            .filter(|entry| entry.session_id.as_ref() == Some(session_id))
            .collect()
    }

    /// The negotiated capabilities of whichever runtime backs this session.
    pub fn capabilities_of(&self, session: &Session) -> Option<&RuntimeCapabilities> {
        self.runtime_of(session)
            .map(|runtime| &runtime.capabilities)
    }

    pub fn runtime_of(&self, session: &Session) -> Option<&ProviderRuntime> {
        let runtime_id = session
            .binding_handle
            .as_ref()
            .map(|handle| &handle.runtime_id)
            .or(session.history_source.runtime_id.as_ref())?;
        self.runtimes.get(runtime_id)
    }

    /// Builds and validates the session snapshot.
    ///
    /// The validation is the point: it re-checks every reference the snapshot
    /// claims to resolve, so a reducer bug becomes a refusal here rather than a
    /// broken read model on a phone.
    pub fn session_snapshot(&self, session_id: &SessionId) -> Result<SessionSnapshot, StateError> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| StateError::UnknownSession {
                session_id: session_id.clone(),
            })?;
        let capabilities =
            self.capabilities_of(session)
                .cloned()
                .ok_or_else(|| StateError::UnknownRuntime {
                    runtime_id: session
                        .binding_handle
                        .as_ref()
                        .map(|handle| handle.runtime_id.clone())
                        .unwrap_or_else(|| ProviderRuntimeId::new("unbound")),
                })?;
        let snapshot = SessionSnapshot {
            session: session.clone(),
            turns: self.turns_of(session_id).into_iter().cloned().collect(),
            items: self.items_of(session_id).into_iter().cloned().collect(),
            queue: self.queue_of(session_id).into_iter().cloned().collect(),
            attention: self.attention_of(session_id).into_iter().cloned().collect(),
            capabilities,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Applies one effect and recomputes every derived field.
    pub fn apply(&mut self, effect: &StateEffect) -> Result<(), StateError> {
        match effect {
            StateEffect::HostUpserted { host } => {
                self.hosts.insert(host.id.clone(), host.clone());
            }
            StateEffect::RuntimeUpserted { runtime } => {
                self.runtimes.insert(runtime.id.clone(), runtime.clone());
            }
            StateEffect::CapabilitiesUpdated { capabilities } => {
                let runtime = self
                    .runtimes
                    .get_mut(&capabilities.runtime_id)
                    .ok_or_else(|| StateError::UnknownRuntime {
                        runtime_id: capabilities.runtime_id.clone(),
                    })?;
                runtime.capabilities = capabilities.clone();
            }
            StateEffect::ProjectUpserted { project } => {
                self.projects.insert(project.id.clone(), project.clone());
            }
            StateEffect::SessionUpserted { session } => {
                // Rule R-P7 in the write path: a live binding is only accepted
                // when the negotiated capabilities actually support it.
                if let Some(capabilities) = self.capabilities_of(session) {
                    session.live_binding.validate_against(capabilities)?;
                }
                let mut merged = session.clone();
                if let Some(existing) = self.sessions.get(&session.id) {
                    merged.created_at_ms = existing.created_at_ms;
                }
                self.sessions.insert(merged.id.clone(), merged);
            }
            StateEffect::SessionStatusChanged { session_id, status } => {
                if !self.sessions.contains_key(session_id) {
                    return Err(StateError::UnknownSession {
                        session_id: session_id.clone(),
                    });
                }
                self.reported_status.insert(session_id.clone(), *status);
            }
            StateEffect::TurnUpserted { turn } => {
                let mut merged = turn.clone();
                if let Some(existing) = self.turns.get(&turn.id) {
                    // Section 4.4: a completion payload may not shrink or
                    // replace the accumulated list, it may only extend it.
                    let mut item_ids = existing.item_ids.clone();
                    for item_id in &turn.item_ids {
                        if !item_ids.contains(item_id) {
                            item_ids.push(item_id.clone());
                        }
                    }
                    merged.item_ids = item_ids;
                    if merged.started_at_ms.is_none() {
                        merged.started_at_ms = existing.started_at_ms;
                    }
                }
                merged.validate()?;
                if !self.turn_order.contains(&merged.id) {
                    self.turn_order.push(merged.id.clone());
                }
                self.turns.insert(merged.id.clone(), merged);
            }
            StateEffect::ItemUpserted { item } => {
                let turn =
                    self.turns
                        .get_mut(&item.turn_id)
                        .ok_or_else(|| StateError::UnknownTurn {
                            turn_id: item.turn_id.clone(),
                        })?;
                if !turn.item_ids.contains(&item.id) {
                    turn.item_ids.push(item.id.clone());
                }
                turn.validate()?;
                self.items.insert(item.id.clone(), item.clone());
            }
            StateEffect::QueueEntryUpserted { entry } => {
                self.queue.insert(entry.id.clone(), entry.clone());
            }
            StateEffect::QueueReordered { session_id, order } => {
                for (position, entry_id) in (0_u32..).zip(order) {
                    if let Some(entry) = self.queue.get_mut(entry_id) {
                        if &entry.session_id == session_id {
                            entry.position = position;
                        }
                    }
                }
            }
            StateEffect::AttentionUpserted { item } => {
                if !self.attention_order.contains(&item.id) {
                    self.attention_order.push(item.id.clone());
                }
                self.attention.insert(item.id.clone(), item.clone());
            }
            StateEffect::CommandAcknowledged { ack } => {
                self.acks.push(ack.clone());
            }
            StateEffect::DiagnosticRecorded { diagnostic } => {
                self.diagnostics
                    .insert(diagnostic_key(diagnostic), diagnostic.clone());
            }
            StateEffect::WorkflowUpserted { .. }
            | StateEffect::StepUpserted { .. }
            | StateEffect::ArtifactUpserted { .. } => {
                // Workflow state is defined by the contract but is out of scope
                // for this slice; recording it here without the board read
                // model would be state nothing can observe.
                return Err(StateError::UnsupportedEffect {
                    detail: "workflow effects are not part of this vertical slice",
                });
            }
        }
        self.recompute_derived();
        Ok(())
    }

    /// Recomputes every field the store owns rather than an effect.
    fn recompute_derived(&mut self) {
        let session_ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            let active_turn_id = self
                .turn_order
                .iter()
                .filter_map(|id| self.turns.get(id))
                .filter(|turn| turn.session_id == session_id)
                .filter(|turn| !turn.status.is_terminal())
                .map(|turn| turn.id.clone())
                .next_back();
            let open_approval = self.attention.values().any(|entry| {
                entry.session_id.as_ref() == Some(&session_id)
                    && entry.state.is_open()
                    && matches!(entry.subject, AttentionSubject::Approval { .. })
            });
            let open_question = self.attention.values().any(|entry| {
                entry.session_id.as_ref() == Some(&session_id)
                    && entry.state.is_open()
                    && matches!(entry.subject, AttentionSubject::Question { .. })
            });
            let open_attention_count = u32::try_from(
                self.attention
                    .values()
                    .filter(|entry| {
                        entry.session_id.as_ref() == Some(&session_id) && entry.state.is_open()
                    })
                    .count(),
            )
            .unwrap_or(u32::MAX);
            let pending_queue_entries = u32::try_from(
                self.queue
                    .values()
                    .filter(|entry| entry.session_id == session_id && entry.state.is_pending())
                    .count(),
            )
            .unwrap_or(u32::MAX);
            let reported = self.reported_status.get(&session_id).copied();
            let last_activity_at_ms = self.last_activity_of(&session_id);
            let runtime_usable = self
                .sessions
                .get(&session_id)
                .and_then(|session| self.runtime_of(session))
                .is_some_and(|runtime| runtime.connection.is_usable());

            let Some(session) = self.sessions.get_mut(&session_id) else {
                continue;
            };
            session.active_turn_id = active_turn_id;
            session.queue_depth = pending_queue_entries;
            session.open_attention_count = open_attention_count;
            session.status = derive_session_status(StatusInputs {
                runtime_usable,
                // A runtime that reports itself active before it announces the
                // turn object is still active; believing the turn object alone
                // would show a momentary, wrong "idle".
                has_active_turn: session.active_turn_id.is_some()
                    || reported == Some(SessionStatus::Running),
                open_approval,
                open_question,
                pending_queue_entries,
                terminal: reported.filter(|status| {
                    matches!(
                        status,
                        SessionStatus::Failed | SessionStatus::Completed | SessionStatus::Cancelled
                    )
                }),
            });
            if last_activity_at_ms > session.last_activity_at_ms {
                session.last_activity_at_ms = last_activity_at_ms;
                session.updated_at_ms = last_activity_at_ms;
            }
        }
        self.recompute_project_counts();
    }

    fn last_activity_of(&self, session_id: &SessionId) -> i64 {
        let mut latest = i64::MIN;
        for item in self.items.values() {
            if &item.session_id == session_id {
                latest = latest.max(item.updated_at_ms);
            }
        }
        for turn in self.turns.values() {
            if &turn.session_id == session_id {
                latest = latest.max(turn.completed_at_ms.unwrap_or(i64::MIN));
                latest = latest.max(turn.started_at_ms.unwrap_or(i64::MIN));
            }
        }
        for entry in self.queue.values() {
            if &entry.session_id == session_id {
                latest = latest.max(entry.updated_at_ms);
            }
        }
        for entry in self.attention.values() {
            if entry.session_id.as_ref() == Some(session_id) {
                latest = latest.max(entry.created_at_ms);
            }
        }
        latest
    }

    fn recompute_project_counts(&mut self) {
        let project_ids = self.projects.keys().cloned().collect::<Vec<_>>();
        for project_id in project_ids {
            let mut counts = SessionCounts::default();
            let mut last_activity_at_ms = i64::MIN;
            for session in self.sessions.values() {
                if session.project_id != project_id {
                    continue;
                }
                counts.total = counts.total.saturating_add(1);
                if session.archived {
                    counts.archived = counts.archived.saturating_add(1);
                }
                match session.status {
                    SessionStatus::Running => counts.running = counts.running.saturating_add(1),
                    SessionStatus::Failed => counts.failed = counts.failed.saturating_add(1),
                    status if status.waits_for_human() => {
                        counts.waiting_human = counts.waiting_human.saturating_add(1);
                    }
                    _ => {}
                }
                last_activity_at_ms = last_activity_at_ms.max(session.last_activity_at_ms);
            }
            let attention_count = u32::try_from(
                self.attention
                    .values()
                    .filter(|entry| entry.project_id == project_id && entry.state.is_open())
                    .count(),
            )
            .unwrap_or(u32::MAX);
            let Some(project) = self.projects.get_mut(&project_id) else {
                continue;
            };
            project.session_counts = counts;
            project.attention_count = attention_count;
            if last_activity_at_ms > project.last_activity_at_ms {
                project.last_activity_at_ms = last_activity_at_ms;
            }
        }
    }

    /// Whether this attention entry has already been decided.
    pub fn attention_is_answered(&self, attention_id: &AttentionId) -> bool {
        self.attention
            .get(attention_id)
            .is_some_and(|entry| matches!(entry.state, AttentionState::Answered { .. }))
    }
}

/// Diagnostics aggregate per code and scope, so the key must include both.
fn diagnostic_key(diagnostic: &DiagnosticRecord) -> String {
    let runtime = diagnostic
        .runtime_id
        .as_ref()
        .map(ProviderRuntimeId::as_str)
        .unwrap_or("-");
    let session = diagnostic
        .session_id
        .as_ref()
        .map(SessionId::as_str)
        .unwrap_or("-");
    format!("{:?}|{runtime}|{session}", diagnostic.code)
}
