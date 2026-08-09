//! Attention inbox: approvals, questions, workflow gates and faults.
//! See `docs/PROTOCOL.md` section 4.7.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::content::{ContentRef, Sensitivity};
use crate::host::ConnectionFaultReason;
use crate::ids::{
    AttentionId, CommandId, HostId, ItemId, ProjectId, ProviderBindingHandle, ProviderBindingKind,
    ProviderRuntimeId, SessionId, StepId, TurnId, WorkflowId,
};
use crate::turn::Item;
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttentionItem {
    pub id: AttentionId,
    pub host_id: HostId,
    pub project_id: ProjectId,
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
    pub workflow_id: Option<WorkflowId>,
    pub subject: AttentionSubject,
    pub state: AttentionState,
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttentionSubject {
    Approval {
        request: ApprovalRequest,
    },
    Question {
        request: QuestionRequest,
    },
    WorkflowGate {
        request: WorkflowGateRequest,
    },
    /// Surfaces a broken runtime connection in the global inbox without
    /// fabricating a turn error.
    ConnectionFault {
        runtime_id: ProviderRuntimeId,
        reason: ConnectionFaultReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttentionState {
    Open,
    Answered {
        option_id: Option<String>,
        free_form_ref: Option<ContentRef>,
        decided_at_ms: i64,
        answer_source: AttentionAnswerSource,
    },
    Expired {
        at_ms: i64,
    },
    Superseded {
        by: AttentionId,
    },
    Cancelled {
        at_ms: i64,
    },
}

/// Whether an answer came from an actual broker command or was observed from
/// another client on the provider's structured protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttentionAnswerSource {
    LocalCommand { command_id: CommandId },
    ObservedExternal { evidence: AttentionAnswerEvidence },
}

/// Auditable facts attached to an externally observed answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttentionAnswerEvidence {
    pub observer_host_id: HostId,
    pub observed_at_ms: i64,
    pub source: AttentionAnswerEvidenceSource,
}

/// The only media that can prove one concrete external answer occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttentionAnswerEvidenceSource {
    ObservedInTraffic,
    RecordedFixture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ApprovalRequest {
    /// Broker-assigned key, stable across reconnects.
    pub request_key: String,
    pub target_item_id: ItemId,
    pub join: JoinState,
    /// Provided by the runtime. Readers must not hard-code a two-option
    /// allow/deny pair.
    pub options: Vec<DecisionOption>,
    pub summary_ref: ContentRef,
    pub detail_ref: Option<ContentRef>,
    pub binding_handle: ProviderBindingHandle,
}

/// Whether the approval request could be correlated with a known item.
///
/// A provider may send an approval request that carries only a reference to the
/// operation, with no displayable context of its own, so this correlation can
/// fail or arrive out of order and must remain renderable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinState {
    Joined { item_id: ItemId },
    Unjoined { reason: JoinFailureReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinFailureReason {
    /// The referenced operation has not been announced yet.
    ItemNotYetSeen,
    ItemUnknown,
    AmbiguousTarget,
    ScopeMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QuestionRequest {
    pub request_key: String,
    pub prompt_ref: ContentRef,
    pub options: Vec<DecisionOption>,
    pub free_form_allowed: bool,
    pub binding_handle: ProviderBindingHandle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WorkflowGateRequest {
    pub request_key: String,
    pub step_id: StepId,
    pub prompt_ref: ContentRef,
    pub options: Vec<DecisionOption>,
    pub free_form_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DecisionOption {
    pub option_id: String,
    pub label: String,
    pub semantics: DecisionSemantics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionSemantics {
    Allow,
    AllowAlways,
    Deny,
    DenyAlways,
    Cancel,
    Choose,
}

/// A reply command binds every mutable fact the client observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttentionResponse {
    pub attention_id: AttentionId,
    pub session_id: Option<SessionId>,
    pub request_key: String,
    pub expected_expires_at_ms: Option<i64>,
    pub option_id: Option<String>,
    pub free_form_ref: Option<ContentRef>,
}

impl AttentionState {
    pub fn is_open(&self) -> bool {
        matches!(self, AttentionState::Open)
    }
}

impl AttentionItem {
    pub fn request_key(&self) -> Option<&str> {
        match &self.subject {
            AttentionSubject::Approval { request } => Some(&request.request_key),
            AttentionSubject::Question { request } => Some(&request.request_key),
            AttentionSubject::WorkflowGate { request } => Some(&request.request_key),
            AttentionSubject::ConnectionFault { .. } => None,
        }
    }

    pub fn options(&self) -> &[DecisionOption] {
        match &self.subject {
            AttentionSubject::Approval { request } => &request.options,
            AttentionSubject::Question { request } => &request.options,
            AttentionSubject::WorkflowGate { request } => &request.options,
            AttentionSubject::ConnectionFault { .. } => &[],
        }
    }

    fn free_form_allowed(&self) -> bool {
        match &self.subject {
            AttentionSubject::Approval { .. } | AttentionSubject::ConnectionFault { .. } => false,
            AttentionSubject::Question { request } => request.free_form_allowed,
            AttentionSubject::WorkflowGate { request } => request.free_form_allowed,
        }
    }

    fn requires_session(&self) -> bool {
        matches!(
            self.subject,
            AttentionSubject::Approval { .. } | AttentionSubject::Question { .. }
        )
    }

    pub fn expired_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_some_and(|expiry| now_ms >= expiry)
    }

    /// Validates a reply against section 4.7: it must bind the session, the
    /// request key and the expiry, and it must select an option the runtime
    /// actually offered.
    pub fn check_reply(
        &self,
        response: &AttentionResponse,
        now_ms: i64,
    ) -> Result<(), ReplyRejection> {
        if response.attention_id != self.id {
            return Err(ReplyRejection::AttentionMismatch);
        }
        if !self.state.is_open() {
            return Err(ReplyRejection::NotOpen);
        }
        let request_key = self.request_key().ok_or(ReplyRejection::NotReplyable)?;
        if self.requires_session() && self.session_id.is_none() {
            return Err(ReplyRejection::SessionRequired);
        }
        if response.session_id != self.session_id {
            return Err(ReplyRejection::SessionMismatch);
        }
        if response.request_key != request_key {
            return Err(ReplyRejection::RequestKeyMismatch);
        }
        if response.expected_expires_at_ms != self.expires_at_ms {
            return Err(ReplyRejection::ExpiryMismatch);
        }
        if self.expired_at(now_ms) {
            return Err(ReplyRejection::Expired);
        }
        if let Some(option_id) = &response.option_id {
            if !self
                .options()
                .iter()
                .any(|option| option.option_id == *option_id)
            {
                return Err(ReplyRejection::UnknownOption);
            }
        }
        if let Some(free_form_ref) = &response.free_form_ref {
            if !self.free_form_allowed() {
                return Err(ReplyRejection::FreeFormNotAllowed);
            }
            if free_form_ref.validate().is_err()
                || free_form_ref.sensitivity != Sensitivity::Sensitive
            {
                return Err(ReplyRejection::InvalidFreeForm);
            }
        }
        if response.option_id.is_none() && response.free_form_ref.is_none() {
            return Err(ReplyRejection::DecisionMissing);
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "attention.id",
            });
        }
        if self.host_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "attention.host_id",
            });
        }
        if self.project_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "attention.project_id",
            });
        }
        for (field, is_empty) in [
            (
                "attention.session_id",
                self.session_id.as_ref().is_some_and(SessionId::is_empty),
            ),
            (
                "attention.turn_id",
                self.turn_id.as_ref().is_some_and(TurnId::is_empty),
            ),
            (
                "attention.workflow_id",
                self.workflow_id.as_ref().is_some_and(WorkflowId::is_empty),
            ),
        ] {
            if is_empty {
                return Err(ContractViolation::EmptyIdentifier { field });
            }
        }
        match &self.subject {
            AttentionSubject::Approval { request } => {
                if self.session_id.is_none() {
                    return Err(ContractViolation::AttentionSessionRequired);
                }
                if request.request_key.is_empty() {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "request_key",
                    });
                }
                if request.target_item_id.is_empty() {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "target_item_id",
                    });
                }
                request
                    .binding_handle
                    .validate_for(ProviderBindingKind::InteractionRequest)?;
                validate_options(&request.options, false)?;
                validate_sensitive(&request.summary_ref, "approval.summary_ref")?;
                if let Some(detail_ref) = &request.detail_ref {
                    validate_sensitive(detail_ref, "approval.detail_ref")?;
                }
                if let JoinState::Joined { item_id } = &request.join {
                    if item_id != &request.target_item_id {
                        return Err(ContractViolation::ApprovalJoinTargetMismatch);
                    }
                }
            }
            AttentionSubject::Question { request } => {
                if self.session_id.is_none() {
                    return Err(ContractViolation::AttentionSessionRequired);
                }
                if request.request_key.is_empty() {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "request_key",
                    });
                }
                request
                    .binding_handle
                    .validate_for(ProviderBindingKind::InteractionRequest)?;
                validate_options(&request.options, request.free_form_allowed)?;
                validate_sensitive(&request.prompt_ref, "question.prompt_ref")?;
            }
            AttentionSubject::WorkflowGate { request } => {
                if self.workflow_id.is_none() {
                    return Err(ContractViolation::AttentionWorkflowRequired);
                }
                if request.request_key.is_empty() {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "request_key",
                    });
                }
                validate_options(&request.options, request.free_form_allowed)?;
                validate_sensitive(&request.prompt_ref, "workflow_gate.prompt_ref")?;
            }
            AttentionSubject::ConnectionFault { .. } => {}
        }
        if let AttentionState::Answered {
            option_id,
            free_form_ref,
            answer_source,
            ..
        } = &self.state
        {
            if option_id.is_none() && free_form_ref.is_none() {
                return Err(ContractViolation::AttentionDecisionMissing);
            }
            if let Some(free_form_ref) = free_form_ref {
                validate_sensitive(free_form_ref, "attention_state.free_form_ref")?;
            }
            match answer_source {
                AttentionAnswerSource::LocalCommand { command_id } => {
                    if command_id.is_empty() {
                        return Err(ContractViolation::EmptyIdentifier {
                            field: "attention_state.answer_source.command_id",
                        });
                    }
                }
                AttentionAnswerSource::ObservedExternal { evidence } => {
                    if evidence.observer_host_id.is_empty() {
                        return Err(ContractViolation::EmptyIdentifier {
                            field: "attention_state.answer_source.evidence.observer_host_id",
                        });
                    }
                    if evidence.observer_host_id != self.host_id {
                        return Err(ContractViolation::AttentionAnswerObserverHostMismatch);
                    }
                }
            }
        }
        Ok(())
    }

    /// Re-evaluates an approval-to-item join as observed items arrive.
    pub fn refresh_approval_join(&mut self, items: &[Item], observation_complete: bool) {
        let session_id = self.session_id.as_ref();
        let turn_id = self.turn_id.as_ref();
        let AttentionSubject::Approval { request } = &mut self.subject else {
            return;
        };
        let mut matches = items
            .iter()
            .filter(|item| item.id == request.target_item_id);
        let first = matches.next();
        let duplicate = matches.next().is_some();
        request.join = match (first, duplicate) {
            (_, true) => JoinState::Unjoined {
                reason: JoinFailureReason::AmbiguousTarget,
            },
            (Some(item), false)
                if session_id == Some(&item.session_id) && turn_id == Some(&item.turn_id) =>
            {
                JoinState::Joined {
                    item_id: item.id.clone(),
                }
            }
            (Some(_), false) => JoinState::Unjoined {
                reason: JoinFailureReason::ScopeMismatch,
            },
            (None, false) if observation_complete => JoinState::Unjoined {
                reason: JoinFailureReason::ItemUnknown,
            },
            (None, false) => JoinState::Unjoined {
                reason: JoinFailureReason::ItemNotYetSeen,
            },
        };
    }
}

impl AttentionResponse {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.attention_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "attention_id",
            });
        }
        if self.request_key.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "request_key",
            });
        }
        if self.session_id.as_ref().is_some_and(SessionId::is_empty) {
            return Err(ContractViolation::EmptyIdentifier {
                field: "session_id",
            });
        }
        if self.option_id.as_ref().is_some_and(String::is_empty) {
            return Err(ContractViolation::EmptyIdentifier { field: "option_id" });
        }
        if self.option_id.is_none() && self.free_form_ref.is_none() {
            return Err(ContractViolation::AttentionDecisionMissing);
        }
        if let Some(free_form_ref) = &self.free_form_ref {
            validate_sensitive(free_form_ref, "attention_response.free_form_ref")?;
        }
        Ok(())
    }
}

fn validate_options(
    options: &[DecisionOption],
    free_form_allowed: bool,
) -> Result<(), ContractViolation> {
    if options.is_empty() && !free_form_allowed {
        return Err(ContractViolation::DecisionOptionsMissing);
    }
    let mut seen = HashSet::new();
    for option in options {
        if option.option_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier { field: "option_id" });
        }
        if !seen.insert(&option.option_id) {
            return Err(ContractViolation::DuplicateDecisionOption {
                option_id: option.option_id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_sensitive(content: &ContentRef, field: &'static str) -> Result<(), ContractViolation> {
    content.validate()?;
    if content.sensitivity != Sensitivity::Sensitive {
        return Err(ContractViolation::SensitiveContentRequired { field });
    }
    Ok(())
}

/// Why a reply to an attention entry was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyRejection {
    AttentionMismatch,
    NotOpen,
    NotReplyable,
    SessionRequired,
    SessionMismatch,
    RequestKeyMismatch,
    ExpiryMismatch,
    Expired,
    UnknownOption,
    FreeFormNotAllowed,
    InvalidFreeForm,
    DecisionMissing,
}
