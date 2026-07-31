//! Turn and item lifecycle. See `docs/PROTOCOL.md` sections 4.4 and 4.5.

use serde::{Deserialize, Serialize};

use crate::content::ContentRef;
use crate::error::CanonicalError;
use crate::ids::{
    AgentTaskId, CommandId, ItemId, ProviderBindingHandle, ProviderBindingKind, SessionId, StepId,
    TurnId,
};
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Turn {
    pub id: TurnId,
    pub session_id: SessionId,
    pub status: TurnStatus,
    pub origin: TurnOrigin,
    pub started_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    /// Complete observed order, accumulated from per-item transitions.
    ///
    /// Section 4.4 forbids replacing this with the summary list a provider may
    /// attach to a turn-completion message; that list has been observed to
    /// contain only the final message of a multi-item turn.
    pub item_ids: Vec<ItemId>,
    pub error: Option<CanonicalError>,
    pub binding_handle: Option<ProviderBindingHandle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Pending,
    Running,
    /// Still in flight, but blocked on an open attention entry.
    AwaitingInteraction,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnOrigin {
    LocalSurface,
    RemoteCommand { command_id: CommandId },
    WorkflowStep { step_id: StepId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Item {
    pub id: ItemId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// Session-scoped monotonic ordinal for deterministic ordering.
    pub sequence: u64,
    pub status: ItemStatus,
    pub body: ItemBody,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub binding_handle: Option<ProviderBindingHandle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    InProgress,
    Completed,
    /// A human refused this operation. Rule R-P8 makes this a normal terminal
    /// state: it is not an error, and it does not fail the enclosing turn.
    Declined,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    UserMessage {
        content: ContentRef,
    },
    AgentMessage {
        content: ContentRef,
        phase: MessagePhase,
    },
    Reasoning {
        content: ContentRef,
    },
    ToolCall {
        tool: ToolDescriptor,
        arguments: Option<ContentRef>,
        output: Option<ContentRef>,
        exit_code: Option<i64>,
    },
    FileEdit {
        change_set: ChangeSet,
    },
    PlanUpdate {
        entries: Vec<PlanEntry>,
    },
    TaskUpdate {
        tasks: Vec<AgentTask>,
    },
    Diagnostic {
        severity: DiagnosticSeverity,
        code: ItemDiagnosticCode,
        detail: ContentRef,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ToolDescriptor {
    pub name: String,
    pub surface: ToolSurface,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSurface {
    ShellCommand,
    FileSystem,
    McpServer { server_name: String },
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ChangeSet {
    pub entries: Vec<FileChange>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FileChange {
    pub path_ref: ContentRef,
    pub kind: FileChangeKind,
    pub diff: Option<ContentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileChangeKind {
    Add,
    Modify,
    Delete,
    Rename { from_ref: ContentRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PlanEntry {
    pub title_ref: ContentRef,
    pub state: PlanEntryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryState {
    Pending,
    InProgress,
    Completed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AgentTask {
    pub id: AgentTaskId,
    pub title_ref: ContentRef,
    pub state: PlanEntryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ItemDiagnosticCode {
    RuntimeNotice,
    UnsupportedContent,
    ContentUnavailable,
    ValidationFailure,
}

impl ItemStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ItemStatus::Completed
                | ItemStatus::Declined
                | ItemStatus::Failed
                | ItemStatus::Cancelled
        )
    }

    /// Whether this terminal state represents a failure.
    ///
    /// `Declined` is deliberately excluded (rule R-P8).
    pub fn is_failure(&self) -> bool {
        matches!(self, ItemStatus::Failed)
    }
}

impl TurnStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Cancelled
        )
    }
}

impl Turn {
    /// Enforces the section 4.4 invariants a reducer must not be able to bypass.
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.error.is_some() && self.status != TurnStatus::Failed {
            return Err(ContractViolation::TurnErrorWithoutFailure {
                status: self.status,
            });
        }
        if self.status == TurnStatus::Failed && self.error.is_none() {
            return Err(ContractViolation::FailedTurnWithoutError);
        }
        if self.status.is_terminal() && self.completed_at_ms.is_none() {
            return Err(ContractViolation::TerminalTurnWithoutTimestamp);
        }
        let mut seen = self.item_ids.clone();
        seen.sort();
        let before = seen.len();
        seen.dedup();
        if seen.len() != before {
            return Err(ContractViolation::DuplicateItemReference);
        }
        if let Some(binding_handle) = &self.binding_handle {
            binding_handle.validate_for(ProviderBindingKind::Turn)?;
        }
        Ok(())
    }

    /// The turn status implied by a declined item, given the current status.
    ///
    /// Rule R-P8 in one place: a decline never changes the turn outcome.
    pub fn status_after_decline(&self) -> TurnStatus {
        self.status
    }
}

impl Item {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier { field: "item.id" });
        }
        if self.session_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "item.session_id",
            });
        }
        if self.turn_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "item.turn_id",
            });
        }
        if let Some(binding_handle) = &self.binding_handle {
            binding_handle.validate_for(ProviderBindingKind::Item)?;
        }

        match &self.body {
            ItemBody::UserMessage { content } => {
                content.ensure_sensitive("item.user_message.content")?;
            }
            ItemBody::AgentMessage { content, .. } => {
                content.ensure_sensitive("item.agent_message.content")?;
            }
            ItemBody::Reasoning { content } => {
                content.ensure_sensitive("item.reasoning.content")?;
            }
            ItemBody::ToolCall {
                arguments, output, ..
            } => {
                if let Some(arguments) = arguments {
                    arguments.ensure_sensitive("item.tool_call.arguments")?;
                }
                if let Some(output) = output {
                    output.ensure_sensitive("item.tool_call.output")?;
                }
            }
            ItemBody::FileEdit { change_set } => {
                for entry in &change_set.entries {
                    entry.path_ref.ensure_sensitive("item.file_edit.path_ref")?;
                    if let Some(diff) = &entry.diff {
                        diff.ensure_sensitive("item.file_edit.diff")?;
                    }
                    if let FileChangeKind::Rename { from_ref } = &entry.kind {
                        from_ref.ensure_sensitive("item.file_edit.rename.from_ref")?;
                    }
                }
            }
            ItemBody::PlanUpdate { entries } => {
                for entry in entries {
                    entry
                        .title_ref
                        .ensure_sensitive("item.plan_update.title_ref")?;
                }
            }
            ItemBody::TaskUpdate { tasks } => {
                for task in tasks {
                    if task.id.is_empty() {
                        return Err(ContractViolation::EmptyIdentifier {
                            field: "agent_task.id",
                        });
                    }
                    task.title_ref
                        .ensure_sensitive("item.task_update.title_ref")?;
                }
            }
            ItemBody::Diagnostic { detail, .. } => {
                detail.ensure_sensitive("item.diagnostic.detail")?;
            }
        }
        Ok(())
    }
}
