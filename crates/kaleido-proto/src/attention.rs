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
        question_answers: Vec<QuestionAnswer>,
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
    pub questions: Vec<QuestionPrompt>,
    pub binding_handle: ProviderBindingHandle,
}

/// One prompt in a multi-question elicitation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QuestionPrompt {
    pub question_key: String,
    pub prompt_ref: ContentRef,
    pub options: Vec<DecisionOption>,
    pub multi_select: bool,
    pub free_form_allowed: bool,
}

/// The answer to one prompt in a [`QuestionRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QuestionAnswer {
    pub question_key: String,
    pub option_ids: Vec<String>,
    pub free_form_ref: Option<ContentRef>,
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
    pub question_answers: Vec<QuestionAnswer>,
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
            // This compatibility accessor is only meaningful for callers
            // rendering a single-prompt question. Multi-question consumers
            // must use `QuestionRequest::questions` so no prompt is hidden.
            AttentionSubject::Question { request } => request
                .questions
                .first()
                .map_or(&[], |question| question.options.as_slice()),
            AttentionSubject::WorkflowGate { request } => &request.options,
            AttentionSubject::ConnectionFault { .. } => &[],
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
        match &self.subject {
            AttentionSubject::Question { request } => {
                if response.option_id.is_some() || response.free_form_ref.is_some() {
                    return Err(ReplyRejection::QuestionTopLevelDecision);
                }
                validate_question_answers(request, &response.question_answers)
            }
            AttentionSubject::Approval { request } => {
                if !response.question_answers.is_empty() {
                    return Err(ReplyRejection::QuestionAnswersUnexpected);
                }
                validate_top_level_response(
                    &request.options,
                    false,
                    response.option_id.as_ref(),
                    response.free_form_ref.as_ref(),
                )
            }
            AttentionSubject::WorkflowGate { request } => {
                if !response.question_answers.is_empty() {
                    return Err(ReplyRejection::QuestionAnswersUnexpected);
                }
                validate_top_level_response(
                    &request.options,
                    request.free_form_allowed,
                    response.option_id.as_ref(),
                    response.free_form_ref.as_ref(),
                )
            }
            AttentionSubject::ConnectionFault { .. } => Err(ReplyRejection::NotReplyable),
        }
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
                validate_questions(&request.questions)?;
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
            question_answers,
            answer_source,
            ..
        } = &self.state
        {
            match &self.subject {
                AttentionSubject::Question { request } => {
                    if option_id.is_some() || free_form_ref.is_some() {
                        return Err(ContractViolation::QuestionTopLevelDecision);
                    }
                    validate_question_answers_state(request, question_answers)?;
                }
                AttentionSubject::Approval { request } => {
                    if !question_answers.is_empty() {
                        return Err(ContractViolation::QuestionAnswersUnexpected);
                    }
                    validate_top_level_state(
                        &request.options,
                        false,
                        option_id.as_ref(),
                        free_form_ref.as_ref(),
                    )?;
                }
                AttentionSubject::WorkflowGate { request } => {
                    if !question_answers.is_empty() {
                        return Err(ContractViolation::QuestionAnswersUnexpected);
                    }
                    validate_top_level_state(
                        &request.options,
                        request.free_form_allowed,
                        option_id.as_ref(),
                        free_form_ref.as_ref(),
                    )?;
                }
                AttentionSubject::ConnectionFault { .. } => {
                    return Err(ContractViolation::AttentionDecisionMissing);
                }
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
        if !self.question_answers.is_empty() {
            if self.option_id.is_some() || self.free_form_ref.is_some() {
                return Err(ContractViolation::QuestionTopLevelDecision);
            }
            validate_question_answers_shape(&self.question_answers)
        } else {
            validate_top_level_response(
                &[],
                true,
                self.option_id.as_ref(),
                self.free_form_ref.as_ref(),
            )
            .map_err(|rejection| rejection.contract_violation())
        }
    }
}

impl ReplyRejection {
    fn from_contract_violation(error: ContractViolation) -> Self {
        match error {
            ContractViolation::QuestionAnswersRequired => ReplyRejection::QuestionAnswersRequired,
            ContractViolation::QuestionAnswerEmpty => ReplyRejection::QuestionAnswerEmpty,
            ContractViolation::QuestionAnswerDuplicateKey => {
                ReplyRejection::QuestionAnswerDuplicateKey
            }
            ContractViolation::QuestionAnswerDuplicateOption => {
                ReplyRejection::QuestionAnswerDuplicateOption
            }
            ContractViolation::QuestionAnswerUnknownKey => ReplyRejection::QuestionAnswerUnknownKey,
            ContractViolation::QuestionAnswerUnknownOption => {
                ReplyRejection::QuestionAnswerUnknownOption
            }
            ContractViolation::QuestionAnswerTooManyOptions => {
                ReplyRejection::QuestionAnswerTooManyOptions
            }
            ContractViolation::QuestionAnswerMissing => ReplyRejection::QuestionAnswerMissing,
            ContractViolation::FreeFormNotAllowed => ReplyRejection::FreeFormNotAllowed,
            ContractViolation::InvalidFreeForm => ReplyRejection::InvalidFreeForm,
            ContractViolation::QuestionTopLevelDecision => ReplyRejection::QuestionTopLevelDecision,
            ContractViolation::QuestionAnswersUnexpected => {
                ReplyRejection::QuestionAnswersUnexpected
            }
            _ => ReplyRejection::DecisionMissing,
        }
    }

    fn contract_violation(self) -> ContractViolation {
        match self {
            ReplyRejection::DecisionMissing => ContractViolation::AttentionDecisionMissing,
            ReplyRejection::QuestionAnswersUnexpected => {
                ContractViolation::QuestionAnswersUnexpected
            }
            ReplyRejection::QuestionTopLevelDecision => ContractViolation::QuestionTopLevelDecision,
            ReplyRejection::QuestionAnswerEmpty => ContractViolation::QuestionAnswerEmpty,
            ReplyRejection::QuestionAnswerDuplicateKey => {
                ContractViolation::QuestionAnswerDuplicateKey
            }
            ReplyRejection::QuestionAnswerDuplicateOption => {
                ContractViolation::QuestionAnswerDuplicateOption
            }
            ReplyRejection::QuestionAnswerUnknownOption => {
                ContractViolation::QuestionAnswerUnknownOption
            }
            ReplyRejection::QuestionAnswerTooManyOptions => {
                ContractViolation::QuestionAnswerTooManyOptions
            }
            ReplyRejection::QuestionAnswerUnknownKey => ContractViolation::QuestionAnswerUnknownKey,
            ReplyRejection::QuestionAnswerMissing => ContractViolation::QuestionAnswerMissing,
            ReplyRejection::FreeFormNotAllowed => ContractViolation::FreeFormNotAllowed,
            ReplyRejection::InvalidFreeForm => ContractViolation::InvalidFreeForm,
            ReplyRejection::UnknownOption => ContractViolation::UnknownOption,
            _ => ContractViolation::AttentionDecisionMissing,
        }
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

fn validate_questions(questions: &[QuestionPrompt]) -> Result<(), ContractViolation> {
    if questions.is_empty() {
        return Err(ContractViolation::QuestionSetEmpty);
    }
    let mut seen = HashSet::new();
    for question in questions {
        if question.question_key.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "question_key",
            });
        }
        if !seen.insert(&question.question_key) {
            return Err(ContractViolation::DuplicateQuestionKey {
                question_key: question.question_key.clone(),
            });
        }
        validate_options(&question.options, question.free_form_allowed)?;
        validate_sensitive(&question.prompt_ref, "question.prompt_ref")?;
    }
    Ok(())
}

fn validate_question_answers_shape(answers: &[QuestionAnswer]) -> Result<(), ContractViolation> {
    if answers.is_empty() {
        return Err(ContractViolation::QuestionAnswersRequired);
    }
    let mut keys = HashSet::new();
    for answer in answers {
        if answer.question_key.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "question_answer.question_key",
            });
        }
        if !keys.insert(&answer.question_key) {
            return Err(ContractViolation::QuestionAnswerDuplicateKey);
        }
        validate_answer_options_shape(answer)?;
    }
    Ok(())
}

fn validate_answer_options_shape(answer: &QuestionAnswer) -> Result<(), ContractViolation> {
    let mut options = HashSet::new();
    for option_id in &answer.option_ids {
        if option_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "question_answer.option_id",
            });
        }
        if !options.insert(option_id) {
            return Err(ContractViolation::QuestionAnswerDuplicateOption);
        }
    }
    if answer.option_ids.is_empty() && answer.free_form_ref.is_none() {
        return Err(ContractViolation::QuestionAnswerEmpty);
    }
    if let Some(free_form_ref) = &answer.free_form_ref {
        if free_form_ref.validate().is_err() || free_form_ref.sensitivity != Sensitivity::Sensitive
        {
            return Err(ContractViolation::InvalidFreeForm);
        }
    }
    Ok(())
}

fn validate_question_answers(
    request: &QuestionRequest,
    answers: &[QuestionAnswer],
) -> Result<(), ReplyRejection> {
    validate_question_answers_shape(answers).map_err(ReplyRejection::from_contract_violation)?;
    let mut prompts = HashSet::new();
    for question in &request.questions {
        prompts.insert(question.question_key.as_str());
    }
    for answer in answers {
        if !prompts.contains(answer.question_key.as_str()) {
            return Err(ReplyRejection::QuestionAnswerUnknownKey);
        }
        let question = request
            .questions
            .iter()
            .find(|question| question.question_key == answer.question_key)
            .ok_or(ReplyRejection::QuestionAnswerUnknownKey)?;
        if !question.multi_select && answer.option_ids.len() > 1 {
            return Err(ReplyRejection::QuestionAnswerTooManyOptions);
        }
        for option_id in &answer.option_ids {
            if !question
                .options
                .iter()
                .any(|option| &option.option_id == option_id)
            {
                return Err(ReplyRejection::QuestionAnswerUnknownOption);
            }
        }
        if answer.free_form_ref.is_some() && !question.free_form_allowed {
            return Err(ReplyRejection::FreeFormNotAllowed);
        }
    }
    if answers.len() != request.questions.len() {
        return Err(ReplyRejection::QuestionAnswerMissing);
    }
    Ok(())
}

fn validate_question_answers_state(
    request: &QuestionRequest,
    answers: &[QuestionAnswer],
) -> Result<(), ContractViolation> {
    validate_question_answers(request, answers).map_err(ReplyRejection::contract_violation)
}

fn validate_top_level_response(
    options: &[DecisionOption],
    free_form_allowed: bool,
    option_id: Option<&String>,
    free_form_ref: Option<&ContentRef>,
) -> Result<(), ReplyRejection> {
    if let Some(option_id) = option_id {
        if option_id.is_empty() {
            return Err(ReplyRejection::UnknownOption);
        }
        if !options.is_empty() && !options.iter().any(|option| &option.option_id == option_id) {
            return Err(ReplyRejection::UnknownOption);
        }
    }
    if let Some(free_form_ref) = free_form_ref {
        if !free_form_allowed {
            return Err(ReplyRejection::FreeFormNotAllowed);
        }
        if free_form_ref.validate().is_err() || free_form_ref.sensitivity != Sensitivity::Sensitive
        {
            return Err(ReplyRejection::InvalidFreeForm);
        }
    }
    if option_id.is_none() && free_form_ref.is_none() {
        return Err(ReplyRejection::DecisionMissing);
    }
    Ok(())
}

fn validate_top_level_state(
    options: &[DecisionOption],
    free_form_allowed: bool,
    option_id: Option<&String>,
    free_form_ref: Option<&ContentRef>,
) -> Result<(), ContractViolation> {
    validate_top_level_response(options, free_form_allowed, option_id, free_form_ref)
        .map_err(ReplyRejection::contract_violation)
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
    QuestionAnswersRequired,
    QuestionAnswersUnexpected,
    QuestionTopLevelDecision,
    QuestionAnswerEmpty,
    QuestionAnswerDuplicateKey,
    QuestionAnswerDuplicateOption,
    QuestionAnswerUnknownKey,
    QuestionAnswerUnknownOption,
    QuestionAnswerTooManyOptions,
    QuestionAnswerMissing,
}
