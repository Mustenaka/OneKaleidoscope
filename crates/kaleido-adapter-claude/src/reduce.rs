//! Reduction of versioned Claude Agent SDK frames into canonical effects.
//!
//! Claude's upstream union is intentionally not reproduced here.  The Node
//! bridge validates it with `@anthropic-ai/claude-agent-sdk` and emits a small,
//! versioned envelope; this reducer only reads the fields needed by the
//! canonical model through `serde_json::Value`.

use std::collections::BTreeMap;

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::content::ContentAccess;
use kaleido_adapter::IdentityMint;
use kaleido_proto::attention::{
    ApprovalRequest, AttentionAnswerEvidence, AttentionAnswerEvidenceSource, AttentionAnswerSource,
    AttentionItem, AttentionState, AttentionSubject, DecisionOption, DecisionSemantics,
    JoinFailureReason, JoinState, QuestionAnswer, QuestionPrompt, QuestionRequest,
};
use kaleido_proto::capability::{
    Capability, CapabilityEvidence, CapabilityUnavailableReason, EvidenceSource,
};
use kaleido_proto::command::{CommandAck, CommandOutcome, RuntimeAcceptanceKind};
use kaleido_proto::content::{ContentKind, ContentRef, Sensitivity};
use kaleido_proto::effect::{DiagnosticCode, DiagnosticRecord, StateEffect};
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::host::{
    ConnectionFaultReason, ConnectionState, Host, HostPlatform, HostReachability, LaunchSurface,
    Project, ProjectBinding, ProviderFamily, ProviderRuntime, SessionCounts,
};
use kaleido_proto::ids::{
    AttentionId, CommandId, HostId, ItemId, ProjectBindingId, ProjectId, ProviderBindingKind,
    ProviderRuntimeId, SessionId, TurnId,
};
use kaleido_proto::session::{
    HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason, OwnershipMode, Session,
    SessionStatus,
};
use kaleido_proto::turn::{
    Item, ItemBody, ItemStatus, MessagePhase, ToolDescriptor, ToolSurface, Turn, TurnOrigin,
    TurnStatus,
};
use kaleido_proto::ContractViolation;
use serde_json::Value;

use crate::error::ClaudeAdapterError;
use crate::transcript::{Transcript, TranscriptFrame};

/// How one reducer describes its connection to the host and project.
#[derive(Debug, Clone)]
pub struct ReducerConfig {
    pub host_display_name: String,
    pub host_platform: HostPlatform,
    pub project_display_name: String,
    pub identity_salt: String,
    pub evidence: EvidenceSource,
    pub launch_surface: LaunchSurface,
    pub turn_origin: TurnOrigin,
    pub base_at_ms: i64,
    pub runtime_version_label: Option<String>,
}

/// A provider-private discovery result.  The raw session id never leaves this
/// crate; callers receive only the canonical id and display metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSession {
    pub id: SessionId,
    pub title: Option<String>,
    pub last_modified_at_ms: i64,
}

#[derive(Debug)]
pub struct ClaudeReducer {
    config: ReducerConfig,
    mint: IdentityMint,
    host_id: HostId,
    runtime_id: ProviderRuntimeId,
    project_id: ProjectId,
    project_binding_id: ProjectBindingId,
    session_id: Option<SessionId>,
    raw_session_id: Option<String>,
    session: Option<Session>,
    turns: BTreeMap<String, Turn>,
    current_turn_raw: Option<String>,
    items: BTreeMap<ItemId, Item>,
    raw_items: BTreeMap<String, ItemId>,
    text_by_raw_item: BTreeMap<String, String>,
    next_sequence: u64,
    attention_by_request: BTreeMap<String, AttentionId>,
    attention: BTreeMap<AttentionId, AttentionItem>,
    local_prompt_commands: BTreeMap<String, CommandId>,
    local_attention_commands: BTreeMap<String, CommandId>,
    diagnostics: BTreeMap<String, u64>,
    discovered: Vec<DiscoveredSession>,
    discovered_raw: BTreeMap<SessionId, String>,
    probe: CapabilityProbe,
    published_capabilities: Vec<Capability>,
    bootstrapped: bool,
    lifecycle_ended: bool,
    authentication_required: bool,
}

impl ClaudeReducer {
    pub fn new(config: ReducerConfig) -> Self {
        let mint = IdentityMint::new(config.identity_salt.clone());
        let host_id = mint.host_id(&config.host_display_name);
        let runtime_id = mint.runtime_id(&format!("{}|claude-agent-sdk", config.host_display_name));
        let project_id = mint.project_id(&config.project_display_name);
        let project_binding_id =
            mint.project_binding_id(&format!("{}|{}", config.project_display_name, runtime_id));
        let probe = CapabilityProbe::new(runtime_id.clone(), config.base_at_ms, config.evidence);
        Self {
            config,
            mint,
            host_id,
            runtime_id,
            project_id,
            project_binding_id,
            session_id: None,
            raw_session_id: None,
            session: None,
            turns: BTreeMap::new(),
            current_turn_raw: None,
            items: BTreeMap::new(),
            raw_items: BTreeMap::new(),
            text_by_raw_item: BTreeMap::new(),
            next_sequence: 0,
            attention_by_request: BTreeMap::new(),
            attention: BTreeMap::new(),
            local_prompt_commands: BTreeMap::new(),
            local_attention_commands: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
            discovered: Vec::new(),
            discovered_raw: BTreeMap::new(),
            probe,
            published_capabilities: Vec::new(),
            bootstrapped: false,
            lifecycle_ended: false,
            authentication_required: false,
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

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub(crate) fn raw_session_id(&self) -> Option<&str> {
        self.raw_session_id.as_deref()
    }

    pub(crate) fn register_local_prompt(&mut self, raw_turn: &str, command_id: &CommandId) {
        self.local_prompt_commands
            .insert(raw_turn.to_owned(), command_id.clone());
    }

    pub(crate) fn is_active_turn(&self, turn_id: &TurnId) -> bool {
        self.current_turn_raw
            .as_deref()
            .map(|raw| self.mint.turn_id(raw))
            .as_ref()
            == Some(turn_id)
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
        let _ = self.probe.observe_runtime_acceptance(&outcome);
        StateEffect::CommandAcknowledged {
            ack: CommandAck {
                command_id: command_id.clone(),
                outcome,
                acked_at_ms: at_ms,
            },
        }
    }

    pub fn capability_probe(&self) -> CapabilityProbe {
        self.probe.clone()
    }

    pub fn discovered_sessions(&self) -> &[DiscoveredSession] {
        &self.discovered
    }

    pub(crate) fn raw_discovered_session(&self, session_id: &SessionId) -> Option<&str> {
        self.discovered_raw.get(session_id).map(String::as_str)
    }

    pub(crate) fn prepare_resume(&mut self, session_id: &SessionId, raw_session_id: &str) {
        self.session_id = Some(session_id.clone());
        self.raw_session_id = Some(raw_session_id.to_owned());
        self.session = None;
        self.lifecycle_ended = false;
        self.authentication_required = false;
        self.probe.reset_connection(self.probe.observed_at_ms());
    }

    /// Resolves a permission request for the runtime transport without exposing
    /// the provider request id to the rest of the broker.
    pub(crate) fn permission_request_id(&self, attention_id: &AttentionId) -> Option<&str> {
        self.attention_by_request
            .iter()
            .find_map(|(raw, id)| (id == attention_id).then_some(raw.as_str()))
    }

    pub(crate) fn attention(&self, attention_id: &AttentionId) -> Option<&AttentionItem> {
        self.attention.get(attention_id)
    }

    pub(crate) fn register_local_attention_answer(
        &mut self,
        attention_id: &AttentionId,
        command_id: &CommandId,
    ) -> Option<String> {
        let raw = self.permission_request_id(attention_id)?.to_owned();
        let attention = self.attention.get(attention_id)?;
        if !attention.state.is_open() || self.local_attention_commands.contains_key(&raw) {
            return None;
        }
        self.local_attention_commands
            .insert(raw.clone(), command_id.clone());
        Some(raw)
    }

    pub(crate) fn forget_local_attention_answer(&mut self, raw_request: &str) {
        self.local_attention_commands.remove(raw_request);
    }

    pub fn ingest(
        &mut self,
        transcript: &Transcript,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let mut effects = Vec::new();
        for frame in transcript.frames() {
            effects.extend(self.ingest_frame(frame, content)?);
        }
        Ok(effects)
    }

    pub fn ingest_frame(
        &mut self,
        frame: &TranscriptFrame,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let at_ms = self
            .config
            .base_at_ms
            .saturating_add(frame.recorded_offset_ms());
        self.probe.advance_observation(at_ms);
        let mut effects = self.ensure_bootstrapped(at_ms);
        effects.extend(self.dispatch(frame, at_ms, content)?);
        if self.probe.proven() != self.published_capabilities.as_slice() {
            self.published_capabilities = self.probe.proven().to_vec();
            effects.push(StateEffect::CapabilitiesUpdated {
                capabilities: self.probe.to_capabilities(),
            });
        }
        Self::validate_effects(&effects)?;
        Ok(effects)
    }

    pub fn process_exited(
        &mut self,
        exit_code: Option<i64>,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        if self.lifecycle_ended {
            return Ok(Vec::new());
        }
        self.lifecycle_ended = true;
        self.probe
            .mark_connection_unavailable(CapabilityUnavailableReason::RuntimeDisconnected);
        let reason = ConnectionFaultReason::ProcessExited { exit_code };
        let mut effects = vec![StateEffect::RuntimeUpserted {
            runtime: self.runtime_with_connection(ConnectionState::Unavailable {
                reason: reason.clone(),
                since_at_ms: at_ms,
            }),
        }];
        if let Some(session) = self.session.as_ref().map(|session| {
            let mut ended = session.clone();
            ended.status = SessionStatus::Offline;
            ended.live_binding = LiveBinding::NotBound {
                reason: LiveUnboundReason::RuntimeExited,
            };
            ended.updated_at_ms = ended.updated_at_ms.max(at_ms);
            ended.last_activity_at_ms = ended.last_activity_at_ms.max(at_ms);
            ended
        }) {
            effects.push(StateEffect::SessionUpserted {
                session: session.clone(),
            });
            effects.push(StateEffect::AttentionUpserted {
                item: AttentionItem {
                    id: self.mint.attention_id(&format!(
                        "connection-fault|{}|{}",
                        self.runtime_id, session.id
                    )),
                    host_id: self.host_id.clone(),
                    project_id: self.project_id.clone(),
                    session_id: Some(session.id.clone()),
                    turn_id: None,
                    workflow_id: None,
                    subject: AttentionSubject::ConnectionFault {
                        runtime_id: self.runtime_id.clone(),
                        reason,
                    },
                    state: AttentionState::Open,
                    created_at_ms: at_ms,
                    expires_at_ms: None,
                },
            });
            self.session = Some(session);
        }
        Self::validate_effects(&effects)?;
        Ok(effects)
    }

    pub fn clean_disconnected(
        &mut self,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        if self.lifecycle_ended {
            return Ok(Vec::new());
        }
        self.lifecycle_ended = true;
        self.probe
            .mark_connection_unavailable(CapabilityUnavailableReason::SubscriptionLost);
        let mut effects = vec![StateEffect::RuntimeUpserted {
            runtime: self.runtime_with_connection(ConnectionState::Disconnected),
        }];
        if let Some(session) = self.session.as_ref().map(|session| {
            let mut ended = session.clone();
            ended.status = SessionStatus::Offline;
            ended.live_binding = LiveBinding::NotBound {
                reason: LiveUnboundReason::SubscriptionLost,
            };
            ended.updated_at_ms = ended.updated_at_ms.max(at_ms);
            ended.last_activity_at_ms = ended.last_activity_at_ms.max(at_ms);
            ended
        }) {
            effects.push(StateEffect::SessionUpserted {
                session: session.clone(),
            });
            self.session = Some(session);
        }
        Self::validate_effects(&effects)?;
        Ok(effects)
    }

    pub(crate) fn mark_connection_unavailable(
        &mut self,
        reason: CapabilityUnavailableReason,
        at_ms: i64,
    ) -> Vec<StateEffect> {
        self.probe.mark_connection_unavailable(reason);
        let mut effects = vec![StateEffect::CapabilitiesUpdated {
            capabilities: self.probe.to_capabilities(),
        }];
        if let Some(session) = self.session.as_ref().map(|session| {
            let mut offline = session.clone();
            offline.status = SessionStatus::Offline;
            offline.live_binding = LiveBinding::NotBound {
                reason: LiveUnboundReason::SubscriptionLost,
            };
            offline.updated_at_ms = offline.updated_at_ms.max(at_ms);
            offline.last_activity_at_ms = offline.last_activity_at_ms.max(at_ms);
            offline
        }) {
            effects.push(StateEffect::SessionUpserted {
                session: session.clone(),
            });
            self.session = Some(session);
        }
        effects
    }

    fn ensure_bootstrapped(&mut self, at_ms: i64) -> Vec<StateEffect> {
        if self.bootstrapped {
            return Vec::new();
        }
        self.bootstrapped = true;
        vec![
            StateEffect::HostUpserted {
                host: Host {
                    id: self.host_id.clone(),
                    display_name: self.config.host_display_name.clone(),
                    platform: self.config.host_platform.clone(),
                    reachability: HostReachability::LanDirect,
                    protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
                    last_seen_at_ms: at_ms,
                },
            },
            StateEffect::RuntimeUpserted {
                runtime: self
                    .runtime_with_connection(ConnectionState::Connected { since_at_ms: at_ms }),
            },
        ]
    }

    fn runtime_with_connection(&self, connection: ConnectionState) -> ProviderRuntime {
        ProviderRuntime {
            id: self.runtime_id.clone(),
            host_id: self.host_id.clone(),
            family: ProviderFamily::ClaudeCode,
            version_label: self.config.runtime_version_label.clone(),
            launch_surface: self.config.launch_surface.clone(),
            connection,
            capabilities: self.probe.to_capabilities(),
            binding_handle: None,
        }
    }

    fn dispatch(
        &mut self,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        match frame.kind() {
            "ready" => self.reduce_ready(frame.payload(), at_ms, content),
            "session_started" | "session_resumed" => self.reduce_session(
                frame.payload(),
                at_ms,
                content,
                frame.kind() == "session_resumed",
            ),
            "session_list" => self.reduce_session_list(frame.payload(), at_ms, content),
            "prompt_sent" | "prompt_accepted" => self.reduce_prompt(
                frame.payload(),
                at_ms,
                content,
                frame.kind() == "prompt_accepted",
            ),
            "permission_request" => self.reduce_permission_request(frame.payload(), at_ms, content),
            "permission_result" => self.reduce_permission_result(frame.payload(), at_ms),
            "question_request" => self.reduce_question_request(frame.payload(), at_ms, content),
            "question_result" => self.reduce_question_result(frame.payload(), at_ms, content),
            "interrupt_result" => self.reduce_interrupt(frame.payload(), at_ms),
            "sdk_message" => self.reduce_sdk_message(frame.payload(), at_ms, content),
            "error" => {
                let detail = frame
                    .payload()
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("sidecar_error");
                Ok(vec![self.record_diagnostic(
                    DiagnosticCode::MalformedProviderMessage,
                    detail,
                    at_ms,
                    content,
                )?])
            }
            _ => Ok(vec![self.record_diagnostic(
                DiagnosticCode::UnknownUpstreamMessage,
                frame.kind(),
                at_ms,
                content,
            )?]),
        }
    }

    fn reduce_ready(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        if self.session_id.is_some() {
            return Ok(Vec::new());
        }
        // Captures made before sidecar v1 added `cwd` remain valid evidence.
        // A live bridge always supplies it, allowing the broker to model the
        // session before Claude assigns an upstream id on the first message.
        let Some(cwd) = string_field(payload, "cwd") else {
            return Ok(Vec::new());
        };
        if cwd.is_empty() {
            return Err(ClaudeAdapterError::ProtocolViolation {
                detail: "ready contains an empty cwd",
            });
        }
        let root_ref = content.store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            cwd.as_bytes(),
        )?;
        let session_id = self.mint.session_id("broker-managed-session");
        let session = Session {
            id: session_id.clone(),
            project_id: self.project_id.clone(),
            project_binding_id: self.project_binding_id.clone(),
            ownership: OwnershipMode::BrokerManaged,
            history_source: HistorySource {
                kind: HistorySourceKind::None,
                runtime_id: None,
                evidence: CapabilityEvidence {
                    source: self.config.evidence,
                    observed_at_ms: at_ms,
                    note_ref: None,
                },
            },
            live_binding: LiveBinding::NotBound {
                reason: LiveUnboundReason::NeverStarted,
            },
            status: SessionStatus::Idle,
            title: None,
            created_at_ms: at_ms,
            updated_at_ms: at_ms,
            last_activity_at_ms: at_ms,
            active_turn_id: None,
            queue_depth: 0,
            open_attention_count: 0,
            archived: false,
            binding_handle: None,
        };
        self.session_id = Some(session_id);
        self.session = Some(session.clone());
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
        Ok(vec![
            StateEffect::ProjectUpserted { project },
            StateEffect::SessionUpserted { session },
        ])
    }

    fn reduce_session(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
        resumed: bool,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let raw =
            string_field(payload, "session_id").ok_or(ClaudeAdapterError::MissingSessionId)?;
        let cwd = string_field(payload, "cwd").ok_or(ClaudeAdapterError::ProtocolViolation {
            detail: "session_started is missing cwd",
        })?;
        let root_ref = content.store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            cwd.as_bytes(),
        )?;
        let session_id = self
            .session_id
            .clone()
            .unwrap_or_else(|| self.mint.session_id(raw));
        let session_handle = self.mint.binding_handle(
            &self.runtime_id,
            kaleido_proto::ids::ProviderBindingKind::Session,
            raw,
        );
        self.raw_session_id = Some(raw.to_owned());
        self.session_id = Some(session_id.clone());
        self.probe.prove(Capability::LiveObserve);
        if resumed {
            self.probe.prove(Capability::HistoryResume);
        }
        let live_binding = live_binding(&self.probe, &self.runtime_id, at_ms, self.config.evidence);
        let title = string_field(payload, "title")
            .map(str::to_owned)
            .or_else(|| {
                self.session
                    .as_ref()
                    .and_then(|session| session.title.clone())
            });
        let created_at_ms = self
            .session
            .as_ref()
            .map_or(at_ms, |session| session.created_at_ms);
        let session = Session {
            id: session_id.clone(),
            project_id: self.project_id.clone(),
            project_binding_id: self.project_binding_id.clone(),
            ownership: OwnershipMode::BrokerManaged,
            history_source: HistorySource {
                kind: HistorySourceKind::ProviderApi,
                runtime_id: Some(self.runtime_id.clone()),
                evidence: CapabilityEvidence {
                    source: self.config.evidence,
                    observed_at_ms: at_ms,
                    note_ref: None,
                },
            },
            live_binding,
            status: SessionStatus::Idle,
            title,
            created_at_ms,
            updated_at_ms: at_ms,
            last_activity_at_ms: at_ms,
            active_turn_id: None,
            queue_depth: 0,
            open_attention_count: 0,
            archived: false,
            binding_handle: Some(session_handle),
        };
        self.session = Some(session.clone());
        let binding = ProjectBinding {
            id: self.project_binding_id.clone(),
            project_id: self.project_id.clone(),
            runtime_id: self.runtime_id.clone(),
            root_ref,
        };
        let project = Project {
            id: self.project_id.clone(),
            display_name: self.config.project_display_name.clone(),
            bindings: vec![binding],
            session_counts: SessionCounts::default(),
            workflow_count: 0,
            attention_count: 0,
            last_activity_at_ms: at_ms,
        };
        Ok(vec![
            StateEffect::ProjectUpserted { project },
            StateEffect::SessionUpserted { session },
        ])
    }

    fn reduce_session_list(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let cwd = string_field(payload, "cwd").ok_or(ClaudeAdapterError::ProtocolViolation {
            detail: "session list is missing cwd",
        })?;
        let entries = payload.get("sessions").and_then(Value::as_array).ok_or(
            ClaudeAdapterError::ProtocolViolation {
                detail: "session list is missing sessions",
            },
        )?;
        self.probe.prove(Capability::HistoryList);
        self.discovered.clear();
        self.discovered_raw.clear();
        let root_ref = content.store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            cwd.as_bytes(),
        )?;
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
        let mut effects = vec![StateEffect::ProjectUpserted { project }];
        for entry in entries {
            let raw = string_field(entry, "session_id")
                .filter(|raw| !raw.is_empty())
                .ok_or(ClaudeAdapterError::ProtocolViolation {
                    detail: "session list contains an invalid session id",
                })?;
            let modified_at_ms = entry.get("last_modified").and_then(Value::as_i64).ok_or(
                ClaudeAdapterError::ProtocolViolation {
                    detail: "session list contains an invalid timestamp",
                },
            )?;
            let session_id = self.mint.session_id(raw);
            let title = string_field(entry, "summary").map(str::to_owned);
            self.discovered.push(DiscoveredSession {
                id: session_id.clone(),
                title: title.clone(),
                last_modified_at_ms: modified_at_ms,
            });
            self.discovered_raw
                .insert(session_id.clone(), raw.to_owned());
            effects.push(StateEffect::SessionUpserted {
                session: Session {
                    id: session_id,
                    project_id: self.project_id.clone(),
                    project_binding_id: self.project_binding_id.clone(),
                    ownership: OwnershipMode::ProviderManaged,
                    history_source: HistorySource {
                        kind: HistorySourceKind::ProviderApi,
                        runtime_id: Some(self.runtime_id.clone()),
                        evidence: CapabilityEvidence {
                            source: self.config.evidence,
                            observed_at_ms: at_ms,
                            note_ref: None,
                        },
                    },
                    live_binding: LiveBinding::NotBound {
                        reason: LiveUnboundReason::NeverStarted,
                    },
                    status: SessionStatus::Offline,
                    title,
                    created_at_ms: modified_at_ms,
                    updated_at_ms: modified_at_ms,
                    last_activity_at_ms: modified_at_ms,
                    active_turn_id: None,
                    queue_depth: 0,
                    open_attention_count: 0,
                    archived: false,
                    binding_handle: Some(self.mint.binding_handle(
                        &self.runtime_id,
                        ProviderBindingKind::Session,
                        raw,
                    )),
                },
            });
        }
        Ok(effects)
    }

    fn reduce_prompt(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
        accepted: bool,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let raw_turn =
            string_field(payload, "turn_id").ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "prompt frame is missing turn_id",
            })?;
        let text = string_field(payload, "text").unwrap_or_default();
        self.probe.prove(Capability::TurnPrompt);
        if accepted {
            self.probe.prove(Capability::LiveControl);
        }
        // The bridge acknowledges a prompt as soon as it enters the SDK input
        // queue.  The SDK's first `system/init` frame (which carries the
        // session id) can arrive afterwards, so keep the turn binding pending
        // rather than rejecting an otherwise valid live stream.
        if self.session_id.is_none() {
            self.current_turn_raw = Some(raw_turn.to_owned());
            return Ok(Vec::new());
        }
        let mut effects = self.ensure_turn(raw_turn, at_ms, content)?;
        if !text.is_empty() {
            effects.extend(self.upsert_user_item(raw_turn, "prompt", text, at_ms, content)?);
        }
        if accepted {
            effects.extend(self.update_session_status(SessionStatus::Running, at_ms));
        }
        Ok(effects)
    }

    fn reduce_permission_request(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let raw_request =
            string_field(payload, "request_id").ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "permission request is missing request_id",
            })?;
        let tool_name = string_field(payload, "tool_name").unwrap_or("unknown");
        if tool_name.eq_ignore_ascii_case("askuserquestion")
            || tool_name.eq_ignore_ascii_case("ask_user_question")
        {
            return Ok(vec![self.record_diagnostic(
                DiagnosticCode::UnknownUpstreamLabel,
                "AskUserQuestion question set is unsupported until QuestionSet is available",
                at_ms,
                content,
            )?]);
        }
        let Some(session_id) = self.session_id.clone() else {
            return Ok(vec![self.record_diagnostic(
                DiagnosticCode::JoinDeferred,
                "permission request arrived before session",
                at_ms,
                content,
            )?]);
        };
        self.probe.prove(Capability::InteractionApproval);
        let attention_id = self.mint.attention_id(raw_request);
        let target_raw = string_field(payload, "tool_use_id").unwrap_or(raw_request);
        let target_item_id = self
            .raw_items
            .get(target_raw)
            .cloned()
            .unwrap_or_else(|| self.mint.item_id(target_raw));
        let (join, turn_id) = if let Some(item) = self.items.get(&target_item_id) {
            (
                JoinState::Joined {
                    item_id: item.id.clone(),
                },
                Some(item.turn_id.clone()),
            )
        } else {
            (
                JoinState::Unjoined {
                    reason: JoinFailureReason::ItemNotYetSeen,
                },
                self.current_turn_raw
                    .as_deref()
                    .map(|raw| self.mint.turn_id(raw)),
            )
        };
        let summary = string_field(payload, "title")
            .or_else(|| string_field(payload, "tool_name"))
            .unwrap_or("Claude requested permission");
        let summary_ref = self.store_text(content, ContentKind::PlainText, summary)?;
        let detail_ref = payload
            .get("input")
            .map(|input| self.store_json(content, ContentKind::ToolArguments, input))
            .transpose()?;
        let binding_handle = self.mint.binding_handle(
            &self.runtime_id,
            kaleido_proto::ids::ProviderBindingKind::InteractionRequest,
            raw_request,
        );
        let request = ApprovalRequest {
            request_key: self.mint.request_key(raw_request),
            target_item_id,
            join,
            options: permission_options(),
            summary_ref,
            detail_ref,
            binding_handle,
        };
        let item = AttentionItem {
            id: attention_id.clone(),
            host_id: self.host_id.clone(),
            project_id: self.project_id.clone(),
            session_id: Some(session_id),
            turn_id,
            workflow_id: None,
            subject: AttentionSubject::Approval { request },
            state: AttentionState::Open,
            created_at_ms: at_ms,
            expires_at_ms: None,
        };
        self.attention_by_request
            .insert(raw_request.to_owned(), attention_id.clone());
        self.attention.insert(attention_id, item.clone());
        let mut effects = vec![StateEffect::AttentionUpserted { item }];
        effects.extend(self.update_session_status(SessionStatus::WaitingApproval, at_ms));
        Ok(effects)
    }

    fn reduce_permission_result(
        &mut self,
        payload: &Value,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let raw_request =
            string_field(payload, "request_id").ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "permission result is missing request_id",
            })?;
        let Some(attention_id) = self.attention_by_request.get(raw_request).cloned() else {
            return Ok(Vec::new());
        };
        let decision =
            string_field(payload, "decision").ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "permission result is missing decision",
            })?;
        if !matches!(decision, "allow" | "allow_always" | "deny" | "cancel") {
            return Err(ClaudeAdapterError::ProtocolViolation {
                detail: "permission result has an unknown decision",
            });
        }
        let local_command = self.local_attention_commands.remove(raw_request);
        let Some(attention) = self.attention.get_mut(&attention_id) else {
            return Ok(Vec::new());
        };
        attention.state = AttentionState::Answered {
            option_id: Some(permission_option_id(decision).to_owned()),
            free_form_ref: None,
            question_answers: Vec::new(),
            decided_at_ms: at_ms,
            answer_source: local_command.clone().map_or_else(
                || AttentionAnswerSource::ObservedExternal {
                    evidence: AttentionAnswerEvidence {
                        observer_host_id: self.host_id.clone(),
                        observed_at_ms: at_ms,
                        source: match self.config.evidence {
                            EvidenceSource::RecordedFixture => {
                                AttentionAnswerEvidenceSource::RecordedFixture
                            }
                            _ => AttentionAnswerEvidenceSource::ObservedInTraffic,
                        },
                    },
                },
                |command_id| AttentionAnswerSource::LocalCommand { command_id },
            ),
        };
        let mut effects = vec![StateEffect::AttentionUpserted {
            item: attention.clone(),
        }];
        if decision == "deny" || decision == "cancel" {
            if let AttentionSubject::Approval { request } = &attention.subject {
                if let Some(item) = self.items.get_mut(&request.target_item_id) {
                    item.status = ItemStatus::Declined;
                    item.updated_at_ms = at_ms;
                    effects.push(StateEffect::ItemUpserted { item: item.clone() });
                }
            }
        }
        effects.extend(self.update_session_status(SessionStatus::Running, at_ms));
        Ok(effects)
    }

    fn reduce_question_request(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let raw_request =
            string_field(payload, "request_id").ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "question request is missing request_id",
            })?;
        let raw_questions = payload
            .get("questions")
            .and_then(Value::as_array)
            .filter(|questions| (1..=4).contains(&questions.len()))
            .ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "question request has an invalid question set",
            })?;
        let session_id = self
            .session_id
            .clone()
            .ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "question request arrived before session",
            })?;
        let mut questions = Vec::with_capacity(raw_questions.len());
        for (question_index, raw_question) in raw_questions.iter().enumerate() {
            let question = string_field(raw_question, "question").filter(|text| !text.is_empty());
            let header = string_field(raw_question, "header").filter(|text| !text.is_empty());
            let multi_select = raw_question.get("multiSelect").and_then(Value::as_bool);
            let raw_options = raw_question
                .get("options")
                .and_then(Value::as_array)
                .filter(|options| (2..=4).contains(&options.len()));
            let (Some(question), Some(_), Some(multi_select), Some(raw_options)) =
                (question, header, multi_select, raw_options)
            else {
                return Err(ClaudeAdapterError::ProtocolViolation {
                    detail: "question request contains a malformed prompt",
                });
            };
            let mut options = Vec::with_capacity(raw_options.len());
            for (option_index, raw_option) in raw_options.iter().enumerate() {
                let label = string_field(raw_option, "label").filter(|text| !text.is_empty());
                let description = string_field(raw_option, "description");
                let (Some(label), Some(_)) = (label, description) else {
                    return Err(ClaudeAdapterError::ProtocolViolation {
                        detail: "question request contains a malformed option",
                    });
                };
                if options
                    .iter()
                    .any(|option: &DecisionOption| option.label == label)
                {
                    return Err(ClaudeAdapterError::ProtocolViolation {
                        detail: "question request contains duplicate option labels",
                    });
                }
                options.push(DecisionOption {
                    option_id: self.mint.request_key(&format!(
                        "{raw_request}|question:{question_index}|option:{option_index}"
                    )),
                    label: label.to_owned(),
                    semantics: DecisionSemantics::Choose,
                });
            }
            questions.push(QuestionPrompt {
                question_key: self
                    .mint
                    .request_key(&format!("{raw_request}|question:{question_index}")),
                prompt_ref: self.store_text(content, ContentKind::PlainText, question)?,
                options,
                multi_select,
                free_form_allowed: true,
            });
        }
        let attention_id = self.mint.attention_id(raw_request);
        let request = QuestionRequest {
            request_key: self.mint.request_key(raw_request),
            questions,
            binding_handle: self.mint.binding_handle(
                &self.runtime_id,
                ProviderBindingKind::InteractionRequest,
                raw_request,
            ),
        };
        let item = AttentionItem {
            id: attention_id.clone(),
            host_id: self.host_id.clone(),
            project_id: self.project_id.clone(),
            session_id: Some(session_id),
            turn_id: self
                .current_turn_raw
                .as_deref()
                .map(|raw| self.mint.turn_id(raw)),
            workflow_id: None,
            subject: AttentionSubject::Question { request },
            state: AttentionState::Open,
            created_at_ms: at_ms,
            expires_at_ms: None,
        };
        self.probe.prove(Capability::InteractionQuestion);
        self.attention_by_request
            .insert(raw_request.to_owned(), attention_id.clone());
        self.attention.insert(attention_id, item.clone());
        let mut effects = vec![StateEffect::AttentionUpserted { item }];
        effects.extend(self.update_session_status(SessionStatus::WaitingUser, at_ms));
        Ok(effects)
    }

    fn reduce_question_result(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let raw_request =
            string_field(payload, "request_id").ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "question result is missing request_id",
            })?;
        let attention_id = self.attention_by_request.get(raw_request).cloned().ok_or(
            ClaudeAdapterError::ProtocolViolation {
                detail: "question result refers to an unknown request",
            },
        )?;
        let attention = self.attention.get(&attention_id).cloned().ok_or(
            ClaudeAdapterError::ProtocolViolation {
                detail: "question result has no attention item",
            },
        )?;
        let AttentionSubject::Question { request } = &attention.subject else {
            return Err(ClaudeAdapterError::ProtocolViolation {
                detail: "question result refers to a non-question attention",
            });
        };
        let raw_answers = payload
            .get("answers")
            .and_then(Value::as_array)
            .filter(|answers| answers.len() == request.questions.len())
            .ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "question result does not cover the question set",
            })?;
        let mut question_answers = Vec::with_capacity(raw_answers.len());
        for (question_index, (question, raw_answer)) in
            request.questions.iter().zip(raw_answers.iter()).enumerate()
        {
            if raw_answer.get("question_index").and_then(Value::as_u64)
                != u64::try_from(question_index).ok()
            {
                return Err(ClaudeAdapterError::ProtocolViolation {
                    detail: "question result index is out of scope",
                });
            }
            let values = raw_answer
                .get("values")
                .and_then(Value::as_array)
                .filter(|values| !values.is_empty())
                .ok_or(ClaudeAdapterError::ProtocolViolation {
                    detail: "question result contains an empty answer",
                })?;
            let mut option_ids = Vec::new();
            let mut free_form = Vec::new();
            for value in values {
                let value = value.as_str().filter(|value| !value.is_empty()).ok_or(
                    ClaudeAdapterError::ProtocolViolation {
                        detail: "question result contains a malformed answer",
                    },
                )?;
                if let Some(option) = question.options.iter().find(|option| option.label == value) {
                    option_ids.push(option.option_id.clone());
                } else {
                    free_form.push(value);
                }
            }
            let free_form_ref = if free_form.is_empty() {
                None
            } else {
                Some(self.store_text(content, ContentKind::PlainText, &free_form.join(", "))?)
            };
            question_answers.push(QuestionAnswer {
                question_key: question.question_key.clone(),
                option_ids,
                free_form_ref,
            });
        }
        let local_command = self.local_attention_commands.remove(raw_request);
        let answer_source = local_command.clone().map_or_else(
            || AttentionAnswerSource::ObservedExternal {
                evidence: AttentionAnswerEvidence {
                    observer_host_id: self.host_id.clone(),
                    observed_at_ms: at_ms,
                    source: match self.config.evidence {
                        EvidenceSource::RecordedFixture => {
                            AttentionAnswerEvidenceSource::RecordedFixture
                        }
                        _ => AttentionAnswerEvidenceSource::ObservedInTraffic,
                    },
                },
            },
            |command_id| AttentionAnswerSource::LocalCommand { command_id },
        );
        let stored =
            self.attention
                .get_mut(&attention_id)
                .ok_or(ClaudeAdapterError::ProtocolViolation {
                    detail: "question result lost its attention item",
                })?;
        stored.state = AttentionState::Answered {
            option_id: None,
            free_form_ref: None,
            question_answers,
            decided_at_ms: at_ms,
            answer_source,
        };
        let mut effects = vec![StateEffect::AttentionUpserted {
            item: stored.clone(),
        }];
        effects.extend(self.update_session_status(SessionStatus::Running, at_ms));
        Ok(effects)
    }

    fn reduce_interrupt(
        &mut self,
        payload: &Value,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        self.probe.prove(Capability::TurnInterrupt);
        let cancelled = payload
            .get("cancelled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if cancelled {
            if let Some(raw_turn) = self.current_turn_raw.as_deref() {
                if let Some(turn) = self.turns.get_mut(raw_turn) {
                    turn.status = TurnStatus::Cancelled;
                    turn.completed_at_ms = Some(at_ms);
                    turn.error = None;
                    let mut effects = vec![StateEffect::TurnUpserted { turn: turn.clone() }];
                    effects.extend(self.update_session_status(SessionStatus::Cancelled, at_ms));
                    return Ok(effects);
                }
            }
        }
        Ok(Vec::new())
    }

    fn reduce_sdk_message(
        &mut self,
        payload: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let message = payload
            .get("message")
            .ok_or(ClaudeAdapterError::MalformedFrame)?;
        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ClaudeAdapterError::MalformedFrame)?;
        let raw_turn = string_field(payload, "turn_id")
            .map(str::to_owned)
            .or_else(|| self.current_turn_raw.clone())
            .unwrap_or_else(|| "sdk-turn".to_owned());
        match kind {
            "user" => self.reduce_user_message(message, &raw_turn, at_ms, content),
            "assistant" => self.reduce_assistant_message(message, &raw_turn, at_ms, content),
            "stream_event" => self.reduce_stream_event(message, &raw_turn, at_ms, content),
            "tool_progress" => self.reduce_tool_progress(message, at_ms),
            "tool_use_summary" => self.reduce_tool_summary(message, at_ms, content),
            "result" => self.reduce_result(message, &raw_turn, at_ms, content),
            "system" => self.reduce_system_message(message, at_ms, content),
            _ => Ok(vec![self.record_diagnostic(
                DiagnosticCode::UnknownUpstreamMessage,
                kind,
                at_ms,
                content,
            )?]),
        }
    }

    fn reduce_system_message(
        &mut self,
        message: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let subtype = string_field(message, "subtype").unwrap_or("unknown");
        if subtype == "init" {
            self.probe.prove(Capability::LiveObserve);
            return Ok(Vec::new());
        }
        Ok(vec![self.record_diagnostic(
            DiagnosticCode::UnknownUpstreamMessage,
            subtype,
            at_ms,
            content,
        )?])
    }

    fn reduce_user_message(
        &mut self,
        message: &Value,
        raw_turn: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let mut effects = self.ensure_turn(raw_turn, at_ms, content)?;
        if let Some(text) = text_from_message(message.get("message")) {
            effects.extend(self.upsert_user_item(
                raw_turn,
                string_field(message, "uuid").unwrap_or("user"),
                text,
                at_ms,
                content,
            )?);
        }
        if let Some(blocks) = message
            .get("message")
            .and_then(|value| value.get("content"))
            .and_then(Value::as_array)
        {
            for block in blocks {
                if string_field(block, "type") == Some("tool_result") {
                    effects.extend(self.reduce_tool_result(block, at_ms, content)?);
                }
            }
        }
        Ok(effects)
    }

    fn reduce_assistant_message(
        &mut self,
        message: &Value,
        raw_turn: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let mut effects = self.ensure_turn(raw_turn, at_ms, content)?;
        if string_field(message, "error") == Some("authentication_failed") {
            self.authentication_required = true;
            self.probe
                .mark_connection_unavailable(CapabilityUnavailableReason::AuthenticationRequired);
        }
        let message_id = string_field(message, "uuid")
            .or_else(|| string_field(message.get("message").unwrap_or(&Value::Null), "id"))
            .unwrap_or("assistant");
        let Some(blocks) = message
            .get("message")
            .and_then(|value| value.get("content"))
            .and_then(Value::as_array)
        else {
            return Ok(effects);
        };
        for (index, block) in blocks.iter().enumerate() {
            let block_type = string_field(block, "type").unwrap_or("unknown");
            let raw_item = string_field(block, "id")
                .or_else(|| string_field(block, "tool_use_id"))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}:{}", message_id, index));
            match block_type {
                "text" => {
                    if let Some(text) = string_field(block, "text") {
                        effects.extend(self.upsert_agent_item(
                            raw_turn,
                            &raw_item,
                            text,
                            ItemStatus::Completed,
                            at_ms,
                            content,
                        )?);
                    }
                }
                "thinking" => {
                    if let Some(text) = string_field(block, "thinking") {
                        effects.extend(
                            self.upsert_reasoning_item(raw_turn, &raw_item, text, at_ms, content)?,
                        );
                    }
                }
                "tool_use" => {
                    effects
                        .extend(self.upsert_tool_item(raw_turn, block, &raw_item, at_ms, content)?);
                }
                _ => {
                    effects.push(self.record_diagnostic(
                        DiagnosticCode::UnknownUpstreamLabel,
                        block_type,
                        at_ms,
                        content,
                    )?);
                }
            }
        }
        Ok(effects)
    }

    fn reduce_stream_event(
        &mut self,
        message: &Value,
        raw_turn: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let event = message
            .get("event")
            .ok_or(ClaudeAdapterError::MalformedFrame)?;
        if string_field(event, "type") != Some("content_block_delta") {
            return Ok(Vec::new());
        }
        let Some(delta) = event.get("delta") else {
            return Ok(Vec::new());
        };
        if string_field(delta, "type") != Some("text_delta") {
            return Ok(Vec::new());
        }
        let Some(text) = string_field(delta, "text") else {
            return Ok(Vec::new());
        };
        let index = event
            .get("index")
            .and_then(|value| {
                value
                    .as_u64()
                    .map(|number| number.to_string())
                    .or_else(|| value.as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| "0".to_owned());
        let raw_item = format!("stream:{index}:{raw_turn}");
        let combined = {
            let entry = self.text_by_raw_item.entry(raw_item.clone()).or_default();
            entry.push_str(text);
            entry.clone()
        };
        self.upsert_agent_item(
            raw_turn,
            &raw_item,
            &combined,
            ItemStatus::InProgress,
            at_ms,
            content,
        )
    }

    fn reduce_tool_progress(
        &mut self,
        message: &Value,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let Some(raw_tool) = string_field(message, "tool_use_id") else {
            return Ok(Vec::new());
        };
        let Some(item_id) = self.raw_items.get(raw_tool).cloned() else {
            return Ok(Vec::new());
        };
        let Some(item) = self.items.get_mut(&item_id) else {
            return Ok(Vec::new());
        };
        item.status = ItemStatus::InProgress;
        item.updated_at_ms = at_ms;
        Ok(vec![StateEffect::ItemUpserted { item: item.clone() }])
    }

    fn reduce_tool_summary(
        &mut self,
        message: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let Some(summary) = string_field(message, "summary") else {
            return Ok(Vec::new());
        };
        let Some(raw_tool) = message
            .get("preceding_tool_use_ids")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
        else {
            return Ok(Vec::new());
        };
        let Some(item_id) = self.raw_items.get(raw_tool).cloned() else {
            return Ok(Vec::new());
        };
        let output = self.store_text(content, ContentKind::ToolOutput, summary)?;
        let Some(item) = self.items.get_mut(&item_id) else {
            return Ok(Vec::new());
        };
        if let ItemBody::ToolCall {
            output: item_output,
            ..
        } = &mut item.body
        {
            *item_output = Some(output);
        }
        item.status = ItemStatus::Completed;
        item.updated_at_ms = at_ms;
        Ok(vec![StateEffect::ItemUpserted { item: item.clone() }])
    }

    fn reduce_tool_result(
        &mut self,
        block: &Value,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let Some(raw_tool) = string_field(block, "tool_use_id") else {
            return Ok(Vec::new());
        };
        let Some(item_id) = self.raw_items.get(raw_tool).cloned() else {
            return Ok(Vec::new());
        };
        let output = block
            .get("content")
            .map(|value| self.store_json(content, ContentKind::ToolOutput, value))
            .transpose()?;
        let Some(item) = self.items.get_mut(&item_id) else {
            return Ok(Vec::new());
        };
        if let ItemBody::ToolCall {
            output: item_output,
            ..
        } = &mut item.body
        {
            *item_output = output;
        }
        item.status = if block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            ItemStatus::Failed
        } else {
            ItemStatus::Completed
        };
        item.updated_at_ms = at_ms;
        Ok(vec![StateEffect::ItemUpserted { item: item.clone() }])
    }

    fn reduce_result(
        &mut self,
        message: &Value,
        raw_turn: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let _ = self.ensure_turn(raw_turn, at_ms, content)?;
        let subtype = string_field(message, "subtype").unwrap_or("error");
        let status = match subtype {
            "success"
                if !message
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) =>
            {
                TurnStatus::Completed
            }
            "error_during_execution"
                if message
                    .get("stop_reason")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| reason == "interrupt") =>
            {
                TurnStatus::Cancelled
            }
            _ => TurnStatus::Failed,
        };
        let error = if status == TurnStatus::Failed {
            Some(CanonicalError {
                code: if self.authentication_required {
                    ErrorCode::AuthRequired
                } else {
                    ErrorCode::UpstreamRejected
                },
                retriable: false,
                detail_ref: message
                    .get("errors")
                    .map(|errors| self.store_json(content, ContentKind::StructuredSummary, errors))
                    .transpose()?,
                at_ms,
            })
        } else {
            None
        };
        let Some(turn) = self.turns.get_mut(raw_turn) else {
            return Ok(Vec::new());
        };
        turn.status = status;
        turn.completed_at_ms = Some(at_ms);
        turn.error = error;
        let status = turn.status;
        let mut effects = vec![StateEffect::TurnUpserted { turn: turn.clone() }];
        effects.extend(self.update_session_status(
            if status == TurnStatus::Completed {
                SessionStatus::Idle
            } else if status == TurnStatus::Cancelled {
                SessionStatus::Cancelled
            } else {
                SessionStatus::Failed
            },
            at_ms,
        ));
        Ok(effects)
    }

    fn ensure_turn(
        &mut self,
        raw_turn: &str,
        at_ms: i64,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let session_id = self
            .session_id
            .clone()
            .ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "SDK traffic arrived before session_started",
            })?;
        self.current_turn_raw = Some(raw_turn.to_owned());
        if let Some(turn) = self.turns.get(raw_turn) {
            if turn.status == TurnStatus::Pending {
                let mut running = turn.clone();
                running.status = TurnStatus::Running;
                running.started_at_ms.get_or_insert(at_ms);
                return Ok(vec![StateEffect::TurnUpserted { turn: running }]);
            }
            return Ok(Vec::new());
        }
        let turn_id = self.mint.turn_id(raw_turn);
        let handle = self.mint.binding_handle(
            &self.runtime_id,
            kaleido_proto::ids::ProviderBindingKind::Turn,
            raw_turn,
        );
        let turn = Turn {
            id: turn_id,
            session_id,
            status: TurnStatus::Running,
            origin: self
                .local_prompt_commands
                .get(raw_turn)
                .cloned()
                .map_or_else(
                    || self.config.turn_origin.clone(),
                    |command_id| TurnOrigin::RemoteCommand { command_id },
                ),
            started_at_ms: Some(at_ms),
            completed_at_ms: None,
            item_ids: Vec::new(),
            error: None,
            binding_handle: Some(handle),
        };
        self.turns.insert(raw_turn.to_owned(), turn.clone());
        Ok(vec![StateEffect::TurnUpserted { turn }])
    }

    fn upsert_user_item(
        &mut self,
        raw_turn: &str,
        raw_item: &str,
        text: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let body = ItemBody::UserMessage {
            content: self.store_text(content, ContentKind::PlainText, text)?,
        };
        self.upsert_item(raw_turn, raw_item, body, ItemStatus::Completed, at_ms)
    }

    fn upsert_agent_item(
        &mut self,
        raw_turn: &str,
        raw_item: &str,
        text: &str,
        status: ItemStatus,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let body = ItemBody::AgentMessage {
            content: self.store_text(content, ContentKind::Markdown, text)?,
            phase: if status == ItemStatus::Completed {
                MessagePhase::FinalAnswer
            } else {
                MessagePhase::Commentary
            },
        };
        self.upsert_item(raw_turn, raw_item, body, status, at_ms)
    }

    fn upsert_reasoning_item(
        &mut self,
        raw_turn: &str,
        raw_item: &str,
        text: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let body = ItemBody::Reasoning {
            content: self.store_text(content, ContentKind::PlainText, text)?,
        };
        self.upsert_item(raw_turn, raw_item, body, ItemStatus::Completed, at_ms)
    }

    fn upsert_tool_item(
        &mut self,
        raw_turn: &str,
        block: &Value,
        raw_item: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let name = string_field(block, "name").unwrap_or("unknown");
        let arguments = block
            .get("input")
            .map(|input| self.store_json(content, ContentKind::ToolArguments, input))
            .transpose()?;
        let body = ItemBody::ToolCall {
            tool: ToolDescriptor {
                name: name.to_owned(),
                surface: ToolSurface::Builtin,
            },
            arguments,
            output: None,
            exit_code: None,
        };
        self.upsert_item(raw_turn, raw_item, body, ItemStatus::InProgress, at_ms)
    }

    fn upsert_item(
        &mut self,
        raw_turn: &str,
        raw_item: &str,
        body: ItemBody,
        status: ItemStatus,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, ClaudeAdapterError> {
        let session_id = self
            .session_id
            .clone()
            .ok_or(ClaudeAdapterError::ProtocolViolation {
                detail: "item arrived before session_started",
            })?;
        let turn_id = self.mint.turn_id(raw_turn);
        let item_id = self
            .raw_items
            .get(raw_item)
            .cloned()
            .unwrap_or_else(|| self.mint.item_id(raw_item));
        let binding_handle = self.mint.binding_handle(
            &self.runtime_id,
            kaleido_proto::ids::ProviderBindingKind::Item,
            raw_item,
        );
        let item = self.items.entry(item_id.clone()).or_insert_with(|| Item {
            id: item_id.clone(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            sequence: 0,
            status: ItemStatus::Pending,
            body: body.clone(),
            created_at_ms: at_ms,
            updated_at_ms: at_ms,
            binding_handle: Some(binding_handle.clone()),
        });
        if item.sequence == 0 {
            self.next_sequence = self.next_sequence.saturating_add(1);
            item.sequence = self.next_sequence;
        }
        item.body = body;
        item.status = status;
        item.updated_at_ms = at_ms;
        self.raw_items.insert(raw_item.to_owned(), item_id.clone());
        if let Some(turn) = self.turns.get_mut(raw_turn) {
            if !turn.item_ids.contains(&item_id) {
                turn.item_ids.push(item_id.clone());
            }
        }
        Ok(vec![StateEffect::ItemUpserted { item: item.clone() }])
    }

    fn update_session_status(&mut self, status: SessionStatus, at_ms: i64) -> Vec<StateEffect> {
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        session.status = status;
        session.updated_at_ms = session.updated_at_ms.max(at_ms);
        session.last_activity_at_ms = session.last_activity_at_ms.max(at_ms);
        session.active_turn_id = self
            .current_turn_raw
            .as_deref()
            .map(|raw| self.mint.turn_id(raw));
        vec![StateEffect::SessionUpserted {
            session: session.clone(),
        }]
    }

    fn record_diagnostic(
        &mut self,
        code: DiagnosticCode,
        detail: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<StateEffect, ClaudeAdapterError> {
        let key = format!("{code:?}");
        let count = {
            let entry = self.diagnostics.entry(key).or_insert(0);
            *entry = entry.saturating_add(1);
            *entry
        };
        let detail_ref = self.store_text(content, ContentKind::StructuredSummary, detail)?;
        Ok(StateEffect::DiagnosticRecorded {
            diagnostic: DiagnosticRecord {
                runtime_id: Some(self.runtime_id.clone()),
                session_id: self.session_id.clone(),
                code,
                count,
                first_at_ms: at_ms,
                last_at_ms: at_ms,
                detail_ref: Some(detail_ref),
            },
        })
    }

    fn store_text(
        &self,
        content: &mut dyn ContentAccess,
        kind: ContentKind,
        text: &str,
    ) -> Result<ContentRef, ClaudeAdapterError> {
        Ok(content.store(kind, Sensitivity::Sensitive, text.as_bytes())?)
    }

    fn store_json(
        &self,
        content: &mut dyn ContentAccess,
        kind: ContentKind,
        value: &Value,
    ) -> Result<ContentRef, ClaudeAdapterError> {
        let bytes =
            serde_json::to_vec(value).map_err(|_| ClaudeAdapterError::ProtocolViolation {
                detail: "SDK JSON field could not be encoded",
            })?;
        Ok(content.store(kind, Sensitivity::Sensitive, &bytes)?)
    }

    fn validate_effects(effects: &[StateEffect]) -> Result<(), ContractViolation> {
        for effect in effects {
            effect.validate_for_log()?;
        }
        Ok(())
    }
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn text_from_message(value: Option<&Value>) -> Option<&str> {
    let value = value?;
    if let Some(text) = value.get("content").and_then(Value::as_str) {
        return Some(text);
    }
    value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.iter().find_map(|block| string_field(block, "text")))
}

fn live_binding(
    probe: &CapabilityProbe,
    runtime_id: &ProviderRuntimeId,
    at_ms: i64,
    evidence: EvidenceSource,
) -> LiveBinding {
    if evidence != EvidenceSource::ObservedInTraffic {
        return LiveBinding::NotBound {
            reason: LiveUnboundReason::NeverStarted,
        };
    }
    let capability_evidence = CapabilityEvidence {
        source: EvidenceSource::ObservedInTraffic,
        observed_at_ms: at_ms,
        note_ref: None,
    };
    if probe.is_proven(Capability::LiveControl) {
        LiveBinding::Controlling {
            runtime_id: runtime_id.clone(),
            since_at_ms: at_ms,
            evidence: capability_evidence,
        }
    } else {
        LiveBinding::Observing {
            runtime_id: runtime_id.clone(),
            since_at_ms: at_ms,
            evidence: capability_evidence,
        }
    }
}

fn permission_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            option_id: "allow".to_owned(),
            label: "Allow once".to_owned(),
            semantics: DecisionSemantics::Allow,
        },
        DecisionOption {
            option_id: "allow_always".to_owned(),
            label: "Always allow".to_owned(),
            semantics: DecisionSemantics::AllowAlways,
        },
        DecisionOption {
            option_id: "deny".to_owned(),
            label: "Deny".to_owned(),
            semantics: DecisionSemantics::Deny,
        },
        DecisionOption {
            option_id: "cancel".to_owned(),
            label: "Cancel".to_owned(),
            semantics: DecisionSemantics::Cancel,
        },
    ]
}

fn permission_option_id(decision: &str) -> &str {
    debug_assert!(matches!(
        decision,
        "allow" | "allow_always" | "deny" | "cancel"
    ));
    decision
}
