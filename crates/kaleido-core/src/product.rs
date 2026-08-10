//! Mobile product helpers that keep command, content and capability semantics in Rust.

use std::collections::HashSet;

use kaleido_proto::attention::{
    AttentionItem, AttentionResponse, AttentionSubject, QuestionAnswer,
};
use kaleido_proto::capability::{Capability, CapabilityState};
use kaleido_proto::command::{Command, CommandAck, DeviceCommandRequest};
use kaleido_proto::content::{
    ContentKind, ContentReadRequest, ContentReadResponse, ContentRef, ContentUnavailableReason,
    ContentWriteRequest, ContentWriteResponse, MAX_CONTENT_READ_BYTES, MAX_CONTENT_WRITE_BYTES,
};
use kaleido_proto::ids::{SessionId, TurnId};
use kaleido_proto::projection::{InputQueueView, RuntimeCapabilityView, SessionSummary};
use kaleido_proto::queue::QueueIntent;
use kaleido_proto::session::LiveBinding;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::mobile::{MobileClient, MobileClientError};

const MOBILE_TEXT_RENDER_LIMIT: u64 = 256 * 1024;

/// User intent evaluated against canonical live/capability/queue facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MobileSessionAction {
    SubmitPrompt,
    EnqueueNewTurn,
    EnqueueSteer,
    ResumeSession,
    InterruptTurn,
}

/// Closed, provider-neutral reason a mobile action is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MobileActionBlocker {
    SessionNotLive,
    RuntimeCapabilityMissing,
    CapabilityUnsupported,
    CapabilityUnavailable,
    CapabilityNotVerified,
    CapabilityUpstreamBlocked,
    QueueUnavailable,
    AttentionNotReplyable,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MobileActionAvailability {
    pub enabled: bool,
    pub blocker: Option<MobileActionBlocker>,
}

/// Body text is deliberately returned ephemerally and is never written to the
/// projection cache. Oversized and unavailable bodies remain explicit states.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MobileTextContent {
    Available { text: String },
    Unavailable { reason: ContentUnavailableReason },
    TooLarge { byte_len: u64 },
}

/// Text entered for one canonical question. The broker uploads `free_form`
/// and turns it into the shared `QuestionAnswer::free_form_ref` before the
/// command is sent; UI code never constructs content references itself.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MobileQuestionAnswer {
    pub question_key: String,
    pub option_ids: Vec<String>,
    pub free_form: Option<String>,
}

#[uniffi::export]
pub fn mobile_session_action_availability(
    session: SessionSummary,
    queue: Option<InputQueueView>,
    capabilities: Option<RuntimeCapabilityView>,
    action: MobileSessionAction,
) -> MobileActionAvailability {
    match action {
        MobileSessionAction::SubmitPrompt => {
            let Some(runtime_id) = live_runtime_id(&session.live_binding) else {
                return blocked(MobileActionBlocker::SessionNotLive);
            };
            let Some(capabilities) = capabilities.filter(|view| &view.runtime_id == runtime_id)
            else {
                return blocked(MobileActionBlocker::RuntimeCapabilityMissing);
            };
            capability_availability(&capabilities, Capability::TurnPrompt)
        }
        MobileSessionAction::EnqueueNewTurn | MobileSessionAction::EnqueueSteer => {
            match queue.filter(|view| view.session_id == session.session_id && view.writable) {
                Some(_) => available(),
                None => blocked(MobileActionBlocker::QueueUnavailable),
            }
        }
        MobileSessionAction::ResumeSession => {
            let Some(capabilities) = capabilities else {
                return blocked(MobileActionBlocker::RuntimeCapabilityMissing);
            };
            capability_availability(&capabilities, Capability::HistoryResume)
        }
        MobileSessionAction::InterruptTurn => {
            let Some(runtime_id) = live_runtime_id(&session.live_binding) else {
                return blocked(MobileActionBlocker::SessionNotLive);
            };
            let Some(capabilities) = capabilities.filter(|view| &view.runtime_id == runtime_id)
            else {
                return blocked(MobileActionBlocker::RuntimeCapabilityMissing);
            };
            capability_availability(&capabilities, Capability::TurnInterrupt)
        }
    }
}

#[uniffi::export]
pub fn mobile_attention_action_availability(
    attention: AttentionItem,
    session: Option<SessionSummary>,
    capabilities: Option<RuntimeCapabilityView>,
) -> MobileActionAvailability {
    let required = match attention.subject {
        AttentionSubject::Approval { .. } => Capability::InteractionApproval,
        AttentionSubject::Question { .. } => Capability::InteractionQuestion,
        AttentionSubject::WorkflowGate { .. } => Capability::WorkflowParticipate,
        AttentionSubject::ConnectionFault { .. } => {
            return blocked(MobileActionBlocker::AttentionNotReplyable);
        }
    };
    let Some(session) =
        session.filter(|candidate| attention.session_id.as_ref() == Some(&candidate.session_id))
    else {
        return blocked(MobileActionBlocker::SessionNotLive);
    };
    let Some(runtime_id) = live_runtime_id(&session.live_binding) else {
        return blocked(MobileActionBlocker::SessionNotLive);
    };
    let Some(capabilities) = capabilities.filter(|view| &view.runtime_id == runtime_id) else {
        return blocked(MobileActionBlocker::RuntimeCapabilityMissing);
    };
    capability_availability(&capabilities, required)
}

#[uniffi::export]
impl MobileClient {
    /// Generates a cryptographically random idempotency key. UI code may retain
    /// it across a retry, but never invents command identity or timestamps.
    pub fn create_action_id(&self) -> Result<String, MobileClientError> {
        let mut random = [0_u8; 16];
        OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| MobileClientError::Storage)?;
        let mut encoded = String::with_capacity(39);
        encoded.push_str("mobile-");
        for byte in random {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        Ok(encoded)
    }

    pub fn submit_prompt_text(
        &self,
        session_id: SessionId,
        text: String,
        idempotency_key: String,
    ) -> Result<CommandAck, MobileClientError> {
        let body = self.upload_sensitive_text(text)?;
        self.submit_mobile_command(idempotency_key, Command::SubmitPrompt { session_id, body })
    }

    pub fn enqueue_text(
        &self,
        session_id: SessionId,
        text: String,
        intent: QueueIntent,
        idempotency_key: String,
    ) -> Result<CommandAck, MobileClientError> {
        let body = self.upload_sensitive_text(text)?;
        self.submit_mobile_command(
            idempotency_key,
            Command::EnqueueInput {
                session_id,
                body,
                intent,
            },
        )
    }

    pub fn resume_session(
        &self,
        session_id: SessionId,
        idempotency_key: String,
    ) -> Result<CommandAck, MobileClientError> {
        self.submit_mobile_command(idempotency_key, Command::ResumeSession { session_id })
    }

    pub fn interrupt_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        idempotency_key: String,
    ) -> Result<CommandAck, MobileClientError> {
        self.submit_mobile_command(
            idempotency_key,
            Command::InterruptTurn {
                session_id,
                turn_id,
            },
        )
    }

    pub fn respond_attention_text(
        &self,
        attention: AttentionItem,
        option_id: Option<String>,
        free_form: Option<String>,
        idempotency_key: String,
    ) -> Result<CommandAck, MobileClientError> {
        if matches!(&attention.subject, AttentionSubject::Question { .. }) {
            return Err(MobileClientError::Contract);
        }
        let request_key = attention
            .request_key()
            .ok_or(MobileClientError::Contract)?
            .to_owned();
        if let Some(option) = &option_id {
            if !attention
                .options()
                .iter()
                .any(|candidate| &candidate.option_id == option)
            {
                return Err(MobileClientError::Contract);
            }
        }
        let free_form_allowed = match &attention.subject {
            AttentionSubject::Question { .. } => false,
            AttentionSubject::WorkflowGate { request } => request.free_form_allowed,
            AttentionSubject::Approval { .. } | AttentionSubject::ConnectionFault { .. } => false,
        };
        let free_form_ref = match free_form {
            Some(text) if free_form_allowed => Some(self.upload_sensitive_text(text)?),
            Some(_) => return Err(MobileClientError::Contract),
            None => None,
        };
        let response = AttentionResponse {
            attention_id: attention.id,
            session_id: attention.session_id,
            request_key,
            expected_expires_at_ms: attention.expires_at_ms,
            option_id,
            free_form_ref,
            question_answers: Vec::new(),
        };
        response
            .validate()
            .map_err(|_| MobileClientError::Contract)?;
        self.submit_mobile_command(idempotency_key, Command::RespondAttention { response })
    }

    /// Uploads every free-form body and submits a complete question set in one
    /// broker command. Questions are keyed by the canonical `question_key`, so
    /// this path is provider-neutral and cannot silently answer the wrong
    /// prompt when a runtime reorders its questions.
    pub fn respond_question_text(
        &self,
        attention: AttentionItem,
        answers: Vec<MobileQuestionAnswer>,
        idempotency_key: String,
    ) -> Result<CommandAck, MobileClientError> {
        let AttentionSubject::Question { request } = &attention.subject else {
            return Err(MobileClientError::Contract);
        };
        if answers.len() != request.questions.len() {
            return Err(MobileClientError::Contract);
        }
        let mut seen = HashSet::new();
        let mut question_answers = Vec::with_capacity(answers.len());
        for answer in answers {
            if !seen.insert(answer.question_key.clone()) {
                return Err(MobileClientError::Contract);
            }
            let question = request
                .questions
                .iter()
                .find(|question| question.question_key == answer.question_key)
                .ok_or(MobileClientError::Contract)?;
            if answer.option_ids.is_empty() && answer.free_form.is_none() {
                return Err(MobileClientError::Contract);
            }
            if answer
                .free_form
                .as_deref()
                .is_some_and(|text| text.trim().is_empty())
            {
                return Err(MobileClientError::Contract);
            }
            if !question.multi_select && answer.option_ids.len() > 1 {
                return Err(MobileClientError::Contract);
            }
            let mut option_ids = HashSet::new();
            for option_id in &answer.option_ids {
                if option_id.is_empty()
                    || !option_ids.insert(option_id)
                    || !question
                        .options
                        .iter()
                        .any(|option| &option.option_id == option_id)
                {
                    return Err(MobileClientError::Contract);
                }
            }
            let free_form_ref = match answer.free_form {
                Some(text) if question.free_form_allowed => Some(self.upload_sensitive_text(text)?),
                Some(_) => return Err(MobileClientError::Contract),
                None => None,
            };
            question_answers.push(QuestionAnswer {
                question_key: answer.question_key,
                option_ids: answer.option_ids,
                free_form_ref,
            });
        }
        let request_key = attention
            .request_key()
            .ok_or(MobileClientError::Contract)?
            .to_owned();
        let response = AttentionResponse {
            attention_id: attention.id,
            session_id: attention.session_id,
            request_key,
            expected_expires_at_ms: attention.expires_at_ms,
            option_id: None,
            free_form_ref: None,
            question_answers,
        };
        response
            .validate()
            .map_err(|_| MobileClientError::Contract)?;
        self.submit_mobile_command(idempotency_key, Command::RespondAttention { response })
    }

    pub fn read_text_content(
        &self,
        content: ContentRef,
    ) -> Result<MobileTextContent, MobileClientError> {
        content
            .validate()
            .map_err(|_| MobileClientError::Contract)?;
        if content.byte_len > MOBILE_TEXT_RENDER_LIMIT {
            return Ok(MobileTextContent::TooLarge {
                byte_len: content.byte_len,
            });
        }
        if !content.body_is_retrievable() {
            return Ok(MobileTextContent::Unavailable {
                reason: match content.availability {
                    kaleido_proto::content::ContentAvailability::Evicted => {
                        ContentUnavailableReason::Evicted
                    }
                    _ => ContentUnavailableReason::NeverStored,
                },
            });
        }
        let capacity =
            usize::try_from(content.byte_len).map_err(|_| MobileClientError::Contract)?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut offset = 0_u64;
        loop {
            let remaining = content
                .byte_len
                .checked_sub(offset)
                .ok_or(MobileClientError::Contract)?;
            let max_bytes = u32::try_from(remaining.min(u64::from(MAX_CONTENT_READ_BYTES)))
                .map_err(|_| MobileClientError::Contract)?;
            if max_bytes == 0 {
                return Err(MobileClientError::Contract);
            }
            let response = self.read_content(ContentReadRequest {
                content_id: content.content_id.clone(),
                offset,
                max_bytes,
            })?;
            response
                .validate()
                .map_err(|_| MobileClientError::Contract)?;
            match response {
                ContentReadResponse::Unavailable { content_id, reason } => {
                    if content_id != content.content_id {
                        return Err(MobileClientError::Contract);
                    }
                    return Ok(MobileTextContent::Unavailable { reason });
                }
                ContentReadResponse::Chunk { chunk } => {
                    if chunk.content_id != content.content_id
                        || chunk.offset != offset
                        || chunk.digest != content.digest
                    {
                        return Err(MobileClientError::Contract);
                    }
                    bytes.extend_from_slice(&chunk.bytes);
                    if chunk.eof {
                        break;
                    }
                    offset = checked_next_offset(
                        offset,
                        chunk.next_offset.ok_or(MobileClientError::Contract)?,
                        content.byte_len,
                    )?;
                }
            }
        }
        if u64::try_from(bytes.len()).ok() != Some(content.byte_len)
            || digest(&bytes) != content.digest
        {
            return Err(MobileClientError::Contract);
        }
        let text = String::from_utf8(bytes).map_err(|_| MobileClientError::Contract)?;
        Ok(MobileTextContent::Available { text })
    }
}

impl MobileClient {
    fn upload_sensitive_text(&self, text: String) -> Result<ContentRef, MobileClientError> {
        let mut bytes = text.into_bytes();
        let byte_len = u64::try_from(bytes.len()).map_err(|_| MobileClientError::Contract)?;
        if !(1..=MAX_CONTENT_WRITE_BYTES).contains(&byte_len) {
            bytes.fill(0);
            return Err(MobileClientError::Contract);
        }
        let request = ContentWriteRequest {
            content_kind: ContentKind::PlainText,
            byte_len,
            digest: digest(&bytes),
        };
        let response = self.write_content(request.clone(), bytes)?;
        response
            .validate_for(&request)
            .map_err(|_| MobileClientError::Contract)?;
        match response {
            ContentWriteResponse::Stored { content_ref } => Ok(content_ref),
            ContentWriteResponse::Rejected { .. } => Err(MobileClientError::RemoteRejected),
        }
    }

    fn submit_mobile_command(
        &self,
        idempotency_key: String,
        body: Command,
    ) -> Result<CommandAck, MobileClientError> {
        let request = DeviceCommandRequest {
            idempotency_key,
            ttl_ms: None,
            body,
        };
        request
            .validate()
            .map_err(|_| MobileClientError::Contract)?;
        let ack = self.submit_command(request)?;
        ack.validate().map_err(|_| MobileClientError::Contract)?;
        Ok(ack)
    }
}

fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn checked_next_offset(current: u64, next: u64, total: u64) -> Result<u64, MobileClientError> {
    if next <= current || next > total {
        Err(MobileClientError::Contract)
    } else {
        Ok(next)
    }
}

fn live_runtime_id(binding: &LiveBinding) -> Option<&kaleido_proto::ids::ProviderRuntimeId> {
    match binding {
        LiveBinding::Observing { runtime_id, .. } | LiveBinding::Controlling { runtime_id, .. } => {
            Some(runtime_id)
        }
        LiveBinding::NotBound { .. } | LiveBinding::Blocked { .. } => None,
    }
}

fn capability_availability(
    capabilities: &RuntimeCapabilityView,
    required: Capability,
) -> MobileActionAvailability {
    match capabilities
        .entries
        .iter()
        .find(|entry| entry.capability == required)
        .map(|entry| &entry.state)
    {
        Some(CapabilityState::Supported) => available(),
        Some(CapabilityState::Unsupported) => blocked(MobileActionBlocker::CapabilityUnsupported),
        Some(CapabilityState::UnavailableOnThisConnection { .. }) => {
            blocked(MobileActionBlocker::CapabilityUnavailable)
        }
        Some(CapabilityState::UpstreamBlocked { .. }) => {
            blocked(MobileActionBlocker::CapabilityUpstreamBlocked)
        }
        Some(CapabilityState::NotVerified) | None => {
            blocked(MobileActionBlocker::CapabilityNotVerified)
        }
    }
}

fn available() -> MobileActionAvailability {
    MobileActionAvailability {
        enabled: true,
        blocker: None,
    }
}

fn blocked(blocker: MobileActionBlocker) -> MobileActionAvailability {
    MobileActionAvailability {
        enabled: false,
        blocker: Some(blocker),
    }
}

#[cfg(test)]
mod tests {
    use kaleido_proto::capability::{
        CapabilityEntry, CapabilityEvidence, CapabilityState, EvidenceSource,
    };
    use kaleido_proto::content::{ContentAvailability, ContentKind, ContentRef, Sensitivity};
    use kaleido_proto::ids::{ContentId, ProjectBindingId, ProviderRuntimeId, SessionId};
    use kaleido_proto::projection::{InputQueueView, RuntimeCapabilityView, SessionSummary};
    use kaleido_proto::session::{LiveBinding, OwnershipMode, SessionStatus};

    use super::{
        checked_next_offset, digest, mobile_session_action_availability, MobileActionBlocker,
        MobileClientError, MobileSessionAction,
    };

    fn session(live: bool) -> SessionSummary {
        let runtime_id = ProviderRuntimeId::new("runtime-a");
        SessionSummary {
            session_id: SessionId::new("session-a"),
            project_binding_id: ProjectBindingId::new("binding-a"),
            title: None,
            status: SessionStatus::Idle,
            ownership: OwnershipMode::BrokerManaged,
            live_binding: if live {
                LiveBinding::Observing {
                    runtime_id,
                    since_at_ms: 1,
                    evidence: CapabilityEvidence {
                        source: EvidenceSource::ObservedInTraffic,
                        observed_at_ms: 1,
                        note_ref: None,
                    },
                }
            } else {
                LiveBinding::NotBound {
                    reason: kaleido_proto::session::LiveUnboundReason::RuntimeExited,
                }
            },
            queue_depth: 0,
            open_attention_count: 0,
            last_activity_at_ms: 1,
        }
    }

    fn capabilities(state: CapabilityState) -> RuntimeCapabilityView {
        capability_view(kaleido_proto::capability::Capability::TurnPrompt, state)
    }

    fn capability_view(
        capability: kaleido_proto::capability::Capability,
        state: CapabilityState,
    ) -> RuntimeCapabilityView {
        RuntimeCapabilityView {
            host_id: kaleido_proto::ids::HostId::new("host-a"),
            runtime_id: ProviderRuntimeId::new("runtime-a"),
            negotiated_at_ms: 1,
            entries: vec![CapabilityEntry {
                capability,
                state,
                evidence: CapabilityEvidence {
                    source: EvidenceSource::ObservedInTraffic,
                    observed_at_ms: 1,
                    note_ref: None,
                },
            }],
        }
    }

    #[test]
    fn prompt_requires_live_session_and_exact_supported_capability() {
        let enabled = mobile_session_action_availability(
            session(true),
            None,
            Some(capabilities(CapabilityState::Supported)),
            MobileSessionAction::SubmitPrompt,
        );
        assert!(enabled.enabled);

        let unverified = mobile_session_action_availability(
            session(true),
            None,
            Some(capabilities(CapabilityState::NotVerified)),
            MobileSessionAction::SubmitPrompt,
        );
        assert_eq!(
            unverified.blocker,
            Some(MobileActionBlocker::CapabilityNotVerified)
        );

        let offline = mobile_session_action_availability(
            session(false),
            None,
            Some(capabilities(CapabilityState::Supported)),
            MobileSessionAction::SubmitPrompt,
        );
        assert_eq!(offline.blocker, Some(MobileActionBlocker::SessionNotLive));
    }

    #[test]
    fn queue_intent_uses_broker_writable_projection_not_steer_guessing() {
        let session = session(false);
        let queue = InputQueueView {
            session_id: session.session_id.clone(),
            entries: Vec::new(),
            writable: true,
            steer_supported: false,
        };
        for action in [
            MobileSessionAction::EnqueueNewTurn,
            MobileSessionAction::EnqueueSteer,
        ] {
            assert!(
                mobile_session_action_availability(
                    session.clone(),
                    Some(queue.clone()),
                    None,
                    action
                )
                .enabled
            );
        }
    }

    #[test]
    fn resume_and_interrupt_are_driven_by_their_exact_capabilities() {
        let resume = mobile_session_action_availability(
            session(false),
            None,
            Some(capability_view(
                kaleido_proto::capability::Capability::HistoryResume,
                CapabilityState::Supported,
            )),
            MobileSessionAction::ResumeSession,
        );
        assert!(resume.enabled);

        let interrupt = mobile_session_action_availability(
            session(true),
            None,
            Some(capability_view(
                kaleido_proto::capability::Capability::TurnInterrupt,
                CapabilityState::Supported,
            )),
            MobileSessionAction::InterruptTurn,
        );
        assert!(interrupt.enabled);

        let offline_interrupt = mobile_session_action_availability(
            session(false),
            None,
            Some(capability_view(
                kaleido_proto::capability::Capability::TurnInterrupt,
                CapabilityState::Supported,
            )),
            MobileSessionAction::InterruptTurn,
        );
        assert_eq!(
            offline_interrupt.blocker,
            Some(MobileActionBlocker::SessionNotLive)
        );
    }

    #[test]
    fn digest_is_canonical_and_sensitive_reference_shape_is_not_invented_in_kotlin() {
        assert_eq!(
            digest(b"hello"),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let reference = ContentRef {
            content_id: ContentId::new("content-a"),
            kind: ContentKind::PlainText,
            byte_len: 5,
            digest: digest(b"hello"),
            preview: None,
            sensitivity: Sensitivity::Sensitive,
            availability: ContentAvailability::Stored,
        };
        assert!(reference.validate().is_ok());
    }

    #[test]
    fn content_reader_rejects_a_zero_progress_or_out_of_range_chunk() {
        assert_eq!(checked_next_offset(0, 1, 2), Ok(1));
        assert_eq!(
            checked_next_offset(1, 1, 2),
            Err(MobileClientError::Contract)
        );
        assert_eq!(
            checked_next_offset(1, 3, 2),
            Err(MobileClientError::Contract)
        );
    }
}
