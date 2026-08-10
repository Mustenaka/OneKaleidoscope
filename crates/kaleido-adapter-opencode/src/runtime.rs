use kaleido_adapter::{
    capability::CapabilityProbe,
    content::ContentAccess,
    session::{ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest},
};
use kaleido_proto::{
    attention::{AttentionResponse, AttentionSubject},
    capability::{Capability, CapabilityUnavailableReason},
    command::RuntimeAcceptanceKind,
    content::ContentRef,
    effect::StateEffect,
    host::ConnectionFaultReason,
    ids::{CommandId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId, TurnId},
    queue::{QueueEntry, QueueIntent, QueueState},
};

use crate::{
    client::{
        OpenCodeClient, OpenCodeClientConfig, PermissionReply, PromptAdmission, PromptDelivery,
        SseEvent, SseStream,
    },
    error::OpenCodeAdapterError,
    reduce::{OpenCodeReducer, ReducerConfig},
};

#[derive(Debug, Clone)]
pub struct OpenCodeRuntimeConfig {
    pub client: OpenCodeClientConfig,
    pub reducer: ReducerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectOutcome {
    pub effects: Vec<StateEffect>,
    /// `/event` has no cursor, so this is always false.  The snapshot is the
    /// authoritative boundary before the newly opened SSE tail.
    pub lossless_replay: bool,
}

#[derive(Debug)]
pub struct OpenCodeRuntimeSession {
    client: OpenCodeClient,
    reducer: OpenCodeReducer,
    stream: Option<SseStream>,
    connected: bool,
    started_at_ms: i64,
    session_raw: Option<String>,
    last_prompt_admission: Option<PromptAdmission>,
}

impl OpenCodeRuntimeSession {
    pub fn new(config: OpenCodeRuntimeConfig) -> Result<Self, OpenCodeAdapterError> {
        Ok(Self {
            client: OpenCodeClient::new(config.client)?,
            reducer: OpenCodeReducer::new(config.reducer),
            stream: None,
            connected: false,
            started_at_ms: 0,
            session_raw: None,
            last_prompt_admission: None,
        })
    }

    pub fn runtime_id(&self) -> &ProviderRuntimeId {
        self.reducer.runtime_id()
    }

    pub fn project_id(&self) -> &ProjectId {
        self.reducer.project_id()
    }

    pub fn project_binding_id(&self) -> &ProjectBindingId {
        self.reducer.project_binding_id()
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_raw
            .as_deref()
            .and_then(|raw| self.reducer.session_id(raw))
    }

    pub fn reducer(&self) -> &OpenCodeReducer {
        &self.reducer
    }

    pub fn reducer_mut(&mut self) -> &mut OpenCodeReducer {
        &mut self.reducer
    }

    /// Last durable admission receipt observed on this connection.  A
    /// legacy `prompt_async` fallback clears this value because that route
    /// does not provide an acknowledgement.
    pub fn last_prompt_admission(&self) -> Option<&PromptAdmission> {
        self.last_prompt_admission.as_ref()
    }

    /// Queue a prompt through OpenCode's durable v2 admission endpoint.  This
    /// provider-specific API exists until the shared runtime trait grows a
    /// queue/admission operation.
    pub fn enqueue_prompt(&mut self, text: &str) -> Result<PromptAdmission, OpenCodeAdapterError> {
        if !self.connected {
            return Err(OpenCodeAdapterError::NotConnected);
        }
        let raw = self
            .session_raw
            .as_deref()
            .ok_or(OpenCodeAdapterError::NotConnected)?;
        let admission = self.client.prompt_v2(raw, text, PromptDelivery::Queue)?;
        self.last_prompt_admission = Some(admission.clone());
        Ok(admission)
    }

    /// Explicit discover API for hostd until the shared trait grows a
    /// provider-neutral history-list operation.
    pub fn discover(
        &mut self,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, OpenCodeAdapterError> {
        let sessions = self.client.list_sessions()?;
        let mut messages = Vec::new();
        for session in &sessions {
            let raw_id = session
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| protocol_error("session id missing"))?;
            messages.push((raw_id.to_owned(), self.client.get_messages(raw_id)?));
        }
        self.reducer
            .reduce_snapshot(&sessions, &messages, at_ms, content)
            .map_err(|error| protocol_error(&error.to_string()))
    }

    /// Explicit resume API: REST snapshot first, then open a fresh SSE tail.
    /// Since OpenCode does not attach an event cursor, the return value makes
    /// the non-lossless boundary visible to callers.
    pub fn resume(
        &mut self,
        session_raw: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<ReconnectOutcome, OpenCodeAdapterError> {
        let session = self.client.get_session(session_raw)?;
        let messages = self.client.get_messages(session_raw)?;
        let effects = self
            .reducer
            .reduce_snapshot(
                &[session],
                &[(session_raw.to_owned(), messages)],
                at_ms,
                content,
            )
            .map_err(|error| protocol_error(&error.to_string()))?;
        self.stream = Some(self.client.subscribe()?);
        self.connected = true;
        self.session_raw = Some(session_raw.to_owned());
        Ok(ReconnectOutcome {
            effects,
            lossless_replay: false,
        })
    }

    pub fn reconnect(
        &mut self,
        session_raw: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<ReconnectOutcome, OpenCodeAdapterError> {
        self.resume(session_raw, at_ms, content)
    }

    /// Provider-specific interrupt path (the shared trait currently exposes
    /// only prompt/attention methods).
    pub fn interrupt(&mut self) -> Result<(), OpenCodeAdapterError> {
        let raw = self
            .session_raw
            .as_deref()
            .ok_or(OpenCodeAdapterError::NotConnected)?;
        self.client.abort(raw)?;
        self.reducer.capabilities_prove(Capability::TurnInterrupt);
        Ok(())
    }

    fn ingest_sse(
        &mut self,
        event: SseEvent,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, OpenCodeAdapterError> {
        if event.cursor.is_none() {
            tracing::debug!(
                event_bytes = event.data.len(),
                "OpenCode SSE event has no replay cursor"
            );
        }
        let event_type = serde_json::from_slice::<serde_json::Value>(&event.data)
            .ok()
            .and_then(|value| {
                value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unknown-event".to_owned());
        let (canonical, mut effects) = self
            .reducer
            .reduce_sse_event(&event.data, None, at_ms, content)
            .map_err(|_| protocol_error(&event_type))?;
        if let Some(session_id) = canonical.session_id().cloned() {
            effects.extend(self.reducer.mark_live_observed(&session_id, at_ms));
        }
        Ok(effects)
    }

    fn require_connected(&self) -> Result<(), RuntimeSessionError> {
        if self.connected {
            Ok(())
        } else {
            Err(RuntimeSessionError::NotConnected)
        }
    }
}

impl ProviderRuntimeSession for OpenCodeRuntimeSession {
    fn start(
        &mut self,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if self.connected {
            return Err(RuntimeSessionError::AlreadyStarted);
        }
        if request.runtime_id != *self.reducer.runtime_id()
            || request.project_id != *self.reducer.project_id()
            || request.project_binding_id != *self.reducer.project_binding_id()
        {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "OpenCode session start identity does not match reducer".to_owned(),
            });
        }
        request
            .project_root_ref
            .ensure_sensitive("session_start.project_root_ref")
            .map_err(RuntimeSessionError::Contract)?;
        let root = content
            .load(&request.project_root_ref)
            .map_err(RuntimeSessionError::Content)?;
        if root.is_empty() {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "OpenCode project root is empty".to_owned(),
            });
        }
        self.started_at_ms = now_ms();
        let session = self.client.create_session(None).map_err(to_runtime_error)?;
        let raw_id = session
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| RuntimeSessionError::ProtocolViolation {
                detail: "OpenCode create session response has no id".to_owned(),
            })?
            .to_owned();
        let messages = self
            .client
            .get_messages(&raw_id)
            .map_err(to_runtime_error)?;
        let effects = self
            .reducer
            .reduce_snapshot(
                &[session],
                &[(raw_id.clone(), messages)],
                self.started_at_ms,
                content,
            )
            .map_err(|error| RuntimeSessionError::ProtocolViolation {
                detail: error.to_string(),
            })?;
        self.stream = Some(self.client.subscribe().map_err(to_runtime_error)?);
        self.session_raw = Some(raw_id);
        self.connected = true;
        Ok(effects)
    }

    fn submit_prompt(
        &mut self,
        command_id: &CommandId,
        body: &ContentRef,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_connected()?;
        body.ensure_sensitive("submit_prompt.body")
            .map_err(RuntimeSessionError::Contract)?;
        let bytes = content.load(body).map_err(RuntimeSessionError::Content)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| RuntimeSessionError::ProtocolViolation {
                detail: "OpenCode prompt body is not UTF-8".to_owned(),
            })?;
        let raw = self
            .session_raw
            .clone()
            .ok_or(RuntimeSessionError::NotConnected)?;
        let session_id = self
            .reducer
            .session_id(&raw)
            .cloned()
            .ok_or(RuntimeSessionError::NotConnected)?;
        match self.enqueue_prompt(text) {
            Ok(admission) => {
                if admission.delivery == PromptDelivery::Queue {
                    self.reducer.capabilities_prove(Capability::QueueWrite);
                }
                self.reducer.capabilities_prove(Capability::TurnPrompt);
                let at_ms = now_ms();
                let receipt_key = format!(
                    "prompt|{}|{}|{}",
                    admission.session_id, admission.id, admission.admitted_seq
                );
                let admitted_turn = self
                    .reducer
                    .admit_prompt_turn(&raw, &admission.id, command_id)
                    .map_err(|error| RuntimeSessionError::ProtocolViolation {
                        detail: error.to_string(),
                    })?;
                Ok(vec![
                    admitted_turn,
                    self.reducer.accepted_command_effect(
                        &session_id,
                        RuntimeAcceptanceKind::PromptTurn,
                        command_id,
                        &receipt_key,
                        at_ms,
                    ),
                    StateEffect::CapabilitiesUpdated {
                        capabilities: self.reducer.capability_probe().to_capabilities(),
                    },
                ]
                .into_iter()
                .chain(self.reducer.refresh_live_binding(&session_id, at_ms))
                .collect())
            }
            Err(OpenCodeAdapterError::HttpStatus {
                status: 404 | 405 | 501,
                ..
            }) => {
                // Older servers may expose only prompt_async.  It has no
                // durable receipt, so do not claim a prompt/queue
                // acknowledgement or promote any capability from it.
                self.last_prompt_admission = None;
                self.client
                    .prompt_async(&raw, text)
                    .map_err(to_runtime_error)?;
                Ok(Vec::new())
            }
            Err(error) => Err(to_runtime_error(error)),
        }
    }

    fn respond_attention(
        &mut self,
        command_id: &CommandId,
        response: &AttentionResponse,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_connected()?;
        if response.free_form_ref.is_some() && response.question_answers.is_empty() {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        let (raw_session, raw_request, question) = self
            .reducer
            .register_local_attention_answer(&response.attention_id, command_id)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        let current_session = self
            .session_raw
            .as_deref()
            .ok_or(RuntimeSessionError::NotConnected)?;
        if raw_session != current_session {
            self.reducer.forget_local_attention_answer(&raw_request);
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        let result = if question {
            let attention = self
                .reducer
                .attention(&response.attention_id)
                .cloned()
                .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
            let AttentionSubject::Question { request } = attention.subject else {
                self.reducer.forget_local_attention_answer(&raw_request);
                return Err(RuntimeSessionError::CapabilityUnavailable);
            };
            let answers = request
                .questions
                .iter()
                .map(|question| {
                    let answer = response
                        .question_answers
                        .iter()
                        .find(|answer| answer.question_key == question.question_key)
                        .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
                    let mut values = answer.option_ids.clone();
                    for (value, option_id) in values.iter_mut().zip(&answer.option_ids) {
                        *value = question
                            .options
                            .iter()
                            .find(|option| &option.option_id == option_id)
                            .map(|option| option.label.clone())
                            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
                    }
                    if let Some(reference) = &answer.free_form_ref {
                        let bytes = content
                            .load(reference)
                            .map_err(RuntimeSessionError::Content)?;
                        let text = std::str::from_utf8(&bytes).map_err(|_| {
                            RuntimeSessionError::ProtocolViolation {
                                detail: "question answer is not UTF-8".to_owned(),
                            }
                        })?;
                        values.push(text.to_owned());
                    }
                    Ok(values)
                })
                .collect::<Result<Vec<_>, RuntimeSessionError>>();
            let answers = match answers {
                Ok(answers) => answers,
                Err(error) => {
                    self.reducer.forget_local_attention_answer(&raw_request);
                    return Err(error);
                }
            };
            self.client
                .reply_question(&raw_session, &raw_request, answers)
                .map_err(to_runtime_error)
        } else {
            let option = response
                .option_id
                .as_deref()
                .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
            let reply = match option {
                "once" | "accept" => PermissionReply::Once,
                "always" | "acceptForSession" => PermissionReply::Always,
                "reject" | "decline" | "cancel" => PermissionReply::Reject,
                _ => {
                    self.reducer.forget_local_attention_answer(&raw_request);
                    return Err(RuntimeSessionError::CapabilityUnavailable);
                }
            };
            match self
                .client
                .reply_permission(&raw_session, &raw_request, reply)
            {
                Ok(()) => Ok(()),
                Err(crate::error::OpenCodeAdapterError::HttpStatus { status: 404, .. }) => self
                    .client
                    .reply_permission_v2(&raw_session, &raw_request, reply)
                    .map_err(to_runtime_error),
                Err(error) => Err(to_runtime_error(error)),
            }
        };
        if let Err(error) = result {
            self.reducer.forget_local_attention_answer(&raw_request);
            return Err(error);
        }
        Ok(vec![StateEffect::CapabilitiesUpdated {
            capabilities: self.reducer.capability_probe().to_capabilities(),
        }])
    }

    fn discover(
        &mut self,
        _request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        OpenCodeRuntimeSession::discover(self, now_ms(), content).map_err(to_runtime_error)
    }

    fn reconnect(
        &mut self,
        _request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let raw = self
            .session_raw
            .clone()
            .ok_or(RuntimeSessionError::NotConnected)?;
        self.stream = None;
        self.connected = false;
        self.reducer.reset_connection(now_ms());
        OpenCodeRuntimeSession::reconnect(self, &raw, now_ms(), content)
            .map(|outcome| outcome.effects)
            .map_err(to_runtime_error)
    }

    fn resume_session(
        &mut self,
        session_id: &SessionId,
        _request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let raw = self
            .reducer
            .raw_session_id(session_id)
            .map(str::to_owned)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        self.stream = None;
        self.connected = false;
        self.reducer.reset_connection(now_ms());
        OpenCodeRuntimeSession::resume(self, &raw, now_ms(), content)
            .map(|outcome| outcome.effects)
            .map_err(to_runtime_error)
    }

    fn connection_lost_effects(
        &mut self,
        reason: CapabilityUnavailableReason,
        at_ms: i64,
    ) -> Vec<StateEffect> {
        self.stream = None;
        self.connected = false;
        self.reducer.mark_connection_unavailable(reason, at_ms)
    }

    fn interrupt_turn(
        &mut self,
        command_id: &CommandId,
        turn_id: &TurnId,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_connected()?;
        let raw = self
            .session_raw
            .clone()
            .ok_or(RuntimeSessionError::NotConnected)?;
        if !self.reducer.is_active_turn(&raw, turn_id) {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        let session_id = self
            .reducer
            .session_id(&raw)
            .cloned()
            .ok_or(RuntimeSessionError::NotConnected)?;
        self.client.abort(&raw).map_err(to_runtime_error)?;
        self.reducer.capabilities_prove(Capability::TurnInterrupt);
        let at_ms = now_ms();
        Ok(vec![
            self.reducer.accepted_command_effect(
                &session_id,
                RuntimeAcceptanceKind::SessionControl,
                command_id,
                &format!("abort|{}|{}", raw, turn_id.as_str()),
                at_ms,
            ),
            StateEffect::CapabilitiesUpdated {
                capabilities: self.reducer.capability_probe().to_capabilities(),
            },
        ]
        .into_iter()
        .chain(self.reducer.refresh_live_binding(&session_id, at_ms))
        .collect())
    }

    fn deliver_queue_entry(
        &mut self,
        command_id: &CommandId,
        entry: &QueueEntry,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_connected()?;
        if entry.intent != QueueIntent::NewTurn {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        let raw = self
            .reducer
            .raw_session_id(&entry.session_id)
            .map(str::to_owned)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        if self.session_raw.as_deref() != Some(raw.as_str()) {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        entry
            .body
            .ensure_sensitive("queue_entry.body")
            .map_err(RuntimeSessionError::Contract)?;
        let bytes = content
            .load(&entry.body)
            .map_err(RuntimeSessionError::Content)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| RuntimeSessionError::ProtocolViolation {
                detail: "OpenCode queue body is not UTF-8".to_owned(),
            })?;
        let admission = self
            .client
            .prompt_v2(&raw, text, PromptDelivery::Queue)
            .map_err(to_runtime_error)?;
        self.last_prompt_admission = Some(admission.clone());
        self.reducer.capabilities_prove(Capability::QueueWrite);
        self.reducer.capabilities_prove(Capability::TurnPrompt);
        let turn_effect = self
            .reducer
            .admit_prompt_turn(&raw, &admission.id, command_id)
            .map_err(|error| RuntimeSessionError::ProtocolViolation {
                detail: error.to_string(),
            })?;
        let turn_id = match &turn_effect {
            StateEffect::TurnUpserted { turn } => turn.id.clone(),
            _ => {
                return Err(RuntimeSessionError::ProtocolViolation {
                    detail: "OpenCode queue admission produced no turn".to_owned(),
                });
            }
        };
        let at_ms = now_ms();
        let mut delivered = entry.clone();
        delivered.state = QueueState::DeliveredAsNewTurn {
            turn_id,
            delivered_at_ms: at_ms,
        };
        delivered.editable = false;
        delivered.updated_at_ms = at_ms;
        Ok(vec![
            turn_effect,
            StateEffect::QueueEntryUpserted { entry: delivered },
            StateEffect::CapabilitiesUpdated {
                capabilities: self.reducer.capability_probe().to_capabilities(),
            },
        ])
    }

    fn drain_effects(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_connected()?;
        let stream = self
            .stream
            .as_mut()
            .ok_or(RuntimeSessionError::NotConnected)?;
        let event = stream.next_event().map_err(to_runtime_error)?.ok_or(
            RuntimeSessionError::ConnectionFault {
                reason: ConnectionFaultReason::TransportError,
            },
        )?;
        self.ingest_sse(event, now_ms(), content)
            .map_err(to_runtime_error)
    }

    fn close(&mut self) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_connected()?;
        self.stream = None;
        self.connected = false;
        Ok(Vec::new())
    }

    fn capability_probe(&self) -> CapabilityProbe {
        self.reducer.capability_probe()
    }
}

fn to_runtime_error(error: OpenCodeAdapterError) -> RuntimeSessionError {
    match error {
        OpenCodeAdapterError::NotConnected => RuntimeSessionError::NotConnected,
        OpenCodeAdapterError::CapabilityUnavailable => RuntimeSessionError::CapabilityUnavailable,
        OpenCodeAdapterError::Content(error) => RuntimeSessionError::Content(error),
        OpenCodeAdapterError::Contract(error) => RuntimeSessionError::Contract(error),
        OpenCodeAdapterError::HttpStatus { .. }
        | OpenCodeAdapterError::Http(_)
        | OpenCodeAdapterError::SseDisconnected
        | OpenCodeAdapterError::CursorlessEvent
        | OpenCodeAdapterError::SnapshotRequired
        | OpenCodeAdapterError::AlreadyConnected
        | OpenCodeAdapterError::Transport(_) => RuntimeSessionError::ConnectionFault {
            reason: ConnectionFaultReason::TransportError,
        },
        OpenCodeAdapterError::Decode(error) => RuntimeSessionError::ProtocolViolation {
            detail: error.to_string(),
        },
    }
}

fn protocol_error(detail: &str) -> OpenCodeAdapterError {
    OpenCodeAdapterError::Decode(crate::error::OpenCodeDecodeError::ReductionFailed(
        detail.to_owned(),
    ))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
