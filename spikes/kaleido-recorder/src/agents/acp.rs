use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;

use super::{
    validate_exact_permission_cwd, validate_exact_permission_path, validate_permission_argv_as,
    validate_permission_command_as, validate_permission_path, PermissionCommand,
    PermissionScopeError,
};
use crate::fixture::{Direction, FixtureSink, Transport};
use crate::platform::{self, ResolvedExecutable};
use crate::stdio_tee::{StdioError, StdioTee};

pub const CLAUDE_ACP_PACKAGE_NAME: &str = "@agentclientprotocol/claude-agent-acp";
pub const CLAUDE_ACP_VERSION: &str = "0.63.0";
pub const CLAUDE_ACP_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.63.0";
pub const CLAUDE_ACP_INSTALL_COMMAND: &str =
    "npm install --global @agentclientprotocol/claude-agent-acp@0.63.0";
pub const ACP_PROTOCOL_VERSION: u64 = 1;
const DEFAULT_MESSAGE_TIMEOUT: Duration = Duration::from_secs(180);

const EXPLICIT_CREDENTIAL_VARIABLES: [&str; 12] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "ANTHROPIC_FOUNDRY_API_KEY",
];

/// Arguments for the already-installed ADR-0004-pinned Claude ACP launcher.
///
/// The executable itself must already have been resolved by the platform-aware
/// discovery layer. On Windows that executable is normally
/// `claude-agent-acp.cmd`. Keeping this empty is deliberate: the recorder must
/// never invoke `npx` or give npm permission to download a missing package.
pub const fn pinned_launcher_arguments() -> &'static [OsString] {
    &[]
}

/// Whether the launcher's `--version` output proves the exact pinned package.
pub fn is_pinned_launcher_version(stdout: &str) -> bool {
    stdout.trim() == CLAUDE_ACP_VERSION
}

/// Names, never values, of explicit credential variables inherited by a child.
///
/// An empty result is a prerequisite for using a successful protocol turn as
/// R-11 evidence that the bundled Claude binary reused the user's normal
/// Claude login state.
pub fn explicit_credential_variables_present() -> Vec<&'static str> {
    EXPLICIT_CREDENTIAL_VARIABLES
        .into_iter()
        .filter(|name| env::var_os(name).is_some())
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpScenario {
    SimpleTurn,
    ToolCall,
    PermissionApprove,
    PermissionDeny,
    FileChange,
    Cancel,
    Error,
    SessionLoad,
    Elicitation,
}

impl AcpScenario {
    fn prompt(self) -> Option<&'static str> {
        match self {
            Self::SimpleTurn => Some(
                "Reply with exactly one short sentence confirming that this is the Kaleido \
                 fixture sandbox.",
            ),
            Self::ToolCall => Some(
                "Read the file notes.txt in the current sandbox and summarize its contents in \
                 one sentence.",
            ),
            Self::PermissionApprove | Self::PermissionDeny => Some(
                "Use a shell tool to run `cargo run` in the current sandbox. Ask for permission \
                 before running it.",
            ),
            Self::FileChange => {
                Some("Edit editable.txt in the current sandbox by replacing ORIGINAL with CHANGED.")
            }
            Self::Cancel => Some(
                "Use a shell tool to run `cargo run -- wait` in the current sandbox and wait for \
                 it to finish.",
            ),
            Self::Error => Some(
                "Use a shell tool to run `cargo run -- fail` in the current sandbox and report \
                 the failure.",
            ),
            Self::SessionLoad | Self::Elicitation => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilityObservation {
    pub cancel_session: bool,
    pub load_session: bool,
    pub list_sessions: bool,
    pub resume_session: bool,
    pub close_session: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScenarioObservations {
    pub session_update_kinds: Vec<String>,
    pub permission_requests: u64,
    pub failed_tool_update: bool,
    pub cancel_sent: bool,
    pub completed_tool_lifecycle: bool,
    pub failed_tool_lifecycle: bool,
    pub nonempty_file_diff: bool,
    pub file_changed_on_disk: bool,
    pub approved_permission_flow: bool,
    pub denied_permission_flow: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationStage {
    Initialize,
    NewSession,
    Prompt,
    ListSessions,
    LoadSession,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnsupportedReason {
    /// The pinned ACP v1 schema has no elicitation request or response method.
    ElicitationAbsentFromPinnedSchema,
    SessionIdRequired,
    SessionListNotAdvertised,
    SessionLoadNotAdvertised,
    NoSessionForSandbox,
    PermissionRequestNotObserved,
    RequiredPermissionOptionNotOffered,
    ToolCallLifecycleIncomplete,
    FileDiffNotObserved,
    PermissionApprovalDidNotComplete,
    PermissionDenialDidNotReachFailure,
    UnexpectedTerminalStopReason {
        actual_stop_reason: String,
    },
    TurnCompletedBeforeCancellation,
    CancellationNotConfirmed {
        actual_stop_reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioOutcome {
    Completed {
        stop_reason: String,
        capabilities: CapabilityObservation,
        observations: ScenarioObservations,
    },
    SessionLoaded {
        session_id: String,
        capabilities: CapabilityObservation,
        observations: ScenarioObservations,
    },
    Unsupported {
        scenario: AcpScenario,
        reason: UnsupportedReason,
        capabilities: Option<CapabilityObservation>,
        observations: ScenarioObservations,
    },
    AuthenticationRequired {
        stage: AuthenticationStage,
        advertised_methods: usize,
        advertised_method_ids: Vec<String>,
        capabilities: Option<CapabilityObservation>,
        observations: ScenarioObservations,
    },
    AgentError {
        stage: AuthenticationStage,
        code: i64,
        message: String,
        capabilities: Option<CapabilityObservation>,
        observations: ScenarioObservations,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineStep {
    Continue,
    Send(Vec<String>),
    Complete(ScenarioOutcome),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Created,
    Initialize { id: u64 },
    NewSession { id: u64 },
    Prompt { id: u64 },
    ListSessions { id: u64 },
    LoadSession { id: u64 },
    Done,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolCallLifecycle {
    tool_call_id: String,
    started: bool,
    permission_scope_safe: bool,
    permission_scope_unsafe: bool,
    update_seen: bool,
    terminal_status: Option<String>,
    nonempty_file_diff: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolScopeEvidence {
    Unproven,
    Safe,
    Unsafe,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PermissionFlow {
    tool_call_id: String,
    decision: PermissionDecision,
    terminal_after_reply: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PermissionDecision {
    Approve,
    Deny,
}

#[derive(Debug)]
pub struct AcpStateMachine {
    scenario: AcpScenario,
    sandbox: PathBuf,
    phase: Phase,
    next_id: u64,
    session_id: Option<String>,
    capabilities: Option<CapabilityObservation>,
    advertised_auth_methods: usize,
    advertised_auth_method_ids: Vec<String>,
    observations: ScenarioObservations,
    seen_cursors: Vec<String>,
    requested_session_id: Option<String>,
    listed_requested_session: bool,
    tool_calls: Vec<ToolCallLifecycle>,
    tool_lifecycle_integrity_failed: bool,
    permission_flows: Vec<PermissionFlow>,
    editable_before: Option<Vec<u8>>,
}

impl AcpStateMachine {
    pub fn new(sandbox: PathBuf, scenario: AcpScenario) -> Self {
        Self::with_requested_session_id(sandbox, scenario, None)
    }

    pub fn for_session_load(sandbox: PathBuf, session_id: String) -> Self {
        Self::with_requested_session_id(sandbox, AcpScenario::SessionLoad, Some(session_id))
    }

    fn with_requested_session_id(
        sandbox: PathBuf,
        scenario: AcpScenario,
        requested_session_id: Option<String>,
    ) -> Self {
        let editable_before = (scenario == AcpScenario::FileChange)
            .then(|| fs::read(sandbox.join("editable.txt")).ok())
            .flatten();
        Self {
            scenario,
            sandbox,
            phase: Phase::Created,
            next_id: 1,
            session_id: None,
            capabilities: None,
            advertised_auth_methods: 0,
            advertised_auth_method_ids: Vec::new(),
            observations: ScenarioObservations::default(),
            seen_cursors: Vec::new(),
            requested_session_id,
            listed_requested_session: false,
            tool_calls: Vec::new(),
            tool_lifecycle_integrity_failed: false,
            permission_flows: Vec::new(),
            editable_before,
        }
    }

    pub fn capabilities(&self) -> Option<&CapabilityObservation> {
        self.capabilities.as_ref()
    }

    pub fn start(&mut self) -> Result<MachineStep, AcpError> {
        if self.phase != Phase::Created {
            return Err(AcpError::InvalidState("state machine was already started"));
        }
        if self.scenario == AcpScenario::SessionLoad
            && self
                .requested_session_id
                .as_deref()
                .is_none_or(str::is_empty)
        {
            self.phase = Phase::Done;
            return Ok(MachineStep::Complete(ScenarioOutcome::Unsupported {
                scenario: self.scenario,
                reason: UnsupportedReason::SessionIdRequired,
                capabilities: None,
                observations: self.observations.clone(),
            }));
        }
        if self.scenario == AcpScenario::Elicitation {
            self.phase = Phase::Done;
            return Ok(MachineStep::Complete(ScenarioOutcome::Unsupported {
                scenario: self.scenario,
                reason: UnsupportedReason::ElicitationAbsentFromPinnedSchema,
                capabilities: None,
                observations: self.observations.clone(),
            }));
        }

        let id = self.take_id()?;
        self.phase = Phase::Initialize { id };
        Ok(MachineStep::Send(vec![serialize_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": ACP_PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": {
                        "readTextFile": false,
                        "writeTextFile": false
                    },
                    "terminal": false
                },
                "clientInfo": {
                    "name": "OneKaleidoscope fixture recorder",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }))?]))
    }

    pub fn accept_raw(&mut self, raw: &str) -> Result<MachineStep, AcpError> {
        if self.phase == Phase::Created || self.phase == Phase::Done {
            return Err(AcpError::InvalidState(
                "cannot accept a message in the current phase",
            ));
        }
        let message: Value = serde_json::from_str(raw)?;
        self.validate_pending_message(&message)?;
        self.accept_message(&message)
    }

    fn validate_pending_message(&self, message: &Value) -> Result<(), AcpError> {
        self.validate_pending_list_response(message)?;
        if message.get("method").and_then(Value::as_str) == Some("session/request_permission") {
            validate_acp_permission_scope(message, &self.sandbox, self.scenario)
                .map_err(|_| AcpError::UnsafePermissionScope)?;
        }
        Ok(())
    }

    fn validate_pending_list_response(&self, message: &Value) -> Result<(), AcpError> {
        let Phase::ListSessions { id } = self.phase else {
            return Ok(());
        };
        if message.get("method").is_some() {
            return Ok(());
        }
        ensure_response_id(message, id)?;
        if message.get("error").is_some() {
            return Ok(());
        }
        let result = response_result(message)?;
        validate_session_list_result(result, &self.sandbox)
    }

    fn accept_message(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        let object = message
            .as_object()
            .ok_or(AcpError::MessageShape("ACP message must be a JSON object"))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(AcpError::MessageShape(
                "ACP message must contain jsonrpc == 2.0",
            ));
        }

        if let Some(method) = object.get("method").and_then(Value::as_str) {
            return self.accept_method(message, method);
        }
        if object.contains_key("id")
            && (object.contains_key("result") || object.contains_key("error"))
        {
            return self.accept_response(message);
        }
        Err(AcpError::MessageShape(
            "ACP message is neither a request, response, nor notification",
        ))
    }

    fn accept_method(&mut self, message: &Value, method: &str) -> Result<MachineStep, AcpError> {
        match method {
            "session/update" => self.accept_session_update(message),
            "session/request_permission" => self.accept_permission_request(message),
            _ if message.get("id").is_some() => {
                let id = message
                    .get("id")
                    .ok_or(AcpError::MessageShape("request id is missing"))?;
                Ok(MachineStep::Send(vec![serialize_message(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": "Method not found"
                    }
                }))?]))
            }
            _ => Ok(MachineStep::Continue),
        }
    }

    fn accept_session_update(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        self.ensure_active_session(message)?;
        let update = message
            .get("params")
            .and_then(|params| params.get("update"))
            .ok_or(AcpError::MessageShape(
                "session/update params.update is missing",
            ))?;
        let update_kind =
            update
                .get("sessionUpdate")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "session/update discriminator is missing",
                ))?;
        let record_update_kind =
            update_kind != "agent_message_chunk" || agent_message_chunk_is_nonempty(update);
        if record_update_kind
            && !self
                .observations
                .session_update_kinds
                .iter()
                .any(|seen| seen == update_kind)
        {
            self.observations
                .session_update_kinds
                .push(update_kind.to_owned());
        }
        if update_kind == "tool_call_update"
            && update.get("status").and_then(Value::as_str) == Some("failed")
        {
            self.observations.failed_tool_update = true;
        }
        match update_kind {
            "tool_call" => self.observe_tool_call_start(update)?,
            "tool_call_update" => self.observe_tool_call_update(update)?,
            _ => {}
        }

        if self.scenario == AcpScenario::Cancel
            && matches!(self.phase, Phase::Prompt { .. })
            && !self.observations.cancel_sent
        {
            let session_id = self
                .session_id
                .as_deref()
                .ok_or(AcpError::InvalidState("active session id is missing"))?;
            self.observations.cancel_sent = true;
            return Ok(MachineStep::Send(vec![serialize_message(json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": {
                    "sessionId": session_id
                }
            }))?]));
        }
        Ok(MachineStep::Continue)
    }

    fn observe_tool_call_start(&mut self, update: &Value) -> Result<(), AcpError> {
        let tool_call_id =
            update
                .get("toolCallId")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "tool_call update.toolCallId is missing",
                ))?;
        let nonempty_file_diff = contains_nonempty_file_diff(update);
        let scope = classify_acp_tool_call_scope(update, &self.sandbox, self.scenario);
        if let Some(lifecycle) = self
            .tool_calls
            .iter_mut()
            .find(|lifecycle| lifecycle.tool_call_id == tool_call_id)
        {
            if lifecycle.terminal_status.is_some() {
                self.tool_lifecycle_integrity_failed = true;
            }
            lifecycle.started = true;
            lifecycle.permission_scope_safe |= scope == ToolScopeEvidence::Safe;
            lifecycle.permission_scope_unsafe |= scope == ToolScopeEvidence::Unsafe;
            lifecycle.nonempty_file_diff |= nonempty_file_diff;
        } else {
            self.tool_calls.push(ToolCallLifecycle {
                tool_call_id: tool_call_id.to_owned(),
                started: true,
                permission_scope_safe: scope == ToolScopeEvidence::Safe,
                permission_scope_unsafe: scope == ToolScopeEvidence::Unsafe,
                update_seen: false,
                terminal_status: None,
                nonempty_file_diff,
            });
        }
        self.refresh_lifecycle_observations();
        Ok(())
    }

    fn observe_tool_call_update(&mut self, update: &Value) -> Result<(), AcpError> {
        let tool_call_id =
            update
                .get("toolCallId")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "tool_call_update update.toolCallId is missing",
                ))?;
        let status = update.get("status").and_then(Value::as_str);
        let nonempty_file_diff = contains_nonempty_file_diff(update);
        let meaningful_update = contains_meaningful_tool_update(update);
        let scope = classify_acp_tool_call_scope(update, &self.sandbox, self.scenario);
        let lifecycle = if let Some(index) = self
            .tool_calls
            .iter()
            .position(|lifecycle| lifecycle.tool_call_id == tool_call_id)
        {
            self.tool_calls
                .get_mut(index)
                .ok_or(AcpError::InvalidState("tool lifecycle index was invalid"))?
        } else {
            self.tool_lifecycle_integrity_failed = true;
            self.tool_calls.push(ToolCallLifecycle {
                tool_call_id: tool_call_id.to_owned(),
                started: false,
                permission_scope_safe: false,
                permission_scope_unsafe: scope == ToolScopeEvidence::Unsafe,
                update_seen: false,
                terminal_status: None,
                nonempty_file_diff: false,
            });
            self.tool_calls
                .last_mut()
                .ok_or(AcpError::InvalidState("tool lifecycle was not stored"))?
        };
        lifecycle.permission_scope_safe |= scope == ToolScopeEvidence::Safe;
        lifecycle.permission_scope_unsafe |= scope == ToolScopeEvidence::Unsafe;
        lifecycle.update_seen |= meaningful_update;
        lifecycle.nonempty_file_diff |= nonempty_file_diff;
        if matches!(status, Some("completed" | "failed")) {
            if lifecycle
                .terminal_status
                .as_deref()
                .is_some_and(|terminal| Some(terminal) != status)
            {
                self.tool_lifecycle_integrity_failed = true;
            }
            lifecycle.terminal_status = status.map(str::to_owned);
            for flow in self.permission_flows.iter_mut().filter(|flow| {
                flow.tool_call_id == tool_call_id && flow.terminal_after_reply.is_none()
            }) {
                flow.terminal_after_reply = status.map(str::to_owned);
            }
        }
        self.refresh_lifecycle_observations();
        Ok(())
    }

    fn refresh_lifecycle_observations(&mut self) {
        let unique_lifecycle = if self.tool_lifecycle_integrity_failed {
            None
        } else {
            match self.tool_calls.as_slice() {
                [lifecycle] => Some(lifecycle),
                _ => None,
            }
        };
        self.observations.completed_tool_lifecycle = unique_lifecycle.is_some_and(|lifecycle| {
            lifecycle.started
                && lifecycle.permission_scope_safe
                && !lifecycle.permission_scope_unsafe
                && lifecycle.update_seen
                && lifecycle.terminal_status.as_deref() == Some("completed")
        });
        self.observations.failed_tool_lifecycle = unique_lifecycle.is_some_and(|lifecycle| {
            lifecycle.started
                && lifecycle.permission_scope_safe
                && !lifecycle.permission_scope_unsafe
                && lifecycle.update_seen
                && lifecycle.terminal_status.as_deref() == Some("failed")
        });
        self.observations.nonempty_file_diff = unique_lifecycle.is_some_and(|lifecycle| {
            lifecycle.started
                && lifecycle.permission_scope_safe
                && !lifecycle.permission_scope_unsafe
                && lifecycle.update_seen
                && lifecycle.terminal_status.as_deref() == Some("completed")
                && lifecycle.nonempty_file_diff
        });
        self.observations.approved_permission_flow = self.permission_flows.iter().any(|flow| {
            flow.decision == PermissionDecision::Approve
                && flow.terminal_after_reply.as_deref() == Some("completed")
                && unique_lifecycle.is_some_and(|lifecycle| {
                    lifecycle.tool_call_id == flow.tool_call_id
                        && lifecycle.started
                        && lifecycle.permission_scope_safe
                        && !lifecycle.permission_scope_unsafe
                        && lifecycle.terminal_status.as_deref() == Some("completed")
                })
        });
        self.observations.denied_permission_flow = self.permission_flows.iter().any(|flow| {
            flow.decision == PermissionDecision::Deny
                && flow.terminal_after_reply.as_deref() == Some("failed")
                && unique_lifecycle.is_some_and(|lifecycle| {
                    lifecycle.tool_call_id == flow.tool_call_id
                        && lifecycle.started
                        && lifecycle.permission_scope_safe
                        && !lifecycle.permission_scope_unsafe
                        && lifecycle.terminal_status.as_deref() == Some("failed")
                })
        });
    }

    fn tool_lifecycles_are_globally_safe(&self) -> bool {
        if self.tool_lifecycle_integrity_failed {
            return false;
        }
        let maximum_lifecycles = match self.scenario {
            AcpScenario::SimpleTurn | AcpScenario::SessionLoad | AcpScenario::Elicitation => 0,
            AcpScenario::ToolCall
            | AcpScenario::PermissionApprove
            | AcpScenario::PermissionDeny
            | AcpScenario::FileChange
            | AcpScenario::Cancel
            | AcpScenario::Error => 1,
        };
        self.tool_calls.len() <= maximum_lifecycles
            && self.tool_calls.iter().all(|lifecycle| {
                lifecycle.started
                    && lifecycle.permission_scope_safe
                    && !lifecycle.permission_scope_unsafe
            })
    }

    fn accept_permission_request(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        if !matches!(self.phase, Phase::Prompt { .. }) {
            return Err(AcpError::InvalidState(
                "permission request arrived outside a prompt turn",
            ));
        }
        self.ensure_active_session(message)?;
        let id = message
            .get("id")
            .ok_or(AcpError::MessageShape("permission request id is missing"))?;
        self.observations.permission_requests = self
            .observations
            .permission_requests
            .checked_add(1)
            .ok_or(AcpError::CounterOverflow)?;

        if self.scenario == AcpScenario::Cancel {
            let session_id = self
                .session_id
                .as_deref()
                .ok_or(AcpError::InvalidState("active session id is missing"))?;
            self.observations.cancel_sent = true;
            return Ok(MachineStep::Send(vec![
                serialize_message(json!({
                    "jsonrpc": "2.0",
                    "method": "session/cancel",
                    "params": {
                        "sessionId": session_id
                    }
                }))?,
                serialize_message(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "outcome": {
                            "outcome": "cancelled"
                        }
                    }
                }))?,
            ]));
        }

        let requested_kinds: &[&str] = if self.scenario == AcpScenario::PermissionDeny {
            &["reject_once", "reject_always"]
        } else {
            &["allow_once", "allow_always"]
        };
        let options = message
            .get("params")
            .and_then(|params| params.get("options"))
            .and_then(Value::as_array)
            .ok_or(AcpError::MessageShape(
                "permission request params.options is missing",
            ))?;
        let selected = requested_kinds.iter().find_map(|wanted| {
            options.iter().find(|option| {
                option.get("kind").and_then(Value::as_str) == Some(*wanted)
                    && option.get("optionId").and_then(Value::as_str).is_some()
            })
        });
        let Some(selected) = selected else {
            self.phase = Phase::Done;
            return Ok(MachineStep::Complete(ScenarioOutcome::Unsupported {
                scenario: self.scenario,
                reason: UnsupportedReason::RequiredPermissionOptionNotOffered,
                capabilities: self.capabilities.clone(),
                observations: self.observations.clone(),
            }));
        };
        let option_id = selected
            .get("optionId")
            .ok_or(AcpError::MessageShape("permission option id is missing"))?;
        let tool_call_id = message
            .pointer("/params/toolCall/toolCallId")
            .and_then(Value::as_str)
            .ok_or(AcpError::MessageShape(
                "permission request params.toolCall.toolCallId is missing",
            ))?;
        let decision = if self.scenario == AcpScenario::PermissionDeny {
            PermissionDecision::Deny
        } else {
            PermissionDecision::Approve
        };
        self.permission_flows.push(PermissionFlow {
            tool_call_id: tool_call_id.to_owned(),
            decision,
            terminal_after_reply: None,
        });
        self.refresh_lifecycle_observations();
        Ok(MachineStep::Send(vec![serialize_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id
                }
            }
        }))?]))
    }

    fn accept_response(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        match self.phase {
            Phase::Initialize { id } => {
                ensure_response_id(message, id)?;
                self.accept_initialize_response(message)
            }
            Phase::NewSession { id } => {
                ensure_response_id(message, id)?;
                self.accept_new_session_response(message)
            }
            Phase::Prompt { id } => {
                ensure_response_id(message, id)?;
                self.accept_prompt_response(message)
            }
            Phase::ListSessions { id } => {
                ensure_response_id(message, id)?;
                self.accept_list_sessions_response(message)
            }
            Phase::LoadSession { id } => {
                ensure_response_id(message, id)?;
                self.accept_load_session_response(message)
            }
            Phase::Created | Phase::Done => Err(AcpError::InvalidState(
                "response arrived outside an active request",
            )),
        }
    }

    fn accept_initialize_response(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        if let Some(step) = self.error_outcome(message, AuthenticationStage::Initialize)? {
            return Ok(step);
        }
        let result = response_result(message)?;
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .ok_or(AcpError::MessageShape(
                "initialize result.protocolVersion is missing",
            ))?;
        if protocol_version != ACP_PROTOCOL_VERSION {
            return Err(AcpError::ProtocolVersion {
                expected: ACP_PROTOCOL_VERSION,
                actual: protocol_version,
            });
        }

        self.advertised_auth_methods = result
            .get("authMethods")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        self.advertised_auth_method_ids = result
            .get("authMethods")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|method| method.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        let agent_capabilities = result.get("agentCapabilities");
        let session_capabilities =
            agent_capabilities.and_then(|capabilities| capabilities.get("sessionCapabilities"));
        let capabilities = CapabilityObservation {
            cancel_session: true,
            load_session: agent_capabilities
                .and_then(|capabilities| capabilities.get("loadSession"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            list_sessions: capability_object_present(session_capabilities, "list"),
            resume_session: capability_object_present(session_capabilities, "resume"),
            close_session: capability_object_present(session_capabilities, "close"),
        };
        self.capabilities = Some(capabilities.clone());

        if self.scenario == AcpScenario::SessionLoad {
            if !capabilities.list_sessions {
                self.phase = Phase::Done;
                return Ok(MachineStep::Complete(ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::SessionListNotAdvertised,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }));
            }
            if !capabilities.load_session {
                self.phase = Phase::Done;
                return Ok(MachineStep::Complete(ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::SessionLoadNotAdvertised,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }));
            }
            return self.send_list_sessions(None);
        }
        self.send_new_session()
    }

    fn send_new_session(&mut self) -> Result<MachineStep, AcpError> {
        let id = self.take_id()?;
        self.phase = Phase::NewSession { id };
        let cwd = sandbox_utf8(&self.sandbox)?;
        Ok(MachineStep::Send(vec![serialize_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": {
                "cwd": cwd,
                "mcpServers": []
            }
        }))?]))
    }

    fn accept_new_session_response(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        if let Some(step) = self.error_outcome(message, AuthenticationStage::NewSession)? {
            return Ok(step);
        }
        let session_id = response_result(message)?
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or(AcpError::MessageShape(
                "session/new result.sessionId is missing",
            ))?
            .to_owned();
        self.session_id = Some(session_id.clone());
        let prompt = self
            .scenario
            .prompt()
            .ok_or(AcpError::InvalidState("scenario has no prompt"))?;
        let id = self.take_id()?;
        self.phase = Phase::Prompt { id };
        Ok(MachineStep::Send(vec![serialize_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [
                    {
                        "type": "text",
                        "text": prompt
                    }
                ]
            }
        }))?]))
    }

    fn accept_prompt_response(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        if let Some(step) = self.error_outcome(message, AuthenticationStage::Prompt)? {
            return Ok(step);
        }
        let stop_reason = response_result(message)?
            .get("stopReason")
            .and_then(Value::as_str)
            .ok_or(AcpError::MessageShape(
                "session/prompt result.stopReason is missing",
            ))?
            .to_owned();
        let capabilities = self
            .capabilities
            .clone()
            .ok_or(AcpError::InvalidState("capability observation is missing"))?;
        if self.scenario == AcpScenario::FileChange {
            self.observations.file_changed_on_disk =
                self.editable_before.as_deref().is_some_and(|before| {
                    fs::read(self.sandbox.join("editable.txt"))
                        .is_ok_and(|after| after.as_slice() != before)
                });
        }
        let outcome = match self.scenario {
            _ if !self.tool_lifecycles_are_globally_safe() => ScenarioOutcome::Unsupported {
                scenario: self.scenario,
                reason: UnsupportedReason::ToolCallLifecycleIncomplete,
                capabilities: Some(capabilities),
                observations: self.observations.clone(),
            },
            AcpScenario::PermissionApprove | AcpScenario::PermissionDeny
                if self.observations.permission_requests == 0 =>
            {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::PermissionRequestNotObserved,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::ToolCall if !self.observations.completed_tool_lifecycle => {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::ToolCallLifecycleIncomplete,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::FileChange
                if !self.observations.nonempty_file_diff
                    || !self.observations.file_changed_on_disk =>
            {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::FileDiffNotObserved,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::Error if !self.observations.failed_tool_lifecycle => {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::ToolCallLifecycleIncomplete,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::PermissionApprove if !self.observations.approved_permission_flow => {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::PermissionApprovalDidNotComplete,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::PermissionDeny if !self.observations.denied_permission_flow => {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::PermissionDenialDidNotReachFailure,
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::ToolCall | AcpScenario::FileChange | AcpScenario::PermissionApprove
                if stop_reason != "end_turn" =>
            {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::UnexpectedTerminalStopReason {
                        actual_stop_reason: stop_reason,
                    },
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::PermissionDeny
                if !matches!(stop_reason.as_str(), "end_turn" | "refusal") =>
            {
                ScenarioOutcome::Unsupported {
                    scenario: self.scenario,
                    reason: UnsupportedReason::UnexpectedTerminalStopReason {
                        actual_stop_reason: stop_reason,
                    },
                    capabilities: Some(capabilities),
                    observations: self.observations.clone(),
                }
            }
            AcpScenario::Cancel if !self.observations.cancel_sent => ScenarioOutcome::Unsupported {
                scenario: self.scenario,
                reason: UnsupportedReason::TurnCompletedBeforeCancellation,
                capabilities: Some(capabilities),
                observations: self.observations.clone(),
            },
            AcpScenario::Cancel if stop_reason != "cancelled" => ScenarioOutcome::Unsupported {
                scenario: self.scenario,
                reason: UnsupportedReason::CancellationNotConfirmed {
                    actual_stop_reason: stop_reason,
                },
                capabilities: Some(capabilities),
                observations: self.observations.clone(),
            },
            _ => ScenarioOutcome::Completed {
                stop_reason,
                capabilities,
                observations: self.observations.clone(),
            },
        };
        self.phase = Phase::Done;
        Ok(MachineStep::Complete(outcome))
    }

    fn send_list_sessions(&mut self, cursor: Option<&str>) -> Result<MachineStep, AcpError> {
        let id = self.take_id()?;
        self.phase = Phase::ListSessions { id };
        let cwd = sandbox_utf8(&self.sandbox)?;
        let params = match cursor {
            Some(cursor) => json!({
                "cwd": cwd,
                "cursor": cursor
            }),
            None => json!({
                "cwd": cwd
            }),
        };
        Ok(MachineStep::Send(vec![serialize_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/list",
            "params": params
        }))?]))
    }

    fn accept_list_sessions_response(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        if let Some(step) = self.error_outcome(message, AuthenticationStage::ListSessions)? {
            return Ok(step);
        }
        let result = response_result(message)?;
        let sessions =
            result
                .get("sessions")
                .and_then(Value::as_array)
                .ok_or(AcpError::MessageShape(
                    "session/list result.sessions is missing",
                ))?;
        let requested_session_id = self
            .requested_session_id
            .as_deref()
            .ok_or(AcpError::InvalidState("session/load request id is missing"))?;
        if sessions.iter().any(|session| {
            session.get("sessionId").and_then(Value::as_str) == Some(requested_session_id)
        }) {
            self.listed_requested_session = true;
        }

        if let Some(cursor) = result.get("nextCursor").and_then(Value::as_str) {
            if self.seen_cursors.iter().any(|seen| seen == cursor) {
                return Err(AcpError::RepeatedCursor(cursor.to_owned()));
            }
            self.seen_cursors.push(cursor.to_owned());
            return self.send_list_sessions(Some(cursor));
        }

        if self.listed_requested_session {
            let session_id = self
                .requested_session_id
                .take()
                .ok_or(AcpError::InvalidState("session/load request id is missing"))?;
            return self.send_load_session(session_id);
        }

        self.phase = Phase::Done;
        Ok(MachineStep::Complete(ScenarioOutcome::Unsupported {
            scenario: self.scenario,
            reason: UnsupportedReason::NoSessionForSandbox,
            capabilities: self.capabilities.clone(),
            observations: self.observations.clone(),
        }))
    }

    fn send_load_session(&mut self, session_id: String) -> Result<MachineStep, AcpError> {
        let id = self.take_id()?;
        self.phase = Phase::LoadSession { id };
        self.session_id = Some(session_id.clone());
        let cwd = sandbox_utf8(&self.sandbox)?;
        Ok(MachineStep::Send(vec![serialize_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/load",
            "params": {
                "mcpServers": [],
                "cwd": cwd,
                "sessionId": session_id
            }
        }))?]))
    }

    fn accept_load_session_response(&mut self, message: &Value) -> Result<MachineStep, AcpError> {
        if let Some(step) = self.error_outcome(message, AuthenticationStage::LoadSession)? {
            return Ok(step);
        }
        let _ = response_result(message)?;
        let session_id = self
            .session_id
            .clone()
            .ok_or(AcpError::InvalidState("loaded session id is missing"))?;
        let capabilities = self
            .capabilities
            .clone()
            .ok_or(AcpError::InvalidState("capability observation is missing"))?;
        self.phase = Phase::Done;
        Ok(MachineStep::Complete(ScenarioOutcome::SessionLoaded {
            session_id,
            capabilities,
            observations: self.observations.clone(),
        }))
    }

    fn error_outcome(
        &mut self,
        message: &Value,
        stage: AuthenticationStage,
    ) -> Result<Option<MachineStep>, AcpError> {
        let Some(error) = message.get("error") else {
            return Ok(None);
        };
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or(AcpError::MessageShape("error.code is missing"))?;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .ok_or(AcpError::MessageShape("error.message is missing"))?
            .to_owned();
        self.phase = Phase::Done;
        let outcome = if code == -32000 {
            ScenarioOutcome::AuthenticationRequired {
                stage,
                advertised_methods: self.advertised_auth_methods,
                advertised_method_ids: self.advertised_auth_method_ids.clone(),
                capabilities: self.capabilities.clone(),
                observations: self.observations.clone(),
            }
        } else {
            ScenarioOutcome::AgentError {
                stage,
                code,
                message,
                capabilities: self.capabilities.clone(),
                observations: self.observations.clone(),
            }
        };
        Ok(Some(MachineStep::Complete(outcome)))
    }

    fn take_id(&mut self) -> Result<u64, AcpError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(AcpError::CounterOverflow)?;
        Ok(id)
    }

    fn ensure_active_session(&self, message: &Value) -> Result<(), AcpError> {
        let actual = message
            .get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str)
            .ok_or(AcpError::MessageShape(
                "session-scoped message params.sessionId is missing",
            ))?;
        let expected = self
            .session_id
            .as_deref()
            .ok_or(AcpError::InvalidState("active session id is missing"))?;
        if actual == expected {
            Ok(())
        } else {
            Err(AcpError::SessionIdMismatch)
        }
    }
}

fn agent_message_chunk_is_nonempty(update: &Value) -> bool {
    let Some(content) = update.get("content") else {
        return false;
    };
    match content.get("type").and_then(Value::as_str) {
        Some("text") => content
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        Some("image" | "audio") => content
            .get("data")
            .and_then(Value::as_str)
            .is_some_and(|data| !data.is_empty()),
        Some("resource_link") => content
            .get("uri")
            .and_then(Value::as_str)
            .is_some_and(|uri| !uri.is_empty()),
        Some("resource") => content
            .get("resource")
            .is_some_and(value_has_meaningful_payload),
        _ => false,
    }
}

fn validate_acp_permission_scope(
    message: &Value,
    sandbox: &Path,
    scenario: AcpScenario,
) -> Result<(), PermissionScopeError> {
    let tool_call = message
        .pointer("/params/toolCall")
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    match scenario {
        AcpScenario::PermissionApprove | AcpScenario::PermissionDeny => {
            validate_acp_command_tool_call_scope(tool_call, sandbox, PermissionCommand::Run, true)
        }
        AcpScenario::ToolCall => {
            validate_acp_file_tool_call_scope(tool_call, sandbox, "read", Some("notes.txt"), true)
        }
        AcpScenario::FileChange => validate_acp_file_tool_call_scope(
            tool_call,
            sandbox,
            "edit",
            Some("editable.txt"),
            true,
        ),
        AcpScenario::Cancel => {
            validate_acp_command_tool_call_scope(tool_call, sandbox, PermissionCommand::Wait, true)
        }
        AcpScenario::Error => {
            validate_acp_command_tool_call_scope(tool_call, sandbox, PermissionCommand::Fail, true)
        }
        AcpScenario::SimpleTurn | AcpScenario::SessionLoad | AcpScenario::Elicitation => {
            Err(PermissionScopeError::UnsafeCommand)
        }
    }
}

fn classify_acp_tool_call_scope(
    tool_call: &Value,
    sandbox: &Path,
    scenario: AcpScenario,
) -> ToolScopeEvidence {
    let expected_kind = match scenario {
        AcpScenario::ToolCall => "read",
        AcpScenario::FileChange => "edit",
        AcpScenario::PermissionApprove
        | AcpScenario::PermissionDeny
        | AcpScenario::Cancel
        | AcpScenario::Error => "execute",
        AcpScenario::SimpleTurn | AcpScenario::SessionLoad | AcpScenario::Elicitation => {
            return ToolScopeEvidence::Unsafe;
        }
    };
    if tool_call
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != expected_kind)
    {
        return ToolScopeEvidence::Unsafe;
    }
    if !acp_tool_call_has_scope_evidence(tool_call) {
        return ToolScopeEvidence::Unproven;
    }
    let validation = match scenario {
        AcpScenario::ToolCall => {
            validate_acp_file_tool_call_scope(tool_call, sandbox, "read", Some("notes.txt"), false)
        }
        AcpScenario::FileChange => validate_acp_file_tool_call_scope(
            tool_call,
            sandbox,
            "edit",
            Some("editable.txt"),
            false,
        ),
        AcpScenario::PermissionApprove | AcpScenario::PermissionDeny => {
            validate_acp_command_tool_call_scope(tool_call, sandbox, PermissionCommand::Run, false)
        }
        AcpScenario::Cancel => {
            validate_acp_command_tool_call_scope(tool_call, sandbox, PermissionCommand::Wait, false)
        }
        AcpScenario::Error => {
            validate_acp_command_tool_call_scope(tool_call, sandbox, PermissionCommand::Fail, false)
        }
        AcpScenario::SimpleTurn | AcpScenario::SessionLoad | AcpScenario::Elicitation => {
            return ToolScopeEvidence::Unsafe;
        }
    };
    if validation.is_ok() {
        ToolScopeEvidence::Safe
    } else {
        ToolScopeEvidence::Unsafe
    }
}

fn acp_tool_call_has_scope_evidence(tool_call: &Value) -> bool {
    tool_call
        .get("rawInput")
        .is_some_and(value_has_meaningful_payload)
        || tool_call
            .get("locations")
            .and_then(Value::as_array)
            .is_some_and(|locations| !locations.is_empty())
        || tool_call
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| {
                content
                    .iter()
                    .any(|entry| entry.get("type").and_then(Value::as_str) == Some("diff"))
            })
}

fn validate_acp_command_tool_call_scope(
    tool_call: &Value,
    sandbox: &Path,
    expected: PermissionCommand,
    permission_request: bool,
) -> Result<(), PermissionScopeError> {
    if tool_call
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "execute")
    {
        return Err(PermissionScopeError::UnsafeCommand);
    }
    let raw_input = tool_call
        .get("rawInput")
        .and_then(Value::as_object)
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    let command_value = raw_input
        .get("command")
        .or_else(|| raw_input.get("cmd"))
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    match command_value {
        Value::String(command) => validate_permission_command_as(command, expected)?,
        Value::Array(arguments) => {
            let arguments = arguments
                .iter()
                .map(|argument| {
                    argument
                        .as_str()
                        .map(str::to_owned)
                        .ok_or(PermissionScopeError::UnsafeCommand)
                })
                .collect::<Result<Vec<_>, _>>()?;
            validate_permission_argv_as(&arguments, expected)?;
        }
        _ => return Err(PermissionScopeError::UnsafeCommand),
    }
    for (field, value) in raw_input {
        match field.as_str() {
            "command" | "cmd" => {}
            "cwd" | "directory" | "workdir" => validate_exact_permission_cwd(
                sandbox,
                value.as_str().ok_or(PermissionScopeError::UnprovablePath)?,
            )?,
            _ => return Err(PermissionScopeError::UnsafeCommand),
        }
    }
    if permission_request
        && tool_call
            .get("rawOutput")
            .is_some_and(|value| !value.is_null())
    {
        return Err(PermissionScopeError::UnsafeCommand);
    }
    if let Some(locations) = tool_call.get("locations") {
        if !locations.is_null() {
            let locations = locations
                .as_array()
                .ok_or(PermissionScopeError::UnprovablePath)?;
            for location in locations {
                let path = location
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or(PermissionScopeError::UnprovablePath)?;
                validate_permission_path(sandbox, path)?;
            }
        }
    }
    if let Some(content) = tool_call.get("content") {
        if !content.is_null() {
            let content = content
                .as_array()
                .ok_or(PermissionScopeError::UnprovablePath)?;
            for entry in content {
                if entry.get("type").and_then(Value::as_str) == Some("diff") {
                    let path = entry
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(PermissionScopeError::UnprovablePath)?;
                    validate_permission_path(sandbox, path)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_acp_file_tool_call_scope(
    tool_call: &Value,
    sandbox: &Path,
    expected_kind: &str,
    expected_relative: Option<&str>,
    permission_request: bool,
) -> Result<(), PermissionScopeError> {
    match tool_call.get("kind").and_then(Value::as_str) {
        Some(kind) if kind != expected_kind => return Err(PermissionScopeError::UnsafeCommand),
        None if permission_request => return Err(PermissionScopeError::UnsafeCommand),
        Some(_) | None => {}
    }
    if permission_request
        && tool_call
            .get("rawOutput")
            .is_some_and(|value| !value.is_null())
    {
        return Err(PermissionScopeError::UnsafeCommand);
    }
    let mut path_evidence = false;
    if let Some(raw_input) = tool_call.get("rawInput") {
        if !raw_input.is_null() {
            path_evidence |= validate_acp_file_input(raw_input, sandbox, expected_relative, None)?;
        }
    }
    if let Some(locations) = tool_call.get("locations") {
        if !locations.is_null() {
            let locations = locations
                .as_array()
                .ok_or(PermissionScopeError::UnprovablePath)?;
            for location in locations {
                let path = location
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or(PermissionScopeError::UnprovablePath)?;
                validate_acp_file_path(sandbox, path, expected_relative)?;
                path_evidence = true;
            }
        }
    }
    if let Some(content) = tool_call.get("content") {
        if !content.is_null() {
            let content = content
                .as_array()
                .ok_or(PermissionScopeError::UnprovablePath)?;
            for entry in content {
                if entry.get("type").and_then(Value::as_str) == Some("diff") {
                    let path = entry
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(PermissionScopeError::UnprovablePath)?;
                    validate_acp_file_path(sandbox, path, expected_relative)?;
                    path_evidence = true;
                }
            }
        }
    }
    if path_evidence {
        Ok(())
    } else {
        Err(PermissionScopeError::UnprovablePath)
    }
}

fn validate_acp_file_input(
    value: &Value,
    sandbox: &Path,
    expected_relative: Option<&str>,
    key: Option<&str>,
) -> Result<bool, PermissionScopeError> {
    match value {
        Value::Object(fields) => {
            let mut path_evidence = false;
            for (field, value) in fields {
                path_evidence |=
                    validate_acp_file_input(value, sandbox, expected_relative, Some(field))?;
            }
            Ok(path_evidence)
        }
        Value::Array(values) => {
            let mut path_evidence = false;
            for value in values {
                path_evidence |= validate_acp_file_input(value, sandbox, expected_relative, key)?;
            }
            Ok(path_evidence)
        }
        Value::String(value) => match key {
            Some("command" | "cmd") => Err(PermissionScopeError::UnsafeCommand),
            Some("cwd" | "directory" | "workdir" | "root") => {
                validate_exact_permission_cwd(sandbox, value)?;
                Ok(false)
            }
            Some("path" | "file" | "filePath" | "file_path" | "target" | "filename") => {
                validate_acp_file_path(sandbox, value, expected_relative)?;
                Ok(true)
            }
            _ => Ok(false),
        },
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(false),
    }
}

fn validate_acp_file_path(
    sandbox: &Path,
    path: &str,
    expected_relative: Option<&str>,
) -> Result<(), PermissionScopeError> {
    if let Some(expected_relative) = expected_relative {
        validate_exact_permission_path(sandbox, path, Path::new(expected_relative))
    } else {
        validate_permission_path(sandbox, path)
    }
}

fn contains_meaningful_tool_update(update: &Value) -> bool {
    ["content", "locations", "rawInput", "rawOutput", "title"]
        .into_iter()
        .filter_map(|field| update.get(field))
        .any(value_has_meaningful_payload)
}

fn value_has_meaningful_payload(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(values) => values.iter().any(value_has_meaningful_payload),
        Value::Object(fields) => fields.values().any(value_has_meaningful_payload),
    }
}

fn contains_nonempty_file_diff(update: &Value) -> bool {
    update
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content.iter().any(|entry| {
                if entry.get("type").and_then(Value::as_str) != Some("diff") {
                    return false;
                }
                let path_is_nonempty = entry
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| !path.trim().is_empty());
                let Some(new_text) = entry.get("newText").and_then(Value::as_str) else {
                    return false;
                };
                let content_changed = match entry.get("oldText") {
                    Some(Value::String(old_text)) => old_text != new_text,
                    Some(Value::Null) | None => !new_text.is_empty(),
                    Some(_) => false,
                };
                path_is_nonempty && content_changed
            })
        })
}

#[derive(Debug, Error)]
pub enum AcpError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Stdio(#[from] StdioError),
    #[error("invalid ACP recorder state: {0}")]
    InvalidState(&'static str),
    #[error("invalid ACP message: {0}")]
    MessageShape(&'static str),
    #[error("ACP protocol version mismatch: expected {expected}, received {actual}")]
    ProtocolVersion { expected: u64, actual: u64 },
    #[error("unexpected JSON-RPC response id; expected {expected}")]
    ResponseId { expected: u64 },
    #[error("ACP message session id does not match the active session")]
    SessionIdMismatch,
    #[error("session/list repeated pagination cursor {0:?}")]
    RepeatedCursor(String),
    #[error(
        "session/list returned an entry whose cwd or additional directory is outside the fixture sandbox"
    )]
    UnsafeSessionList,
    #[error("ACP recorder request counter overflowed")]
    CounterOverflow,
    #[error(
        "ACP permission request could not be proven safe inside the canonical fixture sandbox"
    )]
    UnsafePermissionScope,
    #[error("recording directory must be the dedicated tests/fixtures/sandbox directory")]
    InvalidSandbox,
    #[error("the dedicated fixture sandbox path is not valid UTF-8")]
    SandboxNotUtf8,
    #[error("ACP protocol attempt failed ({source}); child cleanup also failed: {cleanup}")]
    CleanupAfterError {
        #[source]
        source: Box<AcpError>,
        cleanup: StdioError,
    },
}

pub fn record_scenario<W: Write>(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    sandbox: &Path,
    scenario: AcpScenario,
    fixture: &mut FixtureSink<W>,
) -> Result<ScenarioOutcome, AcpError> {
    record_scenario_with_timeout(
        executable,
        arguments,
        sandbox,
        scenario,
        fixture,
        DEFAULT_MESSAGE_TIMEOUT,
    )
}

pub fn record_scenario_with_timeout<W: Write>(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    sandbox: &Path,
    scenario: AcpScenario,
    fixture: &mut FixtureSink<W>,
    message_timeout: Duration,
) -> Result<ScenarioOutcome, AcpError> {
    let sandbox = validate_fixture_sandbox(sandbox)?;
    let machine = AcpStateMachine::new(sandbox.clone(), scenario);
    run_state_machine(
        executable,
        arguments,
        &sandbox,
        machine,
        fixture,
        message_timeout,
    )
}

pub fn record_session_load_with_timeout<W: Write>(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    sandbox: &Path,
    session_id: String,
    fixture: &mut FixtureSink<W>,
    message_timeout: Duration,
) -> Result<ScenarioOutcome, AcpError> {
    let sandbox = validate_fixture_sandbox(sandbox)?;
    let machine = AcpStateMachine::for_session_load(sandbox.clone(), session_id);
    run_state_machine(
        executable,
        arguments,
        &sandbox,
        machine,
        fixture,
        message_timeout,
    )
}

fn run_state_machine<W: Write>(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    sandbox: &Path,
    mut machine: AcpStateMachine,
    fixture: &mut FixtureSink<W>,
    message_timeout: Duration,
) -> Result<ScenarioOutcome, AcpError> {
    let first = machine.start()?;
    let outbound = match first {
        MachineStep::Complete(outcome) => return Ok(outcome),
        MachineStep::Send(outbound) => outbound,
        MachineStep::Continue => {
            return Err(AcpError::InvalidState(
                "initial state did not produce a request",
            ))
        }
    };

    let mut tee = StdioTee::spawn(executable, arguments, sandbox)?;
    let recording = (|| {
        send_all(&mut tee, outbound, fixture)?;
        loop {
            let pending = tee.receive_pending(message_timeout)?;
            match accept_recorded_message(&mut machine, pending.raw(), fixture)? {
                MachineStep::Continue => {}
                MachineStep::Send(outbound) => send_all(&mut tee, outbound, fixture)?,
                MachineStep::Complete(outcome) => return Ok(outcome),
            }
        }
    })();
    finish_recording(recording, move || tee.stop())
}

fn finish_recording(
    recording: Result<ScenarioOutcome, AcpError>,
    cleanup: impl FnOnce() -> Result<(), StdioError>,
) -> Result<ScenarioOutcome, AcpError> {
    match (recording, cleanup()) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(source), Err(cleanup)) => Err(AcpError::CleanupAfterError {
            source: Box::new(source),
            cleanup,
        }),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

pub(crate) fn accept_recorded_message<W: Write>(
    machine: &mut AcpStateMachine,
    raw: &str,
    fixture: &mut FixtureSink<W>,
) -> Result<MachineStep, AcpError> {
    let message: Value = serde_json::from_str(raw)?;
    machine.validate_pending_message(&message)?;
    fixture
        .record(Direction::S2c, Transport::Stdio, raw)
        .map_err(StdioError::from)?;
    machine.accept_message(&message)
}

fn validate_session_list_result(result: &Value, sandbox: &Path) -> Result<(), AcpError> {
    let sessions =
        result
            .get("sessions")
            .and_then(Value::as_array)
            .ok_or(AcpError::MessageShape(
                "session/list result.sessions is missing",
            ))?;
    let canonical_sandbox = sandbox
        .canonicalize()
        .map_err(|_| AcpError::UnsafeSessionList)?;
    for session in sessions {
        let cwd = session
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or(AcpError::UnsafeSessionList)?;
        let canonical_cwd = Path::new(cwd)
            .canonicalize()
            .map_err(|_| AcpError::UnsafeSessionList)?;
        if canonical_cwd != canonical_sandbox {
            return Err(AcpError::UnsafeSessionList);
        }
        let additional_directories = match session.get("additionalDirectories") {
            None => continue,
            Some(Value::Array(directories)) => directories,
            Some(_) => return Err(AcpError::UnsafeSessionList),
        };
        for directory in additional_directories {
            let directory = directory.as_str().ok_or(AcpError::UnsafeSessionList)?;
            let canonical_directory = Path::new(directory)
                .canonicalize()
                .map_err(|_| AcpError::UnsafeSessionList)?;
            if !canonical_directory.starts_with(&canonical_sandbox) {
                return Err(AcpError::UnsafeSessionList);
            }
        }
    }
    Ok(())
}

pub fn validate_fixture_sandbox(sandbox: &Path) -> Result<PathBuf, AcpError> {
    let expected = expected_fixture_sandbox_path()?;
    validate_fixture_sandbox_against(sandbox, &expected)
}

pub(super) fn validate_fixture_sandbox_against(
    sandbox: &Path,
    expected: &Path,
) -> Result<PathBuf, AcpError> {
    platform::validate_fixture_sandbox_root(sandbox, expected)
        .map_err(|_| AcpError::InvalidSandbox)?
        .ok_or(AcpError::InvalidSandbox)
}

fn send_all<W: Write>(
    tee: &mut StdioTee,
    outbound: Vec<String>,
    fixture: &mut FixtureSink<W>,
) -> Result<(), AcpError> {
    for message in outbound {
        tee.send(&message, fixture)?;
    }
    Ok(())
}

fn expected_fixture_sandbox_path() -> Result<PathBuf, AcpError> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or(AcpError::InvalidSandbox)?;
    Ok(workspace.join("tests").join("fixtures").join("sandbox"))
}

fn sandbox_utf8(sandbox: &Path) -> Result<&str, AcpError> {
    sandbox.to_str().ok_or(AcpError::SandboxNotUtf8)
}

fn capability_object_present(session_capabilities: Option<&Value>, name: &str) -> bool {
    session_capabilities
        .and_then(|capabilities| capabilities.get(name))
        .is_some_and(Value::is_object)
}

fn ensure_response_id(message: &Value, expected: u64) -> Result<(), AcpError> {
    if message.get("id").and_then(Value::as_u64) == Some(expected) {
        Ok(())
    } else {
        Err(AcpError::ResponseId { expected })
    }
}

fn response_result(message: &Value) -> Result<&Value, AcpError> {
    message
        .get("result")
        .ok_or(AcpError::MessageShape("response result is missing"))
}

fn serialize_message(message: Value) -> Result<String, AcpError> {
    Ok(serde_json::to_string(&message)?)
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::io;

    use super::*;

    #[test]
    fn protocol_and_cleanup_failures_are_both_reported() -> Result<(), Box<dyn Error>> {
        let protocol = AcpError::MessageShape("forced protocol failure");
        let cleanup = StdioError::Process(platform::ProcessError::Terminate(io::Error::other(
            "forced cleanup failure",
        )));
        let mut cleanup_called = false;

        let error = match finish_recording(Err(protocol), || {
            cleanup_called = true;
            Err(cleanup)
        }) {
            Err(error) => error,
            Ok(_) => return Err(io::Error::other("both failures unexpectedly succeeded").into()),
        };

        assert!(cleanup_called, "the normal API must explicitly run cleanup");
        assert!(matches!(
            &error,
            AcpError::CleanupAfterError {
                source,
                cleanup: StdioError::Process(platform::ProcessError::Terminate(_)),
            } if matches!(
                source.as_ref(),
                AcpError::MessageShape("forced protocol failure")
            )
        ));
        let message = error.to_string();
        assert!(message.contains("forced protocol failure"));
        assert!(message.contains("forced cleanup failure"));
        Ok(())
    }
}
