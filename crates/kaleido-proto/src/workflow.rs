//! Cross-agent workflow. See `docs/PROTOCOL.md` section 4.8.
//!
//! ADR-0010 D-4 puts workflow state and manual progression in v1. Automatic
//! scheduling policy and automatic quality judgement are explicitly out of
//! scope for protocol version 0.1.

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, RuntimeCapabilities};
use crate::command::Actor;
use crate::content::{ContentRef, Sensitivity};
use crate::host::ProviderFamily;
use crate::ids::{
    ArtifactId, AttentionId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId, StepId,
    WorkflowId,
};
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Workflow {
    pub id: WorkflowId,
    pub project_id: ProjectId,
    pub title: String,
    pub state: WorkflowState,
    pub step_ids: Vec<StepId>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Draft,
    Ready,
    Running,
    Blocked,
    WaitingHuman,
    Review,
    Rework,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Step {
    pub id: StepId,
    pub workflow_id: WorkflowId,
    pub title: String,
    pub role: StepRole,
    pub assignment: StepAssignment,
    pub depends_on: Vec<StepId>,
    pub inputs: Vec<ArtifactId>,
    pub outputs: Vec<ArtifactId>,
    pub completion: CompletionCondition,
    pub human_gate: Option<AttentionId>,
    pub session_id: Option<SessionId>,
    pub state: StepState,
    pub attempt: u32,
    pub audit: Vec<StepTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepRole {
    Plan,
    Implement,
    Review,
    Verify,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Draft,
    Ready,
    Running,
    Blocked,
    WaitingHuman,
    Review,
    Rework,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

/// How a step chooses its runtime.
///
/// `family` records user intent for display. Scheduling reads `required`, so a
/// step never depends on a provider name (rule R-P6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RuntimeSelector {
    pub family: ProviderFamily,
    pub required: Vec<Capability>,
    pub runtime_id: Option<ProviderRuntimeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StepAssignment {
    pub selector: RuntimeSelector,
    pub project_binding_id: ProjectBindingId,
    /// A worktree path is always sensitive and must not carry a preview.
    pub worktree_ref: ContentRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionCondition {
    AgentTurnCompleted,
    /// The field is not named `kind` because that name is reserved for the
    /// enum tag on the wire (`docs/PROTOCOL.md` section 1).
    ArtifactProduced {
        artifact_kind: ArtifactKind,
    },
    HumanApproved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Artifact {
    pub id: ArtifactId,
    pub workflow_id: WorkflowId,
    pub produced_by: Option<StepId>,
    pub kind: ArtifactKind,
    pub content: ContentRef,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactKind {
    Plan,
    Diff,
    Commit,
    ReviewNotes,
    TestReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StepTransition {
    pub from: StepState,
    pub to: StepState,
    pub action: WorkflowAction,
    pub actor: Actor,
    pub at_ms: i64,
    pub reason_ref: Option<ContentRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAction {
    Advance,
    Retry,
    Rework,
    Skip,
    Cancel,
    Reassign,
}

/// Why a step is not runnable yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepBlocker {
    DependencyIncomplete { step_id: StepId },
    CapabilityNotSupported { capability: Capability },
    HumanGateOpen { attention_id: AttentionId },
    NotSchedulable { state: StepState },
}

impl Workflow {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "workflow.id",
            });
        }
        if self.project_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "workflow.project_id",
            });
        }
        if self.step_ids.iter().any(StepId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "workflow.step_ids",
            });
        }
        Ok(())
    }
}

impl Step {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier { field: "step.id" });
        }
        if self.workflow_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "step.workflow_id",
            });
        }
        self.assignment.validate()?;
        for transition in &self.audit {
            transition.validate()?;
        }
        Ok(())
    }

    /// Section 4.8 admission check: dependencies complete, every required
    /// capability supported, and any human gate already answered.
    pub fn blockers(
        &self,
        dependency_states: &[(StepId, StepState)],
        capabilities: &RuntimeCapabilities,
        gate_open: bool,
    ) -> Vec<StepBlocker> {
        let mut blockers = Vec::new();
        if self.state != StepState::Ready {
            blockers.push(StepBlocker::NotSchedulable { state: self.state });
        }
        for dependency in &self.depends_on {
            let complete = dependency_states.iter().any(|(id, state)| {
                id == dependency && matches!(state, StepState::Completed | StepState::Skipped)
            });
            if !complete {
                blockers.push(StepBlocker::DependencyIncomplete {
                    step_id: dependency.clone(),
                });
            }
        }
        for capability in &self.assignment.selector.required {
            if !capabilities.permits(capability) {
                blockers.push(StepBlocker::CapabilityNotSupported {
                    capability: *capability,
                });
            }
        }
        if let Some(attention_id) = &self.human_gate {
            if gate_open {
                blockers.push(StepBlocker::HumanGateOpen {
                    attention_id: attention_id.clone(),
                });
            }
        }
        blockers
    }

    pub fn validate_transition(
        &self,
        action: WorkflowAction,
        to: StepState,
    ) -> Result<(), ContractViolation> {
        validate_transition(self.state, action, to)?;
        if action == WorkflowAction::Retry && self.attempt.checked_add(1).is_none() {
            return Err(ContractViolation::WorkflowAttemptOverflow);
        }
        Ok(())
    }

    /// Starts another attempt without silently wrapping at `u32::MAX`.
    pub fn increment_attempt(&mut self) -> Result<(), ContractViolation> {
        self.attempt = self
            .attempt
            .checked_add(1)
            .ok_or(ContractViolation::WorkflowAttemptOverflow)?;
        Ok(())
    }
}

impl Artifact {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "artifact.id",
            });
        }
        if self.workflow_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "artifact.workflow_id",
            });
        }
        if self.produced_by.as_ref().is_some_and(StepId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "artifact.produced_by",
            });
        }
        self.content.validate()
    }
}

impl StepAssignment {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.project_binding_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "project_binding_id",
            });
        }
        validate_sensitive(&self.worktree_ref, "step_assignment.worktree_ref")
    }
}

impl StepTransition {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        validate_transition(self.from, self.action, self.to)?;
        if let Some(reason_ref) = &self.reason_ref {
            validate_sensitive(reason_ref, "step_transition.reason_ref")?;
        }
        Ok(())
    }
}

/// Validates the complete closed transition table for manual workflow actions.
pub fn validate_transition(
    from: StepState,
    action: WorkflowAction,
    to: StepState,
) -> Result<(), ContractViolation> {
    let valid = match action {
        WorkflowAction::Advance => matches!(
            (from, to),
            (StepState::Draft, StepState::Ready)
                | (StepState::Ready, StepState::Running)
                | (StepState::Running, StepState::Review)
                | (StepState::Running, StepState::Completed)
                | (StepState::WaitingHuman, StepState::Ready)
                | (StepState::Blocked, StepState::Ready)
                | (StepState::Rework, StepState::Ready)
                | (StepState::Review, StepState::Completed)
        ),
        WorkflowAction::Retry => from == StepState::Failed && to == StepState::Ready,
        WorkflowAction::Rework => {
            matches!(
                from,
                StepState::Review | StepState::Completed | StepState::Failed
            ) && to == StepState::Rework
        }
        WorkflowAction::Skip => {
            matches!(
                from,
                StepState::Draft
                    | StepState::Ready
                    | StepState::Blocked
                    | StepState::WaitingHuman
                    | StepState::Review
                    | StepState::Rework
                    | StepState::Failed
            ) && to == StepState::Skipped
        }
        WorkflowAction::Cancel => {
            !matches!(
                from,
                StepState::Completed
                    | StepState::Failed
                    | StepState::Skipped
                    | StepState::Cancelled
            ) && to == StepState::Cancelled
        }
        WorkflowAction::Reassign => {
            matches!(
                from,
                StepState::Draft
                    | StepState::Ready
                    | StepState::Blocked
                    | StepState::WaitingHuman
                    | StepState::Rework
                    | StepState::Failed
            ) && to == from
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ContractViolation::WorkflowTransitionNotAllowed)
    }
}

fn validate_sensitive(content: &ContentRef, field: &'static str) -> Result<(), ContractViolation> {
    content.validate()?;
    if content.sensitivity != Sensitivity::Sensitive {
        return Err(ContractViolation::SensitiveContentRequired { field });
    }
    Ok(())
}
