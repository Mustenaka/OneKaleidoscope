//! Subscriber read models. See `docs/PROTOCOL.md` section 8.
//!
//! A projection may select, sort, aggregate and truncate canonical state. It
//! must not introduce semantics that canonical state does not carry, and it
//! must keep unsupported, unverified and upstream-blocked states visible.

use serde::{Deserialize, Serialize};

use crate::attention::AttentionItem;
use crate::capability::{CapabilityEntry, RuntimeCapabilities};
use crate::effect::Cursor;
use crate::error::{CanonicalError, ErrorCode};
use crate::host::{HostReachability, ProviderFamily, SessionCounts};
use crate::ids::{
    HostId, ItemId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId, StepId, TurnId,
    WorkflowId,
};
use crate::queue::QueueEntry;
use crate::session::{LiveBinding, OwnershipMode, SessionStatus};
use crate::turn::{AgentTask, Item, PlanEntry, Turn};
use crate::workflow::{Artifact, StepAssignment, StepBlocker, StepState, WorkflowState};
use crate::ContractViolation;

/// Bumped whenever a payload shape changes so clients know to refresh fully.
pub const PROJECTION_VERSION: u32 = 2;

/// The sole cursor domain for one projection.
///
/// Unlike a canonical [`crate::effect::StreamKey`], the projection kind is
/// part of the key. Two read models over the same session therefore cannot
/// accidentally share a cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectionKey {
    ProjectIndex {
        host_id: HostId,
    },
    SessionIndex {
        project_id: ProjectId,
    },
    Transcript {
        session_id: SessionId,
    },
    LiveActivity {
        session_id: SessionId,
    },
    InputQueue {
        session_id: SessionId,
    },
    AttentionInbox {
        host_id: HostId,
    },
    WorkflowBoard {
        workflow_id: WorkflowId,
    },
    RuntimeCapability {
        host_id: HostId,
        runtime_id: ProviderRuntimeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectionEnvelope {
    pub projection_version: u32,
    pub key: ProjectionKey,
    pub cursor: Cursor,
    pub payload: ProjectionPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectionSubscribe {
    pub key: ProjectionKey,
    pub since: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectionSubscribeAck {
    pub key: ProjectionKey,
    pub outcome: ProjectionSubscribeOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectionSubscribeOutcome {
    Resumed { from_cursor: Cursor },
    CurrentFollows { current_cursor: Cursor },
    Rejected { error: CanonicalError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectionPayload {
    ProjectIndex { view: ProjectIndexView },
    SessionIndex { view: SessionIndexView },
    Transcript { view: TranscriptView },
    LiveActivity { view: LiveActivityView },
    InputQueue { view: InputQueueView },
    AttentionInbox { view: AttentionInboxView },
    WorkflowBoard { view: WorkflowBoardView },
    RuntimeCapability { view: RuntimeCapabilityView },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectIndexView {
    pub host_id: HostId,
    pub reachability: HostReachability,
    pub groups: Vec<ProviderGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProviderGroup {
    pub family: ProviderFamily,
    pub runtime_ids: Vec<ProviderRuntimeId>,
    pub projects: Vec<ProjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectSummary {
    pub project_id: ProjectId,
    pub display_name: String,
    pub bindings: Vec<ProjectBindingSummary>,
    pub session_counts: SessionCounts,
    pub attention_count: u32,
    pub workflow_count: u32,
    pub last_activity_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectBindingSummary {
    pub binding_id: ProjectBindingId,
    pub runtime_id: ProviderRuntimeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SessionIndexView {
    pub project_id: ProjectId,
    pub active: Vec<SessionSummary>,
    pub history: Vec<SessionSummary>,
    pub archived: Vec<SessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub project_binding_id: ProjectBindingId,
    pub title: Option<String>,
    pub status: SessionStatus,
    pub ownership: OwnershipMode,
    pub live_binding: LiveBinding,
    pub queue_depth: u32,
    pub open_attention_count: u32,
    pub last_activity_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TranscriptView {
    pub session_id: SessionId,
    pub turns: Vec<TranscriptTurn>,
    /// True when earlier turns exist before the returned window.
    pub has_earlier: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TranscriptTurn {
    pub turn: Turn,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LiveActivityView {
    pub session_id: SessionId,
    pub active_turn_id: Option<TurnId>,
    pub streaming_item_ids: Vec<ItemId>,
    pub plan: Vec<PlanEntry>,
    pub tasks: Vec<AgentTask>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct InputQueueView {
    pub session_id: SessionId,
    pub entries: Vec<QueueEntry>,
    /// False when the runtime does not permit writing to the queue.
    pub writable: bool,
    /// False when steering is unsupported, so the client must present steering
    /// intents as queued rather than injected.
    pub steer_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttentionInboxView {
    /// Open entries across every project, soonest expiry first.
    pub entries: Vec<AttentionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WorkflowBoardView {
    pub workflow_id: WorkflowId,
    pub state: WorkflowState,
    pub steps: Vec<WorkflowBoardStep>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WorkflowBoardStep {
    pub step_id: StepId,
    pub title: String,
    pub state: StepState,
    pub depends_on: Vec<StepId>,
    pub assignment: StepAssignment,
    pub session_id: Option<SessionId>,
    pub blockers: Vec<StepBlocker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RuntimeCapabilityView {
    pub host_id: HostId,
    pub runtime_id: ProviderRuntimeId,
    pub negotiated_at_ms: i64,
    pub entries: Vec<CapabilityEntry>,
}

impl RuntimeCapabilityView {
    pub fn from_capabilities(host_id: HostId, capabilities: &RuntimeCapabilities) -> Self {
        Self {
            host_id,
            runtime_id: capabilities.runtime_id.clone(),
            negotiated_at_ms: capabilities.negotiated_at_ms,
            entries: capabilities.entries.clone(),
        }
    }

    fn validate(&self) -> Result<(), ContractViolation> {
        if self.host_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "runtime_capability.host_id",
            });
        }
        RuntimeCapabilities {
            runtime_id: self.runtime_id.clone(),
            negotiated_at_ms: self.negotiated_at_ms,
            entries: self.entries.clone(),
        }
        .validate()
    }
}

impl ProjectBindingSummary {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.binding_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "project_binding_summary.binding_id",
            });
        }
        if self.runtime_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "project_binding_summary.runtime_id",
            });
        }
        Ok(())
    }
}

impl ProjectSummary {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.project_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "project_summary.project_id",
            });
        }
        for binding in &self.bindings {
            binding.validate()?;
        }
        Ok(())
    }
}

impl ProviderGroup {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.runtime_ids.iter().any(ProviderRuntimeId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "provider_group.runtime_ids",
            });
        }
        for project in &self.projects {
            project.validate()?;
            for binding in &project.bindings {
                if !self
                    .runtime_ids
                    .iter()
                    .any(|runtime_id| runtime_id == &binding.runtime_id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "provider_group.projects.bindings.runtime_id",
                    });
                }
            }
        }
        Ok(())
    }
}

impl ProjectIndexView {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.host_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "project_index.host_id",
            });
        }
        for group in &self.groups {
            group.validate()?;
        }
        Ok(())
    }
}

impl SessionSummary {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.session_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "session_summary.session_id",
            });
        }
        if self.project_binding_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "session_summary.project_binding_id",
            });
        }
        self.live_binding.validate_shape()
    }
}

impl SessionIndexView {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.project_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "session_index.project_id",
            });
        }
        for summary in self
            .active
            .iter()
            .chain(&self.history)
            .chain(&self.archived)
        {
            summary.validate()?;
        }
        Ok(())
    }
}

impl TranscriptView {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.session_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "transcript.session_id",
            });
        }
        for transcript_turn in &self.turns {
            transcript_turn.turn.validate()?;
            if transcript_turn.turn.session_id != self.session_id {
                return Err(ContractViolation::DanglingReference {
                    field: "transcript.turns.session_id",
                });
            }
            for item_id in &transcript_turn.turn.item_ids {
                if !transcript_turn
                    .items
                    .iter()
                    .any(|item| &item.id == item_id && item.turn_id == transcript_turn.turn.id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "transcript.turns.item_ids",
                    });
                }
            }
            for item in &transcript_turn.items {
                item.validate()?;
                if item.session_id != self.session_id
                    || item.turn_id != transcript_turn.turn.id
                    || !transcript_turn.turn.item_ids.contains(&item.id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "transcript.turns.items",
                    });
                }
            }
        }
        Ok(())
    }
}

impl LiveActivityView {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.session_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "live_activity.session_id",
            });
        }
        if self.active_turn_id.as_ref().is_some_and(TurnId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "live_activity.active_turn_id",
            });
        }
        if self.streaming_item_ids.iter().any(ItemId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "live_activity.streaming_item_ids",
            });
        }
        for entry in &self.plan {
            entry
                .title_ref
                .ensure_sensitive("live_activity.plan.title_ref")?;
        }
        for task in &self.tasks {
            if task.id.is_empty() {
                return Err(ContractViolation::EmptyIdentifier {
                    field: "live_activity.tasks.id",
                });
            }
            task.title_ref
                .ensure_sensitive("live_activity.tasks.title_ref")?;
        }
        Ok(())
    }
}

impl InputQueueView {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.session_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "input_queue.session_id",
            });
        }
        for entry in &self.entries {
            entry.validate()?;
            if entry.session_id != self.session_id {
                return Err(ContractViolation::DanglingReference {
                    field: "input_queue.entries.session_id",
                });
            }
        }
        Ok(())
    }
}

impl AttentionInboxView {
    fn validate(&self) -> Result<(), ContractViolation> {
        for entry in &self.entries {
            entry.validate()?;
        }
        Ok(())
    }
}

impl WorkflowBoardStep {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.step_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "workflow_board_step.step_id",
            });
        }
        if self.depends_on.iter().any(StepId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "workflow_board_step.depends_on",
            });
        }
        if self.session_id.as_ref().is_some_and(SessionId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "workflow_board_step.session_id",
            });
        }
        self.assignment.validate()?;
        for blocker in &self.blockers {
            match blocker {
                StepBlocker::DependencyIncomplete { step_id } if step_id.is_empty() => {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "workflow_board_step.blockers.step_id",
                    });
                }
                StepBlocker::HumanGateOpen { attention_id } if attention_id.is_empty() => {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "workflow_board_step.blockers.attention_id",
                    });
                }
                StepBlocker::DependencyIncomplete { .. }
                | StepBlocker::CapabilityNotSupported { .. }
                | StepBlocker::HumanGateOpen { .. }
                | StepBlocker::NotSchedulable { .. } => {}
            }
        }
        Ok(())
    }
}

impl WorkflowBoardView {
    fn validate(&self) -> Result<(), ContractViolation> {
        if self.workflow_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "workflow_board.workflow_id",
            });
        }
        for step in &self.steps {
            step.validate()?;
            for dependency in &step.depends_on {
                if !self
                    .steps
                    .iter()
                    .any(|candidate| &candidate.step_id == dependency)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "workflow_board.steps.depends_on",
                    });
                }
            }
        }
        for artifact in &self.artifacts {
            artifact.validate()?;
            if artifact.workflow_id != self.workflow_id {
                return Err(ContractViolation::DanglingReference {
                    field: "workflow_board.artifacts.workflow_id",
                });
            }
            if let Some(step_id) = &artifact.produced_by {
                if !self.steps.iter().any(|step| &step.step_id == step_id) {
                    return Err(ContractViolation::DanglingReference {
                        field: "workflow_board.artifacts.produced_by",
                    });
                }
            }
        }
        Ok(())
    }
}

impl ProjectionKey {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        match self {
            ProjectionKey::ProjectIndex { host_id } | ProjectionKey::AttentionInbox { host_id }
                if host_id.is_empty() =>
            {
                Err(ContractViolation::EmptyIdentifier {
                    field: "projection_key.host_id",
                })
            }
            ProjectionKey::SessionIndex { project_id } if project_id.is_empty() => {
                Err(ContractViolation::EmptyIdentifier {
                    field: "projection_key.project_id",
                })
            }
            ProjectionKey::Transcript { session_id }
            | ProjectionKey::LiveActivity { session_id }
            | ProjectionKey::InputQueue { session_id }
                if session_id.is_empty() =>
            {
                Err(ContractViolation::EmptyIdentifier {
                    field: "projection_key.session_id",
                })
            }
            ProjectionKey::WorkflowBoard { workflow_id } if workflow_id.is_empty() => {
                Err(ContractViolation::EmptyIdentifier {
                    field: "projection_key.workflow_id",
                })
            }
            ProjectionKey::RuntimeCapability { host_id, .. } if host_id.is_empty() => {
                Err(ContractViolation::EmptyIdentifier {
                    field: "projection_key.host_id",
                })
            }
            ProjectionKey::RuntimeCapability { runtime_id, .. } if runtime_id.is_empty() => {
                Err(ContractViolation::EmptyIdentifier {
                    field: "projection_key.runtime_id",
                })
            }
            ProjectionKey::ProjectIndex { .. }
            | ProjectionKey::SessionIndex { .. }
            | ProjectionKey::Transcript { .. }
            | ProjectionKey::LiveActivity { .. }
            | ProjectionKey::InputQueue { .. }
            | ProjectionKey::AttentionInbox { .. }
            | ProjectionKey::WorkflowBoard { .. }
            | ProjectionKey::RuntimeCapability { .. } => Ok(()),
        }
    }
}

impl ProjectionSubscribeAck {
    /// Rejects an acknowledgement that was replayed or routed from another
    /// projection subscription.
    pub fn validate_for(&self, request: &ProjectionSubscribe) -> Result<(), ContractViolation> {
        request.key.validate()?;
        self.key.validate()?;
        if self.key != request.key {
            return Err(ContractViolation::ProjectionSubscribeKeyMismatch);
        }
        match &self.outcome {
            ProjectionSubscribeOutcome::Resumed { from_cursor } => {
                let since = request
                    .since
                    .ok_or(ContractViolation::ProjectionResumeWithoutCursor)?;
                let expected = since.next()?;
                if *from_cursor != expected {
                    return Err(ContractViolation::ProjectionResumeCursorMismatch {
                        expected: expected.seq,
                        found: from_cursor.seq,
                    });
                }
            }
            ProjectionSubscribeOutcome::Rejected { error } => error.validate()?,
            ProjectionSubscribeOutcome::CurrentFollows { current_cursor } => {
                if let Some(since) = request.since {
                    if current_cursor.seq <= since.seq {
                        return Err(ContractViolation::ProjectionCurrentCursorNotAhead {
                            since: since.seq,
                            current: current_cursor.seq,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Chooses the control response for a projection subscription without reading
/// or mutating a journal.
///
/// The caller obtains `retained_floor` and `current_head` from one stable
/// journal view. A cursor ahead of the head is a wire-visible rejection; an
/// internally inverted floor/head pair is a contract violation.
pub fn decide_projection_subscription(
    request: &ProjectionSubscribe,
    retained_floor: Cursor,
    current_head: Cursor,
    at_ms: i64,
) -> Result<ProjectionSubscribeAck, ContractViolation> {
    request.key.validate()?;
    if retained_floor.seq > current_head.seq {
        return Err(ContractViolation::InvalidProjectionCursorWindow {
            floor: retained_floor.seq,
            head: current_head.seq,
        });
    }

    let outcome = match request.since {
        None => ProjectionSubscribeOutcome::CurrentFollows {
            current_cursor: current_head,
        },
        Some(since) if since.seq > current_head.seq => ProjectionSubscribeOutcome::Rejected {
            error: CanonicalError {
                code: ErrorCode::CursorGap,
                retriable: true,
                detail_ref: None,
                at_ms,
            },
        },
        Some(since) => match since.next() {
            Err(ContractViolation::CursorOverflow) => ProjectionSubscribeOutcome::Rejected {
                error: CanonicalError {
                    code: ErrorCode::CursorGap,
                    retriable: true,
                    detail_ref: None,
                    at_ms,
                },
            },
            Err(other) => return Err(other),
            Ok(next) if next.seq < retained_floor.seq => {
                ProjectionSubscribeOutcome::CurrentFollows {
                    current_cursor: current_head,
                }
            }
            Ok(next) => ProjectionSubscribeOutcome::Resumed { from_cursor: next },
        },
    };

    Ok(ProjectionSubscribeAck {
        key: request.key.clone(),
        outcome,
    })
}

/// Verifies a retained or live run after the client's last applied cursor.
///
/// This helper deliberately contains no journal implementation. It gives both
/// host and mobile core one fail-closed rule for key mismatch, repetition,
/// gaps and checked cursor overflow.
pub fn validate_projection_sequence(
    key: &ProjectionKey,
    after: Cursor,
    envelopes: &[ProjectionEnvelope],
) -> Result<(), ContractViolation> {
    key.validate()?;
    let mut previous = after;
    for envelope in envelopes {
        envelope.validate_for_transport()?;
        if &envelope.key != key {
            return Err(ContractViolation::MixedProjectionKeys);
        }
        if envelope.cursor == previous {
            return Err(ContractViolation::CursorRepeated {
                cursor: envelope.cursor.seq,
            });
        }
        let expected = previous.next()?;
        if envelope.cursor != expected {
            return Err(ContractViolation::CursorGap {
                expected: expected.seq,
                found: envelope.cursor.seq,
            });
        }
        previous = envelope.cursor;
    }
    Ok(())
}

impl ProjectionEnvelope {
    /// Whether the client must discard cached state and refetch.
    pub fn requires_full_refresh(&self) -> bool {
        self.projection_version != PROJECTION_VERSION
    }

    /// Rejects projections that would leak unsafe content or cross stream scope.
    pub fn validate_for_transport(&self) -> Result<(), ContractViolation> {
        self.key.validate()?;
        self.payload.validate_for_key(&self.key)
    }
}

impl ProjectionPayload {
    /// Validates one complete read model against its unique cursor scope.
    pub fn validate_for_key(&self, key: &ProjectionKey) -> Result<(), ContractViolation> {
        key.validate()?;
        match (key, self) {
            (ProjectionKey::ProjectIndex { host_id }, ProjectionPayload::ProjectIndex { view })
                if host_id == &view.host_id =>
            {
                view.validate()
            }
            (
                ProjectionKey::SessionIndex { project_id },
                ProjectionPayload::SessionIndex { view },
            ) if project_id == &view.project_id => view.validate(),
            (ProjectionKey::Transcript { session_id }, ProjectionPayload::Transcript { view })
                if session_id == &view.session_id =>
            {
                view.validate()
            }
            (
                ProjectionKey::LiveActivity { session_id },
                ProjectionPayload::LiveActivity { view },
            ) if session_id == &view.session_id => view.validate(),
            (ProjectionKey::InputQueue { session_id }, ProjectionPayload::InputQueue { view })
                if session_id == &view.session_id =>
            {
                view.validate()
            }
            (
                ProjectionKey::AttentionInbox { host_id },
                ProjectionPayload::AttentionInbox { view },
            ) => {
                view.validate()?;
                if view.entries.iter().any(|entry| &entry.host_id != host_id) {
                    return Err(ContractViolation::DanglingReference {
                        field: "attention_inbox.entries.host_id",
                    });
                }
                Ok(())
            }
            (
                ProjectionKey::WorkflowBoard { workflow_id },
                ProjectionPayload::WorkflowBoard { view },
            ) if workflow_id == &view.workflow_id => view.validate(),
            (
                ProjectionKey::RuntimeCapability {
                    host_id,
                    runtime_id,
                },
                ProjectionPayload::RuntimeCapability { view },
            ) if host_id == &view.host_id && runtime_id == &view.runtime_id => view.validate(),
            _ => Err(ContractViolation::ProjectionKeyPayloadMismatch),
        }
    }
}
