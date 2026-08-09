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
use kaleido_proto::capability::{Capability, RuntimeCapabilities};
use kaleido_proto::command::{CommandAck, CommandOutcome};
use kaleido_proto::content::ContentRef;
use kaleido_proto::effect::{DiagnosticRecord, SessionSnapshot, StateEffect};
use kaleido_proto::host::{Host, Project, ProviderRuntime, SessionCounts};
use kaleido_proto::ids::{
    ArtifactId, AttentionId, CommandId, ContentId, HostId, ItemId, ProjectId, ProviderRuntimeId,
    QueueEntryId, SessionId, StepId, TurnId, WorkflowId,
};
use kaleido_proto::queue::QueueEntry;
use kaleido_proto::session::{derive_session_status, Session, SessionStatus, StatusInputs};
use kaleido_proto::turn::{FileChangeKind, Item, ItemBody, Turn, TurnOrigin};
use kaleido_proto::workflow::{Artifact, Step, Workflow};

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
    workflows: BTreeMap<WorkflowId, Workflow>,
    steps: BTreeMap<StepId, Step>,
    artifacts: BTreeMap<ArtifactId, Artifact>,
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

    pub fn project(&self, project_id: &ProjectId) -> Option<&Project> {
        self.projects.get(project_id)
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

    pub fn workflows(&self) -> impl Iterator<Item = &Workflow> {
        self.workflows.values()
    }

    pub fn workflow(&self, workflow_id: &WorkflowId) -> Option<&Workflow> {
        self.workflows.get(workflow_id)
    }

    pub fn step(&self, step_id: &StepId) -> Option<&Step> {
        self.steps.get(step_id)
    }

    pub fn steps_of(&self, workflow_id: &WorkflowId) -> Vec<&Step> {
        self.steps
            .values()
            .filter(|step| &step.workflow_id == workflow_id)
            .collect()
    }

    pub fn artifacts_of(&self, workflow_id: &WorkflowId) -> Vec<&Artifact> {
        self.artifacts
            .values()
            .filter(|artifact| &artifact.workflow_id == workflow_id)
            .collect()
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

    pub fn queue_entry(&self, entry_id: &QueueEntryId) -> Option<&QueueEntry> {
        self.queue.get(entry_id)
    }

    /// Resolves only references that are actually reachable from canonical
    /// state. Mobile content reads use this instead of trusting a caller-built
    /// `ContentRef`.
    pub fn content_ref(&self, content_id: &ContentId) -> Option<&ContentRef> {
        self.content_refs()
            .into_iter()
            .find(|reference| &reference.content_id == content_id)
    }

    /// Every content reference reachable from canonical state. Cleanup uses
    /// this complete traversal as its deletion protection set.
    pub fn content_refs(&self) -> Vec<&ContentRef> {
        let mut references = Vec::new();
        for runtime in self.runtimes.values() {
            references.extend(
                runtime
                    .capabilities
                    .entries
                    .iter()
                    .filter_map(|entry| entry.evidence.note_ref.as_ref()),
            );
        }
        for project in self.projects.values() {
            references.extend(project.bindings.iter().map(|binding| &binding.root_ref));
        }
        for session in self.sessions.values() {
            references.extend(session.history_source.evidence.note_ref.iter());
            let live_note = match &session.live_binding {
                kaleido_proto::session::LiveBinding::Observing { evidence, .. }
                | kaleido_proto::session::LiveBinding::Controlling { evidence, .. } => {
                    evidence.note_ref.as_ref()
                }
                kaleido_proto::session::LiveBinding::NotBound { .. }
                | kaleido_proto::session::LiveBinding::Blocked { .. } => None,
            };
            references.extend(live_note);
        }
        for turn in self.turns.values() {
            references.extend(
                turn.error
                    .iter()
                    .filter_map(|error| error.detail_ref.as_ref()),
            );
        }
        for item in self.items.values() {
            references.extend(item_content_refs(item));
        }
        for entry in self.queue.values() {
            references.push(&entry.body);
        }
        for item in self.attention.values() {
            references.extend(attention_content_refs(item));
        }
        for step in self.steps.values() {
            references.push(&step.assignment.worktree_ref);
            references.extend(
                step.audit
                    .iter()
                    .filter_map(|transition| transition.reason_ref.as_ref()),
            );
        }
        for artifact in self.artifacts.values() {
            references.push(&artifact.content);
        }
        references.extend(
            self.diagnostics
                .values()
                .filter_map(|diagnostic| diagnostic.detail_ref.as_ref()),
        );
        references.extend(self.acks.iter().filter_map(|ack| match &ack.outcome {
            CommandOutcome::AcceptedLocally { note_ref } => note_ref.as_ref(),
            CommandOutcome::Rejected { error } => error.detail_ref.as_ref(),
            CommandOutcome::AcceptedByRuntime { .. }
            | CommandOutcome::Enqueued { .. }
            | CommandOutcome::Duplicate { .. } => None,
        }));
        references
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
                if self
                    .runtimes
                    .get(&runtime.id)
                    .is_some_and(|existing| existing.host_id != runtime.host_id)
                {
                    return Err(StateError::RuntimeHostChanged);
                }
                if runtime.capabilities.permits(&Capability::LiveControl)
                    && !self.has_runtime_acceptance_for(&runtime.id)
                {
                    return Err(StateError::LiveControlCapabilityWithoutRuntimeAcceptance);
                }
                self.runtimes.insert(runtime.id.clone(), runtime.clone());
            }
            StateEffect::CapabilitiesUpdated { capabilities } => {
                if capabilities.permits(&Capability::LiveControl)
                    && !self.has_runtime_acceptance_for(&capabilities.runtime_id)
                {
                    return Err(StateError::LiveControlCapabilityWithoutRuntimeAcceptance);
                }
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
                // when the candidate session itself resolves to a runtime whose
                // negotiated capabilities actually support it. Looking up the
                // previous stored session here would let an update remove or
                // rewrite its runtime binding while borrowing stale evidence.
                if let kaleido_proto::session::LiveBinding::Observing { .. }
                | kaleido_proto::session::LiveBinding::Controlling { .. } = &session.live_binding
                {
                    let candidate_runtime = self.live_runtime_of(session)?;
                    session
                        .live_binding
                        .validate_against(&candidate_runtime.capabilities)?;
                }
                if matches!(
                    session.live_binding,
                    kaleido_proto::session::LiveBinding::Controlling { .. }
                ) {
                    let candidate_runtime = self.live_runtime_of(session)?;
                    let has_runtime_acceptance = self.acks.iter().any(|ack| {
                        self.accepted_runtime_turn(ack, &candidate_runtime.id)
                            .is_some_and(|turn| turn.session_id == session.id)
                    });
                    if !has_runtime_acceptance {
                        return Err(StateError::ControllingBindingWithoutRuntimeAcceptance);
                    }
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
                if let TurnOrigin::RemoteCommand { command_id } = &turn.origin {
                    let command_already_bound = self.turns.values().any(|existing| {
                        existing.id != turn.id
                            && matches!(
                                &existing.origin,
                                TurnOrigin::RemoteCommand {
                                    command_id: existing_command_id
                                } if existing_command_id == command_id
                            )
                    });
                    if command_already_bound {
                        return Err(StateError::RemoteCommandTurnConflict);
                    }
                }
                let mut merged = turn.clone();
                if let Some(existing) = self.turns.get(&turn.id) {
                    if existing.session_id != turn.session_id {
                        return Err(StateError::TurnSessionChanged);
                    }
                    if existing.origin != turn.origin {
                        return Err(StateError::TurnOriginChanged);
                    }
                    if existing.binding_handle.is_some()
                        && turn.binding_handle.is_some()
                        && existing.binding_handle != turn.binding_handle
                    {
                        return Err(StateError::TurnBindingChanged);
                    }
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
                    if merged.binding_handle.is_none() {
                        merged.binding_handle = existing.binding_handle.clone();
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
                if let CommandOutcome::AcceptedByRuntime { binding_handle } = &ack.outcome {
                    let accepted_locally = self.acks.iter().any(|existing| {
                        existing.command_id == ack.command_id
                            && matches!(existing.outcome, CommandOutcome::AcceptedLocally { .. })
                    });
                    if !accepted_locally {
                        return Err(StateError::UncorrelatedRuntimeAcknowledgement);
                    }
                    let already_accepted_by_runtime = self.acks.iter().any(|existing| {
                        existing.command_id == ack.command_id
                            && matches!(existing.outcome, CommandOutcome::AcceptedByRuntime { .. })
                    });
                    if already_accepted_by_runtime {
                        return Err(StateError::DuplicateRuntimeAcknowledgement);
                    }
                    let turn = self.remote_command_turn(&ack.command_id)?;
                    let session = self.sessions.get(&turn.session_id).ok_or_else(|| {
                        StateError::UnknownSession {
                            session_id: turn.session_id.clone(),
                        }
                    })?;
                    let runtime_matches = self
                        .runtime_of(session)
                        .is_some_and(|runtime| runtime.id == binding_handle.runtime_id);
                    if !runtime_matches {
                        return Err(StateError::RuntimeAcknowledgementRuntimeMismatch);
                    }
                }
                self.acks.push(ack.clone());
            }
            StateEffect::DiagnosticRecorded { diagnostic } => {
                self.diagnostics
                    .insert(diagnostic_key(diagnostic), diagnostic.clone());
            }
            StateEffect::WorkflowUpserted { workflow } => {
                self.workflows.insert(workflow.id.clone(), workflow.clone());
            }
            StateEffect::StepUpserted { step } => {
                self.steps.insert(step.id.clone(), step.clone());
            }
            StateEffect::ArtifactUpserted { artifact } => {
                self.artifacts.insert(artifact.id.clone(), artifact.clone());
            }
        }
        self.recompute_derived();
        Ok(())
    }

    fn has_runtime_acceptance_for(&self, runtime_id: &ProviderRuntimeId) -> bool {
        self.acks
            .iter()
            .any(|ack| self.accepted_runtime_turn(ack, runtime_id).is_some())
    }

    fn live_runtime_of(&self, session: &Session) -> Result<&ProviderRuntime, StateError> {
        let runtime_id = session
            .binding_handle
            .as_ref()
            .map(|handle| &handle.runtime_id)
            .or(session.history_source.runtime_id.as_ref())
            .ok_or(StateError::LiveSessionWithoutRuntimeReference)?;
        self.runtimes
            .get(runtime_id)
            .ok_or_else(|| StateError::UnknownRuntime {
                runtime_id: runtime_id.clone(),
            })
    }

    fn accepted_runtime_turn<'a>(
        &'a self,
        ack: &CommandAck,
        runtime_id: &ProviderRuntimeId,
    ) -> Option<&'a Turn> {
        let CommandOutcome::AcceptedByRuntime { binding_handle } = &ack.outcome else {
            return None;
        };
        if binding_handle.runtime_id != *runtime_id {
            return None;
        }
        let turn = self.remote_command_turn(&ack.command_id).ok()?;
        let session = self.sessions.get(&turn.session_id)?;
        self.runtime_of(session)
            .is_some_and(|runtime| runtime.id == *runtime_id)
            .then_some(turn)
    }

    fn remote_command_turn(&self, command_id: &CommandId) -> Result<&Turn, StateError> {
        let mut matches = self.turns.values().filter(|turn| {
            matches!(
                &turn.origin,
                TurnOrigin::RemoteCommand {
                    command_id: turn_command_id
                } if turn_command_id == command_id
            )
        });
        let Some(turn) = matches.next() else {
            return Err(StateError::RuntimeAcknowledgementWithoutRemoteTurn);
        };
        if matches.next().is_some() {
            return Err(StateError::AmbiguousRuntimeAcknowledgement);
        }
        Ok(turn)
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
            let workflow_count = u32::try_from(
                self.workflows
                    .values()
                    .filter(|workflow| workflow.project_id == project_id)
                    .count(),
            )
            .unwrap_or(u32::MAX);
            let Some(project) = self.projects.get_mut(&project_id) else {
                continue;
            };
            project.session_counts = counts;
            project.attention_count = attention_count;
            project.workflow_count = workflow_count;
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

fn item_content_refs(item: &Item) -> Vec<&ContentRef> {
    match &item.body {
        ItemBody::UserMessage { content }
        | ItemBody::AgentMessage { content, .. }
        | ItemBody::Reasoning { content } => vec![content],
        ItemBody::ToolCall {
            arguments, output, ..
        } => arguments.iter().chain(output.iter()).collect(),
        ItemBody::FileEdit { change_set } => change_set
            .entries
            .iter()
            .flat_map(|entry| {
                let from = match &entry.kind {
                    FileChangeKind::Rename { from_ref } => Some(from_ref),
                    FileChangeKind::Add | FileChangeKind::Modify | FileChangeKind::Delete => None,
                };
                std::iter::once(&entry.path_ref)
                    .chain(entry.diff.iter())
                    .chain(from)
            })
            .collect(),
        ItemBody::PlanUpdate { entries } => entries.iter().map(|entry| &entry.title_ref).collect(),
        ItemBody::TaskUpdate { tasks } => tasks.iter().map(|task| &task.title_ref).collect(),
        ItemBody::Diagnostic { detail, .. } => vec![detail],
    }
}

fn attention_content_refs(item: &AttentionItem) -> Vec<&ContentRef> {
    let mut references = match &item.subject {
        AttentionSubject::Approval { request } => std::iter::once(&request.summary_ref)
            .chain(request.detail_ref.iter())
            .collect(),
        AttentionSubject::Question { request } => vec![&request.prompt_ref],
        AttentionSubject::WorkflowGate { request } => vec![&request.prompt_ref],
        AttentionSubject::ConnectionFault { .. } => Vec::new(),
    };
    if let AttentionState::Answered {
        free_form_ref: Some(reference),
        ..
    } = &item.state
    {
        references.push(reference);
    }
    references
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
