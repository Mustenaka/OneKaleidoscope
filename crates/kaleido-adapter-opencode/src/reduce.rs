use std::collections::{BTreeMap, BTreeSet};

use kaleido_adapter::{
    capability::CapabilityProbe,
    content::{store_sensitive_text, ContentAccess},
    IdentityMint,
};
use kaleido_proto::{
    attention::{
        ApprovalRequest, AttentionAnswerEvidence, AttentionAnswerEvidenceSource, AttentionItem,
        AttentionState, AttentionSubject, DecisionOption, DecisionSemantics, JoinFailureReason,
        JoinState, QuestionPrompt, QuestionRequest,
    },
    capability::{Capability, CapabilityEvidence, CapabilityUnavailableReason, EvidenceSource},
    command::{CommandAck, CommandOutcome, RuntimeAcceptanceKind},
    content::{ContentKind, ContentRef, Sensitivity},
    effect::StateEffect,
    host::{
        ConnectionState, Host, HostPlatform, HostReachability, LaunchSurface, Project,
        ProjectBinding, ProviderFamily, ProviderRuntime, SessionCounts,
    },
    ids::{
        AttentionId, CommandId, HostId, ProjectBindingId, ProjectId, ProviderBindingKind,
        ProviderRuntimeId, SessionId, TurnId,
    },
    session::{
        HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason, OwnershipMode, Session,
        SessionStatus,
    },
    turn::{
        Item, ItemBody, ItemStatus, MessagePhase, ToolDescriptor, ToolSurface, Turn, TurnOrigin,
        TurnStatus,
    },
};
use serde_json::{Map, Value};

use crate::{bindings::BindingStore, error::OpenCodeDecodeError};

/// Reducer configuration.  The project directory is sensitive and is stored
/// through `ContentAccess` before it reaches a canonical effect.
#[derive(Debug, Clone)]
pub struct ReducerConfig {
    pub host_display_name: String,
    pub host_platform: HostPlatform,
    pub project_display_name: String,
    pub project_directory: String,
    pub identity_salt: String,
    pub evidence: EvidenceSource,
    pub base_at_ms: i64,
    pub runtime_version_label: Option<String>,
}

/// A normalized event discriminator.  The payload itself is decoded and
/// reduced immediately; no provider JSON crosses this public boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalEvent {
    ServerConnected,
    Ignored {
        event_type: String,
        session_id: Option<SessionId>,
    },
    SessionUpsert {
        session_id: SessionId,
    },
    MessageUpsert {
        session_id: SessionId,
        message_id: String,
    },
    PartUpsert {
        session_id: SessionId,
        part_id: String,
    },
    Status {
        session_id: SessionId,
        status: SessionStatus,
    },
    SessionDiff {
        session_id: SessionId,
    },
    PermissionAsked {
        session_id: SessionId,
        attention_id: AttentionId,
    },
    PermissionAnswered {
        session_id: SessionId,
        attention_id: AttentionId,
    },
    QuestionAsked {
        session_id: SessionId,
        attention_id: AttentionId,
    },
    QuestionAnswered {
        session_id: SessionId,
        attention_id: AttentionId,
    },
}

impl CanonicalEvent {
    pub(crate) fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::ServerConnected => None,
            Self::Ignored { session_id, .. } => session_id.as_ref(),
            Self::SessionUpsert { session_id }
            | Self::MessageUpsert { session_id, .. }
            | Self::PartUpsert { session_id, .. }
            | Self::Status { session_id, .. }
            | Self::SessionDiff { session_id }
            | Self::PermissionAsked { session_id, .. }
            | Self::PermissionAnswered { session_id, .. }
            | Self::QuestionAsked { session_id, .. }
            | Self::QuestionAnswered { session_id, .. } => Some(session_id),
        }
    }
}

#[derive(Debug)]
pub struct OpenCodeReducer {
    config: ReducerConfig,
    mint: IdentityMint,
    host_id: HostId,
    runtime_id: ProviderRuntimeId,
    project_id: ProjectId,
    project_binding_id: ProjectBindingId,
    bindings: BindingStore,
    sessions: BTreeMap<String, Session>,
    turns: BTreeMap<String, Turn>,
    items: BTreeMap<String, Item>,
    part_text: BTreeMap<String, String>,
    attentions: BTreeMap<String, AttentionItem>,
    local_attention_commands: BTreeMap<String, CommandId>,
    controlled_sessions: BTreeSet<SessionId>,
    seen_events: BTreeMap<String, CanonicalEvent>,
    capabilities: CapabilityProbe,
    bootstrapped: bool,
    next_sequence: u64,
}

impl OpenCodeReducer {
    pub fn new(config: ReducerConfig) -> Self {
        let mint = IdentityMint::new(config.identity_salt.clone());
        let host_id = mint.host_id(&config.host_display_name);
        let runtime_id = mint.runtime_id(&format!("{}|opencode-server", config.host_display_name));
        let project_id = mint.project_id(&config.project_display_name);
        let project_binding_id =
            mint.project_binding_id(&format!("{}|{}", config.project_display_name, runtime_id));
        let capabilities =
            CapabilityProbe::new(runtime_id.clone(), config.base_at_ms, config.evidence);
        Self {
            config,
            mint,
            host_id,
            runtime_id,
            project_id,
            project_binding_id,
            bindings: BindingStore::default(),
            sessions: BTreeMap::new(),
            turns: BTreeMap::new(),
            items: BTreeMap::new(),
            part_text: BTreeMap::new(),
            attentions: BTreeMap::new(),
            local_attention_commands: BTreeMap::new(),
            controlled_sessions: BTreeSet::new(),
            seen_events: BTreeMap::new(),
            capabilities,
            bootstrapped: false,
            next_sequence: 0,
        }
    }

    pub fn runtime_id(&self) -> &ProviderRuntimeId {
        &self.runtime_id
    }

    pub fn host_id(&self) -> &HostId {
        &self.host_id
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn project_binding_id(&self) -> &ProjectBindingId {
        &self.project_binding_id
    }

    pub fn session_id(&self, raw_id: &str) -> Option<&SessionId> {
        self.sessions.get(raw_id).map(|session| &session.id)
    }

    pub(crate) fn raw_session_id(&self, session_id: &SessionId) -> Option<&str> {
        self.sessions
            .iter()
            .find_map(|(raw, session)| (&session.id == session_id).then_some(raw.as_str()))
    }

    pub fn capability_probe(&self) -> CapabilityProbe {
        self.capabilities.clone()
    }

    pub(crate) fn capabilities_prove(&mut self, capability: Capability) {
        self.capabilities.prove(capability);
    }

    pub(crate) fn accepted_command_effect(
        &mut self,
        session_id: &SessionId,
        acceptance_kind: RuntimeAcceptanceKind,
        command_id: &CommandId,
        receipt_key: &str,
        at_ms: i64,
    ) -> StateEffect {
        let outcome = CommandOutcome::AcceptedByRuntime {
            session_id: session_id.clone(),
            acceptance_kind,
            binding_handle: self.mint.binding_handle(
                &self.runtime_id,
                ProviderBindingKind::RuntimeAcknowledgement,
                receipt_key,
            ),
        };
        self.controlled_sessions.insert(session_id.clone());
        let _ = self.capabilities.observe_runtime_acceptance(&outcome);
        StateEffect::CommandAcknowledged {
            ack: CommandAck {
                command_id: command_id.clone(),
                outcome,
                acked_at_ms: at_ms,
            },
        }
    }

    pub(crate) fn admit_prompt_turn(
        &mut self,
        raw_session: &str,
        raw_message: &str,
        command_id: &CommandId,
    ) -> Result<StateEffect, OpenCodeDecodeError> {
        let session_id = self
            .session_id(raw_session)
            .cloned()
            .ok_or(OpenCodeDecodeError::ScopeMismatch)?;
        let (turn_id, binding_handle, _) =
            self.bindings
                .turn(&self.mint, &self.runtime_id, raw_message, &session_id);
        let turn = Turn {
            id: turn_id,
            session_id,
            status: TurnStatus::Pending,
            origin: TurnOrigin::RemoteCommand {
                command_id: command_id.clone(),
            },
            started_at_ms: None,
            completed_at_ms: None,
            item_ids: Vec::new(),
            error: None,
            binding_handle: Some(binding_handle),
        };
        self.turns.insert(raw_message.to_owned(), turn.clone());
        Ok(StateEffect::TurnUpserted { turn })
    }

    pub(crate) fn is_active_turn(&self, raw_session: &str, turn_id: &TurnId) -> bool {
        self.sessions
            .get(raw_session)
            .and_then(|session| session.active_turn_id.as_ref())
            == Some(turn_id)
    }

    pub(crate) fn raw_interaction(&self, attention_id: &AttentionId) -> Option<(&str, &str, bool)> {
        self.attentions.iter().find_map(|(raw_request, item)| {
            if &item.id != attention_id {
                return None;
            }
            let raw_session = item.session_id.as_ref().and_then(|session_id| {
                self.sessions
                    .iter()
                    .find(|(_, session)| &session.id == session_id)
                    .map(|(raw, _)| raw.as_str())
            })?;
            let question = matches!(item.subject, AttentionSubject::Question { .. });
            Some((raw_session, raw_request.as_str(), question))
        })
    }

    pub(crate) fn attention(&self, attention_id: &AttentionId) -> Option<&AttentionItem> {
        self.attentions
            .values()
            .find(|item| &item.id == attention_id)
    }

    pub(crate) fn register_local_attention_answer(
        &mut self,
        attention_id: &AttentionId,
        command_id: &CommandId,
    ) -> Option<(String, String, bool)> {
        let (raw_session, raw_request, question) = self.raw_interaction(attention_id)?;
        if !self.attentions.get(raw_request)?.state.is_open()
            || self.local_attention_commands.contains_key(raw_request)
        {
            return None;
        }
        let result = (raw_session.to_owned(), raw_request.to_owned(), question);
        self.local_attention_commands
            .insert(result.1.clone(), command_id.clone());
        Some(result)
    }

    pub(crate) fn forget_local_attention_answer(&mut self, raw_request: &str) {
        self.local_attention_commands.remove(raw_request);
    }

    /// Decode only the declared event families.  Unknown labels fail closed;
    /// they never become an agent message by accident.
    pub fn decode_event(&mut self, bytes: &[u8]) -> Result<(String, Value), OpenCodeDecodeError> {
        let value: Value = serde_json::from_slice(bytes)?;
        let object = value
            .as_object()
            .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
        let event_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or(OpenCodeDecodeError::MissingEventType)?;
        if !is_supported_event(event_type) {
            return Err(OpenCodeDecodeError::UnknownEventType(event_type.to_owned()));
        }
        validate_generated_event(event_type, &value)?;
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
        Ok((event_type.to_owned(), Value::Object(properties.clone())))
    }

    /// Reduce one structured SSE event.  A session scope, when supplied, is
    /// checked before any state transition to prevent cross-project leakage.
    pub fn reduce_sse_event(
        &mut self,
        bytes: &[u8],
        expected_session: Option<&str>,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<(CanonicalEvent, Vec<StateEffect>), OpenCodeDecodeError> {
        let event_id = serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned));
        if let Some(observed) = event_id
            .as_ref()
            .and_then(|event_id| self.seen_events.get(event_id))
        {
            return Ok((observed.clone(), Vec::new()));
        }
        let (event_type, properties) = self.decode_event(bytes)?;
        let reduced = self
            .reduce_properties(&event_type, &properties, expected_session, at_ms, content)
            .map_err(|error| match error {
                ReduceError::Decode(error) => error,
                ReduceError::Content(error) => {
                    let _ = error;
                    OpenCodeDecodeError::UnsupportedShape
                }
            })?;
        if let Some(event_id) = event_id {
            self.seen_events.insert(event_id, reduced.0.clone());
        }
        Ok(reduced)
    }

    /// Apply one REST session snapshot and its persisted messages.  A snapshot
    /// proves history only; live observation is not claimed until SSE traffic
    /// is decoded on this connection.
    pub fn reduce_snapshot(
        &mut self,
        sessions: &[Value],
        messages: &[(String, Vec<Value>)],
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, OpenCodeDecodeError> {
        let mut effects = self
            .ensure_bootstrapped(at_ms, content)
            .map_err(|_| OpenCodeDecodeError::UnsupportedShape)?;
        self.capabilities.prove(Capability::HistoryList);
        self.capabilities.prove(Capability::HistoryRead);
        self.capabilities.prove(Capability::HistoryResume);
        for session in sessions {
            effects.extend(
                self.reduce_session_value(session, at_ms, content)
                    .map_err(|_| OpenCodeDecodeError::UnsupportedShape)?,
            );
        }
        for (raw_session, session_messages) in messages {
            if self.sessions.contains_key(raw_session) {
                for message in session_messages {
                    effects.extend(
                        self.reduce_message_value(message, Some(raw_session), at_ms, content)
                            .map_err(|_| OpenCodeDecodeError::UnsupportedShape)?,
                    );
                }
            } else {
                return Err(OpenCodeDecodeError::ScopeMismatch);
            }
        }
        effects.push(StateEffect::CapabilitiesUpdated {
            capabilities: self.capabilities.to_capabilities(),
        });
        Ok(effects)
    }

    /// Mark the connection as observing only after an actual SSE payload was
    /// accepted.  This intentionally does not promote control capabilities.
    pub fn mark_live_observed(&mut self, session_id: &SessionId, at_ms: i64) -> Vec<StateEffect> {
        self.capabilities.prove(Capability::LiveObserve);
        self.refresh_live_binding(session_id, at_ms)
    }

    pub(crate) fn reset_connection(&mut self, at_ms: i64) {
        self.capabilities.reset_connection(at_ms);
        self.controlled_sessions.clear();
    }

    pub(crate) fn mark_connection_unavailable(
        &mut self,
        reason: CapabilityUnavailableReason,
        at_ms: i64,
    ) -> Vec<StateEffect> {
        self.capabilities.advance_observation(at_ms);
        self.capabilities.mark_connection_unavailable(reason);
        self.controlled_sessions.clear();
        let mut effects = vec![StateEffect::CapabilitiesUpdated {
            capabilities: self.capabilities.to_capabilities(),
        }];
        for session in self.sessions.values_mut() {
            session.live_binding = LiveBinding::NotBound {
                reason: LiveUnboundReason::SubscriptionLost,
            };
            session.status = SessionStatus::Offline;
            session.updated_at_ms = session.updated_at_ms.max(at_ms);
            effects.push(StateEffect::SessionUpserted {
                session: session.clone(),
            });
        }
        effects
    }

    pub(crate) fn refresh_live_binding(
        &mut self,
        session_id: &SessionId,
        at_ms: i64,
    ) -> Vec<StateEffect> {
        let mut effects = vec![StateEffect::CapabilitiesUpdated {
            capabilities: self.capabilities.to_capabilities(),
        }];
        if !self.capabilities.is_proven(Capability::LiveObserve) {
            return effects;
        }
        let evidence = self.evidence(at_ms);
        for session in self
            .sessions
            .values_mut()
            .filter(|session| &session.id == session_id)
        {
            session.live_binding = if self.controlled_sessions.contains(session_id) {
                LiveBinding::Controlling {
                    runtime_id: self.runtime_id.clone(),
                    since_at_ms: at_ms,
                    evidence: evidence.clone(),
                }
            } else {
                LiveBinding::Observing {
                    runtime_id: self.runtime_id.clone(),
                    since_at_ms: at_ms,
                    evidence: evidence.clone(),
                }
            };
            session.updated_at_ms = session.updated_at_ms.max(at_ms);
            effects.push(StateEffect::SessionUpserted {
                session: session.clone(),
            });
        }
        effects
    }

    fn reduce_properties(
        &mut self,
        event_type: &str,
        properties: &Value,
        expected_session: Option<&str>,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<(CanonicalEvent, Vec<StateEffect>), ReduceError> {
        let object = properties
            .as_object()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let actual_session = object.get("sessionID").and_then(Value::as_str);
        if let (Some(expected), Some(actual)) = (expected_session, actual_session) {
            if expected != actual {
                return Err(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch));
            }
        }
        let mut effects = self.ensure_bootstrapped(at_ms, content)?;
        self.capabilities.prove(Capability::LiveObserve);
        match event_type {
            "server.connected" => Ok((CanonicalEvent::ServerConnected, effects)),
            "plugin.added" => Ok((
                CanonicalEvent::Ignored {
                    event_type: event_type.to_owned(),
                    session_id: None,
                },
                effects,
            )),
            "session.next.prompt.admitted" => {
                let raw_session = required_id(object, "sessionID", "ses")?;
                Ok((
                    CanonicalEvent::Ignored {
                        event_type: event_type.to_owned(),
                        session_id: self.session_id(raw_session).cloned(),
                    },
                    effects,
                ))
            }
            "session.created" | "session.updated" | "session.deleted" => {
                let session_id = required_id(object, "sessionID", "ses")?;
                let info = object
                    .get("info")
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                effects.extend(self.reduce_session_value(info, at_ms, content)?);
                if event_type == "session.deleted" {
                    if let Some(session) = self.sessions.get_mut(session_id) {
                        session.archived = true;
                        session.status = SessionStatus::Completed;
                        effects.push(StateEffect::SessionUpserted {
                            session: session.clone(),
                        });
                    }
                }
                Ok((
                    CanonicalEvent::SessionUpsert {
                        session_id: self
                            .session_id(session_id)
                            .cloned()
                            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?,
                    },
                    effects,
                ))
            }
            "session.status" | "session.idle" => {
                let session_id = required_id(object, "sessionID", "ses")?;
                let status = if event_type == "session.idle" {
                    SessionStatus::Idle
                } else {
                    status_from_value(object.get("status"))
                        .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?
                };
                let canonical = self
                    .session_id(session_id)
                    .cloned()
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                if let Some(session) = self.sessions.get_mut(session_id) {
                    session.status = status;
                    session.updated_at_ms = session.updated_at_ms.max(at_ms);
                    effects.push(StateEffect::SessionStatusChanged {
                        session_id: canonical.clone(),
                        status,
                    });
                    effects.push(StateEffect::SessionUpserted {
                        session: session.clone(),
                    });
                }
                Ok((
                    CanonicalEvent::Status {
                        session_id: canonical,
                        status,
                    },
                    effects,
                ))
            }
            "session.diff" => {
                let raw_session = required_id(object, "sessionID", "ses")?;
                let session_id = self
                    .session_id(raw_session)
                    .cloned()
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                let diff = object
                    .get("diff")
                    .and_then(Value::as_array)
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                if !diff.is_empty() {
                    return Err(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape));
                }
                Ok((CanonicalEvent::SessionDiff { session_id }, effects))
            }
            "message.updated" => {
                let session_id = required_id(object, "sessionID", "ses")?;
                let info = object
                    .get("info")
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                let message_id = info
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| id.starts_with("msg"))
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                effects.extend(self.reduce_message_value(
                    info,
                    Some(session_id),
                    at_ms,
                    content,
                )?);
                let canonical = self
                    .session_id(session_id)
                    .cloned()
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                Ok((
                    CanonicalEvent::MessageUpsert {
                        session_id: canonical,
                        message_id: message_id.to_owned(),
                    },
                    effects,
                ))
            }
            "message.part.updated" => {
                let session_id = required_id(object, "sessionID", "ses")?;
                let part = object
                    .get("part")
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                let part_id = part
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| id.starts_with("prt"))
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                if part
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "step-start" | "step-finish"))
                {
                    let canonical = self
                        .session_id(session_id)
                        .cloned()
                        .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                    return Ok((
                        CanonicalEvent::PartUpsert {
                            session_id: canonical,
                            part_id: part_id.to_owned(),
                        },
                        effects,
                    ));
                }
                effects.extend(self.reduce_part_value(part, session_id, at_ms, content)?);
                let canonical = self
                    .session_id(session_id)
                    .cloned()
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                Ok((
                    CanonicalEvent::PartUpsert {
                        session_id: canonical,
                        part_id: part_id.to_owned(),
                    },
                    effects,
                ))
            }
            "message.part.delta" => {
                let session_id = required_id(object, "sessionID", "ses")?;
                let part_id = required_id(object, "partID", "prt")?;
                let delta = object
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                let canonical_session = self
                    .session_id(session_id)
                    .cloned()
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                let accumulated = self.part_text.entry(part_id.to_owned()).or_default();
                accumulated.push_str(delta);
                let mut item = self
                    .items
                    .get(part_id)
                    .cloned()
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
                if item.session_id != canonical_session {
                    return Err(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch));
                }
                let (kind, phase) = match &item.body {
                    ItemBody::Reasoning { .. } => (ContentKind::PlainText, None),
                    ItemBody::AgentMessage { phase, .. } => (ContentKind::Markdown, Some(*phase)),
                    _ => {
                        return Err(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape));
                    }
                };
                let content_ref = store_sensitive_text(content, kind, accumulated)?;
                item.body = if let Some(phase) = phase {
                    ItemBody::AgentMessage {
                        content: content_ref,
                        phase,
                    }
                } else {
                    ItemBody::Reasoning {
                        content: content_ref,
                    }
                };
                item.status = ItemStatus::InProgress;
                item.updated_at_ms = at_ms;
                self.items.insert(part_id.to_owned(), item.clone());
                effects.push(StateEffect::ItemUpserted { item });
                Ok((
                    CanonicalEvent::PartUpsert {
                        session_id: canonical_session,
                        part_id: part_id.to_owned(),
                    },
                    effects,
                ))
            }
            "permission.asked" | "permission.v2.asked" => {
                self.reduce_permission_asked(object, at_ms, content, effects)
            }
            "permission.replied" | "permission.v2.replied" => {
                self.reduce_permission_replied(object, at_ms, effects)
            }
            "question.asked" | "question.v2.asked" => {
                self.reduce_question_asked(object, at_ms, content, effects)
            }
            "question.replied"
            | "question.v2.replied"
            | "question.rejected"
            | "question.v2.rejected" => {
                self.reduce_question_replied(object, event_type, at_ms, content, effects)
            }
            _ => Err(ReduceError::Decode(OpenCodeDecodeError::UnknownEventType(
                event_type.to_owned(),
            ))),
        }
    }

    fn ensure_bootstrapped(
        &mut self,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ReduceError> {
        if self.bootstrapped {
            return Ok(Vec::new());
        }
        let root_ref = content.store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            self.config.project_directory.as_bytes(),
        )?;
        let capabilities = self.capabilities.to_capabilities();
        let runtime = ProviderRuntime {
            id: self.runtime_id.clone(),
            host_id: self.host_id.clone(),
            family: ProviderFamily::OpenCode,
            version_label: self.config.runtime_version_label.clone(),
            launch_surface: LaunchSurface::SharedServer,
            connection: ConnectionState::Connected { since_at_ms: at_ms },
            capabilities,
            binding_handle: None,
        };
        let host = Host {
            id: self.host_id.clone(),
            display_name: self.config.host_display_name.clone(),
            platform: self.config.host_platform.clone(),
            reachability: HostReachability::LanDirect,
            protocol_version: "opencode-rest-sse".to_owned(),
            last_seen_at_ms: at_ms,
        };
        let project = Project {
            id: self.project_id.clone(),
            display_name: self.config.project_display_name.clone(),
            bindings: vec![ProjectBinding {
                id: self.project_binding_id.clone(),
                project_id: self.project_id.clone(),
                runtime_id: self.runtime_id.clone(),
                root_ref,
            }],
            session_counts: SessionCounts::default(),
            workflow_count: 0,
            attention_count: 0,
            last_activity_at_ms: at_ms,
        };
        self.bootstrapped = true;
        Ok(vec![
            StateEffect::HostUpserted { host },
            StateEffect::RuntimeUpserted { runtime },
            StateEffect::ProjectUpserted { project },
        ])
    }

    fn reduce_session_value(
        &mut self,
        value: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ReduceError> {
        let object = value
            .as_object()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let raw_id = required_id(object, "id", "ses")?;
        let (session_id, handle) = self.bindings.session(&self.mint, &self.runtime_id, raw_id);
        let directory = object
            .get("directory")
            .and_then(Value::as_str)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
        // The provider project id is opaque and cannot be compared with a
        // canonical id. The selected directory is the public API's actual
        // scope boundary and must match byte-for-byte.
        if directory != self.config.project_directory {
            return Err(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch));
        }
        let title = object
            .get("title")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let (created, updated) = time_fields(object).unwrap_or((at_ms, at_ms));
        let session = Session {
            id: session_id.clone(),
            project_id: self.project_id.clone(),
            project_binding_id: self.project_binding_id.clone(),
            ownership: OwnershipMode::SharedRuntime,
            history_source: HistorySource {
                kind: HistorySourceKind::ProviderApi,
                runtime_id: Some(self.runtime_id.clone()),
                evidence: self.evidence(at_ms),
            },
            live_binding: LiveBinding::NotBound {
                reason: LiveUnboundReason::SubscriptionLost,
            },
            status: SessionStatus::Idle,
            title,
            created_at_ms: created,
            updated_at_ms: updated,
            last_activity_at_ms: updated,
            active_turn_id: None,
            queue_depth: 0,
            open_attention_count: 0,
            archived: false,
            binding_handle: Some(handle),
        };
        self.sessions.insert(raw_id.to_owned(), session.clone());
        let _ = content;
        Ok(vec![StateEffect::SessionUpserted { session }])
    }

    fn reduce_message_value(
        &mut self,
        value: &Value,
        expected_session: Option<&str>,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ReduceError> {
        let object = value
            .as_object()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let info = object
            .get("info")
            .and_then(Value::as_object)
            .unwrap_or(object);
        let raw_message = required_id(info, "id", "msg")?;
        let raw_session = info
            .get("sessionID")
            .and_then(Value::as_str)
            .or(expected_session)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
        if expected_session.is_some_and(|expected| expected != raw_session) {
            return Err(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch));
        }
        let (session_id, _) = self
            .bindings
            .session(&self.mint, &self.runtime_id, raw_session);
        if !self.sessions.contains_key(raw_session) {
            return Err(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch));
        }
        let (turn_id, turn_handle, _) =
            self.bindings
                .turn(&self.mint, &self.runtime_id, raw_message, &session_id);
        let role = info
            .get("role")
            .and_then(Value::as_str)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let mut item_ids = self
            .turns
            .get(raw_message)
            .map_or_else(Vec::new, |turn| turn.item_ids.clone());
        let mut effects = Vec::new();
        if let Some(parts) = object.get("parts").and_then(Value::as_array) {
            for part in parts {
                let raw_part = required_id(
                    part.as_object()
                        .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?,
                    "id",
                    "prt",
                )?;
                let item = self.make_item_from_part(
                    part,
                    raw_part,
                    role,
                    &session_id,
                    &turn_id,
                    at_ms,
                    content,
                )?;
                if !item_ids.contains(&item.id) {
                    item_ids.push(item.id.clone());
                }
                effects.push(StateEffect::ItemUpserted { item });
            }
        }
        let turn = self
            .turns
            .entry(raw_message.to_owned())
            .or_insert_with(|| Turn {
                id: turn_id.clone(),
                session_id: session_id.clone(),
                status: TurnStatus::Running,
                origin: TurnOrigin::LocalSurface,
                started_at_ms: Some(at_ms),
                completed_at_ms: None,
                item_ids: Vec::new(),
                error: None,
                binding_handle: Some(turn_handle.clone()),
            });
        turn.item_ids = item_ids;
        turn.status = if role == "assistant" {
            TurnStatus::Completed
        } else {
            TurnStatus::Running
        };
        if turn.status == TurnStatus::Completed {
            turn.completed_at_ms = Some(at_ms);
        }
        effects.insert(0, StateEffect::TurnUpserted { turn: turn.clone() });
        if let Some(session) = self.sessions.get_mut(raw_session) {
            session.active_turn_id = Some(turn_id);
            session.status = if role == "assistant" {
                SessionStatus::Idle
            } else {
                SessionStatus::Running
            };
            session.updated_at_ms = session.updated_at_ms.max(at_ms);
            effects.push(StateEffect::SessionUpserted {
                session: session.clone(),
            });
        }
        Ok(effects)
    }

    fn reduce_part_value(
        &mut self,
        value: &Value,
        raw_session: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ReduceError> {
        let object = value
            .as_object()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let raw_part = required_id(object, "id", "prt")?;
        let raw_message = required_id(object, "messageID", "msg")?;
        let (session_id, _) = self
            .bindings
            .session(&self.mint, &self.runtime_id, raw_session);
        let (turn_id, _, _) =
            self.bindings
                .turn(&self.mint, &self.runtime_id, raw_message, &session_id);
        let item = self.make_item_from_part(
            value,
            raw_part,
            "assistant",
            &session_id,
            &turn_id,
            at_ms,
            content,
        )?;
        self.items.insert(raw_part.to_owned(), item.clone());
        Ok(vec![StateEffect::ItemUpserted { item }])
    }

    #[allow(clippy::too_many_arguments)]
    fn make_item_from_part(
        &mut self,
        value: &Value,
        raw_part: &str,
        role: &str,
        session_id: &SessionId,
        turn_id: &TurnId,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Item, ReduceError> {
        let object = value
            .as_object()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let raw_text = object
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| object.get("content").and_then(Value::as_str));
        let (body, status) = match (role, kind, raw_text) {
            ("user", "text", Some(text)) => (
                ItemBody::UserMessage {
                    content: store_sensitive_text(content, ContentKind::PlainText, text)?,
                },
                ItemStatus::Completed,
            ),
            (_, "text", Some(text)) => (
                ItemBody::AgentMessage {
                    content: store_sensitive_text(content, ContentKind::Markdown, text)?,
                    phase: MessagePhase::FinalAnswer,
                },
                ItemStatus::Completed,
            ),
            (_, "reasoning", Some(text)) => (
                ItemBody::Reasoning {
                    content: store_sensitive_text(content, ContentKind::PlainText, text)?,
                },
                ItemStatus::Completed,
            ),
            (_, "tool", _) => {
                let state = object
                    .get("state")
                    .and_then(Value::as_object)
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                let status = match state.get("status").and_then(Value::as_str) {
                    Some("pending") => ItemStatus::Pending,
                    Some("running") => ItemStatus::InProgress,
                    Some("completed") => ItemStatus::Completed,
                    Some("error") | Some("failed") => ItemStatus::Failed,
                    _ => {
                        return Err(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape));
                    }
                };
                let arguments = state
                    .get("input")
                    .map(|input| store_sensitive_json(content, ContentKind::ToolArguments, input))
                    .transpose()?;
                let output = state
                    .get("output")
                    .map(|output| store_sensitive_json(content, ContentKind::ToolOutput, output))
                    .transpose()?;
                (
                    ItemBody::ToolCall {
                        tool: ToolDescriptor {
                            name: object
                                .get("tool")
                                .and_then(Value::as_str)
                                .unwrap_or("opencode.tool")
                                .to_owned(),
                            surface: ToolSurface::Builtin,
                        },
                        arguments,
                        output,
                        exit_code: None,
                    },
                    status,
                )
            }
            _ => return Err(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape)),
        };
        if let Some(text) = raw_text {
            self.part_text.insert(raw_part.to_owned(), text.to_owned());
        }
        let binding =
            self.bindings
                .item(&self.mint, &self.runtime_id, raw_part, session_id, turn_id);
        let item = Item {
            id: binding.item_id,
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            sequence: self.next_sequence,
            status,
            body,
            created_at_ms: at_ms,
            updated_at_ms: at_ms,
            binding_handle: Some(binding.handle),
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.items.insert(raw_part.to_owned(), item.clone());
        Ok(item)
    }

    fn reduce_permission_asked(
        &mut self,
        object: &Map<String, Value>,
        at_ms: i64,
        content: &mut dyn ContentAccess,
        mut effects: Vec<StateEffect>,
    ) -> Result<(CanonicalEvent, Vec<StateEffect>), ReduceError> {
        let raw_session = required_id(object, "sessionID", "ses")?;
        let raw_request = required_id(object, "id", "per")?;
        let (session_id, _) = self
            .bindings
            .session(&self.mint, &self.runtime_id, raw_session);
        let (attention_id, handle) =
            self.bindings
                .interaction(&self.mint, &self.runtime_id, raw_request);
        let summary = object
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or("OpenCode permission request");
        let summary_ref = store_sensitive_text(content, ContentKind::PlainText, summary)?;
        let target = object
            .get("tool")
            .and_then(Value::as_object)
            .and_then(|tool| tool.get("callID"))
            .and_then(Value::as_str)
            .map(|id| self.mint.item_id(id))
            .unwrap_or_else(|| self.mint.item_id(raw_request));
        let join = if self.items.values().any(|item| item.id == target) {
            JoinState::Joined {
                item_id: target.clone(),
            }
        } else {
            JoinState::Unjoined {
                reason: JoinFailureReason::ItemNotYetSeen,
            }
        };
        let request = ApprovalRequest {
            request_key: self.mint.request_key(raw_request),
            target_item_id: target,
            join,
            options: vec![
                DecisionOption {
                    option_id: "once".to_owned(),
                    label: "Allow once".to_owned(),
                    semantics: DecisionSemantics::Allow,
                },
                DecisionOption {
                    option_id: "always".to_owned(),
                    label: "Always allow".to_owned(),
                    semantics: DecisionSemantics::AllowAlways,
                },
                DecisionOption {
                    option_id: "reject".to_owned(),
                    label: "Reject".to_owned(),
                    semantics: DecisionSemantics::Deny,
                },
            ],
            summary_ref,
            detail_ref: None,
            binding_handle: handle,
        };
        let attention = AttentionItem {
            id: attention_id.clone(),
            host_id: self.host_id.clone(),
            project_id: self.project_id.clone(),
            session_id: Some(session_id.clone()),
            turn_id: None,
            workflow_id: None,
            subject: AttentionSubject::Approval { request },
            state: AttentionState::Open,
            created_at_ms: at_ms,
            expires_at_ms: None,
        };
        self.attentions
            .insert(raw_request.to_owned(), attention.clone());
        self.capabilities.prove(Capability::InteractionApproval);
        effects.push(StateEffect::AttentionUpserted { item: attention });
        effects.push(StateEffect::CapabilitiesUpdated {
            capabilities: self.capabilities.to_capabilities(),
        });
        Ok((
            CanonicalEvent::PermissionAsked {
                session_id,
                attention_id,
            },
            effects,
        ))
    }

    fn reduce_permission_replied(
        &mut self,
        object: &Map<String, Value>,
        at_ms: i64,
        mut effects: Vec<StateEffect>,
    ) -> Result<(CanonicalEvent, Vec<StateEffect>), ReduceError> {
        let raw_session = required_id(object, "sessionID", "ses")?;
        let raw_request = required_id(object, "requestID", "per")?;
        let reply = object
            .get("reply")
            .and_then(Value::as_str)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let session_id = self
            .session_id(raw_session)
            .cloned()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
        let local_command = self.local_attention_commands.remove(raw_request);
        let attention = self
            .attentions
            .get_mut(raw_request)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
        attention.state = AttentionState::Answered {
            option_id: Some(reply.to_owned()),
            free_form_ref: None,
            question_answers: Vec::new(),
            decided_at_ms: at_ms,
            answer_source: local_command.clone().map_or_else(
                || kaleido_proto::attention::AttentionAnswerSource::ObservedExternal {
                    evidence: AttentionAnswerEvidence {
                        observer_host_id: self.host_id.clone(),
                        observed_at_ms: at_ms,
                        source: AttentionAnswerEvidenceSource::ObservedInTraffic,
                    },
                },
                |command_id| kaleido_proto::attention::AttentionAnswerSource::LocalCommand {
                    command_id,
                },
            ),
        };
        effects.push(StateEffect::AttentionUpserted {
            item: attention.clone(),
        });
        Ok((
            CanonicalEvent::PermissionAnswered {
                session_id,
                attention_id: attention.id.clone(),
            },
            effects,
        ))
    }

    fn reduce_question_asked(
        &mut self,
        object: &Map<String, Value>,
        at_ms: i64,
        content: &mut dyn ContentAccess,
        mut effects: Vec<StateEffect>,
    ) -> Result<(CanonicalEvent, Vec<StateEffect>), ReduceError> {
        let raw_session = required_id(object, "sessionID", "ses")?;
        let raw_request = required_id(object, "id", "que")?;
        let (session_id, _) = self
            .bindings
            .session(&self.mint, &self.runtime_id, raw_session);
        let (attention_id, handle) =
            self.bindings
                .interaction(&self.mint, &self.runtime_id, raw_request);
        let questions = object
            .get("questions")
            .and_then(Value::as_array)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
        let mut prompts = Vec::new();
        for (index, question) in questions.iter().enumerate() {
            let question = question
                .as_object()
                .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
            let text = question
                .get("question")
                .or_else(|| question.get("header"))
                .and_then(Value::as_str)
                .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
            let prompt_ref = store_sensitive_text(content, ContentKind::PlainText, text)?;
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| option.as_object())
                        .filter_map(|option| {
                            let id = option.get("label").and_then(Value::as_str)?;
                            Some(DecisionOption {
                                option_id: id.to_owned(),
                                label: id.to_owned(),
                                semantics: DecisionSemantics::Choose,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            prompts.push(QuestionPrompt {
                question_key: index.to_string(),
                prompt_ref,
                options,
                multi_select: question
                    .get("multiple")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                free_form_allowed: true,
            });
        }
        let attention = AttentionItem {
            id: attention_id.clone(),
            host_id: self.host_id.clone(),
            project_id: self.project_id.clone(),
            session_id: Some(session_id.clone()),
            turn_id: None,
            workflow_id: None,
            subject: AttentionSubject::Question {
                request: QuestionRequest {
                    request_key: self.mint.request_key(raw_request),
                    questions: prompts,
                    binding_handle: handle,
                },
            },
            state: AttentionState::Open,
            created_at_ms: at_ms,
            expires_at_ms: None,
        };
        self.attentions
            .insert(raw_request.to_owned(), attention.clone());
        self.capabilities.prove(Capability::InteractionQuestion);
        effects.push(StateEffect::AttentionUpserted { item: attention });
        effects.push(StateEffect::CapabilitiesUpdated {
            capabilities: self.capabilities.to_capabilities(),
        });
        Ok((
            CanonicalEvent::QuestionAsked {
                session_id,
                attention_id,
            },
            effects,
        ))
    }

    fn reduce_question_replied(
        &mut self,
        object: &Map<String, Value>,
        event_type: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
        mut effects: Vec<StateEffect>,
    ) -> Result<(CanonicalEvent, Vec<StateEffect>), ReduceError> {
        let raw_session = required_id(object, "sessionID", "ses")?;
        let raw_request = required_id(object, "requestID", "que")?;
        let session_id = self
            .session_id(raw_session)
            .cloned()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
        let current = self
            .attentions
            .get(raw_request)
            .cloned()
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
        let local_command = self.local_attention_commands.remove(raw_request);
        let state = if event_type.ends_with("rejected") {
            AttentionState::Cancelled { at_ms }
        } else {
            let AttentionSubject::Question { request } = &current.subject else {
                return Err(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch));
            };
            let raw_answers = object
                .get("answers")
                .and_then(Value::as_array)
                .filter(|answers| answers.len() == request.questions.len())
                .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
            let mut answers = Vec::with_capacity(raw_answers.len());
            for (question, raw_answer) in request.questions.iter().zip(raw_answers) {
                let values = raw_answer
                    .as_array()
                    .filter(|values| !values.is_empty())
                    .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                let mut option_ids = Vec::new();
                let mut free_form = Vec::new();
                for value in values {
                    let value = value
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
                    if let Some(option) =
                        question.options.iter().find(|option| option.label == value)
                    {
                        option_ids.push(option.option_id.clone());
                    } else {
                        free_form.push(value);
                    }
                }
                let free_form_ref = if free_form.is_empty() {
                    None
                } else {
                    Some(store_sensitive_text(
                        content,
                        ContentKind::PlainText,
                        &free_form.join(", "),
                    )?)
                };
                answers.push(kaleido_proto::attention::QuestionAnswer {
                    question_key: question.question_key.clone(),
                    option_ids,
                    free_form_ref,
                });
            }
            AttentionState::Answered {
                option_id: None,
                free_form_ref: None,
                question_answers: answers,
                decided_at_ms: at_ms,
                answer_source: local_command.clone().map_or_else(
                    || kaleido_proto::attention::AttentionAnswerSource::ObservedExternal {
                        evidence: AttentionAnswerEvidence {
                            observer_host_id: self.host_id.clone(),
                            observed_at_ms: at_ms,
                            source: AttentionAnswerEvidenceSource::ObservedInTraffic,
                        },
                    },
                    |command_id| kaleido_proto::attention::AttentionAnswerSource::LocalCommand {
                        command_id,
                    },
                ),
            }
        };
        let attention = self
            .attentions
            .get_mut(raw_request)
            .ok_or(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))?;
        attention.state = state;
        effects.push(StateEffect::AttentionUpserted {
            item: attention.clone(),
        });
        Ok((
            CanonicalEvent::QuestionAnswered {
                session_id,
                attention_id: attention.id.clone(),
            },
            effects,
        ))
    }

    fn evidence(&self, at_ms: i64) -> CapabilityEvidence {
        CapabilityEvidence {
            source: self.config.evidence,
            observed_at_ms: at_ms,
            note_ref: None,
        }
    }
}

#[derive(Debug)]
enum ReduceError {
    Decode(OpenCodeDecodeError),
    Content(kaleido_adapter::content::ContentAccessError),
}

impl From<OpenCodeDecodeError> for ReduceError {
    fn from(error: OpenCodeDecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<kaleido_adapter::content::ContentAccessError> for ReduceError {
    fn from(error: kaleido_adapter::content::ContentAccessError) -> Self {
        Self::Content(error)
    }
}

fn store_sensitive_json(
    content: &mut dyn ContentAccess,
    kind: ContentKind,
    value: &Value,
) -> Result<ContentRef, ReduceError> {
    if let Some(text) = value.as_str() {
        return store_sensitive_text(content, kind, text).map_err(ReduceError::Content);
    }
    let bytes = serde_json::to_vec(value)
        .map_err(|_| ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
    content
        .store(kind, Sensitivity::Sensitive, &bytes)
        .map_err(ReduceError::Content)
}

fn required_id<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    prefix: &str,
) -> Result<&'a str, ReduceError> {
    let id = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or(ReduceError::Decode(OpenCodeDecodeError::UnsupportedShape))?;
    if id.starts_with(prefix) {
        Ok(id)
    } else {
        Err(ReduceError::Decode(OpenCodeDecodeError::ScopeMismatch))
    }
}

fn time_fields(object: &Map<String, Value>) -> Option<(i64, i64)> {
    let time = object.get("time")?.as_object()?;
    let created = time.get("created").and_then(Value::as_i64)?;
    let updated = time
        .get("updated")
        .and_then(Value::as_i64)
        .unwrap_or(created);
    Some((created, updated))
}

fn status_from_value(value: Option<&Value>) -> Option<SessionStatus> {
    let status = value?.as_object()?.get("type")?.as_str()?;
    match status {
        "idle" => Some(SessionStatus::Idle),
        "busy" | "running" => Some(SessionStatus::Running),
        "retry" => Some(SessionStatus::WaitingUser),
        _ => None,
    }
}

fn is_supported_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "server.connected"
            | "plugin.added"
            | "session.next.prompt.admitted"
            | "session.created"
            | "session.updated"
            | "session.deleted"
            | "session.status"
            | "session.idle"
            | "session.diff"
            | "message.updated"
            | "message.part.updated"
            | "message.part.delta"
            | "permission.asked"
            | "permission.replied"
            | "permission.v2.asked"
            | "permission.v2.replied"
            | "question.asked"
            | "question.replied"
            | "question.rejected"
            | "question.v2.asked"
            | "question.v2.replied"
            | "question.v2.rejected"
    )
}

fn validate_generated_event(event_type: &str, value: &Value) -> Result<(), OpenCodeDecodeError> {
    macro_rules! generated {
        ($ty:path) => {
            serde_json::from_value::<$ty>(value.clone()).map(|_| ())?
        };
    }
    match event_type {
        "server.connected" => generated!(crate::wire::EventServerConnected),
        "plugin.added" => generated!(crate::wire::EventPluginAdded),
        "session.next.prompt.admitted" => {
            generated!(crate::wire::EventSessionNextPromptAdmitted)
        }
        "session.created" => generated!(crate::wire::EventSessionCreated),
        "session.updated" => generated!(crate::wire::EventSessionUpdated),
        "session.deleted" => generated!(crate::wire::EventSessionDeleted),
        "session.status" => generated!(crate::wire::EventSessionStatus),
        "session.idle" => generated!(crate::wire::EventSessionIdle),
        "session.diff" => generated!(crate::wire::EventSessionDiff),
        "message.updated" => generated!(crate::wire::EventMessageUpdated),
        "message.part.updated" => generated!(crate::wire::EventMessagePartUpdated),
        "message.part.delta" => generated!(crate::wire::EventMessagePartDelta),
        "permission.asked" => generated!(crate::wire::EventPermissionAsked),
        "permission.replied" => generated!(crate::wire::EventPermissionReplied),
        "permission.v2.asked" => generated!(crate::wire::EventPermissionV2Asked),
        "permission.v2.replied" => generated!(crate::wire::EventPermissionV2Replied),
        "question.asked" => generated!(crate::wire::EventQuestionAsked),
        "question.replied" => generated!(crate::wire::EventQuestionReplied),
        "question.rejected" => generated!(crate::wire::EventQuestionRejected),
        "question.v2.asked" => generated!(crate::wire::EventQuestionV2Asked),
        "question.v2.replied" => generated!(crate::wire::EventQuestionV2Replied),
        "question.v2.rejected" => generated!(crate::wire::EventQuestionV2Rejected),
        _ => return Err(OpenCodeDecodeError::UnknownEventType(event_type.to_owned())),
    }
    Ok(())
}
