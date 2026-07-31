//! State transitions, cursors, the durable log and snapshots.
//! See `docs/PROTOCOL.md` section 5.

use serde::{Deserialize, Serialize};

use crate::attention::{AttentionItem, AttentionSubject, JoinState};
use crate::capability::RuntimeCapabilities;
use crate::command::CommandAck;
use crate::content::ContentRef;
use crate::error::CanonicalError;
use crate::host::{Host, Project, ProviderRuntime};
use crate::ids::{HostId, ProjectId, ProviderRuntimeId, QueueEntryId, SessionId, WorkflowId};
use crate::queue::{QueueEntry, QueueState};
use crate::session::{Session, SessionStatus};
use crate::turn::{Item, Turn};
use crate::workflow::{Artifact, Step, Workflow};
use crate::ContractViolation;

/// The only way canonical state may change.
///
/// These are state transitions, not renamed upstream messages: ADR-0010 D-2
/// retired the fixed upstream-event enumeration, and the set below is derived
/// from what the read models in section 8 must be able to rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateEffect {
    HostUpserted {
        host: Host,
    },
    RuntimeUpserted {
        runtime: ProviderRuntime,
    },
    CapabilitiesUpdated {
        capabilities: RuntimeCapabilities,
    },
    ProjectUpserted {
        project: Project,
    },
    SessionUpserted {
        session: Session,
    },
    SessionStatusChanged {
        session_id: SessionId,
        status: SessionStatus,
    },
    TurnUpserted {
        turn: Turn,
    },
    ItemUpserted {
        item: Item,
    },
    QueueEntryUpserted {
        entry: QueueEntry,
    },
    QueueReordered {
        session_id: SessionId,
        order: Vec<QueueEntryId>,
    },
    AttentionUpserted {
        item: AttentionItem,
    },
    WorkflowUpserted {
        workflow: Workflow,
    },
    StepUpserted {
        step: Step,
    },
    ArtifactUpserted {
        artifact: Artifact,
    },
    CommandAcknowledged {
        ack: CommandAck,
    },
    /// Where unmodelled or malformed upstream traffic lands (ADR-0012 D-3).
    /// It is never allowed to panic, and never allowed to masquerade as a
    /// supported projection.
    DiagnosticRecorded {
        diagnostic: DiagnosticRecord,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DiagnosticRecord {
    pub runtime_id: Option<ProviderRuntimeId>,
    pub session_id: Option<SessionId>,
    pub code: DiagnosticCode,
    pub count: u64,
    pub first_at_ms: i64,
    pub last_at_ms: i64,
    pub detail_ref: Option<ContentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticCode {
    UnknownUpstreamMessage,
    UnknownUpstreamLabel,
    PointerResolutionFailed,
    JoinDeferred,
    JoinFailed,
    BackpressureCoalesced,
    MalformedProviderMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamKey {
    Host { host_id: HostId },
    Project { project_id: ProjectId },
    Session { session_id: SessionId },
    Workflow { workflow_id: WorkflowId },
}

/// Strictly monotonic position within one [`StreamKey`], stepping by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Cursor {
    pub seq: u64,
}

impl Cursor {
    pub const START: Cursor = Cursor { seq: 0 };

    /// Returns the next stream position without ever repeating a cursor.
    pub fn next(&self) -> Result<Cursor, ContractViolation> {
        self.seq
            .checked_add(1)
            .map(|seq| Cursor { seq })
            .ok_or(ContractViolation::CursorOverflow)
    }

    /// Whether `self` is the immediate successor of `previous`.
    pub fn follows(&self, previous: Cursor) -> bool {
        previous.next().is_ok_and(|expected| *self == expected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LogRecord {
    pub cursor: Cursor,
    pub stream: StreamKey,
    pub appended_at_ms: i64,
    pub effect: StateEffect,
}

/// Verifies that a run of records for one stream has no gap and no repetition.
///
/// Section 5.2 treats a gap as log corruption, so this is a hard check rather
/// than a warning.
pub fn verify_contiguous(records: &[LogRecord]) -> Result<(), ContractViolation> {
    let mut previous: Option<(Cursor, &StreamKey)> = None;
    for record in records {
        match previous {
            None => {}
            Some((last_cursor, last_stream)) => {
                if last_stream != &record.stream {
                    return Err(ContractViolation::MixedStreams);
                }
                let expected = last_cursor.next()?;
                if record.cursor == last_cursor {
                    return Err(ContractViolation::CursorRepeated {
                        cursor: record.cursor.seq,
                    });
                }
                if record.cursor != expected {
                    return Err(ContractViolation::CursorGap {
                        expected: expected.seq,
                        found: record.cursor.seq,
                    });
                }
            }
        }
        previous = Some((record.cursor, &record.stream));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SnapshotEnvelope {
    pub stream: StreamKey,
    pub cursor: Cursor,
    pub payload: SnapshotPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
// Snapshot payloads stay inline because Box is outside the R-P1 wire surface
// and is not a UniFFI value type.
#[allow(clippy::large_enum_variant)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotPayload {
    Host { snapshot: HostSnapshot },
    Project { snapshot: ProjectSnapshot },
    Session { snapshot: SessionSnapshot },
    Workflow { snapshot: WorkflowSnapshot },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct HostSnapshot {
    pub host: Host,
    pub runtimes: Vec<ProviderRuntime>,
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectSnapshot {
    pub project: Project,
    pub sessions: Vec<Session>,
    pub workflows: Vec<Workflow>,
    pub attention: Vec<AttentionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SessionSnapshot {
    pub session: Session,
    pub turns: Vec<Turn>,
    pub items: Vec<Item>,
    pub queue: Vec<QueueEntry>,
    pub attention: Vec<AttentionItem>,
    pub capabilities: RuntimeCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WorkflowSnapshot {
    pub workflow: Workflow,
    pub steps: Vec<Step>,
    pub artifacts: Vec<Artifact>,
    pub attention: Vec<AttentionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Subscribe {
    pub stream: StreamKey,
    pub since: Option<Cursor>,
}

/// The control response to a [`Subscribe`].
///
/// A snapshot can be large, so it is never a variant of this response: the
/// response only says whether resumption succeeded, and a [`SnapshotEnvelope`]
/// matching the subscribed stream follows as its own message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SubscribeAck {
    pub stream: StreamKey,
    pub outcome: SubscribeOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubscribeOutcome {
    Resumed {
        from_cursor: Cursor,
    },
    /// `since` was absent or has already been compacted away. The server must
    /// send a snapshot rather than silently start from the current position.
    SnapshotFollows {
        snapshot_cursor: Cursor,
    },
    Rejected {
        error: CanonicalError,
    },
}

impl DiagnosticRecord {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self
            .runtime_id
            .as_ref()
            .is_some_and(ProviderRuntimeId::is_empty)
        {
            return Err(ContractViolation::EmptyIdentifier {
                field: "diagnostic.runtime_id",
            });
        }
        if self.session_id.as_ref().is_some_and(SessionId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "diagnostic.session_id",
            });
        }
        if let Some(detail_ref) = &self.detail_ref {
            detail_ref.ensure_sensitive("diagnostic.detail_ref")?;
        }
        Ok(())
    }
}

impl StateEffect {
    /// Checks that an effect is safe and structurally valid before appending it.
    pub fn validate_for_log(&self) -> Result<(), ContractViolation> {
        match self {
            StateEffect::HostUpserted { host } => host.validate(),
            StateEffect::RuntimeUpserted { runtime } => runtime.validate(),
            StateEffect::CapabilitiesUpdated { capabilities } => capabilities.validate(),
            StateEffect::ProjectUpserted { project } => project.validate(),
            StateEffect::SessionUpserted { session } => session.validate_shape(),
            StateEffect::SessionStatusChanged { session_id, .. } => {
                if session_id.is_empty() {
                    Err(ContractViolation::EmptyIdentifier {
                        field: "session_status_changed.session_id",
                    })
                } else {
                    Ok(())
                }
            }
            StateEffect::TurnUpserted { turn } => turn.validate(),
            StateEffect::ItemUpserted { item } => item.validate(),
            StateEffect::QueueEntryUpserted { entry } => entry.validate(),
            StateEffect::QueueReordered { session_id, order } => {
                if session_id.is_empty() {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "queue_reordered.session_id",
                    });
                }
                if order.iter().any(QueueEntryId::is_empty) {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "queue_reordered.order",
                    });
                }
                let mut seen: Vec<&QueueEntryId> = Vec::with_capacity(order.len());
                for entry_id in order {
                    if seen.contains(&entry_id) {
                        return Err(ContractViolation::QueueReorderDuplicate);
                    }
                    seen.push(entry_id);
                }
                Ok(())
            }
            StateEffect::AttentionUpserted { item } => item.validate(),
            StateEffect::WorkflowUpserted { workflow } => workflow.validate(),
            StateEffect::StepUpserted { step } => step.validate(),
            StateEffect::ArtifactUpserted { artifact } => artifact.validate(),
            StateEffect::CommandAcknowledged { ack } => ack.validate(),
            StateEffect::DiagnosticRecorded { diagnostic } => diagnostic.validate(),
        }
    }
}

impl HostSnapshot {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        self.host.validate()?;
        for runtime in &self.runtimes {
            runtime.validate()?;
            if runtime.host_id != self.host.id {
                return Err(ContractViolation::DanglingReference {
                    field: "host_snapshot.runtimes.host_id",
                });
            }
        }
        for project in &self.projects {
            project.validate()?;
            for binding in &project.bindings {
                if !self
                    .runtimes
                    .iter()
                    .any(|runtime| runtime.id == binding.runtime_id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "host_snapshot.projects.bindings.runtime_id",
                    });
                }
            }
        }
        Ok(())
    }
}

impl ProjectSnapshot {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        self.project.validate()?;
        for session in &self.sessions {
            session.validate_shape()?;
            if session.project_id != self.project.id {
                return Err(ContractViolation::DanglingReference {
                    field: "project_snapshot.sessions.project_id",
                });
            }
            if !self
                .project
                .bindings
                .iter()
                .any(|binding| binding.id == session.project_binding_id)
            {
                return Err(ContractViolation::DanglingReference {
                    field: "project_snapshot.sessions.project_binding_id",
                });
            }
        }
        for workflow in &self.workflows {
            workflow.validate()?;
            if workflow.project_id != self.project.id {
                return Err(ContractViolation::DanglingReference {
                    field: "project_snapshot.workflows.project_id",
                });
            }
        }
        for attention in &self.attention {
            attention.validate()?;
            if attention.project_id != self.project.id {
                return Err(ContractViolation::DanglingReference {
                    field: "project_snapshot.attention.project_id",
                });
            }
            if let Some(session_id) = &attention.session_id {
                if !self
                    .sessions
                    .iter()
                    .any(|session| &session.id == session_id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "project_snapshot.attention.session_id",
                    });
                }
            }
            if let Some(workflow_id) = &attention.workflow_id {
                if !self
                    .workflows
                    .iter()
                    .any(|workflow| &workflow.id == workflow_id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "project_snapshot.attention.workflow_id",
                    });
                }
            }
        }
        Ok(())
    }
}

impl SessionSnapshot {
    /// Checks session scope, references and all nested safety invariants.
    pub fn validate(&self) -> Result<(), ContractViolation> {
        self.capabilities.validate()?;
        self.session.validate(&self.capabilities)?;
        if let Some(binding_handle) = &self.session.binding_handle {
            if binding_handle.runtime_id != self.capabilities.runtime_id {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.session.binding_handle.runtime_id",
                });
            }
        }
        if let Some(active) = &self.session.active_turn_id {
            if !self.turns.iter().any(|turn| &turn.id == active) {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.session.active_turn_id",
                });
            }
        }
        let mut turn_ids = Vec::with_capacity(self.turns.len());
        for turn in &self.turns {
            turn.validate()?;
            if turn_ids.contains(&&turn.id) {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.turns.id",
                });
            }
            turn_ids.push(&turn.id);
            if turn.session_id != self.session.id {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.turns.session_id",
                });
            }
            for item_id in &turn.item_ids {
                if !self
                    .items
                    .iter()
                    .any(|item| &item.id == item_id && item.turn_id == turn.id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "session_snapshot.turns.item_ids",
                    });
                }
            }
            if let Some(binding_handle) = &turn.binding_handle {
                if binding_handle.runtime_id != self.capabilities.runtime_id {
                    return Err(ContractViolation::DanglingReference {
                        field: "session_snapshot.turns.binding_handle.runtime_id",
                    });
                }
            }
        }
        let mut item_ids = Vec::with_capacity(self.items.len());
        for item in &self.items {
            item.validate()?;
            if item_ids.contains(&&item.id) {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.items.id",
                });
            }
            item_ids.push(&item.id);
            if item.session_id != self.session.id
                || !self
                    .turns
                    .iter()
                    .any(|turn| turn.id == item.turn_id && turn.item_ids.contains(&item.id))
            {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.items.turn_id",
                });
            }
            if let Some(binding_handle) = &item.binding_handle {
                if binding_handle.runtime_id != self.capabilities.runtime_id {
                    return Err(ContractViolation::DanglingReference {
                        field: "session_snapshot.items.binding_handle.runtime_id",
                    });
                }
            }
        }
        let mut queue_ids = Vec::with_capacity(self.queue.len());
        for entry in &self.queue {
            entry.validate()?;
            if queue_ids.contains(&&entry.id) {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.queue.id",
                });
            }
            queue_ids.push(&entry.id);
            if entry.session_id != self.session.id {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.queue.session_id",
                });
            }
            match &entry.state {
                QueueState::DeliveredAsNewTurn { turn_id, .. } => {
                    if !self.turns.iter().any(|turn| &turn.id == turn_id) {
                        return Err(ContractViolation::DanglingReference {
                            field: "session_snapshot.queue.turn_id",
                        });
                    }
                }
                QueueState::DeliveredAsSteer { .. } => {
                    let active_turn_id = self.session.active_turn_id.as_ref().ok_or(
                        ContractViolation::DanglingReference {
                            field: "session_snapshot.queue.active_turn_id",
                        },
                    )?;
                    entry.validate_for_active_turn(active_turn_id, &self.capabilities)?;
                }
                QueueState::Pending
                | QueueState::Submitting { .. }
                | QueueState::Rejected { .. }
                | QueueState::Cancelled { .. } => {}
            }
        }
        let mut pending = self
            .queue
            .iter()
            .filter(|entry| entry.state.is_pending())
            .collect::<Vec<_>>();
        pending.sort_by_key(|entry| entry.position);
        for (expected, entry) in (0_u32..).zip(pending) {
            if entry.position != expected {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.queue.position",
                });
            }
        }
        let mut attention_ids = Vec::with_capacity(self.attention.len());
        for attention in &self.attention {
            attention.validate()?;
            if attention_ids.contains(&&attention.id) {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.attention.id",
                });
            }
            attention_ids.push(&attention.id);
            if attention.session_id.as_ref() != Some(&self.session.id) {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.attention.session_id",
                });
            }
            if attention.project_id != self.session.project_id {
                return Err(ContractViolation::DanglingReference {
                    field: "session_snapshot.attention.project_id",
                });
            }
            if let Some(turn_id) = &attention.turn_id {
                if !self.turns.iter().any(|turn| &turn.id == turn_id) {
                    return Err(ContractViolation::DanglingReference {
                        field: "session_snapshot.attention.turn_id",
                    });
                }
            }
            if let AttentionSubject::Approval { request } = &attention.subject {
                if let JoinState::Joined { item_id } = &request.join {
                    if !self.items.iter().any(|item| {
                        &item.id == item_id
                            && Some(&item.turn_id) == attention.turn_id.as_ref()
                            && item.session_id == self.session.id
                    }) {
                        return Err(ContractViolation::DanglingReference {
                            field: "session_snapshot.attention.approval.join",
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl WorkflowSnapshot {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        self.workflow.validate()?;
        for step_id in &self.workflow.step_ids {
            if !self.steps.iter().any(|step| &step.id == step_id) {
                return Err(ContractViolation::DanglingReference {
                    field: "workflow_snapshot.workflow.step_ids",
                });
            }
        }
        for step in &self.steps {
            step.validate()?;
            if step.workflow_id != self.workflow.id || !self.workflow.step_ids.contains(&step.id) {
                return Err(ContractViolation::DanglingReference {
                    field: "workflow_snapshot.steps.workflow_id",
                });
            }
            for dependency in &step.depends_on {
                if !self
                    .steps
                    .iter()
                    .any(|candidate| &candidate.id == dependency)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "workflow_snapshot.steps.depends_on",
                    });
                }
            }
            for artifact_id in step.inputs.iter().chain(&step.outputs) {
                if !self
                    .artifacts
                    .iter()
                    .any(|artifact| &artifact.id == artifact_id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "workflow_snapshot.steps.artifacts",
                    });
                }
            }
            if let Some(attention_id) = &step.human_gate {
                if !self
                    .attention
                    .iter()
                    .any(|attention| &attention.id == attention_id)
                {
                    return Err(ContractViolation::DanglingReference {
                        field: "workflow_snapshot.steps.human_gate",
                    });
                }
            }
        }
        for artifact in &self.artifacts {
            artifact.validate()?;
            if artifact.workflow_id != self.workflow.id {
                return Err(ContractViolation::DanglingReference {
                    field: "workflow_snapshot.artifacts.workflow_id",
                });
            }
            if let Some(step_id) = &artifact.produced_by {
                if !self.steps.iter().any(|step| &step.id == step_id) {
                    return Err(ContractViolation::DanglingReference {
                        field: "workflow_snapshot.artifacts.produced_by",
                    });
                }
            }
        }
        for attention in &self.attention {
            attention.validate()?;
            if attention.project_id != self.workflow.project_id {
                return Err(ContractViolation::DanglingReference {
                    field: "workflow_snapshot.attention.project_id",
                });
            }
            if attention.workflow_id.as_ref() != Some(&self.workflow.id) {
                return Err(ContractViolation::DanglingReference {
                    field: "workflow_snapshot.attention.workflow_id",
                });
            }
            if let AttentionSubject::WorkflowGate { request } = &attention.subject {
                if !self.steps.iter().any(|step| step.id == request.step_id) {
                    return Err(ContractViolation::DanglingReference {
                        field: "workflow_snapshot.attention.step_id",
                    });
                }
            }
        }
        Ok(())
    }
}

impl SnapshotEnvelope {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        match (&self.stream, &self.payload) {
            (StreamKey::Host { host_id }, SnapshotPayload::Host { snapshot })
                if host_id == &snapshot.host.id =>
            {
                snapshot.validate()
            }
            (StreamKey::Project { project_id }, SnapshotPayload::Project { snapshot })
                if project_id == &snapshot.project.id =>
            {
                snapshot.validate()
            }
            (StreamKey::Session { session_id }, SnapshotPayload::Session { snapshot })
                if session_id == &snapshot.session.id =>
            {
                snapshot.validate()
            }
            (StreamKey::Workflow { workflow_id }, SnapshotPayload::Workflow { snapshot })
                if workflow_id == &snapshot.workflow.id =>
            {
                snapshot.validate()
            }
            _ => Err(ContractViolation::SnapshotStreamMismatch),
        }
    }
}

/// Validates the replay segment that follows a snapshot.
pub fn validate_replay_window(
    snapshot: &SnapshotEnvelope,
    records: &[LogRecord],
) -> Result<(), ContractViolation> {
    snapshot.validate()?;
    let Some(first) = records.first() else {
        return Ok(());
    };
    if first.stream != snapshot.stream {
        return Err(ContractViolation::MixedStreams);
    }
    let expected = snapshot.cursor.next()?;
    if first.cursor == snapshot.cursor {
        return Err(ContractViolation::CursorRepeated {
            cursor: first.cursor.seq,
        });
    }
    if first.cursor != expected {
        return Err(ContractViolation::CursorGap {
            expected: expected.seq,
            found: first.cursor.seq,
        });
    }
    verify_contiguous(records)?;
    for record in records {
        record.effect.validate_for_log()?;
    }
    Ok(())
}
