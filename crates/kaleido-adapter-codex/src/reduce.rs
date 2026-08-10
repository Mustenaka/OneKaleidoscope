//! Turning Codex app-server traffic into canonical state transitions.
//!
//! Three behaviours here come from recordings in this repository rather than
//! from reading a schema, and each one is a place a plausible implementation
//! would have been wrong:
//!
//! * **A refusal is not a failure.** After the client answers an approval with
//!   a decline, the operation ends `declined` and the enclosing turn still
//!   completes. So a decline maps to a terminal item state and never touches
//!   `Turn.error` (rule R-P8).
//! * **An approval request has no context of its own.** It carries only the
//!   thread, turn and operation identifiers, and the displayable content sits
//!   in an earlier message. The join is therefore a first-class field that must
//!   render while it is still unresolved.
//! * **A turn-completion payload is not a transcript.** It arrives marked as a
//!   summary view whose item array holds only the last message, so the item
//!   list is accumulated from per-item transitions and the summary array has no
//!   pinned path at all.

use std::collections::{BTreeMap, BTreeSet};

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::content::ContentAccess;
use kaleido_adapter::IdentityMint;
use kaleido_proto::attention::{
    ApprovalRequest, AttentionAnswerEvidence, AttentionAnswerEvidenceSource, AttentionAnswerSource,
    AttentionItem, AttentionState, AttentionSubject, DecisionOption, DecisionSemantics,
    JoinFailureReason, JoinState,
};
use kaleido_proto::capability::{Capability, CapabilityEvidence, EvidenceSource};
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
use kaleido_proto::session::Session;
use kaleido_proto::session::{
    HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason, OwnershipMode, SessionStatus,
};
use kaleido_proto::turn::{
    ChangeSet, FileChange, FileChangeKind, Item, ItemBody, ItemStatus, MessagePhase, Turn,
    TurnOrigin, TurnStatus,
};
use serde_json::Value;

use crate::bindings::BindingStore;
use crate::decode::{Reader, APPROVAL_DECISIONS, DECODED_ITEM_KINDS};
use crate::error::CodexAdapterError;
use crate::surface::{self, SurfacePurpose, APPROVAL_METHODS};
use crate::transcript::{Direction, Transcript, TranscriptFrame};

/// How the reducer should describe the connection it is reading.
#[derive(Debug, Clone)]
pub struct ReducerConfig {
    pub host_display_name: String,
    pub host_platform: HostPlatform,
    pub project_display_name: String,
    pub identity_salt: String,
    /// Where the traffic came from.
    ///
    /// Only [`EvidenceSource::ObservedInTraffic`] permits an observing live
    /// binding (section 4.3), which is why a replay of a recording cannot
    /// present itself as an attached session.
    pub evidence: EvidenceSource,
    pub launch_surface: LaunchSurface,
    pub turn_origin: TurnOrigin,
    /// Base instant for frames that carry only a relative offset.
    pub base_at_ms: i64,
    pub runtime_version_label: Option<String>,
}

/// What a client request was, so its response can be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientCall {
    ThreadStart,
    TurnStart,
    Unmodelled,
}

/// An approval waiting for the operation it refers to.
#[derive(Debug, Clone)]
struct PendingApproval {
    attention_id: AttentionId,
    raw_item_id: String,
}

/// A broker command registered for one specific upstream approval reply.
#[derive(Debug, Clone)]
struct PendingLocalAnswer {
    command_id: CommandId,
    option_id: String,
}

/// Reduces Codex app-server frames to canonical state transitions.
#[derive(Debug)]
pub struct CodexReducer {
    config: ReducerConfig,
    mint: IdentityMint,
    host_id: HostId,
    runtime_id: ProviderRuntimeId,
    project_id: ProjectId,
    project_binding_id: ProjectBindingId,
    bindings: BindingStore,
    pending_client_calls: BTreeMap<i64, ClientCall>,
    pending_local_control: BTreeMap<i64, CommandId>,
    pending_approvals: BTreeMap<i64, PendingApproval>,
    raw_thread_id: Option<String>,
    session_id: Option<SessionId>,
    session: Option<Session>,
    current_turn: Option<TurnId>,
    turn_origins: BTreeMap<TurnId, TurnOrigin>,
    next_sequence: u64,
    agent_text: BTreeMap<ItemId, String>,
    items: BTreeMap<ItemId, Item>,
    attention: BTreeMap<AttentionId, AttentionItem>,
    attention_by_raw_item: BTreeMap<String, Vec<AttentionId>>,
    diagnostics: BTreeMap<String, DiagnosticRecord>,
    pending_local_answers: BTreeMap<AttentionId, PendingLocalAnswer>,
    locally_answered_attention: BTreeSet<AttentionId>,
    probe: CapabilityProbe,
    published_capabilities: Vec<Capability>,
    exercised: BTreeSet<SurfacePurpose>,
    bootstrapped: bool,
    lifecycle_ended: bool,
}

impl CodexReducer {
    pub fn new(config: ReducerConfig) -> Self {
        let mint = IdentityMint::new(config.identity_salt.clone());
        let host_id = mint.host_id(&config.host_display_name);
        let runtime_id = mint.runtime_id(&format!("{}|app-server", config.host_display_name));
        let project_id = mint.project_id(&config.project_display_name);
        let project_binding_id =
            mint.project_binding_id(&format!("{}|{}", config.project_display_name, runtime_id));
        let probe = CapabilityProbe::new(runtime_id.clone(), config.base_at_ms, config.evidence);
        Self {
            mint,
            host_id,
            runtime_id,
            project_id,
            project_binding_id,
            bindings: BindingStore::default(),
            pending_client_calls: BTreeMap::new(),
            pending_local_control: BTreeMap::new(),
            pending_approvals: BTreeMap::new(),
            raw_thread_id: None,
            session_id: None,
            session: None,
            current_turn: None,
            turn_origins: BTreeMap::new(),
            next_sequence: 0,
            agent_text: BTreeMap::new(),
            items: BTreeMap::new(),
            attention: BTreeMap::new(),
            attention_by_raw_item: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
            pending_local_answers: BTreeMap::new(),
            locally_answered_attention: BTreeSet::new(),
            probe,
            published_capabilities: Vec::new(),
            exercised: BTreeSet::new(),
            bootstrapped: false,
            lifecycle_ended: false,
            config,
        }
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
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

    /// The raw thread identifier decoded through the pinned reader.
    ///
    /// It stays crate-private because section 3 forbids provider identifiers
    /// from crossing the adapter boundary.
    pub(crate) fn raw_thread_id(&self) -> Option<&str> {
        self.raw_thread_id.as_deref()
    }

    /// Finds the server-side JSON-RPC request identifier for an open approval.
    ///
    /// Server request IDs and client request IDs are deliberately kept in
    /// separate maps because their numeric spaces overlap in real traffic.
    pub(crate) fn approval_request_id(&self, attention_id: &AttentionId) -> Option<i64> {
        self.pending_approvals
            .iter()
            .find_map(|(request_id, pending)| {
                (&pending.attention_id == attention_id).then_some(*request_id)
            })
    }

    /// Associates one outgoing wire reply with the real broker command that
    /// caused it. Live traffic without this per-attention association is an
    /// externally observed answer, not a local command echo.
    pub fn register_local_attention_answer(
        &mut self,
        attention_id: &AttentionId,
        command_id: &CommandId,
        option_id: &str,
    ) -> bool {
        if command_id.is_empty()
            || option_id.is_empty()
            || self.approval_request_id(attention_id).is_none()
            || self.pending_local_answers.contains_key(attention_id)
        {
            return false;
        }
        self.pending_local_answers.insert(
            attention_id.clone(),
            PendingLocalAnswer {
                command_id: command_id.clone(),
                option_id: option_id.to_owned(),
            },
        );
        true
    }

    /// Rolls back an association when the corresponding wire send fails.
    pub(crate) fn forget_local_attention_answer(&mut self, attention_id: &AttentionId) {
        self.pending_local_answers.remove(attention_id);
    }

    pub fn capability_probe(&self) -> CapabilityProbe {
        self.probe.clone()
    }

    /// Correlates one broker command with the exact client request that will
    /// carry it to the live runtime.
    ///
    /// Recorded traffic and duplicate request/command identifiers are refused:
    /// neither can prove that this connection accepted a command now.
    pub fn register_local_turn_start(&mut self, request_id: i64, command_id: &CommandId) -> bool {
        if self.config.evidence != EvidenceSource::ObservedInTraffic
            || command_id.is_empty()
            || self.pending_local_control.contains_key(&request_id)
            || self
                .pending_local_control
                .values()
                .any(|pending| pending == command_id)
        {
            return false;
        }
        self.pending_local_control
            .insert(request_id, command_id.clone());
        true
    }

    /// Removes a correlation when the request never reached a matching
    /// structured response (for example, a transport write failure).
    pub fn cancel_local_turn_start(&mut self, request_id: i64) {
        self.pending_local_control.remove(&request_id);
    }

    /// Records an unexpected child-process exit exactly once.
    pub fn process_exited(
        &mut self,
        exit_code: Option<i64>,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        if self.lifecycle_ended {
            return Ok(Vec::new());
        }

        let reason = ConnectionFaultReason::ProcessExited { exit_code };
        let mut effects = vec![StateEffect::RuntimeUpserted {
            runtime: self.runtime_with_connection(ConnectionState::Unavailable {
                reason: reason.clone(),
                since_at_ms: at_ms,
            }),
        }];
        let ended_session = self.runtime_ended_session(at_ms);
        if let Some(session) = &ended_session {
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
        }

        Self::validate_effects(&effects)?;
        self.session = ended_session;
        self.lifecycle_ended = true;
        Ok(effects)
    }

    /// Records an intentional transport shutdown exactly once.
    pub fn clean_disconnected(
        &mut self,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        if self.lifecycle_ended {
            return Ok(Vec::new());
        }

        let mut effects = vec![StateEffect::RuntimeUpserted {
            runtime: self.runtime_with_connection(ConnectionState::Disconnected),
        }];
        let ended_session = self.runtime_ended_session(at_ms);
        if let Some(session) = &ended_session {
            effects.push(StateEffect::SessionUpserted {
                session: session.clone(),
            });
        }

        Self::validate_effects(&effects)?;
        self.session = ended_session;
        self.lifecycle_ended = true;
        Ok(effects)
    }

    /// Which pinned purposes the decoder has consulted.
    pub fn exercised_purposes(&self) -> &BTreeSet<SurfacePurpose> {
        &self.exercised
    }

    /// Reduces a whole recorded transcript and closes the observation window.
    pub fn ingest(
        &mut self,
        transcript: &Transcript,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let mut effects = Vec::new();
        for frame in transcript.frames() {
            effects.extend(self.ingest_frame(frame, content)?);
        }
        effects.extend(self.finish(content)?);
        Ok(effects)
    }

    /// Reduces one frame.
    pub fn ingest_frame(
        &mut self,
        frame: &TranscriptFrame,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let at_ms = self.at_ms(frame);
        self.probe.advance_observation(at_ms);
        // The host and its runtime must exist before anything else can be
        // routed to a stream, including the diagnostic an unmodelled frame
        // produces. Neither depends on the session, so both can be emitted from
        // the first frame observed.
        let mut effects = self.ensure_bootstrapped(at_ms);
        let mut frame_effects = self.dispatch_frame(frame, at_ms, content)?;
        // Capabilities are evidence-driven, so they can only ever grow as
        // traffic proves them. Republishing on change is what lets a reader see
        // "approval works here" the moment the runtime demonstrates it, without
        // ever inferring it from a provider name.
        if self.probe.proven() != self.published_capabilities.as_slice() {
            self.published_capabilities = self.probe.proven().to_vec();
            let capability_effect = StateEffect::CapabilitiesUpdated {
                capabilities: self.probe.to_capabilities(),
            };
            if let Some(controlling_index) = frame_effects.iter().position(|effect| {
                matches!(
                    effect,
                    StateEffect::SessionUpserted { session }
                        if matches!(session.live_binding, LiveBinding::Controlling { .. })
                )
            }) {
                frame_effects.insert(controlling_index, capability_effect);
            } else {
                frame_effects.push(capability_effect);
            }
        }
        // CanonicalStore::apply_all is deliberately non-transactional. Keep a
        // correlated Turn and runtime acknowledgement before LiveControl so a
        // rejected acknowledgement cannot leave a false capability behind;
        // publish the capability immediately before Controlling so that binding
        // validation still observes the newly proved runtime state.
        effects.extend(frame_effects);
        Ok(effects)
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
                    // The adapter only observes the provider runtime. LAN
                    // reachability belongs to hostd's listener lifecycle and
                    // must not be inferred from the first provider frame.
                    reachability: HostReachability::Offline,
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
            family: ProviderFamily::Codex,
            version_label: self.config.runtime_version_label.clone(),
            launch_surface: self.config.launch_surface.clone(),
            connection,
            // Never reuse the bootstrap copy: traffic may have proved more
            // capabilities since then.
            capabilities: self.probe.to_capabilities(),
            binding_handle: None,
        }
    }

    fn runtime_ended_session(&self, at_ms: i64) -> Option<Session> {
        self.session.clone().map(|mut session| {
            session.status = SessionStatus::Offline;
            session.live_binding = LiveBinding::NotBound {
                reason: LiveUnboundReason::RuntimeExited,
            };
            session.updated_at_ms = session.updated_at_ms.max(at_ms);
            session.last_activity_at_ms = session.last_activity_at_ms.max(at_ms);
            session
        })
    }

    fn validate_effects(effects: &[StateEffect]) -> Result<(), CodexAdapterError> {
        for effect in effects {
            effect.validate_for_log()?;
        }
        Ok(())
    }

    fn dispatch_frame(
        &mut self,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        match (frame.direction(), frame.method(), frame.request_id()) {
            (Direction::ClientToServer, Some(method), Some(id)) => {
                self.pending_client_calls.insert(id, client_call(method));
                Ok(Vec::new())
            }
            // A client notification carries nothing this slice consumes.
            (Direction::ClientToServer, Some(_), None) => Ok(Vec::new()),
            (Direction::ClientToServer, None, Some(id)) => {
                self.reduce_approval_reply(id, frame, at_ms, content)
            }
            (Direction::ClientToServer, None, None) => Ok(Vec::new()),
            (Direction::ServerToClient, Some(method), Some(id))
                if APPROVAL_METHODS.contains(&method) =>
            {
                self.reduce_approval_request(id, frame, at_ms, content)
            }
            (Direction::ServerToClient, Some(method), _) => {
                self.reduce_notification(method, frame, at_ms, content)
            }
            (Direction::ServerToClient, None, Some(id)) => {
                self.reduce_client_response(id, frame, at_ms, content)
            }
            (Direction::ServerToClient, None, None) => Ok(vec![self.record_diagnostic(
                DiagnosticCode::MalformedProviderMessage,
                None,
                at_ms,
                content,
            )?]),
        }
    }

    /// Closes the observation window.
    ///
    /// An approval still waiting for its operation becomes a structured
    /// `ItemUnknown` join failure rather than an optimistic `Joined`.
    pub fn finish(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let mut effects = Vec::new();
        let unresolved = self
            .attention
            .values()
            .filter(|entry| join_is_deferred(entry))
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        for attention_id in unresolved {
            let Some(entry) = self.attention.get_mut(&attention_id) else {
                continue;
            };
            let at_ms = entry.created_at_ms;
            if let AttentionSubject::Approval { request } = &mut entry.subject {
                request.join = JoinState::Unjoined {
                    reason: JoinFailureReason::ItemUnknown,
                };
            }
            let updated = entry.clone();
            if !self.locally_answered_attention.contains(&attention_id) {
                effects.push(StateEffect::AttentionUpserted { item: updated });
            }
            effects.push(self.record_diagnostic(
                DiagnosticCode::JoinFailed,
                self.session_id.clone(),
                at_ms,
                content,
            )?);
        }
        Ok(effects)
    }

    fn at_ms(&self, frame: &TranscriptFrame) -> i64 {
        self.config
            .base_at_ms
            .saturating_add(frame.recorded_offset_ms())
    }

    fn reader(&mut self) -> Reader<'_> {
        Reader::new(&mut self.exercised)
    }

    // ---------------------------------------------------------------- responses

    fn reduce_client_response(
        &mut self,
        id: i64,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        match self.pending_client_calls.remove(&id) {
            Some(ClientCall::ThreadStart) => self.bootstrap_session(frame, at_ms, content),
            Some(ClientCall::TurnStart) => {
                let local_command = self.pending_local_control.remove(&id);
                if frame.payload().get("error").is_some() {
                    return Ok(local_command
                        .map(|command_id| StateEffect::CommandAcknowledged {
                            ack: CommandAck {
                                command_id,
                                outcome: CommandOutcome::Rejected {
                                    error: CanonicalError {
                                        code: ErrorCode::UpstreamRejected,
                                        retriable: false,
                                        detail_ref: None,
                                        at_ms,
                                    },
                                },
                                acked_at_ms: at_ms,
                            },
                        })
                        .into_iter()
                        .collect());
                }

                let origin = local_command
                    .as_ref()
                    .map(|command_id| TurnOrigin::RemoteCommand {
                        command_id: command_id.clone(),
                    })
                    .unwrap_or_else(|| self.config.turn_origin.clone());
                let mut effects = self.reduce_turn_created(frame, at_ms, origin)?;
                if let Some(command_id) = local_command {
                    effects.extend(self.runtime_acceptance_effects(command_id, at_ms)?);
                }
                Ok(effects)
            }
            Some(ClientCall::Unmodelled) | None => Ok(Vec::new()),
        }
    }

    fn runtime_acceptance_effects(
        &mut self,
        command_id: CommandId,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let (since_at_ms, already_controlling, mut controlling_session) = {
            let Some(session) = self.session.as_ref() else {
                return Err(CodexAdapterError::ControlEvidenceUnavailable);
            };
            let since_at_ms = match &session.live_binding {
                LiveBinding::Observing {
                    runtime_id,
                    since_at_ms,
                    ..
                }
                | LiveBinding::Controlling {
                    runtime_id,
                    since_at_ms,
                    ..
                } if runtime_id == &self.runtime_id => *since_at_ms,
                _ => return Err(CodexAdapterError::ControlEvidenceUnavailable),
            };
            (
                since_at_ms,
                session.live_binding.accepts_control(),
                session.clone(),
            )
        };

        let outcome = CommandOutcome::AcceptedByRuntime {
            session_id: controlling_session.id.clone(),
            acceptance_kind: RuntimeAcceptanceKind::PromptTurn,
            binding_handle: self.mint.binding_handle(
                &self.runtime_id,
                ProviderBindingKind::RuntimeAcknowledgement,
                command_id.as_str(),
            ),
        };
        if !self.probe.observe_runtime_acceptance(&outcome) {
            return Err(CodexAdapterError::ControlEvidenceUnavailable);
        }

        let ack = StateEffect::CommandAcknowledged {
            ack: CommandAck {
                command_id,
                outcome,
                acked_at_ms: at_ms,
            },
        };
        if already_controlling {
            return Ok(vec![ack]);
        }
        controlling_session.live_binding = LiveBinding::Controlling {
            runtime_id: self.runtime_id.clone(),
            since_at_ms,
            evidence: CapabilityEvidence {
                source: EvidenceSource::ObservedInTraffic,
                observed_at_ms: at_ms,
                note_ref: None,
            },
        };
        controlling_session.updated_at_ms = controlling_session.updated_at_ms.max(at_ms);
        controlling_session.last_activity_at_ms =
            controlling_session.last_activity_at_ms.max(at_ms);
        self.session = Some(controlling_session.clone());

        Ok(vec![
            ack,
            StateEffect::SessionUpserted {
                session: controlling_session,
            },
        ])
    }

    fn bootstrap_session(
        &mut self,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_thread = self
            .reader()
            .string(&payload, SurfacePurpose::SessionThreadId)?;
        let project_root = self
            .reader()
            .string(&payload, SurfacePurpose::SessionProjectRoot)?;
        // Section 10: a project root is a full filesystem path, so it enters
        // canonical state only as a sensitive reference.
        let root_ref = content.store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            project_root.as_bytes(),
        )?;

        let (session_id, session_handle) =
            self.bindings
                .bind_session(&self.mint, &self.runtime_id, &raw_thread);
        self.raw_thread_id = Some(raw_thread);
        self.session_id = Some(session_id.clone());
        self.probe.prove(Capability::TurnPrompt);
        self.probe.prove(Capability::StateToolLifecycle);
        if self.config.evidence == EvidenceSource::ObservedInTraffic {
            self.probe.prove(Capability::LiveObserve);
        }
        // Published here, so the per-frame republish does not immediately
        // repeat the same set.
        self.published_capabilities = self.probe.proven().to_vec();

        let mut effects = vec![
            // Capabilities first: an observing live binding is only accepted
            // once the negotiated set actually supports observation.
            StateEffect::CapabilitiesUpdated {
                capabilities: self.probe.to_capabilities(),
            },
            StateEffect::ProjectUpserted {
                project: Project {
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
                },
            },
        ];

        let session = Session {
            id: session_id,
            project_id: self.project_id.clone(),
            project_binding_id: self.project_binding_id.clone(),
            ownership: OwnershipMode::BrokerManaged,
            history_source: HistorySource {
                kind: HistorySourceKind::BrokerLog,
                runtime_id: Some(self.runtime_id.clone()),
                evidence: self.evidence(at_ms),
            },
            live_binding: self.live_binding(at_ms),
            // The store derives the status from the four state families;
            // an idle placeholder here would only be able to disagree.
            status: SessionStatus::Idle,
            title: None,
            created_at_ms: at_ms,
            updated_at_ms: at_ms,
            last_activity_at_ms: at_ms,
            active_turn_id: None,
            queue_depth: 0,
            open_attention_count: 0,
            archived: false,
            binding_handle: Some(session_handle),
        };
        self.session = Some(session.clone());
        effects.push(StateEffect::SessionUpserted { session });
        Ok(effects)
    }

    fn evidence(&self, at_ms: i64) -> CapabilityEvidence {
        CapabilityEvidence {
            source: self.config.evidence,
            observed_at_ms: at_ms,
            note_ref: None,
        }
    }

    /// The live binding this evidence actually supports.
    ///
    /// Rule R-P7 and section 4.3 only allow `Observing` on traffic observed
    /// from a live runtime. Replaying a recording proves the shape of the
    /// protocol, not that anything is attached now, so it stays unbound.
    fn live_binding(&self, at_ms: i64) -> LiveBinding {
        if self.config.evidence == EvidenceSource::ObservedInTraffic {
            LiveBinding::Observing {
                runtime_id: self.runtime_id.clone(),
                since_at_ms: at_ms,
                evidence: self.evidence(at_ms),
            }
        } else {
            LiveBinding::NotBound {
                reason: LiveUnboundReason::NeverStarted,
            }
        }
    }

    fn reduce_turn_created(
        &mut self,
        frame: &TranscriptFrame,
        at_ms: i64,
        origin: TurnOrigin,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_turn = self
            .reader()
            .string(&payload, SurfacePurpose::TurnResponseTurnId)?;
        let status = self
            .reader()
            .string(&payload, SurfacePurpose::TurnResponseStatus)?;
        let status = turn_status(&status, SurfacePurpose::TurnResponseStatus)?;
        let Some(session_id) = self.session_id.clone() else {
            return Err(CodexAdapterError::UnknownBinding { scope: "session" });
        };
        let (turn_id, handle, _) =
            self.bindings
                .bind_turn(&self.mint, &self.runtime_id, &raw_turn, &session_id);
        self.current_turn = Some(turn_id.clone());
        self.turn_origins.insert(turn_id.clone(), origin.clone());
        Ok(vec![StateEffect::TurnUpserted {
            turn: Turn {
                id: turn_id,
                session_id,
                status,
                origin,
                started_at_ms: None,
                completed_at_ms: terminal_timestamp(status, at_ms),
                // Section 4.4: accumulated from item transitions only.
                item_ids: Vec::new(),
                error: None,
                binding_handle: Some(handle),
            },
        }])
    }

    // ------------------------------------------------------------ notifications

    fn reduce_notification(
        &mut self,
        method: &str,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        if !surface::method_is_declared(method) {
            // ADR-0012 D-3: count it, keep the raw label behind a sensitive
            // reference, and change nothing a reader can see.
            return Ok(vec![self.record_diagnostic(
                DiagnosticCode::UnknownUpstreamMessage,
                self.session_id.clone(),
                at_ms,
                content,
            )?]);
        }
        match method {
            "thread/started" => self.reduce_thread_started(frame),
            "thread/status/changed" => self.reduce_status_changed(frame, at_ms),
            "turn/started" | "turn/completed" => {
                self.reduce_turn_transition(method, frame, at_ms, content)
            }
            "item/started" | "item/completed" => self.reduce_item(method, frame, at_ms, content),
            "item/agentMessage/delta" => self.reduce_delta(frame, at_ms, content),
            _ => Ok(vec![self.record_diagnostic(
                DiagnosticCode::UnknownUpstreamMessage,
                self.session_id.clone(),
                at_ms,
                content,
            )?]),
        }
    }

    fn reduce_thread_started(
        &mut self,
        frame: &TranscriptFrame,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_thread = self
            .reader()
            .string(&payload, SurfacePurpose::SessionStartedThreadId)?;
        // Confirmation only: the session was already created from the response
        // to our own request, so re-emitting it would add nothing.
        if self.bindings.session(&raw_thread).is_none() {
            return Err(CodexAdapterError::UnknownBinding { scope: "session" });
        }
        Ok(Vec::new())
    }

    fn reduce_status_changed(
        &mut self,
        frame: &TranscriptFrame,
        at_ms: i64,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_thread = self
            .reader()
            .string(&payload, SurfacePurpose::ThreadStatusThreadId)?;
        let status = self
            .reader()
            .string(&payload, SurfacePurpose::ThreadStatusType)?;
        let Some((session_id, _)) = self.bindings.session(&raw_thread).cloned() else {
            return Err(CodexAdapterError::UnknownBinding { scope: "session" });
        };
        let status = match status.as_str() {
            "active" => SessionStatus::Running,
            "idle" => SessionStatus::Idle,
            "notLoaded" => SessionStatus::Offline,
            "systemError" => SessionStatus::Failed,
            _ => {
                return Err(CodexAdapterError::UnmodelledEnumeration {
                    purpose: SurfacePurpose::ThreadStatusType,
                });
            }
        };
        if let Some(session) = &mut self.session {
            session.status = status;
            session.updated_at_ms = session.updated_at_ms.max(at_ms);
            session.last_activity_at_ms = session.last_activity_at_ms.max(at_ms);
        }
        Ok(vec![StateEffect::SessionStatusChanged {
            session_id,
            status,
        }])
    }

    fn reduce_turn_transition(
        &mut self,
        method: &str,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_thread = self
            .reader()
            .string(&payload, SurfacePurpose::TurnNotificationThreadId)?;
        let raw_turn = self
            .reader()
            .string(&payload, SurfacePurpose::TurnNotificationTurnId)?;
        let raw_status = self
            .reader()
            .string(&payload, SurfacePurpose::TurnNotificationStatus)?;
        // Read but never trusted as a transcript: this field is the recorded
        // proof that the payload's item array is partial.
        let _items_view = self
            .reader()
            .string(&payload, SurfacePurpose::TurnNotificationItemsView)?;
        let started_at = self
            .reader()
            .optional_integer(&payload, SurfacePurpose::TurnNotificationStartedAt)?;
        let completed_at = self
            .reader()
            .optional_integer(&payload, SurfacePurpose::TurnNotificationCompletedAt)?;
        let has_error = self
            .reader()
            .is_present(&payload, SurfacePurpose::TurnNotificationError)?;

        let Some((session_id, _)) = self.bindings.session(&raw_thread).cloned() else {
            return Err(CodexAdapterError::UnknownBinding { scope: "session" });
        };
        let (turn_id, handle, _) =
            self.bindings
                .bind_turn(&self.mint, &self.runtime_id, &raw_turn, &session_id);
        let status = turn_status(&raw_status, SurfacePurpose::TurnNotificationStatus)?;
        let origin = self
            .turn_origins
            .entry(turn_id.clone())
            .or_insert_with(|| self.config.turn_origin.clone())
            .clone();
        let error = if status == TurnStatus::Failed {
            Some(CanonicalError {
                code: ErrorCode::UpstreamRejected,
                retriable: false,
                detail_ref: None,
                at_ms,
            })
        } else {
            None
        };
        if status.is_terminal() {
            self.current_turn = None;
        } else {
            self.current_turn = Some(turn_id.clone());
        }
        let mut effects = vec![StateEffect::TurnUpserted {
            turn: Turn {
                id: turn_id,
                session_id: session_id.clone(),
                status,
                origin,
                started_at_ms: started_at.map(seconds_to_millis),
                completed_at_ms: completed_at
                    .map(seconds_to_millis)
                    .or_else(|| terminal_timestamp(status, at_ms)),
                item_ids: Vec::new(),
                error,
                binding_handle: Some(handle),
            },
        }];
        if has_error && status != TurnStatus::Failed {
            // The runtime attached failure detail to a turn it did not mark as
            // failed. Record it rather than reinterpreting the outcome.
            effects.push(self.record_diagnostic(
                DiagnosticCode::MalformedProviderMessage,
                Some(session_id),
                at_ms,
                content,
            )?);
        }
        if method == "turn/completed" {
            effects.extend(self.finish(content)?);
        }
        Ok(effects)
    }

    // -------------------------------------------------------------------- items

    fn reduce_item(
        &mut self,
        method: &str,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_thread = self
            .reader()
            .string(&payload, SurfacePurpose::ItemLifecycleThreadId)?;
        let raw_turn = self
            .reader()
            .string(&payload, SurfacePurpose::ItemLifecycleTurnId)?;
        let raw_item = self
            .reader()
            .string(&payload, SurfacePurpose::ItemIdentifier)?;
        let kind = self.reader().string(&payload, SurfacePurpose::ItemType)?;
        let started = method == "item/started";
        let observed_at = if started {
            self.reader()
                .integer(&payload, SurfacePurpose::ItemStartedAt)?
        } else {
            self.reader()
                .integer(&payload, SurfacePurpose::ItemCompletedAt)?
        };

        let Some((session_id, _)) = self.bindings.session(&raw_thread).cloned() else {
            return Err(CodexAdapterError::UnknownBinding { scope: "session" });
        };
        let (turn_id, _, _) =
            self.bindings
                .bind_turn(&self.mint, &self.runtime_id, &raw_turn, &session_id);

        if !DECODED_ITEM_KINDS.contains(&kind.as_str()) {
            // Section 4.5: an unmodelled item kind is never guessed into a
            // known body. The raw label is kept behind a sensitive reference.
            return Ok(vec![self.record_labelled_diagnostic(
                DiagnosticCode::UnknownUpstreamLabel,
                Some(session_id),
                &kind,
                at_ms,
                content,
            )?]);
        }

        let binding = self.bindings.bind_item(
            &self.mint,
            &self.runtime_id,
            &raw_item,
            &session_id,
            &turn_id,
        );
        let sequence = match self.items.get(&binding.item_id) {
            Some(existing) => existing.sequence,
            None => {
                let sequence = self.next_sequence;
                self.next_sequence = self.next_sequence.saturating_add(1);
                sequence
            }
        };
        let created_at_ms = self
            .items
            .get(&binding.item_id)
            .map_or(observed_at, |existing| existing.created_at_ms);

        let (body, status) =
            self.decode_item_body(&payload, &kind, &binding.item_id, started, content)?;
        let item = Item {
            id: binding.item_id.clone(),
            session_id: session_id.clone(),
            turn_id,
            sequence,
            status,
            body,
            created_at_ms,
            updated_at_ms: observed_at,
            binding_handle: Some(binding.handle.clone()),
        };
        item.validate()?;
        self.items.insert(binding.item_id.clone(), item.clone());
        let mut effects = vec![StateEffect::ItemUpserted { item }];
        effects.extend(self.upgrade_joins(&raw_item, content)?);
        Ok(effects)
    }

    fn decode_item_body(
        &mut self,
        payload: &Value,
        kind: &str,
        item_id: &ItemId,
        started: bool,
        content: &mut dyn ContentAccess,
    ) -> Result<(ItemBody, ItemStatus), CodexAdapterError> {
        match kind {
            "userMessage" => {
                let parts = self
                    .reader()
                    .array(payload, SurfacePurpose::UserMessageContent)?;
                let mut text = String::new();
                for part in &parts {
                    if let Some(fragment) = self
                        .reader()
                        .optional_string(part, SurfacePurpose::UserMessageContentText)?
                    {
                        text.push_str(&fragment);
                    }
                }
                let reference = self.store_text(content, ContentKind::PlainText, &text)?;
                Ok((
                    ItemBody::UserMessage { content: reference },
                    lifecycle_status(started),
                ))
            }
            "agentMessage" => {
                let text = self
                    .reader()
                    .string(payload, SurfacePurpose::AgentMessageText)?;
                let phase = self
                    .reader()
                    .optional_string(payload, SurfacePurpose::AgentMessagePhase)?;
                let phase = message_phase(phase.as_deref())?;
                // On completion the runtime's own text is authoritative; while
                // streaming, the accumulated deltas are all there is.
                let text = if started {
                    self.agent_text
                        .entry(item_id.clone())
                        .or_insert_with(|| text.clone())
                        .clone()
                } else {
                    self.agent_text.insert(item_id.clone(), text.clone());
                    text
                };
                let reference = self.store_text(content, ContentKind::Markdown, &text)?;
                Ok((
                    ItemBody::AgentMessage {
                        content: reference,
                        phase,
                    },
                    lifecycle_status(started),
                ))
            }
            "reasoning" => {
                let summary = self.reader().joined_strings(
                    payload,
                    SurfacePurpose::ReasoningSummary,
                    "\n",
                )?;
                let body = self.reader().joined_strings(
                    payload,
                    SurfacePurpose::ReasoningContent,
                    "\n",
                )?;
                let mut text = summary;
                if !body.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&body);
                }
                let reference = self.store_text(content, ContentKind::PlainText, &text)?;
                Ok((
                    ItemBody::Reasoning { content: reference },
                    lifecycle_status(started),
                ))
            }
            "fileChange" => {
                let raw_status = self
                    .reader()
                    .string(payload, SurfacePurpose::FileChangeStatus)?;
                let status = item_status(&raw_status)?;
                let entries = self
                    .reader()
                    .array(payload, SurfacePurpose::FileChangeEntries)?;
                let mut changes = Vec::with_capacity(entries.len());
                for entry in &entries {
                    let path = self
                        .reader()
                        .string(entry, SurfacePurpose::FileChangeEntryPath)?;
                    let raw_kind = self
                        .reader()
                        .string(entry, SurfacePurpose::FileChangeEntryKind)?;
                    let diff = self
                        .reader()
                        .optional_string(entry, SurfacePurpose::FileChangeEntryDiff)?;
                    let path_ref = self.store_text(content, ContentKind::FilePath, &path)?;
                    let diff_ref = match diff {
                        Some(diff) => {
                            Some(self.store_text(content, ContentKind::UnifiedDiff, &diff)?)
                        }
                        None => None,
                    };
                    changes.push(FileChange {
                        path_ref,
                        kind: file_change_kind(&raw_kind)?,
                        diff: diff_ref,
                    });
                }
                Ok((
                    ItemBody::FileEdit {
                        change_set: ChangeSet {
                            entries: changes,
                            truncated: false,
                        },
                    },
                    status,
                ))
            }
            _ => Err(CodexAdapterError::UnmodelledEnumeration {
                purpose: SurfacePurpose::ItemType,
            }),
        }
    }

    fn reduce_delta(
        &mut self,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_thread = self
            .reader()
            .string(&payload, SurfacePurpose::DeltaThreadId)?;
        let _raw_turn = self
            .reader()
            .string(&payload, SurfacePurpose::DeltaTurnId)?;
        let raw_item = self
            .reader()
            .string(&payload, SurfacePurpose::DeltaItemId)?;
        let fragment = self.reader().string(&payload, SurfacePurpose::DeltaText)?;
        if self.bindings.session(&raw_thread).is_none() {
            return Err(CodexAdapterError::UnknownBinding { scope: "session" });
        }
        let Some(binding) = self.bindings.item(&raw_item).cloned() else {
            return Ok(vec![self.record_diagnostic(
                DiagnosticCode::MalformedProviderMessage,
                self.session_id.clone(),
                at_ms,
                content,
            )?]);
        };
        let Some(existing) = self.items.get(&binding.item_id).cloned() else {
            return Ok(vec![self.record_diagnostic(
                DiagnosticCode::MalformedProviderMessage,
                self.session_id.clone(),
                at_ms,
                content,
            )?]);
        };
        let ItemBody::AgentMessage { phase, .. } = &existing.body else {
            return Ok(vec![self.record_diagnostic(
                DiagnosticCode::MalformedProviderMessage,
                self.session_id.clone(),
                at_ms,
                content,
            )?]);
        };
        let phase = *phase;
        let text = {
            let accumulated = self.agent_text.entry(binding.item_id.clone()).or_default();
            accumulated.push_str(&fragment);
            accumulated.clone()
        };
        let reference = self.store_text(content, ContentKind::Markdown, &text)?;
        // Section 4.5: a streaming increment updates the existing item. It
        // keeps the same identifier and sequence, so no new item appears.
        let updated = Item {
            body: ItemBody::AgentMessage {
                content: reference,
                phase,
            },
            updated_at_ms: at_ms,
            ..existing
        };
        updated.validate()?;
        self.items.insert(binding.item_id, updated.clone());
        Ok(vec![StateEffect::ItemUpserted { item: updated }])
    }

    // ---------------------------------------------------------------- approvals

    fn reduce_approval_request(
        &mut self,
        request_id: i64,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let payload = frame.payload().clone();
        let raw_thread = self
            .reader()
            .string(&payload, SurfacePurpose::ApprovalThreadId)?;
        let raw_turn = self
            .reader()
            .string(&payload, SurfacePurpose::ApprovalTurnId)?;
        let raw_item = self
            .reader()
            .string(&payload, SurfacePurpose::ApprovalItemId)?;
        let started_at = self
            .reader()
            .integer(&payload, SurfacePurpose::ApprovalStartedAt)?;
        let reason = self
            .reader()
            .optional_string(&payload, SurfacePurpose::ApprovalReason)?;

        let Some((session_id, _)) = self.bindings.session(&raw_thread).cloned() else {
            return Err(CodexAdapterError::UnknownBinding { scope: "session" });
        };
        let (turn_id, _, _) =
            self.bindings
                .bind_turn(&self.mint, &self.runtime_id, &raw_turn, &session_id);
        let correlation = format!("{raw_thread}|{raw_turn}|{raw_item}|{started_at}");
        let (attention_id, handle) =
            self.bindings
                .bind_interaction(&self.mint, &self.runtime_id, &correlation);
        let request_key = self.mint.request_key(&correlation);
        let target_item_id = self.mint.item_id(&raw_item);

        let join = self.join_for(&raw_item, &session_id, &turn_id, &target_item_id);
        let summary = self.summarise_target(&target_item_id, &join);
        let summary_ref = self.store_text(content, ContentKind::StructuredSummary, &summary)?;
        let detail_ref = match reason {
            Some(reason) => Some(self.store_text(content, ContentKind::PlainText, &reason)?),
            None => None,
        };

        let entry = AttentionItem {
            id: attention_id.clone(),
            host_id: self.host_id.clone(),
            project_id: self.project_id.clone(),
            session_id: Some(session_id.clone()),
            turn_id: Some(turn_id),
            workflow_id: None,
            subject: AttentionSubject::Approval {
                request: ApprovalRequest {
                    request_key,
                    target_item_id,
                    join: join.clone(),
                    options: approval_options(),
                    summary_ref,
                    detail_ref,
                    binding_handle: handle,
                },
            },
            state: AttentionState::Open,
            created_at_ms: started_at,
            // The recorded protocol declares no expiry for an approval.
            expires_at_ms: None,
        };
        entry.validate()?;
        self.attention.insert(attention_id.clone(), entry.clone());
        self.attention_by_raw_item
            .entry(raw_item.clone())
            .or_default()
            .push(attention_id.clone());
        self.pending_approvals.insert(
            request_id,
            PendingApproval {
                attention_id,
                raw_item_id: raw_item,
            },
        );
        self.probe.prove(Capability::InteractionApproval);

        let mut effects = vec![StateEffect::AttentionUpserted { item: entry }];
        if matches!(join, JoinState::Unjoined { .. }) {
            effects.push(self.record_diagnostic(
                DiagnosticCode::JoinDeferred,
                Some(session_id),
                at_ms,
                content,
            )?);
        }
        Ok(effects)
    }

    fn reduce_approval_reply(
        &mut self,
        request_id: i64,
        frame: &TranscriptFrame,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let Some(pending) = self.pending_approvals.remove(&request_id) else {
            return Ok(Vec::new());
        };
        let payload = frame.payload().clone();
        let decision = self
            .reader()
            .string(&payload, SurfacePurpose::ApprovalDecision)?;
        if !APPROVAL_DECISIONS.contains(&decision.as_str()) {
            return Err(CodexAdapterError::UnmodelledEnumeration {
                purpose: SurfacePurpose::ApprovalDecision,
            });
        }
        let attention_id = pending.attention_id.clone();
        let local_answer = self.pending_local_answers.remove(&attention_id);
        if let Some(local_answer) = &local_answer {
            if local_answer.option_id != decision {
                return Err(CodexAdapterError::LocalAttentionAnswerMismatch);
            }
        }
        let answer_source = match &local_answer {
            Some(local_answer) => AttentionAnswerSource::LocalCommand {
                command_id: local_answer.command_id.clone(),
            },
            None => AttentionAnswerSource::ObservedExternal {
                evidence: AttentionAnswerEvidence {
                    observer_host_id: self.host_id.clone(),
                    observed_at_ms: at_ms,
                    source: self.external_answer_evidence_source()?,
                },
            },
        };
        let Some(entry) = self.attention.get_mut(&attention_id) else {
            return Ok(Vec::new());
        };
        entry.state = AttentionState::Answered {
            option_id: Some(decision),
            free_form_ref: None,
            question_answers: Vec::new(),
            decided_at_ms: at_ms,
            answer_source,
        };
        let updated = entry.clone();
        updated.validate()?;
        let _ = content;
        let _ = pending.raw_item_id;
        if local_answer.is_some() {
            // This structured provider reply is the first evidence that the
            // locally requested answer actually took effect. Suppress later
            // join-only refreshes, but publish this terminal answer once.
            self.locally_answered_attention.insert(attention_id);
        }
        Ok(vec![StateEffect::AttentionUpserted { item: updated }])
    }

    fn external_answer_evidence_source(
        &self,
    ) -> Result<AttentionAnswerEvidenceSource, CodexAdapterError> {
        match self.config.evidence {
            EvidenceSource::ObservedInTraffic => {
                Ok(AttentionAnswerEvidenceSource::ObservedInTraffic)
            }
            EvidenceSource::RecordedFixture => Ok(AttentionAnswerEvidenceSource::RecordedFixture),
            _ => Err(CodexAdapterError::InvalidExternalAnswerEvidence),
        }
    }

    fn join_for(
        &self,
        raw_item: &str,
        session_id: &SessionId,
        turn_id: &TurnId,
        target_item_id: &ItemId,
    ) -> JoinState {
        match self.bindings.item(raw_item) {
            None => JoinState::Unjoined {
                reason: JoinFailureReason::ItemNotYetSeen,
            },
            Some(binding) if &binding.session_id != session_id || &binding.turn_id != turn_id => {
                JoinState::Unjoined {
                    reason: JoinFailureReason::ScopeMismatch,
                }
            }
            Some(_) => JoinState::Joined {
                item_id: target_item_id.clone(),
            },
        }
    }

    fn upgrade_joins(
        &mut self,
        raw_item: &str,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, CodexAdapterError> {
        let Some(attention_ids) = self.attention_by_raw_item.get(raw_item).cloned() else {
            return Ok(Vec::new());
        };
        let mut effects = Vec::new();
        for attention_id in attention_ids {
            let Some(entry) = self.attention.get(&attention_id).cloned() else {
                continue;
            };
            if !join_is_deferred(&entry) {
                continue;
            }
            let (Some(session_id), Some(turn_id)) =
                (entry.session_id.clone(), entry.turn_id.clone())
            else {
                continue;
            };
            let AttentionSubject::Approval { request } = &entry.subject else {
                continue;
            };
            let target_item_id = request.target_item_id.clone();
            let join = self.join_for(raw_item, &session_id, &turn_id, &target_item_id);
            let summary = self.summarise_target(&target_item_id, &join);
            let summary_ref = self.store_text(content, ContentKind::StructuredSummary, &summary)?;
            let Some(entry) = self.attention.get_mut(&attention_id) else {
                continue;
            };
            if let AttentionSubject::Approval { request } = &mut entry.subject {
                request.join = join;
                request.summary_ref = summary_ref;
            }
            let updated = entry.clone();
            updated.validate()?;
            if !self.locally_answered_attention.contains(&attention_id) {
                effects.push(StateEffect::AttentionUpserted { item: updated });
            }
        }
        Ok(effects)
    }

    /// A short, body-free description of the operation under approval.
    fn summarise_target(&self, target_item_id: &ItemId, join: &JoinState) -> String {
        match join {
            JoinState::Unjoined { .. } => {
                "approval is waiting for the operation it refers to".to_owned()
            }
            JoinState::Joined { .. } => match self.items.get(target_item_id).map(|item| &item.body)
            {
                Some(ItemBody::FileEdit { change_set }) => {
                    format!("file edit with {} change(s)", change_set.entries.len())
                }
                Some(ItemBody::ToolCall { tool, .. }) => {
                    format!("tool call on the {:?} surface", tool.surface)
                }
                Some(_) => "operation awaiting approval".to_owned(),
                None => "approval is waiting for the operation it refers to".to_owned(),
            },
        }
    }

    // ------------------------------------------------------------- diagnostics

    fn record_diagnostic(
        &mut self,
        code: DiagnosticCode,
        session_id: Option<SessionId>,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<StateEffect, CodexAdapterError> {
        let _ = content;
        Ok(self.aggregate_diagnostic(code, session_id, None, at_ms))
    }

    fn record_labelled_diagnostic(
        &mut self,
        code: DiagnosticCode,
        session_id: Option<SessionId>,
        label: &str,
        at_ms: i64,
        content: &mut dyn ContentAccess,
    ) -> Result<StateEffect, CodexAdapterError> {
        // Section 1: an unmodelled upstream label may be kept, but only behind
        // a sensitive reference, never as plain log text.
        let detail_ref = self.store_text(content, ContentKind::StructuredSummary, label)?;
        Ok(self.aggregate_diagnostic(code, session_id, Some(detail_ref), at_ms))
    }

    fn aggregate_diagnostic(
        &mut self,
        code: DiagnosticCode,
        session_id: Option<SessionId>,
        detail_ref: Option<ContentRef>,
        at_ms: i64,
    ) -> StateEffect {
        let key = format!("{code:?}|{}", detail_ref.is_some());
        // Only the code, the scope identifier and the count are logged. The raw
        // upstream label, if any, stays behind the sensitive reference on the
        // record itself (section 1 and section 10).
        tracing::debug!(
            target: "kaleido.adapter",
            code = ?code,
            runtime = %self.runtime_id,
            "recorded an unmodelled or malformed upstream frame"
        );
        let record = self
            .diagnostics
            .entry(key)
            .and_modify(|record| {
                record.count = record.count.saturating_add(1);
                record.last_at_ms = at_ms;
            })
            .or_insert(DiagnosticRecord {
                runtime_id: Some(self.runtime_id.clone()),
                session_id,
                code,
                count: 1,
                first_at_ms: at_ms,
                last_at_ms: at_ms,
                detail_ref,
            });
        StateEffect::DiagnosticRecorded {
            diagnostic: record.clone(),
        }
    }

    fn store_text(
        &self,
        content: &mut dyn ContentAccess,
        kind: ContentKind,
        text: &str,
    ) -> Result<ContentRef, CodexAdapterError> {
        // Every provider-derived string this slice handles is on the section 10
        // list, so there is deliberately no business-sensitivity path.
        Ok(content.store(kind, Sensitivity::Sensitive, text.as_bytes())?)
    }
}

fn client_call(method: &str) -> ClientCall {
    match method {
        "thread/start" => ClientCall::ThreadStart,
        "turn/start" => ClientCall::TurnStart,
        _ => ClientCall::Unmodelled,
    }
}

fn join_is_deferred(entry: &AttentionItem) -> bool {
    matches!(
        &entry.subject,
        AttentionSubject::Approval { request }
            if matches!(
                request.join,
                JoinState::Unjoined {
                    reason: JoinFailureReason::ItemNotYetSeen
                }
            )
    )
}

/// The decision vocabulary, mirrored from the committed approval schema.
fn approval_options() -> Vec<DecisionOption> {
    APPROVAL_DECISIONS
        .iter()
        .map(|option_id| DecisionOption {
            option_id: (*option_id).to_owned(),
            label: (*option_id).to_owned(),
            semantics: match *option_id {
                "accept" => DecisionSemantics::Allow,
                "acceptForSession" => DecisionSemantics::AllowAlways,
                "decline" => DecisionSemantics::Deny,
                _ => DecisionSemantics::Cancel,
            },
        })
        .collect()
}

fn lifecycle_status(started: bool) -> ItemStatus {
    if started {
        ItemStatus::InProgress
    } else {
        ItemStatus::Completed
    }
}

/// Maps an upstream item status.
///
/// `declined` is the case this whole slice turns on: it is a terminal state a
/// human chose, not a failure (rule R-P8).
fn item_status(raw: &str) -> Result<ItemStatus, CodexAdapterError> {
    match raw {
        "inProgress" => Ok(ItemStatus::InProgress),
        "completed" => Ok(ItemStatus::Completed),
        "declined" => Ok(ItemStatus::Declined),
        "failed" => Ok(ItemStatus::Failed),
        _ => Err(CodexAdapterError::UnmodelledEnumeration {
            purpose: SurfacePurpose::FileChangeStatus,
        }),
    }
}

fn turn_status(raw: &str, purpose: SurfacePurpose) -> Result<TurnStatus, CodexAdapterError> {
    match raw {
        "inProgress" => Ok(TurnStatus::Running),
        "completed" => Ok(TurnStatus::Completed),
        "failed" => Ok(TurnStatus::Failed),
        "interrupted" => Ok(TurnStatus::Cancelled),
        _ => Err(CodexAdapterError::UnmodelledEnumeration { purpose }),
    }
}

fn message_phase(raw: Option<&str>) -> Result<MessagePhase, CodexAdapterError> {
    match raw {
        Some("commentary") => Ok(MessagePhase::Commentary),
        Some("final_answer") => Ok(MessagePhase::FinalAnswer),
        // Upstream documents the field as unreliable, so an absent phase is
        // treated as interim rather than promoted to a final answer.
        None => Ok(MessagePhase::Commentary),
        Some(_) => Err(CodexAdapterError::UnmodelledEnumeration {
            purpose: SurfacePurpose::AgentMessagePhase,
        }),
    }
}

fn file_change_kind(raw: &str) -> Result<FileChangeKind, CodexAdapterError> {
    match raw {
        "add" => Ok(FileChangeKind::Add),
        "update" => Ok(FileChangeKind::Modify),
        "delete" => Ok(FileChangeKind::Delete),
        _ => Err(CodexAdapterError::UnmodelledEnumeration {
            purpose: SurfacePurpose::FileChangeEntryKind,
        }),
    }
}

/// Upstream turn timestamps are Unix seconds; rule R-P2 requires milliseconds.
fn seconds_to_millis(seconds: i64) -> i64 {
    seconds.saturating_mul(1_000)
}

fn terminal_timestamp(status: TurnStatus, at_ms: i64) -> Option<i64> {
    if status.is_terminal() {
        Some(at_ms)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;

    #[test]
    fn later_runtime_acceptance_keeps_the_first_control_evidence() {
        let mut reducer = CodexReducer::new(ReducerConfig {
            host_display_name: "control-test-host".to_owned(),
            host_platform: HostPlatform::Windows,
            project_display_name: "control-test-project".to_owned(),
            identity_salt: "control-test-salt".to_owned(),
            evidence: EvidenceSource::ObservedInTraffic,
            launch_surface: LaunchSurface::BrokerLaunched,
            turn_origin: TurnOrigin::LocalSurface,
            base_at_ms: 100,
            runtime_version_label: None,
        });
        let session_id = SessionId::new("ses_control_test");
        reducer.session_id = Some(session_id.clone());
        reducer.session = Some(Session {
            id: session_id,
            project_id: reducer.project_id.clone(),
            project_binding_id: reducer.project_binding_id.clone(),
            ownership: OwnershipMode::BrokerManaged,
            history_source: HistorySource {
                kind: HistorySourceKind::BrokerLog,
                runtime_id: Some(reducer.runtime_id.clone()),
                evidence: reducer.evidence(100),
            },
            live_binding: LiveBinding::Observing {
                runtime_id: reducer.runtime_id.clone(),
                since_at_ms: 100,
                evidence: reducer.evidence(100),
            },
            status: SessionStatus::Idle,
            title: None,
            created_at_ms: 100,
            updated_at_ms: 100,
            last_activity_at_ms: 100,
            active_turn_id: None,
            queue_depth: 0,
            open_attention_count: 0,
            archived: false,
            binding_handle: None,
        });

        let first = reducer
            .runtime_acceptance_effects(CommandId::new("cmd_first"), 200)
            .unwrap_or_else(|error| panic!("first acceptance failed: {error}"));
        assert!(first
            .iter()
            .any(|effect| matches!(effect, StateEffect::SessionUpserted { .. })));

        let second = reducer
            .runtime_acceptance_effects(CommandId::new("cmd_second"), 300)
            .unwrap_or_else(|error| panic!("second acceptance failed: {error}"));
        assert_eq!(second.len(), 1);
        assert!(matches!(
            second.first(),
            Some(StateEffect::CommandAcknowledged { .. })
        ));
        let Some(session) = reducer.session.as_ref() else {
            panic!("the session must remain bound");
        };
        assert!(matches!(
            session.live_binding,
            LiveBinding::Controlling {
                since_at_ms: 100,
                evidence: CapabilityEvidence {
                    observed_at_ms: 200,
                    ..
                },
                ..
            }
        ));
    }
}
