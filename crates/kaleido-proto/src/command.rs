//! Commands and acknowledgements. See `docs/PROTOCOL.md` section 6.

use serde::{Deserialize, Serialize};

use crate::attention::AttentionResponse;
use crate::content::{ContentRef, Sensitivity};
use crate::error::CanonicalError;
use crate::ids::{
    CommandId, DeviceId, ProjectId, ProviderBindingHandle, ProviderBindingKind, ProviderRuntimeId,
    QueueEntryId, SessionId, StepId, TurnId, WorkflowId,
};
use crate::queue::QueueIntent;
use crate::workflow::StepAssignment;
use crate::ContractViolation;

/// Longest device-supplied idempotency key, measured as encoded UTF-8 bytes.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

/// Longest relative lifetime a mobile command may request.
pub const MAX_DEVICE_COMMAND_TTL_MS: u64 = 300_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CommandEnvelope {
    pub command_id: CommandId,
    /// Client-generated. Unique per `(actor, idempotency_key)`.
    pub idempotency_key: String,
    pub actor: Actor,
    pub issued_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub body: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    Human { device_id: DeviceId },
    Workflow { workflow_id: WorkflowId },
    Broker,
}

/// The only command shape accepted from an authenticated mobile device.
///
/// Identity, canonical command identifiers and absolute timestamps are
/// deliberately absent. The host injects them from the authenticated
/// connection before constructing a [`CommandEnvelope`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[serde(deny_unknown_fields)]
pub struct DeviceCommandRequest {
    pub idempotency_key: String,
    /// Relative to the host's receive time. `None` selects the host policy.
    pub ttl_ms: Option<u64>,
    pub body: Command,
}

/// There is deliberately no direct steering command.
///
/// Steering is expressed as [`Command::EnqueueInput`] with
/// [`QueueIntent::SteerActiveTurn`], and the broker decides from capabilities
/// and runtime acknowledgement whether it ever becomes a delivered steer. This
/// removes the protocol-level ability to claim an injection that never happened
/// (rule R-P9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    SubmitPrompt {
        session_id: SessionId,
        body: ContentRef,
    },
    EnqueueInput {
        session_id: SessionId,
        body: ContentRef,
        intent: QueueIntent,
    },
    EditQueueEntry {
        entry_id: QueueEntryId,
        body: ContentRef,
    },
    ReorderQueue {
        session_id: SessionId,
        order: Vec<QueueEntryId>,
    },
    CancelQueueEntry {
        entry_id: QueueEntryId,
    },
    InterruptTurn {
        session_id: SessionId,
        turn_id: TurnId,
    },
    RetryTurn {
        session_id: SessionId,
        turn_id: TurnId,
    },
    RespondAttention {
        response: AttentionResponse,
    },
    OpenSession {
        project_id: ProjectId,
        runtime_id: ProviderRuntimeId,
    },
    ResumeSession {
        session_id: SessionId,
    },
    CloseSession {
        session_id: SessionId,
    },
    AdvanceStep {
        step_id: StepId,
    },
    RetryStep {
        step_id: StepId,
    },
    ReworkStep {
        step_id: StepId,
        reason_ref: Option<ContentRef>,
    },
    SkipStep {
        step_id: StepId,
        reason_ref: Option<ContentRef>,
    },
    CancelStep {
        step_id: StepId,
    },
    ReassignStep {
        step_id: StepId,
        assignment: StepAssignment,
    },
    CancelWorkflow {
        workflow_id: WorkflowId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CommandAck {
    pub command_id: CommandId,
    pub outcome: CommandOutcome,
    pub acked_at_ms: i64,
}

/// Rule R-P10: local acceptance and runtime acceptance are different facts and
/// must never be collapsed into one "sent" state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// Durably recorded by the broker. The runtime has not accepted it yet.
    AcceptedLocally {
        note_ref: Option<ContentRef>,
    },
    AcceptedByRuntime {
        /// Canonical session whose state-changing command was accepted.
        /// This makes non-turn commands such as interrupt independently
        /// scopeable without inventing a `RemoteCommand` turn.
        session_id: SessionId,
        acceptance_kind: RuntimeAcceptanceKind,
        binding_handle: ProviderBindingHandle,
    },
    Enqueued {
        entry_id: QueueEntryId,
    },
    Rejected {
        error: CanonicalError,
    },
    /// A replay of an earlier `(actor, idempotency_key)`.
    Duplicate {
        original_command_id: CommandId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAcceptanceKind {
    /// A prompt admission must also materialize one correlated RemoteCommand turn.
    PromptTurn,
    /// A session-scoped state change, such as interrupt, does not create a turn.
    SessionControl,
}

impl CommandOutcome {
    /// Whether the runtime itself accepted the command.
    pub fn reached_runtime(&self) -> bool {
        matches!(self, CommandOutcome::AcceptedByRuntime { .. })
    }

    pub fn is_rejection(&self) -> bool {
        matches!(self, CommandOutcome::Rejected { .. })
    }

    pub fn validate(&self) -> Result<(), ContractViolation> {
        match self {
            CommandOutcome::AcceptedLocally {
                note_ref: Some(note_ref),
            } => validate_sensitive(note_ref, "command_outcome.note_ref"),
            CommandOutcome::AcceptedByRuntime {
                session_id,
                acceptance_kind: _,
                binding_handle,
            } => {
                if session_id.is_empty() {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "command_outcome.session_id",
                    });
                }
                binding_handle.validate_for(ProviderBindingKind::RuntimeAcknowledgement)
            }
            CommandOutcome::Rejected { error } => error.validate(),
            CommandOutcome::AcceptedLocally { note_ref: None }
            | CommandOutcome::Enqueued { .. }
            | CommandOutcome::Duplicate { .. } => Ok(()),
        }
    }
}

impl CommandEnvelope {
    pub fn expired_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_some_and(|expiry| now_ms >= expiry)
    }

    /// The idempotency scope key. Two envelopes sharing it are the same command.
    pub fn dedupe_key(&self) -> String {
        let actor = match &self.actor {
            Actor::Human { device_id } => {
                format!("human:{}:{}", device_id.as_str().len(), device_id)
            }
            Actor::Workflow { workflow_id } => {
                format!("workflow:{}:{}", workflow_id.as_str().len(), workflow_id)
            }
            Actor::Broker => "broker:0:".to_owned(),
        };
        format!(
            "{actor}|key:{}:{}",
            self.idempotency_key.len(),
            self.idempotency_key
        )
    }

    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.command_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "command_id",
            });
        }
        if self.idempotency_key.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "idempotency_key",
            });
        }
        self.actor.validate()?;
        self.body.validate()
    }
}

impl Actor {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        match self {
            Actor::Human { device_id } if device_id.is_empty() => {
                Err(ContractViolation::EmptyIdentifier {
                    field: "actor.device_id",
                })
            }
            Actor::Workflow { workflow_id } if workflow_id.is_empty() => {
                Err(ContractViolation::EmptyIdentifier {
                    field: "actor.workflow_id",
                })
            }
            Actor::Human { .. } | Actor::Workflow { .. } | Actor::Broker => Ok(()),
        }
    }
}

impl DeviceCommandRequest {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.idempotency_key.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "device_command.idempotency_key",
            });
        }
        if self.idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(ContractViolation::IdempotencyKeyTooLong {
                byte_len: self.idempotency_key.len(),
            });
        }
        if let Some(ttl_ms) = self.ttl_ms {
            if !(1..=MAX_DEVICE_COMMAND_TTL_MS).contains(&ttl_ms) {
                return Err(ContractViolation::InvalidDeviceCommandTtl { ttl_ms });
            }
        }
        self.body.validate()
    }
}

impl Command {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        match self {
            Command::SubmitPrompt { body, .. }
            | Command::EnqueueInput { body, .. }
            | Command::EditQueueEntry { body, .. } => validate_sensitive(body, "command.body"),
            Command::RespondAttention { response } => response.validate(),
            Command::ReworkStep {
                reason_ref: Some(reason_ref),
                ..
            } => validate_sensitive(reason_ref, "command.rework.reason_ref"),
            Command::SkipStep {
                reason_ref: Some(reason_ref),
                ..
            } => validate_sensitive(reason_ref, "command.skip.reason_ref"),
            Command::ReassignStep { assignment, .. } => assignment.validate(),
            Command::ReorderQueue { .. }
            | Command::CancelQueueEntry { .. }
            | Command::InterruptTurn { .. }
            | Command::RetryTurn { .. }
            | Command::OpenSession { .. }
            | Command::ResumeSession { .. }
            | Command::CloseSession { .. }
            | Command::AdvanceStep { .. }
            | Command::RetryStep { .. }
            | Command::ReworkStep {
                reason_ref: None, ..
            }
            | Command::SkipStep {
                reason_ref: None, ..
            }
            | Command::CancelStep { .. }
            | Command::CancelWorkflow { .. } => Ok(()),
        }
    }
}

impl CommandAck {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.command_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "command_id",
            });
        }
        self.outcome.validate()
    }
}

fn validate_sensitive(content: &ContentRef, field: &'static str) -> Result<(), ContractViolation> {
    content.validate()?;
    if content.sensitivity != Sensitivity::Sensitive {
        return Err(ContractViolation::SensitiveContentRequired { field });
    }
    Ok(())
}
