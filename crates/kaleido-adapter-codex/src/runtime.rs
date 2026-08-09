//! A broker-owned Codex app-server connection.
//!
//! The transport only frames stdio. Both directions are handed to
//! [`CodexReducer`] exactly as the recorded transcript path does, so live and
//! replay cannot acquire separate protocol interpretations.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::content::{ContentAccess, ContentAccessError};
use kaleido_adapter::session::{ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest};
use kaleido_proto::attention::AttentionResponse;
use kaleido_proto::content::{ContentKind, ContentRef};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::ConnectionFaultReason;
use kaleido_proto::ids::{CommandId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId};
use serde_json::{json, Value};

use crate::process::{ChildTransport, Receive};
use crate::transcript::{Direction, TranscriptFrame};
use crate::{CodexAdapterError, CodexReducer, ReducerConfig};

const INITIALIZE_REQUEST_ID: i64 = 1;
const THREAD_START_REQUEST_ID: i64 = 2;
const FIRST_TURN_REQUEST_ID: i64 = 3;

enum AwaitResponse {
    Received,
    ProcessExited(Option<i64>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexSandboxMode {
    WorkspaceWrite,
    ReadOnly,
}

impl CodexSandboxMode {
    fn wire_name(self) -> &'static str {
        match self {
            Self::WorkspaceWrite => "workspace-write",
            Self::ReadOnly => "read-only",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexRuntimeConfig {
    pub executable: PathBuf,
    pub reducer: ReducerConfig,
    pub sandbox: CodexSandboxMode,
    pub request_timeout: Duration,
}

#[derive(Debug)]
pub struct CodexRuntimeSession {
    reducer: CodexReducer,
    executable: PathBuf,
    sandbox: CodexSandboxMode,
    request_timeout: Duration,
    base_at_ms: i64,
    transport: Option<ChildTransport>,
    observation_started: Option<Instant>,
    started: bool,
    exit_reported: bool,
    next_client_request_id: i64,
}

impl CodexRuntimeSession {
    pub fn new(config: CodexRuntimeConfig) -> Self {
        let base_at_ms = config.reducer.base_at_ms;
        Self {
            reducer: CodexReducer::new(config.reducer),
            executable: config.executable,
            sandbox: config.sandbox,
            request_timeout: config.request_timeout,
            base_at_ms,
            transport: None,
            observation_started: None,
            started: false,
            exit_reported: false,
            next_client_request_id: FIRST_TURN_REQUEST_ID,
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

    fn offset_ms(&self) -> i64 {
        self.observation_started
            .map(|started| i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    fn absolute_at_ms(&self) -> i64 {
        self.base_at_ms.saturating_add(self.offset_ms())
    }

    fn validate_effects(effects: &[StateEffect]) -> Result<(), RuntimeSessionError> {
        for effect in effects {
            effect.validate_for_log()?;
        }
        Ok(())
    }

    fn ingest_with_content(
        &mut self,
        direction: Direction,
        bytes: &[u8],
        content: &mut dyn ContentAccess,
    ) -> Result<(TranscriptFrame, Vec<StateEffect>), RuntimeSessionError> {
        let frame = TranscriptFrame::from_wire(direction, self.offset_ms(), bytes)
            .map_err(protocol_violation)?;
        let effects = self
            .reducer
            .ingest_frame(&frame, content)
            .map_err(protocol_violation)?;
        Self::validate_effects(&effects)?;
        Ok((frame, effects))
    }

    fn send_value(
        &mut self,
        value: &Value,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let bytes =
            serde_json::to_vec(value).map_err(|_| RuntimeSessionError::ProtocolViolation {
                detail: "a local JSON-RPC frame could not be encoded".to_owned(),
            })?;
        let sent = self
            .transport
            .as_mut()
            .ok_or(RuntimeSessionError::NotConnected)?
            .send(&bytes);
        if let Err(error) = sent {
            if error.kind() == std::io::ErrorKind::BrokenPipe && self.reducer.session_id().is_some()
            {
                self.exit_reported = true;
                let at_ms = self.absolute_at_ms();
                let effects = self
                    .reducer
                    .process_exited(None, at_ms)
                    .map_err(protocol_violation)?;
                Self::validate_effects(&effects)?;
                return Ok(effects);
            }
            return Err(transport_error(error));
        }
        let (_, effects) = self.ingest_with_content(Direction::ClientToServer, &bytes, content)?;
        Ok(effects)
    }

    fn await_response(
        &mut self,
        request_id: i64,
        content: &mut dyn ContentAccess,
        effects: &mut Vec<StateEffect>,
    ) -> Result<AwaitResponse, RuntimeSessionError> {
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
                .map_err(transport_error)?;
            match received {
                Receive::Line(bytes) => {
                    let (frame, frame_effects) =
                        self.ingest_with_content(Direction::ServerToClient, &bytes, content)?;
                    effects.extend(frame_effects);
                    if frame.method().is_none() && frame.request_id() == Some(request_id) {
                        return Ok(AwaitResponse::Received);
                    }
                }
                Receive::EndOfStream(exit_code) => {
                    self.exit_reported = true;
                    let at_ms = self.absolute_at_ms();
                    let exit_effects = self
                        .reducer
                        .process_exited(exit_code, at_ms)
                        .map_err(protocol_violation)?;
                    Self::validate_effects(&exit_effects)?;
                    effects.extend(exit_effects);
                    return Ok(AwaitResponse::ProcessExited(exit_code));
                }
                Receive::TimedOut => return Err(timeout_error()),
            }
        }
    }

    fn require_started(&self) -> Result<(), RuntimeSessionError> {
        if self.started && !self.exit_reported {
            Ok(())
        } else {
            Err(RuntimeSessionError::NotConnected)
        }
    }
}

impl ProviderRuntimeSession for CodexRuntimeSession {
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
            ChildTransport::spawn(&self.executable, Path::new(&root)).map_err(transport_error)?,
        );
        self.observation_started = Some(Instant::now());
        self.started = true;
        self.exit_reported = false;

        let mut effects = self.send_value(
            &json!({
                "id": INITIALIZE_REQUEST_ID,
                "method": "initialize",
                "params": {
                    "capabilities": { "mcpServerOpenaiFormElicitation": true },
                    "clientInfo": {
                        "name": "kaleido-hostd",
                        "title": "OneKaleidoscope host daemon",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
            content,
        )?;
        if let AwaitResponse::ProcessExited(exit_code) =
            self.await_response(INITIALIZE_REQUEST_ID, content, &mut effects)?
        {
            return Err(RuntimeSessionError::ConnectionFault {
                reason: ConnectionFaultReason::ProcessExited { exit_code },
            });
        }
        effects.extend(self.send_value(&json!({ "method": "initialized" }), content)?);
        effects.extend(self.send_value(
            &json!({
                "id": THREAD_START_REQUEST_ID,
                "method": "thread/start",
                "params": {
                    "approvalPolicy": "on-request",
                    "cwd": root,
                    "ephemeral": true,
                    "sandbox": self.sandbox.wire_name()
                }
            }),
            content,
        )?);
        if let AwaitResponse::ProcessExited(exit_code) =
            self.await_response(THREAD_START_REQUEST_ID, content, &mut effects)?
        {
            return Err(RuntimeSessionError::ConnectionFault {
                reason: ConnectionFaultReason::ProcessExited { exit_code },
            });
        }
        Ok(effects)
    }

    fn submit_prompt(
        &mut self,
        command_id: &CommandId,
        body: &ContentRef,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        body.ensure_sensitive("submit_prompt.body")?;
        let body = content.load(body).map_err(content_load_error)?;
        let body = std::str::from_utf8(&body)
            .map_err(|_| RuntimeSessionError::ProtocolViolation {
                detail: "the prompt body is not valid UTF-8".to_owned(),
            })?
            .to_owned();
        let raw_thread = self
            .reducer
            .raw_thread_id()
            .ok_or_else(|| RuntimeSessionError::ProtocolViolation {
                detail: "the runtime did not bind a thread".to_owned(),
            })?
            .to_owned();
        let request_id = self.next_client_request_id;
        self.next_client_request_id = self.next_client_request_id.saturating_add(1);
        if !self
            .reducer
            .register_local_turn_start(request_id, command_id)
        {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        let sent = self.send_value(
            &json!({
                "id": request_id,
                "method": "turn/start",
                "params": {
                    "input": [{ "text": body, "type": "text" }],
                    "threadId": raw_thread
                }
            }),
            content,
        );
        let mut effects = match sent {
            Ok(effects) => effects,
            Err(error) => {
                self.reducer.cancel_local_turn_start(request_id);
                return Err(error);
            }
        };
        if self.exit_reported {
            self.reducer.cancel_local_turn_start(request_id);
            return Ok(effects);
        }
        if let Err(error) = self.await_response(request_id, content, &mut effects) {
            self.reducer.cancel_local_turn_start(request_id);
            return Err(error);
        }
        self.reducer.cancel_local_turn_start(request_id);
        Ok(effects)
    }

    fn respond_attention(
        &mut self,
        response: &AttentionResponse,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        if response.free_form_ref.is_some() {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        let option = response
            .option_id
            .as_deref()
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        if !matches!(option, "accept" | "acceptForSession" | "decline" | "cancel") {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        let request_id = self
            .reducer
            .approval_request_id(&response.attention_id)
            .ok_or(RuntimeSessionError::CapabilityUnavailable)?;
        // A live reply is still decoded by the shared reducer. The reducer
        // deliberately emits no synthetic canonical answer in live mode: the
        // store has already applied the real RespondAttention command.
        self.send_value(
            &json!({ "id": request_id, "result": { "decision": option } }),
            content,
        )
    }

    fn drain_effects(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        let mut effects = Vec::new();
        loop {
            let received = self
                .transport
                .as_mut()
                .ok_or(RuntimeSessionError::NotConnected)?
                .try_receive()
                .map_err(transport_error)?;
            match received {
                Some(Receive::Line(bytes)) => {
                    let (_, frame_effects) =
                        self.ingest_with_content(Direction::ServerToClient, &bytes, content)?;
                    effects.extend(frame_effects);
                }
                Some(Receive::EndOfStream(exit_code)) => {
                    self.exit_reported = true;
                    let at_ms = self.absolute_at_ms();
                    let exit_effects = self
                        .reducer
                        .process_exited(exit_code, at_ms)
                        .map_err(protocol_violation)?;
                    Self::validate_effects(&exit_effects)?;
                    effects.extend(exit_effects);
                    break;
                }
                Some(Receive::TimedOut) | None => break,
            }
        }
        Ok(effects)
    }

    fn close(&mut self) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.require_started()?;
        let mut transport = self
            .transport
            .take()
            .ok_or(RuntimeSessionError::NotConnected)?;
        transport.terminate().map_err(transport_error)?;
        self.started = false;
        let at_ms = self.absolute_at_ms();
        let effects = self
            .reducer
            .clean_disconnected(at_ms)
            .map_err(protocol_violation)?;
        Self::validate_effects(&effects)?;
        Ok(effects)
    }

    fn capability_probe(&self) -> CapabilityProbe {
        self.reducer.capability_probe()
    }
}

fn protocol_violation(_error: CodexAdapterError) -> RuntimeSessionError {
    RuntimeSessionError::ProtocolViolation {
        detail: "Codex traffic failed pinned-surface decoding".to_owned(),
    }
}

fn transport_error(_error: std::io::Error) -> RuntimeSessionError {
    RuntimeSessionError::ConnectionFault {
        reason: ConnectionFaultReason::TransportError,
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
