//! Broker-owned Claude Agent SDK sidecar session.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::content::{ContentAccess, ContentAccessError};
use kaleido_adapter::session::{ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest};
use kaleido_proto::attention::{AttentionResponse, AttentionSubject};
use kaleido_proto::capability::CapabilityUnavailableReason;
use kaleido_proto::command::RuntimeAcceptanceKind;
use kaleido_proto::content::{ContentKind, ContentRef};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::ConnectionFaultReason;
use kaleido_proto::ids::{
    CommandId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId, TurnId,
};
use kaleido_proto::queue::{QueueEntry, QueueIntent, QueueState};
use serde_json::{json, Value};

use crate::error::ClaudeAdapterError;
use crate::process::{ChildTransport, Receive};
use crate::reduce::{ClaudeReducer, ReducerConfig};
use crate::transcript::{Direction, TranscriptFrame, SIDECAR_PROTOCOL, SIDECAR_VERSION};

#[derive(Debug, Clone)]
pub struct ClaudeRuntimeConfig {
    pub node_executable: PathBuf,
    pub bridge_script: PathBuf,
    pub reducer: ReducerConfig,
    pub request_timeout: Duration,
    pub resume_session: Option<String>,
}

impl ClaudeRuntimeConfig {
    pub fn new(
        node_executable: impl Into<PathBuf>,
        bridge_script: impl Into<PathBuf>,
        reducer: ReducerConfig,
    ) -> Self {
        Self {
            node_executable: node_executable.into(),
            bridge_script: bridge_script.into(),
            reducer,
            request_timeout: Duration::from_secs(30),
            resume_session: None,
        }
    }
}

#[derive(Debug)]
pub struct ClaudeRuntimeSession {
    reducer: ClaudeReducer,
    node_executable: PathBuf,
    bridge_script: PathBuf,
    request_timeout: Duration,
    resume_session: Option<String>,
    transport: Option<ChildTransport>,
    observation_started: Option<Instant>,
    started: bool,
    exit_reported: bool,
    next_turn: u64,
    active_cwd: Option<String>,
    discovery_cwd: Option<PathBuf>,
}

impl ClaudeRuntimeSession {
    pub fn new(config: ClaudeRuntimeConfig) -> Self {
        Self {
            reducer: ClaudeReducer::new(config.reducer),
            node_executable: config.node_executable,
            bridge_script: config.bridge_script,
            request_timeout: config.request_timeout,
            resume_session: config.resume_session,
            transport: None,
            observation_started: None,
            started: false,
            exit_reported: false,
            next_turn: 0,
            active_cwd: None,
            discovery_cwd: None,
        }
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
        self.reducer.session_id()
    }

    pub fn reducer(&self) -> &ClaudeReducer {
        &self.reducer
    }

    /// Discover persisted Claude sessions through the SDK's official
    /// `listSessions` API.  The returned effects contain only host/runtime
    /// bootstrap and capability evidence; provider session ids remain behind
    /// `ClaudeReducer::discovered_sessions`.
    pub fn discover(
        &mut self,
        cwd: &Path,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if self.started {
            return Err(RuntimeSessionError::AlreadyStarted);
        }
        if !cwd.is_absolute() {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "Claude discovery cwd must be absolute".to_owned(),
            });
        }
        let transport = ChildTransport::spawn(&self.node_executable, &self.bridge_script, cwd)
            .map_err(|_| RuntimeSessionError::ConnectionFault {
                reason: ConnectionFaultReason::TransportError,
            })?;
        self.transport = Some(transport);
        self.observation_started = Some(Instant::now());
        self.exit_reported = false;
        let cwd_text = cwd.to_string_lossy().into_owned();
        self.active_cwd = Some(cwd_text.clone());
        let response = self.send_command("list_sessions", json!({ "cwd": cwd_text }));
        let result = response.and_then(|_| self.await_kind("session_list", content));
        let cleanup = self.close_transport();
        match result {
            Ok(effects) => {
                cleanup?;
                self.discovery_cwd = Some(cwd.to_path_buf());
                Ok(effects)
            }
            Err(error) => {
                let _ = cleanup;
                Err(error)
            }
        }
    }

    /// Read one bounded page through the official `getSessionMessages` API.
    /// The canonical session must have been returned by `discover` for this
    /// exact directory; neither the raw provider id nor an ambient project
    /// search is accepted.
    pub fn read_history(
        &mut self,
        session_id: &SessionId,
        cwd: &Path,
        offset: u64,
        limit: u64,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if self.started || self.transport.is_some() {
            return Err(RuntimeSessionError::AlreadyStarted);
        }
        if !cwd.is_absolute() || self.discovery_cwd.as_deref() != Some(cwd) {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "Claude history cwd does not match the exact discovery scope".to_owned(),
            });
        }
        if !(1..=100).contains(&limit) {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "Claude history page limit must be between 1 and 100".to_owned(),
            });
        }
        let raw = self
            .reducer
            .raw_discovered_session(session_id)
            .map(str::to_owned)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        self.transport = Some(
            ChildTransport::spawn(&self.node_executable, &self.bridge_script, cwd).map_err(
                |_| RuntimeSessionError::ConnectionFault {
                    reason: ConnectionFaultReason::TransportError,
                },
            )?,
        );
        self.observation_started = Some(Instant::now());
        self.exit_reported = false;
        let cwd_text = cwd.to_string_lossy().into_owned();
        self.active_cwd = Some(cwd_text.clone());
        let response = self.send_command(
            "get_session_messages",
            json!({
                "cwd": cwd_text,
                "session_id": raw,
                "offset": offset,
                "limit": limit,
            }),
        );
        let result = response.and_then(|_| self.await_kind("session_messages", content));
        let cleanup = self.close_transport();
        match result {
            Ok(effects) => {
                cleanup?;
                Ok(effects)
            }
            Err(error) => {
                let _ = self.force_terminate_transport();
                Err(error)
            }
        }
    }

    /// Start a fresh SDK query that resumes a provider session id observed in
    /// a prior discovery/result frame.  The resume id stays provider-private;
    /// canonical ids are minted by the reducer when traffic arrives.
    pub fn resume(
        &mut self,
        raw_session_id: &str,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if self.started || self.transport.is_some() {
            return Err(RuntimeSessionError::AlreadyStarted);
        }
        if raw_session_id.trim().is_empty() {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "Claude resume requires a non-empty provider session id".to_owned(),
            });
        }
        self.resume_session = Some(raw_session_id.to_owned());
        self.start(request, content)
    }

    /// Reconnect is an explicit alias for the SDK resume path.  Claude's
    /// sidecar has no event cursor; a resumed query is the lossless boundary.
    pub fn reconnect(
        &mut self,
        raw_session_id: &str,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.resume(raw_session_id, request, content)
    }

    fn offset_ms(&self) -> i64 {
        self.observation_started
            .and_then(|started| i64::try_from(started.elapsed().as_millis()).ok())
            .unwrap_or(0)
    }

    fn absolute_at_ms(&self) -> i64 {
        self.reducer
            .capability_probe()
            .observed_at_ms()
            .saturating_add(self.offset_ms())
    }

    fn require_started(&self) -> Result<(), RuntimeSessionError> {
        if self.started && !self.exit_reported {
            Ok(())
        } else {
            Err(RuntimeSessionError::NotConnected)
        }
    }

    fn send_command(&mut self, kind: &str, payload: Value) -> Result<(), RuntimeSessionError> {
        let bytes = encode_command(kind, payload)?;
        let sent = self
            .transport
            .as_mut()
            .ok_or(RuntimeSessionError::NotConnected)?
            .send(&bytes)
            .map_err(|_| RuntimeSessionError::ConnectionFault {
                reason: ConnectionFaultReason::TransportError,
            });
        if sent.is_err() {
            let _ = self.force_terminate_transport();
        }
        sent
    }

    fn force_terminate_transport(&mut self) -> Result<(), RuntimeSessionError> {
        let termination = if let Some(mut transport) = self.transport.take() {
            transport
                .terminate()
                .map_err(|_| RuntimeSessionError::ConnectionFault {
                    reason: ConnectionFaultReason::TransportError,
                })
        } else {
            Ok(())
        };
        self.transport = None;
        self.observation_started = None;
        self.active_cwd = None;
        self.started = false;
        self.exit_reported = false;
        termination
    }

    fn close_transport(&mut self) -> Result<(), RuntimeSessionError> {
        if self.transport.is_none() {
            return Ok(());
        }
        let handshake = (|| {
            self.send_command("close", json!({}))?;
            let deadline = Instant::now()
                .checked_add(self.request_timeout)
                .ok_or_else(timeout_error)?;
            loop {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(timeout_error)?;
                let received = self
                    .transport
                    .as_mut()
                    .ok_or(RuntimeSessionError::NotConnected)?
                    .receive(remaining)
                    .map_err(|_| RuntimeSessionError::ConnectionFault {
                        reason: ConnectionFaultReason::TransportError,
                    })?;
                match received {
                    Receive::Line(line) => {
                        let frame = TranscriptFrame::from_wire(
                            Direction::BridgeToHost,
                            self.offset_ms(),
                            &line,
                        )
                        .map_err(protocol_violation)?;
                        if frame.kind() == "error" {
                            return Err(RuntimeSessionError::ProtocolViolation {
                                detail: "Claude sidecar rejected the close handshake".to_owned(),
                            });
                        }
                        if frame.kind() == "closed" {
                            return Ok(());
                        }
                    }
                    Receive::EndOfStream(exit_code) => {
                        return Err(RuntimeSessionError::ConnectionFault {
                            reason: ConnectionFaultReason::ProcessExited { exit_code },
                        });
                    }
                    Receive::TimedOut => return Err(timeout_error()),
                }
            }
        })();
        let cleanup = self.force_terminate_transport();
        handshake?;
        cleanup
    }

    fn ingest_line(
        &mut self,
        line: &[u8],
        content: &mut dyn ContentAccess,
    ) -> Result<(String, Vec<StateEffect>), RuntimeSessionError> {
        let frame =
            match TranscriptFrame::from_wire(Direction::BridgeToHost, self.offset_ms(), line) {
                Ok(frame) => frame,
                Err(error) => {
                    let _ = self.force_terminate_transport();
                    return Err(protocol_violation(error));
                }
            };
        let kind = frame.kind().to_owned();
        if kind == "error" {
            let _ = self.force_terminate_transport();
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "Claude sidecar returned an explicit error frame".to_owned(),
            });
        }
        if matches!(
            kind.as_str(),
            "ready" | "session_started" | "session_resumed" | "session_list" | "session_messages"
        ) {
            let returned_cwd = frame
                .payload()
                .get("cwd")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol_violation(ClaudeAdapterError::MalformedFrame))?;
            if self.active_cwd.as_deref() != Some(returned_cwd) {
                let _ = self.force_terminate_transport();
                return Err(RuntimeSessionError::ProtocolViolation {
                    detail: "Claude sidecar changed the exact project cwd".to_owned(),
                });
            }
        }
        if kind == "ready" {
            let returned_resume = frame
                .payload()
                .get("resume_session_id")
                .and_then(Value::as_str);
            if returned_resume != self.resume_session.as_deref() {
                let _ = self.force_terminate_transport();
                return Err(RuntimeSessionError::ProtocolViolation {
                    detail: "Claude sidecar changed the requested resume identity".to_owned(),
                });
            }
        }
        let effects = match self.reducer.ingest_frame(&frame, content) {
            Ok(effects) => effects,
            Err(error) => {
                let _ = self.force_terminate_transport();
                return Err(protocol_violation(error));
            }
        };
        if let Err(error) = validate_effects(&effects) {
            let _ = self.force_terminate_transport();
            return Err(error);
        }
        Ok((kind, effects))
    }

    fn await_kind(
        &mut self,
        expected: &str,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let deadline = Instant::now()
            .checked_add(self.request_timeout)
            .ok_or_else(timeout_error)?;
        let mut effects = Vec::new();
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                let _ = self.force_terminate_transport();
                return Err(timeout_error());
            };
            let received = match self
                .transport
                .as_mut()
                .ok_or(RuntimeSessionError::NotConnected)?
                .receive(remaining)
            {
                Ok(received) => received,
                Err(_) => {
                    let _ = self.force_terminate_transport();
                    return Err(RuntimeSessionError::ConnectionFault {
                        reason: ConnectionFaultReason::TransportError,
                    });
                }
            };
            match received {
                Receive::Line(line) => {
                    let (kind, frame_effects) = self.ingest_line(&line, content)?;
                    effects.extend(frame_effects);
                    if kind == expected {
                        return Ok(effects);
                    }
                }
                Receive::EndOfStream(exit_code) => {
                    self.exit_reported = true;
                    let exit_effects = self
                        .reducer
                        .process_exited(exit_code, self.absolute_at_ms())
                        .map_err(protocol_violation)?;
                    effects.extend(exit_effects);
                    let _ = self.force_terminate_transport();
                    return Err(RuntimeSessionError::ConnectionFault {
                        reason: ConnectionFaultReason::ProcessExited { exit_code },
                    });
                }
                Receive::TimedOut => {
                    let _ = self.force_terminate_transport();
                    return Err(timeout_error());
                }
            }
        }
    }

    fn drain_available(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let mut effects = Vec::new();
        loop {
            let received = match self
                .transport
                .as_mut()
                .ok_or(RuntimeSessionError::NotConnected)?
                .try_receive()
            {
                Ok(received) => received,
                Err(_) => {
                    let _ = self.force_terminate_transport();
                    return Err(RuntimeSessionError::ConnectionFault {
                        reason: ConnectionFaultReason::TransportError,
                    });
                }
            };
            let Some(received) = received else {
                break;
            };
            match received {
                Receive::Line(line) => {
                    let (_, frame_effects) = self.ingest_line(&line, content)?;
                    effects.extend(frame_effects);
                }
                Receive::EndOfStream(exit_code) => {
                    self.exit_reported = true;
                    effects.extend(
                        self.reducer
                            .process_exited(exit_code, self.absolute_at_ms())
                            .map_err(protocol_violation)?,
                    );
                    let _ = self.force_terminate_transport();
                    break;
                }
                Receive::TimedOut => break,
            }
        }
        Ok(effects)
    }
}

impl ProviderRuntimeSession for ClaudeRuntimeSession {
    fn start(
        &mut self,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if self.started {
            return Err(RuntimeSessionError::AlreadyStarted);
        }
        if request.runtime_id != *self.reducer.runtime_id()
            || request.project_id != *self.reducer.project_id()
            || request.project_binding_id != *self.reducer.project_binding_id()
        {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "session start identity does not match the reducer".to_owned(),
            });
        }
        request
            .project_root_ref
            .ensure_sensitive("session_start.project_root_ref")?;
        if request.project_root_ref.kind != ContentKind::FilePath {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "the project root reference has the wrong content kind".to_owned(),
            });
        }
        let root = content
            .load(&request.project_root_ref)
            .map_err(content_load_error)?;
        let root = std::str::from_utf8(&root)
            .map_err(|_| RuntimeSessionError::ProtocolViolation {
                detail: "the project root is not valid UTF-8".to_owned(),
            })?
            .to_owned();
        if !Path::new(&root).is_absolute() {
            return Err(RuntimeSessionError::ProtocolViolation {
                detail: "the project root is not absolute".to_owned(),
            });
        }
        self.transport = Some(
            ChildTransport::spawn(&self.node_executable, &self.bridge_script, Path::new(&root))
                .map_err(|_| RuntimeSessionError::ConnectionFault {
                    reason: ConnectionFaultReason::TransportError,
                })?,
        );
        self.observation_started = Some(Instant::now());
        self.active_cwd = Some(root.clone());
        self.started = true;
        self.exit_reported = false;
        let result = self
            .send_command(
                "start",
                json!({
                    "cwd": root,
                    "resume": self.resume_session,
                }),
            )
            .and_then(|_| self.await_kind("ready", content));
        if result.is_err() {
            let _ = self.force_terminate_transport();
        }
        result
    }

    fn submit_prompt(
        &mut self,
        command_id: &CommandId,
        body: &ContentRef,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        body.ensure_sensitive("submit_prompt.body")?;
        let bytes = content.load(body).map_err(content_load_error)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| RuntimeSessionError::ProtocolViolation {
                detail: "the prompt body is not valid UTF-8".to_owned(),
            })?
            .to_owned();
        self.next_turn = self.next_turn.saturating_add(1);
        let turn_id = format!("turn-{}", self.next_turn);
        self.reducer.register_local_prompt(&turn_id, command_id);
        self.send_command("prompt", json!({ "turn_id": turn_id, "text": text }))?;
        let mut effects = self.await_kind("prompt_accepted", content)?;
        let session_id = self
            .reducer
            .session_id()
            .cloned()
            .ok_or(RuntimeSessionError::NotConnected)?;
        let at_ms = self.absolute_at_ms();
        effects.push(self.reducer.accepted_command_effect(
            &session_id,
            RuntimeAcceptanceKind::PromptTurn,
            command_id,
            &format!("prompt|{turn_id}"),
            at_ms,
        ));
        effects.push(self.reducer.publish_capabilities_effect());
        effects.extend(self.reducer.refresh_live_binding(at_ms));
        Ok(effects)
    }

    fn respond_attention(
        &mut self,
        command_id: &CommandId,
        response: &AttentionResponse,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        let raw_request = self
            .reducer
            .register_local_attention_answer(&response.attention_id, command_id)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?
            .to_owned();
        let attention = self
            .reducer
            .attention(&response.attention_id)
            .cloned()
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        let route = (|| -> Result<(&'static str, Value), RuntimeSessionError> {
            Ok(match attention.subject {
                AttentionSubject::Approval { .. } => {
                    if response.free_form_ref.is_some() || !response.question_answers.is_empty() {
                        return Err(RuntimeSessionError::CapabilityUnavailable);
                    }
                    let decision = response
                        .option_id
                        .as_deref()
                        .filter(|decision| {
                            matches!(*decision, "allow" | "allow_always" | "deny" | "cancel")
                        })
                        .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
                    (
                        "permission_result",
                        json!({ "request_id": raw_request.clone(), "decision": decision }),
                    )
                }
                AttentionSubject::Question { request } => {
                    if response.option_id.is_some()
                        || response.free_form_ref.is_some()
                        || response.question_answers.len() != request.questions.len()
                    {
                        return Err(RuntimeSessionError::CapabilityUnavailable);
                    }
                    let mut answers = Vec::with_capacity(request.questions.len());
                    for (question_index, question) in request.questions.iter().enumerate() {
                        let answer = response
                            .question_answers
                            .iter()
                            .find(|answer| answer.question_key == question.question_key)
                            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
                        let mut values = Vec::new();
                        for option_id in &answer.option_ids {
                            let option = question
                                .options
                                .iter()
                                .find(|option| &option.option_id == option_id)
                                .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
                            values.push(option.label.clone());
                        }
                        if let Some(reference) = &answer.free_form_ref {
                            let bytes = content.load(reference).map_err(content_load_error)?;
                            let text = std::str::from_utf8(&bytes).map_err(|_| {
                                RuntimeSessionError::ProtocolViolation {
                                    detail: "the question answer is not valid UTF-8".to_owned(),
                                }
                            })?;
                            values.push(text.to_owned());
                        }
                        if values.is_empty() {
                            return Err(RuntimeSessionError::CapabilityUnavailable);
                        }
                        answers.push(json!({
                            "question_index": question_index,
                            "values": values,
                        }));
                    }
                    (
                        "question_result",
                        json!({ "request_id": raw_request.clone(), "answers": answers }),
                    )
                }
                AttentionSubject::WorkflowGate { .. }
                | AttentionSubject::ConnectionFault { .. } => {
                    return Err(RuntimeSessionError::CapabilityUnavailable);
                }
            })
        })();
        let (kind, payload) = match route {
            Ok(route) => route,
            Err(error) => {
                self.reducer.forget_local_attention_answer(&raw_request);
                return Err(error);
            }
        };
        if let Err(error) = self.send_command(kind, payload) {
            self.reducer.forget_local_attention_answer(&raw_request);
            return Err(error);
        }
        self.await_kind(kind, content)
    }

    fn discover(
        &mut self,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let root = load_project_root(request, content)?;
        ClaudeRuntimeSession::discover(self, Path::new(&root), content)
    }

    fn reconnect(
        &mut self,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let raw = self
            .reducer
            .raw_session_id()
            .map(str::to_owned)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        let session_id = self
            .reducer
            .session_id()
            .cloned()
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        if self.transport.is_some() {
            self.close_transport()?;
        }
        self.started = false;
        self.exit_reported = false;
        self.reducer.prepare_resume(&session_id, &raw);
        ClaudeRuntimeSession::reconnect(self, &raw, request, content)
    }

    fn resume_session(
        &mut self,
        session_id: &SessionId,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let raw = self
            .reducer
            .raw_discovered_session(session_id)
            .map(str::to_owned)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        if self.transport.is_some() {
            self.close_transport()?;
        }
        self.started = false;
        self.exit_reported = false;
        self.reducer.prepare_resume(session_id, &raw);
        ClaudeRuntimeSession::resume(self, &raw, request, content)
    }

    fn connection_lost_effects(
        &mut self,
        reason: CapabilityUnavailableReason,
        at_ms: i64,
    ) -> Vec<StateEffect> {
        self.reducer.mark_connection_unavailable(reason, at_ms)
    }

    fn interrupt_turn(
        &mut self,
        command_id: &CommandId,
        turn_id: &TurnId,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        if !self.reducer.is_active_turn(turn_id) {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        self.send_command("interrupt", json!({ "turn_id": turn_id.as_str() }))?;
        let mut effects = self.await_kind("interrupt_result", content)?;
        let session_id = self
            .reducer
            .session_id()
            .cloned()
            .ok_or(RuntimeSessionError::NotConnected)?;
        let at_ms = self.absolute_at_ms();
        effects.push(self.reducer.accepted_command_effect(
            &session_id,
            RuntimeAcceptanceKind::SessionControl,
            command_id,
            &format!("interrupt|{}", turn_id.as_str()),
            at_ms,
        ));
        effects.push(self.reducer.publish_capabilities_effect());
        effects.extend(self.reducer.refresh_live_binding(at_ms));
        Ok(effects)
    }

    fn deliver_queue_entry(
        &mut self,
        command_id: &CommandId,
        entry: &QueueEntry,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        if entry.intent != QueueIntent::NewTurn
            || self.reducer.session_id() != Some(&entry.session_id)
        {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        entry.body.ensure_sensitive("queue_entry.body")?;
        let bytes = content.load(&entry.body).map_err(content_load_error)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| RuntimeSessionError::ProtocolViolation {
                detail: "the queued prompt body is not valid UTF-8".to_owned(),
            })?
            .to_owned();
        self.next_turn = self.next_turn.saturating_add(1);
        let raw_turn = format!("turn-{}", self.next_turn);
        self.reducer.register_queued_prompt(&raw_turn, command_id);
        if let Err(error) = self.send_command(
            "prompt",
            json!({ "turn_id": raw_turn.as_str(), "text": text }),
        ) {
            self.reducer.forget_local_prompt(&raw_turn);
            return Err(error);
        }
        let mut effects = match self.await_kind("prompt_accepted", content) {
            Ok(effects) => effects,
            Err(error) => {
                self.reducer.forget_local_prompt(&raw_turn);
                return Err(error);
            }
        };
        let turn_id = effects
            .iter()
            .filter_map(|effect| match effect {
                StateEffect::TurnUpserted { turn }
                    if turn.session_id == entry.session_id
                        && matches!(
                            &turn.origin,
                            kaleido_proto::turn::TurnOrigin::RemoteCommand {
                                command_id: turn_command_id
                            } if turn_command_id == command_id
                        ) =>
                {
                    Some(turn.id.clone())
                }
                _ => None,
            })
            .next_back()
            .ok_or(RuntimeSessionError::ProtocolViolation {
                detail: "the queued prompt receipt produced no correlated turn".to_owned(),
            })?;
        let at_ms = self.absolute_at_ms();
        let mut delivered = entry.clone();
        delivered.state = QueueState::DeliveredAsNewTurn {
            turn_id,
            delivered_at_ms: at_ms,
        };
        delivered.editable = false;
        delivered.updated_at_ms = at_ms;
        effects.push(StateEffect::QueueEntryUpserted { entry: delivered });
        Ok(effects)
    }

    fn drain_effects(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        self.drain_available(content)
    }

    fn close(&mut self) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        let at_ms = self.absolute_at_ms();
        self.close_transport()?;
        self.reducer
            .clean_disconnected(at_ms)
            .map_err(protocol_violation)
    }

    fn capability_probe(&self) -> CapabilityProbe {
        self.reducer.capability_probe()
    }
}

fn validate_effects(effects: &[StateEffect]) -> Result<(), RuntimeSessionError> {
    for effect in effects {
        effect.validate_for_log()?;
    }
    Ok(())
}

fn protocol_violation(_error: ClaudeAdapterError) -> RuntimeSessionError {
    RuntimeSessionError::ProtocolViolation {
        detail: "Claude sidecar traffic violated the versioned protocol".to_owned(),
    }
}

fn content_load_error(_error: ContentAccessError) -> RuntimeSessionError {
    RuntimeSessionError::ProtocolViolation {
        detail: "a referenced sensitive body could not be loaded".to_owned(),
    }
}

fn timeout_error() -> RuntimeSessionError {
    RuntimeSessionError::ConnectionFault {
        reason: ConnectionFaultReason::Timeout,
    }
}

fn encode_command(kind: &str, payload: Value) -> Result<Vec<u8>, RuntimeSessionError> {
    serde_json::to_vec(&json!({
        "v": SIDECAR_VERSION,
        "protocol": SIDECAR_PROTOCOL,
        "kind": kind,
        "payload": payload,
    }))
    .map_err(|_| RuntimeSessionError::ProtocolViolation {
        detail: "sidecar command could not be encoded".to_owned(),
    })
}

fn load_project_root(
    request: &SessionStartRequest,
    content: &mut dyn ContentAccess,
) -> Result<String, RuntimeSessionError> {
    request
        .project_root_ref
        .ensure_sensitive("session_start.project_root_ref")?;
    if request.project_root_ref.kind != ContentKind::FilePath {
        return Err(RuntimeSessionError::ProtocolViolation {
            detail: "the project root reference has the wrong content kind".to_owned(),
        });
    }
    let root = content
        .load(&request.project_root_ref)
        .map_err(content_load_error)?;
    let root = std::str::from_utf8(&root)
        .map_err(|_| RuntimeSessionError::ProtocolViolation {
            detail: "the project root is not valid UTF-8".to_owned(),
        })?
        .to_owned();
    if !Path::new(&root).is_absolute() {
        return Err(RuntimeSessionError::ProtocolViolation {
            detail: "the project root is not absolute".to_owned(),
        });
    }
    Ok(root)
}
