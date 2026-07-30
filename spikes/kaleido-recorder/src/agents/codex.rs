//! Codex app-server JSON-RPC recording state machine.
//!
//! This module deliberately works with `serde_json::Value`: the recorder must
//! preserve and record the real upstream wire payload, while generated Rust
//! protocol types belong to the later adapter task. The method names and the
//! fields emitted here are taken from the checked-in `schemas/codex` snapshot.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use thiserror::Error;

use super::{
    validate_exact_permission_cwd, validate_exact_permission_path, validate_permission_argv,
    validate_permission_command, validate_permission_command_as, validate_permission_path,
    CompletedRecording, PermissionCommand, PermissionScopeError,
};
use crate::fixture::{Direction, FixtureSink, Transport};
use crate::platform::{self, ResolvedExecutable};
use crate::stdio_tee::{StdioError, StdioTee};

const INITIALIZE_ID: i64 = 1;
const PRIMARY_REQUEST_ID: i64 = 2;
const TURN_REQUEST_ID: i64 = 3;
const INTERRUPT_REQUEST_ID: i64 = 4;

const METHOD_INITIALIZE: &str = "initialize";
const METHOD_INITIALIZED: &str = "initialized";
const METHOD_THREAD_START: &str = "thread/start";
const METHOD_THREAD_LIST: &str = "thread/list";
const METHOD_THREAD_RESUME: &str = "thread/resume";
const METHOD_TURN_START: &str = "turn/start";
const METHOD_TURN_INTERRUPT: &str = "turn/interrupt";
const METHOD_TURN_COMPLETED: &str = "turn/completed";
const METHOD_ITEM_STARTED: &str = "item/started";
const METHOD_ITEM_COMPLETED: &str = "item/completed";
const METHOD_TURN_DIFF_UPDATED: &str = "turn/diff/updated";
const METHOD_COMMAND_OUTPUT_DELTA: &str = "item/commandExecution/outputDelta";
const METHOD_FILE_CHANGE_PATCH_UPDATED: &str = "item/fileChange/patchUpdated";
const METHOD_MCP_TOOL_PROGRESS: &str = "item/mcpToolCall/progress";

const METHOD_COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
const METHOD_FILE_CHANGE_APPROVAL: &str = "item/fileChange/requestApproval";
const METHOD_PERMISSION_PROFILE_APPROVAL: &str = "item/permissions/requestApproval";
const METHOD_LEGACY_EXEC_APPROVAL: &str = "execCommandApproval";
const METHOD_LEGACY_PATCH_APPROVAL: &str = "applyPatchApproval";
const METHOD_MCP_ELICITATION: &str = "mcpServer/elicitation/request";
const METHOD_TOOL_USER_INPUT: &str = "item/tool/requestUserInput";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexScenario {
    SimpleTurn,
    ToolCall,
    PermissionApprove,
    PermissionDeny,
    FileChange,
    Cancel,
    Error,
    SessionLoad { thread_id: Option<String> },
    Elicitation,
}

impl CodexScenario {
    pub const fn file_name(&self) -> &'static str {
        match self {
            Self::SimpleTurn => "01-simple-turn.jsonl",
            Self::ToolCall => "02-tool-call.jsonl",
            Self::PermissionApprove => "03-permission-approve.jsonl",
            Self::PermissionDeny => "04-permission-deny.jsonl",
            Self::FileChange => "05-file-change.jsonl",
            Self::Cancel => "06-cancel.jsonl",
            Self::Error => "07-error.jsonl",
            Self::SessionLoad { .. } => "08-session-load.jsonl",
            Self::Elicitation => "09-elicitation.jsonl",
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::SimpleTurn => "simple-turn",
            Self::ToolCall => "tool-call",
            Self::PermissionApprove => "permission-approve",
            Self::PermissionDeny => "permission-deny",
            Self::FileChange => "file-change",
            Self::Cancel => "cancel",
            Self::Error => "error",
            Self::SessionLoad { .. } => "session-load",
            Self::Elicitation => "elicitation",
        }
    }

    const fn permission_decision(&self) -> PermissionDecision {
        if matches!(self, Self::PermissionDeny) {
            PermissionDecision::Deny
        } else {
            PermissionDecision::Approve
        }
    }

    const fn sandbox_mode(&self) -> &'static str {
        if matches!(self, Self::PermissionApprove | Self::PermissionDeny) {
            "read-only"
        } else {
            "workspace-write"
        }
    }

    const fn prompt(&self) -> Option<&'static str> {
        match self {
            Self::SimpleTurn => Some(
                "Reply with exactly this plain text and do not use any tool: \
                 KALEIDO SIMPLE TURN",
            ),
            Self::ToolCall => Some(
                "Read notes.txt in the current sandbox with a file-reading tool, \
                 then summarize it in one sentence. Do not read any path outside \
                 the current sandbox.",
            ),
            Self::PermissionApprove | Self::PermissionDeny => Some(
                "Create permission-probe.txt in the current sandbox containing \
                 exactly KALEIDO PERMISSION PROBE. Do not access any path outside \
                 the current sandbox.",
            ),
            Self::FileChange => Some(
                "Edit editable.txt in the current sandbox by replacing the marker \
                 ORIGINAL with UPDATED. Do not touch any other file.",
            ),
            Self::Cancel => Some(
                "Use the shell tool to run `cargo run -- wait` in the current sandbox. \
                 Do not run any other command and do not access any path outside the \
                 current sandbox.",
            ),
            Self::Error => Some(
                "Use the shell tool to run `cargo run -- fail` in the current sandbox. \
                 Do not run any other command and do not access any path outside the \
                 current sandbox, then report the failure.",
            ),
            Self::SessionLoad { .. } => None,
            Self::Elicitation => Some(
                "If a configured MCP server supports structured elicitation, ask it \
                 to request a one-field form from the user. If no configured MCP \
                 server supports this, state that fact; do not fabricate a form.",
            ),
        }
    }
}

impl fmt::Display for CodexScenario {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl FromStr for CodexScenario {
    type Err = ScenarioParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "simple-turn" | "01-simple-turn" => Ok(Self::SimpleTurn),
            "tool-call" | "02-tool-call" => Ok(Self::ToolCall),
            "permission-approve" | "03-permission-approve" => Ok(Self::PermissionApprove),
            "permission-deny" | "04-permission-deny" => Ok(Self::PermissionDeny),
            "file-change" | "05-file-change" => Ok(Self::FileChange),
            "cancel" | "06-cancel" => Ok(Self::Cancel),
            "error" | "07-error" => Ok(Self::Error),
            "session-load" | "08-session-load" => Ok(Self::SessionLoad { thread_id: None }),
            "elicitation" | "09-elicitation" => Ok(Self::Elicitation),
            _ => Err(ScenarioParseError),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PermissionDecision {
    Approve,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ItemLifecycleObservation {
    thread_id: String,
    turn_id: String,
    item_id: String,
    item_type: String,
    permission_scope_safe: bool,
    exact_notes_read: bool,
    exact_editable_change: bool,
    exact_failure_command: bool,
    meaningful_update_seen: bool,
    terminal_status: Option<String>,
    exit_code: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PermissionFlowObservation {
    thread_id: String,
    turn_id: String,
    target_item_id: String,
    decision: PermissionDecision,
    target_started_before_reply: bool,
    terminal_status_after_reply: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexRecorderConfig {
    pub request_timeout: Duration,
    pub turn_timeout: Duration,
}

impl Default for CodexRecorderConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            turn_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexRecording {
    pub scenario: CodexScenario,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub completion_status: Option<String>,
    pub notification_methods: Vec<String>,
    pub server_request_methods: Vec<String>,
    pub permission_requests: usize,
    pub elicitation_requests: usize,
    pub user_input_requests: usize,
    pub item_types_started: Vec<String>,
    pub item_types_completed: Vec<String>,
    pub command_exit_codes: Vec<i64>,
    pub diff_updates: usize,
    pub nonempty_diff_updates: usize,
    pub error_info_count: usize,
    editable_file_changed: bool,
    item_lifecycles: Vec<ItemLifecycleObservation>,
    permission_flows: Vec<PermissionFlowObservation>,
    nonempty_diff_turns: Vec<(String, String)>,
}

impl CodexRecording {
    pub const fn observed_permission_request(&self) -> bool {
        self.permission_requests > 0
    }

    pub const fn observed_elicitation_request(&self) -> bool {
        self.elicitation_requests > 0
    }

    pub fn observed_tool_call(&self) -> bool {
        self.active_tool_lifecycles_are_safe()
            && self.item_lifecycles.iter().any(|lifecycle| {
                self.lifecycle_is_in_active_turn(lifecycle)
                    && lifecycle.item_type == "commandExecution"
                    && lifecycle.permission_scope_safe
                    && lifecycle.exact_notes_read
                    && lifecycle.meaningful_update_seen
                    && lifecycle.terminal_status.as_deref() == Some("completed")
            })
    }

    pub fn observed_file_change(&self) -> bool {
        self.active_tool_lifecycles_are_safe()
            && self.editable_file_changed
            && self.item_lifecycles.iter().any(|lifecycle| {
                self.lifecycle_is_in_active_turn(lifecycle)
                    && lifecycle.item_type == "fileChange"
                    && lifecycle.permission_scope_safe
                    && lifecycle.exact_editable_change
                    && lifecycle.meaningful_update_seen
                    && lifecycle.terminal_status.as_deref() == Some("completed")
            })
            && self.nonempty_diff_turns.iter().any(|(thread_id, turn_id)| {
                thread_id == &self.thread_id && self.turn_id.as_deref() == Some(turn_id.as_str())
            })
    }

    pub fn observed_approved_permission_flow(&self) -> bool {
        self.active_tool_lifecycles_are_safe()
            && self.completion_status.as_deref() == Some("completed")
            && self.permission_flows.iter().any(|flow| {
                flow.thread_id == self.thread_id
                    && self.turn_id.as_deref() == Some(flow.turn_id.as_str())
                    && flow.decision == PermissionDecision::Approve
                    && flow.target_started_before_reply
                    && flow.terminal_status_after_reply.as_deref() == Some("completed")
            })
    }

    pub fn observed_denied_permission_flow(&self) -> bool {
        self.active_tool_lifecycles_are_safe()
            && matches!(
                self.completion_status.as_deref(),
                Some("completed" | "failed")
            )
            && self.permission_flows.iter().any(|flow| {
                flow.thread_id == self.thread_id
                    && self.turn_id.as_deref() == Some(flow.turn_id.as_str())
                    && flow.decision == PermissionDecision::Deny
                    && flow.target_started_before_reply
                    && matches!(
                        flow.terminal_status_after_reply.as_deref(),
                        Some("declined" | "failed")
                    )
            })
    }

    pub fn observed_failed_command(&self) -> bool {
        self.active_tool_lifecycles_are_safe()
            && self.item_lifecycles.iter().any(|lifecycle| {
                self.lifecycle_is_in_active_turn(lifecycle)
                    && lifecycle.item_type == "commandExecution"
                    && lifecycle.permission_scope_safe
                    && lifecycle.exact_failure_command
                    && lifecycle.meaningful_update_seen
                    && lifecycle.terminal_status.as_deref() == Some("failed")
                    && lifecycle.exit_code.is_some_and(|exit_code| exit_code != 0)
            })
    }

    fn lifecycle_is_in_active_turn(&self, lifecycle: &ItemLifecycleObservation) -> bool {
        lifecycle.thread_id == self.thread_id
            && self.turn_id.as_deref() == Some(lifecycle.turn_id.as_str())
    }

    fn active_tool_lifecycles_are_safe(&self) -> bool {
        self.turn_id.as_deref().is_none_or(|turn_id| {
            active_turn_tool_lifecycles_are_safe(&self.item_lifecycles, &self.thread_id, turn_id)
        })
    }
}

fn active_turn_tool_lifecycles_are_safe(
    lifecycles: &[ItemLifecycleObservation],
    thread_id: &str,
    turn_id: &str,
) -> bool {
    lifecycles
        .iter()
        .filter(|lifecycle| lifecycle.thread_id == thread_id && lifecycle.turn_id == turn_id)
        .all(|lifecycle| lifecycle.permission_scope_safe)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error(
    "unknown Codex scenario; expected simple-turn, tool-call, permission-approve, \
     permission-deny, file-change, cancel, error, session-load, or elicitation"
)]
pub struct ScenarioParseError;

#[derive(Debug, Error)]
pub enum CodexRecorderError {
    #[error(transparent)]
    Stdio(#[from] StdioError),
    #[error("failed to encode or decode a Codex protocol message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("recording directory must resolve to tests/fixtures/sandbox")]
    UnsafeSandbox,
    #[error("recording directory is not valid UTF-8 and cannot be sent as a JSON path")]
    NonUtf8Sandbox,
    #[error("failed to inspect the recording directory: {0}")]
    SandboxIo(#[source] std::io::Error),
    #[error("Codex sent a malformed protocol message: {0}")]
    Malformed(&'static str),
    #[error("Codex returned an error for {method} (code {code:?})")]
    Rpc {
        method: &'static str,
        code: Option<i64>,
    },
    #[error("Codex returned a response for an unexpected request id")]
    UnexpectedResponse,
    #[error("timed out while waiting for {0}")]
    Timeout(&'static str),
    #[error("thread/list returned no matching sandbox thread")]
    NoSession,
    #[error("session-load requires an explicit thread id from the seed command")]
    ThreadIdRequired,
    #[error("thread/list returned an entry whose cwd is not the fixture sandbox")]
    UnsafeThreadList,
    #[error("thread/list repeated pagination cursor")]
    RepeatedThreadCursor,
    #[error("Codex returned a different thread than the one requested")]
    ThreadMismatch,
    #[error("cancel scenario completed before a commandExecution item started")]
    CancelToolDidNotStart,
    #[error(
        "Codex permission request could not be proven safe inside the canonical fixture sandbox"
    )]
    UnsafePermissionScope,
    #[error("Codex active turn contained a tool lifecycle whose scope could not be proven safe")]
    UnsafeToolLifecycleScope,
    #[error("Codex protocol attempt failed ({source}); child cleanup also failed: {cleanup}")]
    CleanupAfterError {
        #[source]
        source: Box<CodexRecorderError>,
        cleanup: StdioError,
    },
}

pub fn record<W: Write>(
    executable: &ResolvedExecutable,
    sandbox: &Path,
    scenario: CodexScenario,
    fixture: &mut FixtureSink<W>,
) -> Result<CompletedRecording<CodexRecording>, CodexRecorderError> {
    record_with_config(
        executable,
        sandbox,
        scenario,
        fixture,
        &CodexRecorderConfig::default(),
    )
}

pub fn record_with_config<W: Write>(
    executable: &ResolvedExecutable,
    sandbox: &Path,
    scenario: CodexScenario,
    fixture: &mut FixtureSink<W>,
    config: &CodexRecorderConfig,
) -> Result<CompletedRecording<CodexRecording>, CodexRecorderError> {
    if let CodexScenario::SessionLoad { thread_id } = &scenario {
        if thread_id.as_deref().is_none_or(str::is_empty) {
            return Err(CodexRecorderError::ThreadIdRequired);
        }
    }
    let sandbox = validated_sandbox(sandbox)?;
    let arguments = [OsString::from("app-server")];
    let mut tee = StdioTee::spawn(executable, &arguments, &sandbox)?;
    let recording = {
        let mut io = LiveProtocolIo {
            tee: &mut tee,
            fixture,
            pending_server_request: None,
        };
        run_protocol(&mut io, &sandbox, scenario, config)
    };
    finish_recording(recording, move || tee.stop())
}

fn finish_recording(
    recording: Result<CodexRecording, CodexRecorderError>,
    cleanup: impl FnOnce() -> Result<(), StdioError>,
) -> Result<CompletedRecording<CodexRecording>, CodexRecorderError> {
    let cleanup = cleanup();
    match (recording, cleanup) {
        (Ok(value), cleanup) => Ok(CompletedRecording::with_cleanup_result(value, cleanup)),
        (Err(source), Err(cleanup)) => Err(CodexRecorderError::CleanupAfterError {
            source: Box::new(source),
            cleanup,
        }),
        (Err(error), Ok(())) => Err(error),
    }
}

fn validated_sandbox(path: &Path) -> Result<PathBuf, CodexRecorderError> {
    let expected = expected_fixture_sandbox_path()?;
    validated_sandbox_against(path, &expected)
}

fn validated_sandbox_against(path: &Path, expected: &Path) -> Result<PathBuf, CodexRecorderError> {
    platform::validate_fixture_sandbox_root(path, expected)
        .map_err(CodexRecorderError::SandboxIo)?
        .ok_or(CodexRecorderError::UnsafeSandbox)
}

fn expected_fixture_sandbox_path() -> Result<PathBuf, CodexRecorderError> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or(CodexRecorderError::UnsafeSandbox)?
        .join("tests")
        .join("fixtures")
        .join("sandbox"))
}

#[cfg(test)]
fn expected_fixture_sandbox() -> Result<PathBuf, CodexRecorderError> {
    expected_fixture_sandbox_path()?
        .canonicalize()
        .map_err(CodexRecorderError::SandboxIo)
}

trait ProtocolIo {
    fn send(&mut self, message: &Value) -> Result<(), CodexRecorderError>;
    fn receive(&mut self, timeout: Duration) -> Result<Value, CodexRecorderError>;
    fn commit_pending_server_request(&mut self) -> Result<(), CodexRecorderError> {
        Ok(())
    }
    fn receive_thread_list(
        &mut self,
        timeout: Duration,
        response_id: i64,
        sandbox: &Path,
    ) -> Result<Value, CodexRecorderError> {
        let message = self.receive(timeout)?;
        validate_thread_list_message(&message, response_id, sandbox)?;
        Ok(message)
    }
}

struct LiveProtocolIo<'a, W> {
    tee: &'a mut StdioTee,
    fixture: &'a mut FixtureSink<W>,
    pending_server_request: Option<String>,
}

impl<W: Write> ProtocolIo for LiveProtocolIo<'_, W> {
    fn send(&mut self, message: &Value) -> Result<(), CodexRecorderError> {
        let raw = serde_json::to_string(message)?;
        self.tee.send(&raw, self.fixture)?;
        Ok(())
    }

    fn receive(&mut self, timeout: Duration) -> Result<Value, CodexRecorderError> {
        let pending = self.tee.receive_pending(timeout)?;
        let message: Value = serde_json::from_str(pending.raw())?;
        if is_permission_server_request(&message) {
            if self.pending_server_request.is_some() {
                return Err(CodexRecorderError::Malformed(
                    "a permission request was already pending validation",
                ));
            }
            self.pending_server_request = Some(pending.raw().to_owned());
        } else {
            pending.commit(self.fixture)?;
        }
        Ok(message)
    }

    fn commit_pending_server_request(&mut self) -> Result<(), CodexRecorderError> {
        let raw = self
            .pending_server_request
            .take()
            .ok_or(CodexRecorderError::Malformed(
                "validated permission request was not pending",
            ))?;
        self.fixture
            .record(Direction::S2c, Transport::Stdio, &raw)
            .map_err(StdioError::from)?;
        Ok(())
    }

    fn receive_thread_list(
        &mut self,
        timeout: Duration,
        response_id: i64,
        sandbox: &Path,
    ) -> Result<Value, CodexRecorderError> {
        let pending = self.tee.receive_pending(timeout)?;
        let message: Value = serde_json::from_str(pending.raw())?;
        if is_permission_server_request(&message) {
            if self.pending_server_request.is_some() {
                return Err(CodexRecorderError::Malformed(
                    "a permission request was already pending validation",
                ));
            }
            self.pending_server_request = Some(pending.raw().to_owned());
            Ok(message)
        } else {
            record_validated_thread_list_raw(pending.raw(), response_id, sandbox, self.fixture)
        }
    }
}

fn record_validated_thread_list_raw<W: Write>(
    raw: &str,
    response_id: i64,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<Value, CodexRecorderError> {
    let message: Value = serde_json::from_str(raw)?;
    validate_thread_list_message(&message, response_id, sandbox)?;
    fixture
        .record(Direction::S2c, Transport::Stdio, raw)
        .map_err(StdioError::from)?;
    Ok(message)
}

#[derive(Debug)]
struct Observations {
    sandbox: PathBuf,
    active_thread_id: Option<String>,
    active_turn_id: Option<String>,
    notification_methods: Vec<String>,
    server_request_methods: Vec<String>,
    permission_requests: usize,
    elicitation_requests: usize,
    user_input_requests: usize,
    item_types_started: Vec<String>,
    item_types_completed: Vec<String>,
    command_exit_codes: Vec<i64>,
    diff_updates: usize,
    nonempty_diff_updates: usize,
    error_info_count: usize,
    item_lifecycles: Vec<ItemLifecycleObservation>,
    permission_flows: Vec<PermissionFlowObservation>,
    nonempty_diff_turns: Vec<(String, String)>,
    turn_completion: Option<TurnCompletion>,
}

impl Observations {
    fn new(sandbox: &Path) -> Self {
        Self {
            sandbox: sandbox.to_path_buf(),
            active_thread_id: None,
            active_turn_id: None,
            notification_methods: Vec::new(),
            server_request_methods: Vec::new(),
            permission_requests: 0,
            elicitation_requests: 0,
            user_input_requests: 0,
            item_types_started: Vec::new(),
            item_types_completed: Vec::new(),
            command_exit_codes: Vec::new(),
            diff_updates: 0,
            nonempty_diff_updates: 0,
            error_info_count: 0,
            item_lifecycles: Vec::new(),
            permission_flows: Vec::new(),
            nonempty_diff_turns: Vec::new(),
            turn_completion: None,
        }
    }
}

#[derive(Debug)]
struct TurnCompletion {
    thread_id: String,
    turn_id: String,
    status: String,
}

fn run_protocol(
    io: &mut impl ProtocolIo,
    sandbox: &Path,
    scenario: CodexScenario,
    config: &CodexRecorderConfig,
) -> Result<CodexRecording, CodexRecorderError> {
    let mut observations = Observations::new(sandbox);
    let editable_before = matches!(&scenario, CodexScenario::FileChange)
        .then(|| exact_sandbox_file_bytes(sandbox, Path::new("editable.txt")))
        .flatten();
    let sandbox_text = sandbox
        .to_str()
        .map(str::to_owned)
        .ok_or(CodexRecorderError::NonUtf8Sandbox)?;
    let requested_thread_id = match &scenario {
        CodexScenario::SessionLoad { thread_id } => Some(
            thread_id
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or(CodexRecorderError::ThreadIdRequired)?
                .to_owned(),
        ),
        _ => None,
    };

    let initialize = json!({
        "id": INITIALIZE_ID,
        "method": METHOD_INITIALIZE,
        "params": {
            "clientInfo": {
                "name": "kaleido-recorder",
                "title": "OneKaleidoscope fixture recorder",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "mcpServerOpenaiFormElicitation": true
            }
        }
    });
    let _ = request(
        io,
        initialize,
        INITIALIZE_ID,
        METHOD_INITIALIZE,
        scenario.permission_decision(),
        &mut observations,
        config.request_timeout,
    )?;
    io.send(&json!({"method": METHOD_INITIALIZED}))?;

    if let Some(requested_thread_id) = requested_thread_id {
        return run_session_load(
            io,
            sandbox_text,
            scenario,
            &requested_thread_id,
            observations,
            config,
        );
    }

    let start_thread = json!({
        "id": PRIMARY_REQUEST_ID,
        "method": METHOD_THREAD_START,
        "params": {
            "cwd": sandbox_text,
            "approvalPolicy": "on-request",
            "sandbox": scenario.sandbox_mode(),
            "ephemeral": true
        }
    });
    let thread_result = request(
        io,
        start_thread,
        PRIMARY_REQUEST_ID,
        METHOD_THREAD_START,
        scenario.permission_decision(),
        &mut observations,
        config.request_timeout,
    )?;
    let thread_id = required_string(&thread_result, &["thread", "id"], "thread/start thread id")?;
    observations.active_thread_id = Some(thread_id.clone());

    let prompt = scenario
        .prompt()
        .ok_or(CodexRecorderError::Malformed("scenario prompt"))?;
    let start_turn = json!({
        "id": TURN_REQUEST_ID,
        "method": METHOD_TURN_START,
        "params": {
            "threadId": thread_id,
            "input": [{
                "type": "text",
                "text": prompt
            }]
        }
    });
    let turn_result = request(
        io,
        start_turn,
        TURN_REQUEST_ID,
        METHOD_TURN_START,
        scenario.permission_decision(),
        &mut observations,
        config.request_timeout,
    )?;
    let turn_id = required_string(&turn_result, &["turn", "id"], "turn/start turn id")?;
    observations.active_turn_id = Some(turn_id.clone());

    if matches!(scenario, CodexScenario::Cancel) {
        wait_for_command_execution_start(
            io,
            &thread_id,
            &turn_id,
            scenario.permission_decision(),
            &mut observations,
            config.turn_timeout,
        )?;
        let interrupt = json!({
            "id": INTERRUPT_REQUEST_ID,
            "method": METHOD_TURN_INTERRUPT,
            "params": {
                "threadId": thread_id,
                "turnId": turn_id
            }
        });
        let _ = request(
            io,
            interrupt,
            INTERRUPT_REQUEST_ID,
            METHOD_TURN_INTERRUPT,
            scenario.permission_decision(),
            &mut observations,
            config.request_timeout,
        )?;
    }

    let completion_status = wait_for_turn_completion(
        io,
        &thread_id,
        &turn_id,
        scenario.permission_decision(),
        &mut observations,
        config.turn_timeout,
    )?;
    if !active_turn_tool_lifecycles_are_safe(&observations.item_lifecycles, &thread_id, &turn_id) {
        return Err(CodexRecorderError::UnsafeToolLifecycleScope);
    }
    let editable_file_changed = editable_before.as_deref().is_some_and(|before| {
        exact_sandbox_file_changed(sandbox, Path::new("editable.txt"), before)
    });

    Ok(CodexRecording {
        scenario,
        thread_id,
        turn_id: Some(turn_id),
        completion_status: Some(completion_status),
        notification_methods: observations.notification_methods,
        server_request_methods: observations.server_request_methods,
        permission_requests: observations.permission_requests,
        elicitation_requests: observations.elicitation_requests,
        user_input_requests: observations.user_input_requests,
        item_types_started: observations.item_types_started,
        item_types_completed: observations.item_types_completed,
        command_exit_codes: observations.command_exit_codes,
        diff_updates: observations.diff_updates,
        nonempty_diff_updates: observations.nonempty_diff_updates,
        error_info_count: observations.error_info_count,
        editable_file_changed,
        item_lifecycles: observations.item_lifecycles,
        permission_flows: observations.permission_flows,
        nonempty_diff_turns: observations.nonempty_diff_turns,
    })
}

fn run_session_load(
    io: &mut impl ProtocolIo,
    sandbox_text: String,
    scenario: CodexScenario,
    requested_thread_id: &str,
    mut observations: Observations,
    config: &CodexRecorderConfig,
) -> Result<CodexRecording, CodexRecorderError> {
    let sandbox = Path::new(&sandbox_text);
    let mut cursor: Option<String> = None;
    let mut seen_cursors = Vec::new();
    let mut thread_id = None;
    loop {
        let mut params = json!({
            "cwd": sandbox_text,
            "limit": 100,
            "sortKey": "updated_at",
            "sortDirection": "desc"
        });
        if let Some(cursor) = cursor.as_deref() {
            params
                .as_object_mut()
                .ok_or(CodexRecorderError::Malformed("thread/list request params"))?
                .insert("cursor".to_owned(), Value::from(cursor));
        }
        let list_result = request_thread_list_page(
            io,
            json!({
                "id": PRIMARY_REQUEST_ID,
                "method": METHOD_THREAD_LIST,
                "params": params
            }),
            sandbox,
            &mut observations,
            config.request_timeout,
        )?;
        if thread_id.is_none() {
            thread_id = select_thread(&list_result, requested_thread_id).ok();
        }
        let next_cursor = match list_result.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(next)) => Some(next.clone()),
            Some(_) => return Err(CodexRecorderError::Malformed("thread/list nextCursor")),
        };
        let Some(next_cursor) = next_cursor else {
            break;
        };
        if seen_cursors.iter().any(|seen| seen == &next_cursor) {
            return Err(CodexRecorderError::RepeatedThreadCursor);
        }
        seen_cursors.push(next_cursor.clone());
        cursor = Some(next_cursor);
    }
    let thread_id = thread_id.ok_or(CodexRecorderError::NoSession)?;
    let resume = json!({
        "id": TURN_REQUEST_ID,
        "method": METHOD_THREAD_RESUME,
        "params": {
            "threadId": thread_id,
            "cwd": sandbox_text
        }
    });
    let resume_result = request(
        io,
        resume,
        TURN_REQUEST_ID,
        METHOD_THREAD_RESUME,
        PermissionDecision::Approve,
        &mut observations,
        config.request_timeout,
    )?;
    let resumed_id = required_string(&resume_result, &["thread", "id"], "thread/resume thread id")?;
    if resumed_id != thread_id {
        return Err(CodexRecorderError::ThreadMismatch);
    }

    Ok(CodexRecording {
        scenario,
        thread_id,
        turn_id: None,
        completion_status: None,
        notification_methods: observations.notification_methods,
        server_request_methods: observations.server_request_methods,
        permission_requests: observations.permission_requests,
        elicitation_requests: observations.elicitation_requests,
        user_input_requests: observations.user_input_requests,
        item_types_started: observations.item_types_started,
        item_types_completed: observations.item_types_completed,
        command_exit_codes: observations.command_exit_codes,
        diff_updates: observations.diff_updates,
        nonempty_diff_updates: observations.nonempty_diff_updates,
        error_info_count: observations.error_info_count,
        editable_file_changed: false,
        item_lifecycles: observations.item_lifecycles,
        permission_flows: observations.permission_flows,
        nonempty_diff_turns: observations.nonempty_diff_turns,
    })
}

fn validate_thread_list_message(
    message: &Value,
    response_id: i64,
    sandbox: &Path,
) -> Result<(), CodexRecorderError> {
    if message.get("method").is_some() {
        return Ok(());
    }
    if message.get("id") != Some(&Value::from(response_id)) {
        return Err(CodexRecorderError::UnexpectedResponse);
    }
    if message.get("error").is_some() {
        return Ok(());
    }
    let result = message
        .get("result")
        .ok_or(CodexRecorderError::Malformed("thread/list result"))?;
    validate_thread_list_result(result, sandbox)
}

fn validate_thread_list_result(result: &Value, sandbox: &Path) -> Result<(), CodexRecorderError> {
    let threads = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or(CodexRecorderError::Malformed("thread/list data"))?;
    let canonical_sandbox = sandbox
        .canonicalize()
        .map_err(|_| CodexRecorderError::UnsafeThreadList)?;
    for thread in threads {
        let cwd = thread
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or(CodexRecorderError::UnsafeThreadList)?;
        let canonical_cwd = Path::new(cwd)
            .canonicalize()
            .map_err(|_| CodexRecorderError::UnsafeThreadList)?;
        if canonical_cwd != canonical_sandbox {
            return Err(CodexRecorderError::UnsafeThreadList);
        }
    }
    Ok(())
}

fn select_thread(result: &Value, requested_thread_id: &str) -> Result<String, CodexRecorderError> {
    let threads = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or(CodexRecorderError::Malformed("thread/list data"))?;
    threads
        .iter()
        .filter_map(|thread| thread.get("id").and_then(Value::as_str))
        .find(|candidate| requested_thread_id == *candidate)
        .map(str::to_owned)
        .ok_or(CodexRecorderError::NoSession)
}

fn request_thread_list_page(
    io: &mut impl ProtocolIo,
    request_message: Value,
    sandbox: &Path,
    observations: &mut Observations,
    timeout: Duration,
) -> Result<Value, CodexRecorderError> {
    io.send(&request_message)?;
    let deadline = deadline_after(timeout);
    loop {
        let message = io.receive_thread_list(
            remaining(deadline, "a Codex thread/list response")?,
            PRIMARY_REQUEST_ID,
            sandbox,
        )?;
        if is_server_request(&message) {
            handle_server_request(io, &message, PermissionDecision::Approve, observations)?;
            continue;
        }
        if let Some(notification_method) = notification_method(&message) {
            observe_notification(&message, notification_method, observations);
            continue;
        }
        if let Some(result) = message.get("result") {
            return Ok(result.clone());
        }
        if let Some(error) = message.get("error") {
            return Err(CodexRecorderError::Rpc {
                method: METHOD_THREAD_LIST,
                code: error.get("code").and_then(Value::as_i64),
            });
        }
        return Err(CodexRecorderError::Malformed("response result or error"));
    }
}

fn request(
    io: &mut impl ProtocolIo,
    request: Value,
    id: i64,
    method: &'static str,
    decision: PermissionDecision,
    observations: &mut Observations,
    timeout: Duration,
) -> Result<Value, CodexRecorderError> {
    io.send(&request)?;
    let deadline = deadline_after(timeout);
    loop {
        let message = io.receive(remaining(deadline, "a Codex response")?)?;
        if is_server_request(&message) {
            handle_server_request(io, &message, decision, observations)?;
            continue;
        }
        if let Some(notification_method) = notification_method(&message) {
            observe_notification(&message, notification_method, observations);
            continue;
        }
        let response_id = message.get("id");
        if response_id != Some(&Value::from(id)) {
            return Err(CodexRecorderError::UnexpectedResponse);
        }
        if let Some(result) = message.get("result") {
            return Ok(result.clone());
        }
        if let Some(error) = message.get("error") {
            return Err(CodexRecorderError::Rpc {
                method,
                code: error.get("code").and_then(Value::as_i64),
            });
        }
        return Err(CodexRecorderError::Malformed("response result or error"));
    }
}

fn wait_for_turn_completion(
    io: &mut impl ProtocolIo,
    thread_id: &str,
    turn_id: &str,
    decision: PermissionDecision,
    observations: &mut Observations,
    timeout: Duration,
) -> Result<String, CodexRecorderError> {
    if let Some(status) = matching_completion(observations, thread_id, turn_id) {
        return Ok(status);
    }
    let deadline = deadline_after(timeout);
    loop {
        let message = io.receive(remaining(deadline, "turn/completed")?)?;
        if is_server_request(&message) {
            handle_server_request(io, &message, decision, observations)?;
            continue;
        }
        let Some(method) = notification_method(&message) else {
            return Err(CodexRecorderError::UnexpectedResponse);
        };
        observe_notification(&message, method, observations);
        if method != METHOD_TURN_COMPLETED {
            continue;
        }
        let params = message
            .get("params")
            .ok_or(CodexRecorderError::Malformed("turn/completed params"))?;
        let completed_thread = required_string(params, &["threadId"], "turn/completed thread id")?;
        let completed_turn = required_string(params, &["turn", "id"], "turn/completed turn id")?;
        if completed_thread != thread_id || completed_turn != turn_id {
            continue;
        }
        return required_string(params, &["turn", "status"], "turn/completed status");
    }
}

fn wait_for_command_execution_start(
    io: &mut impl ProtocolIo,
    thread_id: &str,
    turn_id: &str,
    decision: PermissionDecision,
    observations: &mut Observations,
    timeout: Duration,
) -> Result<(), CodexRecorderError> {
    let deadline = deadline_after(timeout);
    loop {
        let message = io.receive(remaining(deadline, "a commandExecution item")?)?;
        if is_server_request(&message) {
            handle_server_request(io, &message, decision, observations)?;
            continue;
        }
        let Some(method) = notification_method(&message) else {
            return Err(CodexRecorderError::UnexpectedResponse);
        };
        observe_notification(&message, method, observations);
        if method == METHOD_ITEM_STARTED
            && message.pointer("/params/threadId").and_then(Value::as_str) == Some(thread_id)
            && message.pointer("/params/turnId").and_then(Value::as_str) == Some(turn_id)
            && message.pointer("/params/item/type").and_then(Value::as_str)
                == Some("commandExecution")
        {
            return Ok(());
        }
        if matching_completion(observations, thread_id, turn_id).is_some() {
            return Err(CodexRecorderError::CancelToolDidNotStart);
        }
    }
}

fn deadline_after(timeout: Duration) -> Option<Instant> {
    Instant::now().checked_add(timeout)
}

fn remaining(
    deadline: Option<Instant>,
    operation: &'static str,
) -> Result<Duration, CodexRecorderError> {
    deadline
        .and_then(|value| value.checked_duration_since(Instant::now()))
        .filter(|value| !value.is_zero())
        .ok_or(CodexRecorderError::Timeout(operation))
}

fn is_server_request(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str).is_some() && message.get("id").is_some()
}

fn is_permission_server_request(message: &Value) -> bool {
    matches!(
        message.get("method").and_then(Value::as_str),
        Some(
            METHOD_COMMAND_APPROVAL
                | METHOD_FILE_CHANGE_APPROVAL
                | METHOD_PERMISSION_PROFILE_APPROVAL
                | METHOD_LEGACY_EXEC_APPROVAL
                | METHOD_LEGACY_PATCH_APPROVAL
        )
    ) && message.get("id").is_some()
}

fn notification_method(message: &Value) -> Option<&str> {
    if message.get("id").is_some() {
        None
    } else {
        message.get("method").and_then(Value::as_str)
    }
}

fn observe_notification(message: &Value, method: &str, observations: &mut Observations) {
    let current_agent_delta = method == "item/agentMessage/delta"
        && message
            .pointer("/params/delta")
            .and_then(Value::as_str)
            .is_some_and(|delta| !delta.is_empty())
        && notification_matches_active_turn(message, observations);
    if method != "item/agentMessage/delta" || current_agent_delta {
        observations.notification_methods.push(method.to_owned());
    }
    if method == "error" {
        observe_error_info(
            message.pointer("/params/error/codexErrorInfo"),
            observations,
        );
    } else if method == METHOD_TURN_COMPLETED {
        observe_error_info(
            message.pointer("/params/turn/error/codexErrorInfo"),
            observations,
        );
    }
    if method == METHOD_ITEM_STARTED {
        if let Some(item_type) = message.pointer("/params/item/type").and_then(Value::as_str) {
            observations.item_types_started.push(item_type.to_owned());
        }
        observe_item_started(message, observations);
    } else if method == METHOD_ITEM_COMPLETED {
        if let Some(item_type) = message.pointer("/params/item/type").and_then(Value::as_str) {
            observations.item_types_completed.push(item_type.to_owned());
        }
        if notification_matches_active_turn(message, observations) {
            if let Some(exit_code) = message
                .pointer("/params/item/exitCode")
                .and_then(Value::as_i64)
            {
                observations.command_exit_codes.push(exit_code);
            }
        }
        observe_item_completed(message, observations);
    } else if method == METHOD_TURN_DIFF_UPDATED {
        observations.diff_updates += 1;
        observe_turn_diff(message, observations);
    } else if let Some(item_type) = meaningful_update_item_type(message, method) {
        observe_item_update(message, item_type, observations);
    }
    if method == METHOD_TURN_COMPLETED {
        let thread_id = message.pointer("/params/threadId").and_then(Value::as_str);
        let turn_id = message.pointer("/params/turn/id").and_then(Value::as_str);
        let status = message
            .pointer("/params/turn/status")
            .and_then(Value::as_str);
        if let (Some(thread_id), Some(turn_id), Some(status)) = (thread_id, turn_id, status) {
            observations.turn_completion = Some(TurnCompletion {
                thread_id: thread_id.to_owned(),
                turn_id: turn_id.to_owned(),
                status: status.to_owned(),
            });
        }
    }
}

fn notification_matches_active_turn(message: &Value, observations: &Observations) -> bool {
    message.pointer("/params/threadId").and_then(Value::as_str)
        == observations.active_thread_id.as_deref()
        && message.pointer("/params/turnId").and_then(Value::as_str)
            == observations.active_turn_id.as_deref()
}

fn exact_sandbox_file_bytes(sandbox: &Path, expected_relative: &Path) -> Option<Vec<u8>> {
    let raw = expected_relative.to_str()?;
    validate_exact_permission_path(sandbox, raw, expected_relative).ok()?;
    fs::read(sandbox.join(expected_relative)).ok()
}

fn exact_sandbox_file_changed(sandbox: &Path, expected_relative: &Path, before: &[u8]) -> bool {
    exact_sandbox_file_bytes(sandbox, expected_relative)
        .is_some_and(|after| after.as_slice() != before)
}

fn observe_item_started(message: &Value, observations: &mut Observations) {
    let thread_id = message.pointer("/params/threadId").and_then(Value::as_str);
    let turn_id = message.pointer("/params/turnId").and_then(Value::as_str);
    let item_id = message.pointer("/params/item/id").and_then(Value::as_str);
    let item_type = message.pointer("/params/item/type").and_then(Value::as_str);
    let status = message
        .pointer("/params/item/status")
        .and_then(Value::as_str);
    let (Some(thread_id), Some(turn_id), Some(item_id), Some(item_type)) =
        (thread_id, turn_id, item_id, item_type)
    else {
        return;
    };
    if status != Some("inProgress")
        || !matches!(item_type, "commandExecution" | "mcpToolCall" | "fileChange")
    {
        return;
    }
    if observations.item_lifecycles.iter().any(|lifecycle| {
        lifecycle.thread_id == thread_id
            && lifecycle.turn_id == turn_id
            && lifecycle.item_id == item_id
    }) {
        return;
    }
    let item = message.pointer("/params/item").unwrap_or(&Value::Null);
    let permission_scope_safe =
        item_permission_scope_is_safe(item, item_type, &observations.sandbox);
    observations.item_lifecycles.push(ItemLifecycleObservation {
        thread_id: thread_id.to_owned(),
        turn_id: turn_id.to_owned(),
        item_id: item_id.to_owned(),
        item_type: item_type.to_owned(),
        permission_scope_safe,
        exact_notes_read: command_execution_reads_exact_path(
            item,
            &observations.sandbox,
            Path::new("notes.txt"),
        ),
        exact_editable_change: file_change_targets_exact_path(
            item,
            &observations.sandbox,
            Path::new("editable.txt"),
        ),
        exact_failure_command: command_execution_matches_recorder_command(
            item,
            &observations.sandbox,
            PermissionCommand::Fail,
        ),
        meaningful_update_seen: false,
        terminal_status: None,
        exit_code: None,
    });
}

fn item_permission_scope_is_safe(item: &Value, item_type: &str, sandbox: &Path) -> bool {
    match item_type {
        "commandExecution" => {
            let Some(cwd) = item.get("cwd").and_then(Value::as_str) else {
                return false;
            };
            let Some(command) = item.get("command").and_then(Value::as_str) else {
                return false;
            };
            let Some(actions) = item.get("commandActions") else {
                return false;
            };
            validate_exact_permission_cwd(sandbox, cwd).is_ok()
                && ((validate_permission_command(command).is_ok()
                    && validate_command_actions(actions, sandbox).is_ok())
                    || command_execution_matches_recorder_command(
                        item,
                        sandbox,
                        PermissionCommand::Wait,
                    )
                    || command_execution_matches_recorder_command(
                        item,
                        sandbox,
                        PermissionCommand::Fail,
                    )
                    || command_execution_read_scope_is_safe(item, sandbox, None))
        }
        "fileChange" => item
            .get("changes")
            .and_then(Value::as_array)
            .filter(|changes| !changes.is_empty())
            .is_some_and(|changes| {
                changes.iter().all(|change| {
                    change
                        .get("path")
                        .and_then(Value::as_str)
                        .is_some_and(|path| validate_permission_path(sandbox, path).is_ok())
                })
            }),
        _ => false,
    }
}

fn command_execution_reads_exact_path(
    item: &Value,
    sandbox: &Path,
    expected_relative: &Path,
) -> bool {
    command_execution_read_scope_is_safe(item, sandbox, Some(expected_relative))
}

fn command_execution_read_scope_is_safe(
    item: &Value,
    sandbox: &Path,
    expected_relative: Option<&Path>,
) -> bool {
    let Some(cwd) = item.get("cwd").and_then(Value::as_str) else {
        return false;
    };
    if validate_exact_permission_cwd(sandbox, cwd).is_err() {
        return false;
    }
    let Some(command) = item.get("command").and_then(Value::as_str) else {
        return false;
    };
    let Some(actions) = item.get("commandActions").and_then(Value::as_array) else {
        return false;
    };
    let [action] = actions.as_slice() else {
        return false;
    };
    if action.get("type").and_then(Value::as_str) != Some("read")
        || action.get("command").and_then(Value::as_str) != Some(command)
    {
        return false;
    }
    let Some(path) = action.get("path").and_then(Value::as_str) else {
        return false;
    };
    expected_relative.map_or_else(
        || validate_permission_path(sandbox, path).is_ok(),
        |expected| validate_exact_permission_path(sandbox, path, expected).is_ok(),
    )
}

fn command_execution_matches_recorder_command(
    item: &Value,
    sandbox: &Path,
    expected: PermissionCommand,
) -> bool {
    let Some(cwd) = item.get("cwd").and_then(Value::as_str) else {
        return false;
    };
    if validate_exact_permission_cwd(sandbox, cwd).is_err() {
        return false;
    }
    let Some(command) = item.get("command").and_then(Value::as_str) else {
        return false;
    };
    if validate_permission_command_as(command, expected).is_err() {
        return false;
    }
    item.get("commandActions")
        .and_then(Value::as_array)
        .filter(|actions| !actions.is_empty())
        .is_some_and(|actions| {
            actions.iter().all(|action| {
                action
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|action_command| {
                        validate_permission_command_as(action_command, expected).is_ok()
                    })
                    && action
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|action_type| {
                            matches!(action_type, "unknown" | "listFiles" | "search")
                        })
                    && action.get("path").is_none_or(|path| {
                        path.is_null()
                            || path
                                .as_str()
                                .is_some_and(|path| validate_permission_path(sandbox, path).is_ok())
                    })
            })
        })
}

fn file_change_targets_exact_path(item: &Value, sandbox: &Path, expected_relative: &Path) -> bool {
    item.get("changes")
        .and_then(Value::as_array)
        .filter(|changes| !changes.is_empty())
        .is_some_and(|changes| {
            changes.iter().all(|change| {
                change
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| {
                        validate_exact_permission_path(sandbox, path, expected_relative).is_ok()
                    })
            })
        })
}

fn meaningful_update_item_type<'a>(message: &Value, method: &'a str) -> Option<&'a str> {
    let meaningful = match method {
        METHOD_COMMAND_OUTPUT_DELTA => message
            .pointer("/params/delta")
            .and_then(Value::as_str)
            .is_some_and(|delta| !delta.is_empty()),
        METHOD_MCP_TOOL_PROGRESS => message
            .pointer("/params/message")
            .and_then(Value::as_str)
            .is_some_and(|progress| !progress.is_empty()),
        METHOD_FILE_CHANGE_PATCH_UPDATED => message
            .pointer("/params/changes")
            .and_then(Value::as_array)
            .is_some_and(|changes| {
                changes.iter().any(|change| {
                    change
                        .get("diff")
                        .and_then(Value::as_str)
                        .is_some_and(|diff| !diff.trim().is_empty())
                })
            }),
        _ => false,
    };
    if !meaningful {
        return None;
    }
    match method {
        METHOD_COMMAND_OUTPUT_DELTA => Some("commandExecution"),
        METHOD_MCP_TOOL_PROGRESS => Some("mcpToolCall"),
        METHOD_FILE_CHANGE_PATCH_UPDATED => Some("fileChange"),
        _ => None,
    }
}

fn observe_item_update(message: &Value, expected_item_type: &str, observations: &mut Observations) {
    let thread_id = message.pointer("/params/threadId").and_then(Value::as_str);
    let turn_id = message.pointer("/params/turnId").and_then(Value::as_str);
    let item_id = message.pointer("/params/itemId").and_then(Value::as_str);
    let (Some(thread_id), Some(turn_id), Some(item_id)) = (thread_id, turn_id, item_id) else {
        return;
    };
    if expected_item_type == "fileChange"
        && !file_change_targets_exact_path(
            message.pointer("/params").unwrap_or(&Value::Null),
            &observations.sandbox,
            Path::new("editable.txt"),
        )
    {
        return;
    }
    if let Some(lifecycle) = observations.item_lifecycles.iter_mut().find(|lifecycle| {
        lifecycle.thread_id == thread_id
            && lifecycle.turn_id == turn_id
            && lifecycle.item_id == item_id
            && lifecycle.item_type == expected_item_type
            && lifecycle.terminal_status.is_none()
    }) {
        lifecycle.meaningful_update_seen = true;
    }
}

fn observe_item_completed(message: &Value, observations: &mut Observations) {
    let thread_id = message.pointer("/params/threadId").and_then(Value::as_str);
    let turn_id = message.pointer("/params/turnId").and_then(Value::as_str);
    let item_id = message.pointer("/params/item/id").and_then(Value::as_str);
    let item_type = message.pointer("/params/item/type").and_then(Value::as_str);
    let status = message
        .pointer("/params/item/status")
        .and_then(Value::as_str);
    let (Some(thread_id), Some(turn_id), Some(item_id), Some(item_type), Some(status)) =
        (thread_id, turn_id, item_id, item_type, status)
    else {
        return;
    };
    if let Some(lifecycle) = observations.item_lifecycles.iter_mut().find(|lifecycle| {
        lifecycle.thread_id == thread_id
            && lifecycle.turn_id == turn_id
            && lifecycle.item_id == item_id
            && lifecycle.item_type == item_type
            && lifecycle.terminal_status.is_none()
    }) {
        lifecycle.terminal_status = Some(status.to_owned());
        lifecycle.exit_code = message
            .pointer("/params/item/exitCode")
            .and_then(Value::as_i64);
    }
    for flow in observations.permission_flows.iter_mut().filter(|flow| {
        flow.thread_id == thread_id
            && flow.turn_id == turn_id
            && flow.target_item_id == item_id
            && flow.terminal_status_after_reply.is_none()
    }) {
        flow.terminal_status_after_reply = Some(status.to_owned());
    }
}

fn observe_turn_diff(message: &Value, observations: &mut Observations) {
    let thread_id = message.pointer("/params/threadId").and_then(Value::as_str);
    let turn_id = message.pointer("/params/turnId").and_then(Value::as_str);
    let diff = message.pointer("/params/diff").and_then(Value::as_str);
    let (Some(thread_id), Some(turn_id), Some(diff)) = (thread_id, turn_id, diff) else {
        return;
    };
    if !is_nonempty_unified_diff_for_path(diff, Path::new("editable.txt")) {
        return;
    }
    observations.nonempty_diff_updates += 1;
    if !observations
        .nonempty_diff_turns
        .iter()
        .any(|seen| seen.0 == thread_id && seen.1 == turn_id)
    {
        observations
            .nonempty_diff_turns
            .push((thread_id.to_owned(), turn_id.to_owned()));
    }
}

fn is_nonempty_unified_diff_for_path(diff: &str, expected_relative: &Path) -> bool {
    let mut old_file = false;
    let mut new_file = false;
    let mut hunk = false;
    let mut changed_line = false;
    for line in diff.lines() {
        if line.starts_with("--- ") {
            old_file = true;
            if !diff_header_matches_path(line, "--- ", "a/", expected_relative) {
                return false;
            }
        }
        if line.starts_with("+++ ") {
            new_file = true;
            if !diff_header_matches_path(line, "+++ ", "b/", expected_relative) {
                return false;
            }
        }
        hunk |= line.starts_with("@@ ");
        changed_line |= (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"));
    }
    old_file && new_file && hunk && changed_line
}

fn diff_header_matches_path(
    line: &str,
    marker: &str,
    side_prefix: &str,
    expected_relative: &Path,
) -> bool {
    line.strip_prefix(marker)
        .and_then(|header| header.split_whitespace().next())
        .and_then(|path| path.strip_prefix(side_prefix).or(Some(path)))
        .is_some_and(|path| Path::new(path) == expected_relative)
}

fn observe_error_info(value: Option<&Value>, observations: &mut Observations) {
    if value.is_some() {
        observations.error_info_count += 1;
    }
}

fn matching_completion(
    observations: &Observations,
    thread_id: &str,
    turn_id: &str,
) -> Option<String> {
    observations
        .turn_completion
        .as_ref()
        .filter(|completion| completion.thread_id == thread_id && completion.turn_id == turn_id)
        .map(|completion| completion.status.clone())
}

fn handle_server_request(
    io: &mut impl ProtocolIo,
    request: &Value,
    decision: PermissionDecision,
    observations: &mut Observations,
) -> Result<(), CodexRecorderError> {
    let id = request
        .get("id")
        .cloned()
        .ok_or(CodexRecorderError::Malformed("server request id"))?;
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or(CodexRecorderError::Malformed("server request method"))?;
    let params = request.get("params").unwrap_or(&Value::Null);
    observations.server_request_methods.push(method.to_owned());

    if is_permission_server_request(request) {
        validate_permission_request_scope(method, params, observations)
            .map_err(|_| CodexRecorderError::UnsafePermissionScope)?;
        io.commit_pending_server_request()?;
    }

    let result = match method {
        METHOD_COMMAND_APPROVAL => {
            observations.permission_requests += 1;
            json!({"decision": command_decision(decision)})
        }
        METHOD_FILE_CHANGE_APPROVAL => {
            observations.permission_requests += 1;
            json!({"decision": file_change_decision(decision)})
        }
        METHOD_PERMISSION_PROFILE_APPROVAL => {
            observations.permission_requests += 1;
            permission_profile_result(params, decision)?
        }
        METHOD_LEGACY_EXEC_APPROVAL | METHOD_LEGACY_PATCH_APPROVAL => {
            observations.permission_requests += 1;
            json!({"decision": legacy_review_decision(decision)})
        }
        METHOD_MCP_ELICITATION => {
            if notification_matches_active_turn(request, observations) {
                observations.elicitation_requests += 1;
            }
            json!({"action": "cancel"})
        }
        METHOD_TOOL_USER_INPUT => {
            observations.user_input_requests += 1;
            tool_user_input_result(params)?
        }
        _ => {
            return io.send(&json!({
                "id": id,
                "error": {
                    "code": -32601,
                    "message": "kaleido-recorder does not implement this server request"
                }
            }));
        }
    };
    if matches!(
        method,
        METHOD_COMMAND_APPROVAL
            | METHOD_FILE_CHANGE_APPROVAL
            | METHOD_PERMISSION_PROFILE_APPROVAL
            | METHOD_LEGACY_EXEC_APPROVAL
            | METHOD_LEGACY_PATCH_APPROVAL
    ) {
        observe_permission_reply(params, decision, observations);
    }
    io.send(&json!({"id": id, "result": result}))
}

fn validate_permission_request_scope(
    method: &str,
    params: &Value,
    observations: &Observations,
) -> Result<(), PermissionScopeError> {
    match method {
        METHOD_COMMAND_APPROVAL => validate_modern_command_scope(params, observations),
        METHOD_FILE_CHANGE_APPROVAL => validate_modern_file_scope(params, observations),
        METHOD_PERMISSION_PROFILE_APPROVAL => {
            validate_permission_profile_scope(params, &observations.sandbox)?;
            validate_any_correlated_lifecycle(params, observations)
        }
        METHOD_LEGACY_EXEC_APPROVAL => validate_legacy_command_scope(params, observations),
        METHOD_LEGACY_PATCH_APPROVAL => validate_legacy_patch_scope(params, observations),
        _ => Ok(()),
    }
}

fn validate_modern_command_scope(
    params: &Value,
    observations: &Observations,
) -> Result<(), PermissionScopeError> {
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or(PermissionScopeError::UnprovablePath)?;
    validate_exact_permission_cwd(&observations.sandbox, cwd)?;
    let command = params
        .get("command")
        .and_then(Value::as_str)
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    validate_permission_command(command)?;
    validate_command_actions(
        params
            .get("commandActions")
            .ok_or(PermissionScopeError::UnsafeCommand)?,
        &observations.sandbox,
    )?;
    if params
        .get("networkApprovalContext")
        .is_some_and(|value| !value.is_null())
        || params
            .get("proposedNetworkPolicyAmendments")
            .is_some_and(value_has_entries)
        || params
            .get("proposedExecpolicyAmendment")
            .is_some_and(value_has_entries)
    {
        return Err(PermissionScopeError::UnsafeCommand);
    }
    validate_correlated_lifecycle(params, observations, "commandExecution")
}

fn validate_modern_file_scope(
    params: &Value,
    observations: &Observations,
) -> Result<(), PermissionScopeError> {
    validate_optional_grant_root(params, &observations.sandbox)?;
    validate_correlated_lifecycle(params, observations, "fileChange")
}

fn validate_legacy_command_scope(
    params: &Value,
    observations: &Observations,
) -> Result<(), PermissionScopeError> {
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or(PermissionScopeError::UnprovablePath)?;
    validate_exact_permission_cwd(&observations.sandbox, cwd)?;
    let command = params
        .get("command")
        .and_then(Value::as_array)
        .ok_or(PermissionScopeError::UnsafeCommand)?
        .iter()
        .map(|part| {
            part.as_str()
                .map(str::to_owned)
                .ok_or(PermissionScopeError::UnsafeCommand)
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_permission_argv(&command)?;
    validate_parsed_commands(
        params
            .get("parsedCmd")
            .ok_or(PermissionScopeError::UnsafeCommand)?,
        &observations.sandbox,
    )?;
    validate_correlated_lifecycle(params, observations, "commandExecution")
}

fn validate_legacy_patch_scope(
    params: &Value,
    observations: &Observations,
) -> Result<(), PermissionScopeError> {
    let changes = params
        .get("fileChanges")
        .and_then(Value::as_object)
        .filter(|changes| !changes.is_empty())
        .ok_or(PermissionScopeError::UnprovablePath)?;
    for path in changes.keys() {
        validate_permission_path(&observations.sandbox, path)?;
    }
    validate_optional_grant_root(params, &observations.sandbox)?;
    validate_correlated_lifecycle(params, observations, "fileChange")
}

fn validate_permission_profile_scope(
    params: &Value,
    sandbox: &Path,
) -> Result<(), PermissionScopeError> {
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or(PermissionScopeError::UnprovablePath)?;
    validate_exact_permission_cwd(sandbox, cwd)?;
    let permissions = params
        .get("permissions")
        .and_then(Value::as_object)
        .ok_or(PermissionScopeError::UnprovablePath)?;
    if permissions
        .keys()
        .any(|key| !matches!(key.as_str(), "fileSystem" | "network"))
    {
        return Err(PermissionScopeError::UnprovablePath);
    }
    if permissions
        .get("network")
        .is_some_and(|network| !network.is_null())
    {
        return Err(PermissionScopeError::UnsafeCommand);
    }
    let Some(file_system) = permissions.get("fileSystem") else {
        return Ok(());
    };
    if file_system.is_null() {
        return Ok(());
    }
    let file_system = file_system
        .as_object()
        .ok_or(PermissionScopeError::UnprovablePath)?;
    if file_system.keys().any(|key| {
        !matches!(
            key.as_str(),
            "entries" | "globScanMaxDepth" | "read" | "write"
        )
    }) {
        return Err(PermissionScopeError::UnprovablePath);
    }
    for field in ["read", "write"] {
        if let Some(paths) = file_system.get(field) {
            if paths.is_null() {
                continue;
            }
            super::validate_path_array(sandbox, paths)?;
        }
    }
    if let Some(entries) = file_system.get("entries") {
        if entries.is_null() {
            return Ok(());
        }
        let entries = entries
            .as_array()
            .ok_or(PermissionScopeError::UnprovablePath)?;
        for entry in entries {
            if entry.pointer("/path/type").and_then(Value::as_str) != Some("path") {
                return Err(PermissionScopeError::UnprovablePath);
            }
            let path = entry
                .pointer("/path/path")
                .and_then(Value::as_str)
                .ok_or(PermissionScopeError::UnprovablePath)?;
            validate_permission_path(sandbox, path)?;
        }
    }
    Ok(())
}

fn validate_command_actions(value: &Value, sandbox: &Path) -> Result<(), PermissionScopeError> {
    let actions = value
        .as_array()
        .filter(|actions| !actions.is_empty())
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    for action in actions {
        let action_type = action
            .get("type")
            .and_then(Value::as_str)
            .filter(|action_type| matches!(*action_type, "read" | "listFiles" | "search"))
            .ok_or(PermissionScopeError::UnsafeCommand)?;
        let action_command = action
            .get("command")
            .and_then(Value::as_str)
            .ok_or(PermissionScopeError::UnsafeCommand)?;
        validate_permission_command(action_command)?;
        if action_type == "read" && action.get("path").is_none_or(Value::is_null) {
            return Err(PermissionScopeError::UnprovablePath);
        }
        if let Some(path) = action.get("path") {
            if !path.is_null() {
                validate_permission_path(
                    sandbox,
                    path.as_str().ok_or(PermissionScopeError::UnprovablePath)?,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_parsed_commands(value: &Value, sandbox: &Path) -> Result<(), PermissionScopeError> {
    let commands = value
        .as_array()
        .filter(|commands| !commands.is_empty())
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    for command in commands {
        let command_type = command
            .get("type")
            .and_then(Value::as_str)
            .filter(|command_type| matches!(*command_type, "read" | "list_files" | "search"))
            .ok_or(PermissionScopeError::UnsafeCommand)?;
        let raw = command
            .get("cmd")
            .and_then(Value::as_str)
            .ok_or(PermissionScopeError::UnsafeCommand)?;
        validate_permission_command(raw)?;
        if command_type == "read" && command.get("path").is_none_or(Value::is_null) {
            return Err(PermissionScopeError::UnprovablePath);
        }
        if let Some(path) = command.get("path") {
            if !path.is_null() {
                validate_permission_path(
                    sandbox,
                    path.as_str().ok_or(PermissionScopeError::UnprovablePath)?,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_optional_grant_root(
    params: &Value,
    sandbox: &Path,
) -> Result<(), PermissionScopeError> {
    if let Some(grant_root) = params.get("grantRoot") {
        if !grant_root.is_null() {
            validate_permission_path(
                sandbox,
                grant_root
                    .as_str()
                    .ok_or(PermissionScopeError::UnprovablePath)?,
            )?;
        }
    }
    Ok(())
}

fn validate_correlated_lifecycle(
    params: &Value,
    observations: &Observations,
    expected_item_type: &str,
) -> Result<(), PermissionScopeError> {
    let thread_id = params
        .get("threadId")
        .or_else(|| params.get("conversationId"))
        .and_then(Value::as_str)
        .ok_or(PermissionScopeError::UnprovablePath)?;
    let turn_id = params.get("turnId").and_then(Value::as_str);
    let item_id = params
        .get("itemId")
        .or_else(|| params.get("callId"))
        .and_then(Value::as_str)
        .ok_or(PermissionScopeError::UnprovablePath)?;
    if observations.active_thread_id.as_deref() != Some(thread_id)
        || turn_id.is_some_and(|turn_id| observations.active_turn_id.as_deref() != Some(turn_id))
    {
        return Err(PermissionScopeError::UnprovablePath);
    }
    let matched = observations.item_lifecycles.iter().any(|lifecycle| {
        lifecycle.thread_id == thread_id
            && turn_id.is_none_or(|turn_id| lifecycle.turn_id == turn_id)
            && lifecycle.item_id == item_id
            && lifecycle.item_type == expected_item_type
            && lifecycle.permission_scope_safe
            && lifecycle.terminal_status.is_none()
    });
    if matched {
        Ok(())
    } else {
        Err(PermissionScopeError::UnprovablePath)
    }
}

fn validate_any_correlated_lifecycle(
    params: &Value,
    observations: &Observations,
) -> Result<(), PermissionScopeError> {
    ["commandExecution", "fileChange"]
        .into_iter()
        .find_map(|item_type| {
            validate_correlated_lifecycle(params, observations, item_type)
                .is_ok()
                .then_some(())
        })
        .ok_or(PermissionScopeError::UnprovablePath)
}

fn value_has_entries(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(values) => !values.is_empty(),
        _ => true,
    }
}

fn observe_permission_reply(
    params: &Value,
    decision: PermissionDecision,
    observations: &mut Observations,
) {
    let thread_id = params
        .get("threadId")
        .or_else(|| params.get("conversationId"))
        .and_then(Value::as_str);
    let turn_id = params.get("turnId").and_then(Value::as_str);
    let target_item_id = params
        .get("itemId")
        .or_else(|| params.get("callId"))
        .and_then(Value::as_str);
    let (Some(thread_id), Some(turn_id), Some(target_item_id)) =
        (thread_id, turn_id, target_item_id)
    else {
        return;
    };
    let target_started_before_reply = observations.item_lifecycles.iter().any(|lifecycle| {
        lifecycle.thread_id == thread_id
            && lifecycle.turn_id == turn_id
            && lifecycle.item_id == target_item_id
            && lifecycle.terminal_status.is_none()
    });
    observations
        .permission_flows
        .push(PermissionFlowObservation {
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            target_item_id: target_item_id.to_owned(),
            decision,
            target_started_before_reply,
            terminal_status_after_reply: None,
        });
}

const fn command_decision(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Approve => "accept",
        PermissionDecision::Deny => "decline",
    }
}

const fn file_change_decision(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Approve => "accept",
        PermissionDecision::Deny => "decline",
    }
}

const fn legacy_review_decision(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Approve => "approved",
        PermissionDecision::Deny => "denied",
    }
}

fn permission_profile_result(
    params: &Value,
    decision: PermissionDecision,
) -> Result<Value, CodexRecorderError> {
    let permissions = match decision {
        PermissionDecision::Approve => {
            params
                .get("permissions")
                .cloned()
                .ok_or(CodexRecorderError::Malformed(
                    "item/permissions/requestApproval permissions",
                ))?
        }
        PermissionDecision::Deny => Value::Object(Map::new()),
    };
    Ok(json!({
        "permissions": permissions,
        "scope": "turn"
    }))
}

fn tool_user_input_result(params: &Value) -> Result<Value, CodexRecorderError> {
    let questions =
        params
            .get("questions")
            .and_then(Value::as_array)
            .ok_or(CodexRecorderError::Malformed(
                "item/tool/requestUserInput questions",
            ))?;
    let mut answers = Map::new();
    for question in questions {
        let id =
            question
                .get("id")
                .and_then(Value::as_str)
                .ok_or(CodexRecorderError::Malformed(
                    "item/tool/requestUserInput question id",
                ))?;
        answers.insert(id.to_owned(), json!({"answers": []}));
    }
    Ok(json!({"answers": answers}))
}

fn required_string(
    value: &Value,
    path: &[&str],
    description: &'static str,
) -> Result<String, CodexRecorderError> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .ok_or(CodexRecorderError::Malformed(description))?;
    }
    current
        .as_str()
        .map(str::to_owned)
        .ok_or(CodexRecorderError::Malformed(description))
}

#[cfg(test)]
mod tests {
    // These JSON values are synthetic state-machine inputs only. They verify
    // routing and response semantics and are never written as fixture data;
    // contract fixtures must come exclusively from a real app-server process.
    use std::collections::VecDeque;
    use std::error::Error;
    use std::{fs, io};

    use super::*;

    #[test]
    fn completed_protocol_recording_survives_cleanup_failure() -> Result<(), Box<dyn Error>> {
        let recording = CodexRecording {
            scenario: CodexScenario::SimpleTurn,
            thread_id: "thread-test".to_owned(),
            turn_id: Some("turn-test".to_owned()),
            completion_status: Some("completed".to_owned()),
            notification_methods: vec!["item/agentMessage/delta".to_owned()],
            server_request_methods: Vec::new(),
            permission_requests: 0,
            elicitation_requests: 0,
            user_input_requests: 0,
            item_types_started: Vec::new(),
            item_types_completed: Vec::new(),
            command_exit_codes: Vec::new(),
            diff_updates: 0,
            nonempty_diff_updates: 0,
            error_info_count: 0,
            editable_file_changed: false,
            item_lifecycles: Vec::new(),
            permission_flows: Vec::new(),
            nonempty_diff_turns: Vec::new(),
        };
        let expected = recording.clone();
        let completed = finish_recording(Ok(recording), || {
            Err(StdioError::Process(
                platform::ProcessError::IncompleteCleanup {
                    root_pid: 7,
                    unconfirmed_pids: vec![42],
                    detail: "forced cleanup failure".to_owned(),
                },
            ))
        })?;

        assert_eq!(completed.outcome, expected);
        assert_eq!(completed.cleanup_issues.len(), 1);
        assert_eq!(
            completed
                .cleanup_issues
                .first()
                .map(|issue| issue.unconfirmed_pids.as_slice()),
            Some([42].as_slice())
        );
        Ok(())
    }

    #[test]
    fn protocol_and_cleanup_failures_are_both_reported() -> Result<(), Box<dyn Error>> {
        let protocol = CodexRecorderError::Malformed("forced protocol failure");
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
            CodexRecorderError::CleanupAfterError {
                source,
                cleanup: StdioError::Process(platform::ProcessError::Terminate(_)),
            } if matches!(
                source.as_ref(),
                CodexRecorderError::Malformed("forced protocol failure")
            )
        ));
        let message = error.to_string();
        assert!(message.contains("forced protocol failure"));
        assert!(message.contains("forced cleanup failure"));
        Ok(())
    }

    #[derive(Debug, Default)]
    struct MemoryIo {
        incoming: VecDeque<Value>,
        outgoing: Vec<Value>,
    }

    impl MemoryIo {
        fn with_incoming(incoming: impl IntoIterator<Item = Value>) -> Self {
            Self {
                incoming: incoming.into_iter().collect(),
                outgoing: Vec::new(),
            }
        }
    }

    impl ProtocolIo for MemoryIo {
        fn send(&mut self, message: &Value) -> Result<(), CodexRecorderError> {
            self.outgoing.push(message.clone());
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Value, CodexRecorderError> {
            self.incoming
                .pop_front()
                .ok_or(CodexRecorderError::Malformed(
                    "synthetic unit-test input exhausted",
                ))
        }
    }

    fn test_config() -> CodexRecorderConfig {
        CodexRecorderConfig {
            request_timeout: Duration::from_secs(1),
            turn_timeout: Duration::from_secs(1),
        }
    }

    fn initialized() -> Value {
        json!({"id": 1, "result": {
            "codexHome": "<HOME>/.codex",
            "platformFamily": "windows",
            "platformOs": "windows",
            "userAgent": "test"
        }})
    }

    fn thread_started() -> Value {
        json!({"id": 2, "result": {"thread": {"id": "thread-1"}}})
    }

    fn turn_started() -> Value {
        json!({"id": 3, "result": {"turn": {"id": "turn-1"}}})
    }

    fn turn_completed(status: &str) -> Value {
        json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread-1",
                "turn": {"id": "turn-1", "status": status}
            }
        })
    }

    fn run_test(
        scenario: CodexScenario,
        incoming: impl IntoIterator<Item = Value>,
    ) -> (Result<CodexRecording, CodexRecorderError>, MemoryIo) {
        let mut io = MemoryIo::with_incoming(incoming);
        let sandbox = test_sandbox_path();
        let result = run_protocol(&mut io, &sandbox, scenario, &test_config());
        (result, io)
    }

    fn test_sandbox_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("sandbox")
    }

    fn item_started(item_id: &str, item_type: &str) -> Value {
        let sandbox = test_sandbox_path().to_string_lossy().into_owned();
        let item = match item_type {
            "commandExecution" => json!({
                "id": item_id,
                "type": item_type,
                "status": "inProgress",
                "command": "cargo run --",
                "commandActions": [{
                    "type": "listFiles",
                    "command": "cargo run --",
                    "path": null
                }],
                "cwd": sandbox
            }),
            "fileChange" => json!({
                "id": item_id,
                "type": item_type,
                "status": "inProgress",
                "changes": [{
                    "path": "editable.txt",
                    "kind": "update",
                    "diff": "@@ -1 +1 @@"
                }]
            }),
            _ => json!({
                "id": item_id,
                "type": item_type,
                "status": "inProgress"
            }),
        };
        json!({
            "method": "item/started",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "startedAtMs": 1,
                "item": item
            }
        })
    }

    fn command_started(
        item_id: &str,
        command: &str,
        action_type: &str,
        path: Option<&str>,
        cwd: &Path,
    ) -> Value {
        json!({
            "method": "item/started",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "startedAtMs": 1,
                "item": {
                    "id": item_id,
                    "type": "commandExecution",
                    "status": "inProgress",
                    "command": command,
                    "commandActions": [{
                        "type": action_type,
                        "command": command,
                        "path": path
                    }],
                    "cwd": cwd.to_string_lossy()
                }
            }
        })
    }

    fn notes_read_started(item_id: &str, path: &str) -> Value {
        command_started(
            item_id,
            "read notes.txt",
            "read",
            Some(path),
            &test_sandbox_path(),
        )
    }

    fn failure_command_started(item_id: &str, command: &str) -> Value {
        command_started(item_id, command, "unknown", None, &test_sandbox_path())
    }

    fn file_change_started(item_id: &str, path: &str) -> Value {
        json!({
            "method": "item/started",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "startedAtMs": 1,
                "item": {
                    "id": item_id,
                    "type": "fileChange",
                    "status": "inProgress",
                    "changes": [{
                        "path": path,
                        "kind": "update",
                        "diff": "@@ -1 +1 @@"
                    }]
                }
            }
        })
    }

    fn item_completed(item_id: &str, item_type: &str, status: &str) -> Value {
        json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "item": {
                    "id": item_id,
                    "type": item_type,
                    "status": status
                }
            }
        })
    }

    fn command_completed(item_id: &str, status: &str, exit_code: i64) -> Value {
        json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "item": {
                    "id": item_id,
                    "type": "commandExecution",
                    "status": status,
                    "exitCode": exit_code
                }
            }
        })
    }

    fn command_output(item_id: &str, delta: &str) -> Value {
        json!({
            "method": "item/commandExecution/outputDelta",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": item_id,
                "delta": delta
            }
        })
    }

    fn file_patch_update(item_id: &str, path: &str, diff: &str) -> Value {
        json!({
            "method": "item/fileChange/patchUpdated",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": item_id,
                "changes": [{
                    "path": path,
                    "diff": diff
                }]
            }
        })
    }

    fn command_permission_request(item_id: &str) -> Value {
        let sandbox = test_sandbox_path().to_string_lossy().into_owned();
        json!({
            "id": "permission-1",
            "method": "item/commandExecution/requestApproval",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": item_id,
                "startedAtMs": 1,
                "cwd": sandbox,
                "command": "cargo run --",
                "commandActions": [{
                    "type": "listFiles",
                    "command": "cargo run --",
                    "path": null
                }]
            }
        })
    }

    #[test]
    fn simple_turn_uses_schema_methods_and_waits_for_completion() -> Result<(), Box<dyn Error>> {
        let (result, io) = run_test(
            CodexScenario::SimpleTurn,
            [
                initialized(),
                thread_started(),
                turn_started(),
                json!({"method": "item/agentMessage/delta", "params": {}}),
                turn_completed("completed"),
            ],
        );
        let recording = result?;
        assert_eq!(recording.completion_status.as_deref(), Some("completed"));
        assert_eq!(
            io.outgoing
                .iter()
                .filter_map(|message| message.get("method").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            vec!["initialize", "initialized", "thread/start", "turn/start"]
        );
        assert_eq!(
            io.outgoing
                .iter()
                .find(|message| message.get("method") == Some(&Value::from("thread/start")))
                .and_then(|message| message.pointer("/params/ephemeral")),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            io.outgoing.first().and_then(|message| {
                message.pointer("/params/capabilities/mcpServerOpenaiFormElicitation")
            }),
            Some(&Value::Bool(true))
        );
        Ok(())
    }

    #[test]
    fn simple_turn_rejects_an_unsafe_complete_tool_lifecycle() -> Result<(), Box<dyn Error>> {
        let outside = test_sandbox_path()
            .parent()
            .ok_or("fixture sandbox must have a parent")?
            .join("README.md")
            .to_string_lossy()
            .into_owned();
        let (result, _) = run_test(
            CodexScenario::SimpleTurn,
            [
                initialized(),
                thread_started(),
                turn_started(),
                notes_read_started("unsafe-tool", &outside),
                command_output("unsafe-tool", "outside contents"),
                item_completed("unsafe-tool", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );

        assert!(matches!(
            result,
            Err(CodexRecorderError::UnsafeToolLifecycleScope)
        ));
        Ok(())
    }

    #[test]
    fn cancel_rejects_an_unsafe_complete_tool_lifecycle() -> Result<(), Box<dyn Error>> {
        let outside = test_sandbox_path()
            .parent()
            .ok_or("fixture sandbox must have a parent")?
            .join("README.md")
            .to_string_lossy()
            .into_owned();
        let (result, _) = run_test(
            CodexScenario::Cancel,
            [
                initialized(),
                thread_started(),
                turn_started(),
                notes_read_started("unsafe-tool", &outside),
                json!({"id": 4, "result": {}}),
                command_output("unsafe-tool", "outside contents"),
                item_completed("unsafe-tool", "commandExecution", "completed"),
                turn_completed("interrupted"),
            ],
        );

        assert!(matches!(
            result,
            Err(CodexRecorderError::UnsafeToolLifecycleScope)
        ));
        Ok(())
    }

    #[test]
    fn simple_error_and_elicitation_evidence_is_scoped_to_the_active_turn(
    ) -> Result<(), Box<dyn Error>> {
        let current_delta = json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "message-1",
                "delta": "real reply"
            }
        });
        let foreign_delta = json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "thread-foreign",
                "turnId": "turn-foreign",
                "itemId": "message-foreign",
                "delta": "foreign reply"
            }
        });
        let empty_delta = json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "message-empty",
                "delta": ""
            }
        });
        let (recording, _) = run_test(
            CodexScenario::SimpleTurn,
            [
                initialized(),
                thread_started(),
                turn_started(),
                foreign_delta,
                empty_delta,
                current_delta,
                turn_completed("completed"),
            ],
        );
        let recording = recording?;
        assert_eq!(
            recording
                .notification_methods
                .iter()
                .filter(|method| method.as_str() == "item/agentMessage/delta")
                .count(),
            1
        );

        let foreign_failure = json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread-foreign",
                "turnId": "turn-foreign",
                "item": {
                    "id": "tool-foreign",
                    "type": "commandExecution",
                    "status": "failed",
                    "exitCode": 1
                }
            }
        });
        let (recording, _) = run_test(
            CodexScenario::Error,
            [
                initialized(),
                thread_started(),
                turn_started(),
                foreign_failure,
                turn_completed("completed"),
            ],
        );
        assert!(!recording?.observed_failed_command());

        let foreign_elicitation = json!({
            "id": "foreign-elicit",
            "method": "mcpServer/elicitation/request",
            "params": {
                "threadId": "thread-foreign",
                "turnId": "turn-foreign",
                "serverName": "synthetic",
                "mode": "form",
                "message": "foreign",
                "requestedSchema": {"type": "object"}
            }
        });
        let (recording, _) = run_test(
            CodexScenario::Elicitation,
            [
                initialized(),
                thread_started(),
                turn_started(),
                foreign_elicitation,
                turn_completed("completed"),
            ],
        );
        assert!(!recording?.observed_elicitation_request());
        Ok(())
    }

    #[test]
    fn tool_call_requires_matching_start_update_and_successful_end() -> Result<(), Box<dyn Error>> {
        let (missing_update, _) = run_test(
            CodexScenario::ToolCall,
            [
                initialized(),
                thread_started(),
                turn_started(),
                notes_read_started("tool-1", "notes.txt"),
                item_completed("tool-1", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(!missing_update?.observed_tool_call());

        let (mismatched_end, _) = run_test(
            CodexScenario::ToolCall,
            [
                initialized(),
                thread_started(),
                turn_started(),
                notes_read_started("tool-1", "notes.txt"),
                command_output("tool-1", "sandbox contents"),
                item_completed("tool-2", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(!mismatched_end?.observed_tool_call());

        let (complete, _) = run_test(
            CodexScenario::ToolCall,
            [
                initialized(),
                thread_started(),
                turn_started(),
                notes_read_started("tool-1", "notes.txt"),
                command_output("tool-1", "sandbox contents"),
                item_completed("tool-1", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(complete?.observed_tool_call());

        let (wrong_target, _) = run_test(
            CodexScenario::ToolCall,
            [
                initialized(),
                thread_started(),
                turn_started(),
                notes_read_started("tool-1", "other.txt"),
                command_output("tool-1", "other contents"),
                item_completed("tool-1", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(!wrong_target?.observed_tool_call());

        let outside = test_sandbox_path()
            .parent()
            .ok_or("fixture sandbox must have a parent")?
            .join("README.md")
            .to_string_lossy()
            .into_owned();
        let (unsafe_extra_lifecycle, _) = run_test(
            CodexScenario::ToolCall,
            [
                initialized(),
                thread_started(),
                turn_started(),
                notes_read_started("tool-1", "notes.txt"),
                command_output("tool-1", "sandbox contents"),
                item_completed("tool-1", "commandExecution", "completed"),
                notes_read_started("unsafe-tool", &outside),
                command_output("unsafe-tool", "outside contents"),
                item_completed("unsafe-tool", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(matches!(
            unsafe_extra_lifecycle,
            Err(CodexRecorderError::UnsafeToolLifecycleScope)
        ));
        Ok(())
    }

    #[test]
    fn file_change_requires_nonempty_unified_diff() -> Result<(), Box<dyn Error>> {
        let run_file_change = |item_path: &str, patch_path: &str, diff: &str| {
            let diff_notification = json!({
                "method": "turn/diff/updated",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "diff": diff
                }
            });
            run_test(
                CodexScenario::FileChange,
                [
                    initialized(),
                    thread_started(),
                    turn_started(),
                    file_change_started("file-1", item_path),
                    file_patch_update("file-1", patch_path, "@@ -1 +1 @@\n-OLD\n+NEW"),
                    item_completed("file-1", "fileChange", "completed"),
                    diff_notification,
                    turn_completed("completed"),
                ],
            )
        };
        let mark_real_bytes_changed = |result: Result<CodexRecording, CodexRecorderError>| {
            result.map(|mut recording| {
                recording.editable_file_changed = true;
                recording
            })
        };

        let (empty, _) = run_file_change("editable.txt", "editable.txt", "   ");
        assert!(!mark_real_bytes_changed(empty)?.observed_file_change());

        let (not_a_unified_diff, _) = run_file_change("editable.txt", "editable.txt", "UPDATED");
        assert!(!mark_real_bytes_changed(not_a_unified_diff)?.observed_file_change());

        let (empty_hunk, _) = run_file_change(
            "editable.txt",
            "editable.txt",
            "--- a/editable.txt\n+++ b/editable.txt\n@@ -1,0 +1,0 @@\n unchanged\n",
        );
        assert!(!mark_real_bytes_changed(empty_hunk)?.observed_file_change());

        let exact_diff =
            "--- a/editable.txt\n+++ b/editable.txt\n@@ -1 +1 @@\n-ORIGINAL\n+UPDATED\n";
        let (unchanged_bytes, _) = run_file_change("editable.txt", "editable.txt", exact_diff);
        assert!(!unchanged_bytes?.observed_file_change());

        let (changed, _) = run_file_change("editable.txt", "editable.txt", exact_diff);
        assert!(mark_real_bytes_changed(changed)?.observed_file_change());

        let (wrong_item_target, _) = run_file_change("other.txt", "editable.txt", exact_diff);
        assert!(!mark_real_bytes_changed(wrong_item_target)?.observed_file_change());

        let (wrong_patch_target, _) = run_file_change("editable.txt", "other.txt", exact_diff);
        assert!(!mark_real_bytes_changed(wrong_patch_target)?.observed_file_change());

        let wrong_diff = "--- a/other.txt\n+++ b/other.txt\n@@ -1 +1 @@\n-ORIGINAL\n+UPDATED\n";
        let (wrong_diff_target, _) = run_file_change("editable.txt", "editable.txt", wrong_diff);
        assert!(!mark_real_bytes_changed(wrong_diff_target)?.observed_file_change());

        let outside = test_sandbox_path()
            .parent()
            .ok_or("fixture sandbox must have a parent")?
            .join("README.md")
            .to_string_lossy()
            .into_owned();
        let diff_notification = {
            let diff = exact_diff;
            json!({
                "method": "turn/diff/updated",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "diff": diff
                }
            })
        };
        let (unsafe_extra_lifecycle, _) = run_test(
            CodexScenario::FileChange,
            [
                initialized(),
                thread_started(),
                turn_started(),
                file_change_started("file-1", "editable.txt"),
                file_patch_update("file-1", "editable.txt", "@@ -1 +1 @@\n-ORIGINAL\n+UPDATED"),
                item_completed("file-1", "fileChange", "completed"),
                notes_read_started("unsafe-tool", &outside),
                command_output("unsafe-tool", "outside contents"),
                item_completed("unsafe-tool", "commandExecution", "completed"),
                diff_notification,
                turn_completed("completed"),
            ],
        );
        assert!(matches!(
            unsafe_extra_lifecycle,
            Err(CodexRecorderError::UnsafeToolLifecycleScope)
        ));
        Ok(())
    }

    #[test]
    fn editable_file_evidence_requires_real_byte_change() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let editable = directory.path().join("editable.txt");
        fs::write(&editable, b"ORIGINAL\n")?;
        let before = exact_sandbox_file_bytes(directory.path(), Path::new("editable.txt"))
            .ok_or("could not capture the initial editable.txt bytes")?;

        assert!(!exact_sandbox_file_changed(
            directory.path(),
            Path::new("editable.txt"),
            &before,
        ));
        fs::write(&editable, b"UPDATED\n")?;
        assert!(exact_sandbox_file_changed(
            directory.path(),
            Path::new("editable.txt"),
            &before,
        ));
        Ok(())
    }

    #[test]
    fn failed_command_requires_exact_safe_complete_lifecycle() -> Result<(), Box<dyn Error>> {
        let (exact, _) = run_test(
            CodexScenario::Error,
            [
                initialized(),
                thread_started(),
                turn_started(),
                failure_command_started("failure-1", "cargo run -- fail"),
                command_output("failure-1", "deterministic failure"),
                command_completed("failure-1", "failed", 9),
                turn_completed("completed"),
            ],
        );
        assert!(exact?.observed_failed_command());

        let (wrong_command, _) = run_test(
            CodexScenario::Error,
            [
                initialized(),
                thread_started(),
                turn_started(),
                failure_command_started("failure-1", "cargo run -- wait"),
                command_output("failure-1", "wrong command failed"),
                command_completed("failure-1", "failed", 9),
                turn_completed("completed"),
            ],
        );
        assert!(!wrong_command?.observed_failed_command());

        let sandbox = test_sandbox_path();
        let outside_cwd = sandbox
            .parent()
            .ok_or("fixture sandbox must have a parent")?;
        let (unsafe_scope, _) = run_test(
            CodexScenario::Error,
            [
                initialized(),
                thread_started(),
                turn_started(),
                command_started(
                    "failure-1",
                    "cargo run -- fail",
                    "unknown",
                    None,
                    outside_cwd,
                ),
                command_output("failure-1", "outside failure"),
                command_completed("failure-1", "failed", 9),
                turn_completed("completed"),
            ],
        );
        assert!(matches!(
            unsafe_scope,
            Err(CodexRecorderError::UnsafeToolLifecycleScope)
        ));

        let outside_path = outside_cwd.join("README.md").to_string_lossy().into_owned();
        let (unsafe_extra_lifecycle, _) = run_test(
            CodexScenario::Error,
            [
                initialized(),
                thread_started(),
                turn_started(),
                failure_command_started("failure-1", "cargo run -- fail"),
                command_output("failure-1", "deterministic failure"),
                command_completed("failure-1", "failed", 9),
                notes_read_started("unsafe-tool", &outside_path),
                command_output("unsafe-tool", "outside contents"),
                item_completed("unsafe-tool", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(matches!(
            unsafe_extra_lifecycle,
            Err(CodexRecorderError::UnsafeToolLifecycleScope)
        ));
        Ok(())
    }

    #[test]
    fn approved_permission_requires_target_success_and_completed_turn() -> Result<(), Box<dyn Error>>
    {
        let (no_target_end, _) = run_test(
            CodexScenario::PermissionApprove,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("tool-1", "commandExecution"),
                command_permission_request("tool-1"),
                turn_completed("completed"),
            ],
        );
        assert!(!no_target_end?.observed_approved_permission_flow());

        let (failed_target, _) = run_test(
            CodexScenario::PermissionApprove,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("tool-1", "commandExecution"),
                command_permission_request("tool-1"),
                item_completed("tool-1", "commandExecution", "failed"),
                turn_completed("completed"),
            ],
        );
        assert!(!failed_target?.observed_approved_permission_flow());

        let (continued, _) = run_test(
            CodexScenario::PermissionApprove,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("tool-1", "commandExecution"),
                command_permission_request("tool-1"),
                item_completed("tool-1", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(continued?.observed_approved_permission_flow());
        Ok(())
    }

    #[test]
    fn denied_permission_requires_declined_target_and_terminal_turn() -> Result<(), Box<dyn Error>>
    {
        let (wrong_target_status, _) = run_test(
            CodexScenario::PermissionDeny,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("tool-1", "commandExecution"),
                command_permission_request("tool-1"),
                item_completed("tool-1", "commandExecution", "completed"),
                turn_completed("completed"),
            ],
        );
        assert!(!wrong_target_status?.observed_denied_permission_flow());

        let (nonterminal_turn, _) = run_test(
            CodexScenario::PermissionDeny,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("tool-1", "commandExecution"),
                command_permission_request("tool-1"),
                item_completed("tool-1", "commandExecution", "declined"),
                turn_completed("interrupted"),
            ],
        );
        assert!(!nonterminal_turn?.observed_denied_permission_flow());

        let (denied, _) = run_test(
            CodexScenario::PermissionDeny,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("tool-1", "commandExecution"),
                command_permission_request("tool-1"),
                item_completed("tool-1", "commandExecution", "declined"),
                turn_completed("completed"),
            ],
        );
        assert!(denied?.observed_denied_permission_flow());
        Ok(())
    }

    #[test]
    fn approval_request_is_answered_with_accept() -> Result<(), Box<dyn Error>> {
        let mut server_request = command_permission_request("item-1");
        server_request
            .as_object_mut()
            .ok_or("synthetic request was not an object")?
            .insert("id".to_owned(), Value::from("server-1"));
        let (result, io) = run_test(
            CodexScenario::PermissionApprove,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("item-1", "commandExecution"),
                server_request,
                turn_completed("completed"),
            ],
        );
        let recording = result?;
        assert_eq!(recording.permission_requests, 1);
        assert!(io.outgoing.iter().any(|message| {
            message.get("id") == Some(&Value::from("server-1"))
                && message.pointer("/result/decision") == Some(&Value::from("accept"))
        }));
        Ok(())
    }

    #[test]
    fn denial_request_is_answered_with_decline() -> Result<(), Box<dyn Error>> {
        let server_request = json!({
            "id": 91,
            "method": "item/fileChange/requestApproval",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "startedAtMs": 1
            }
        });
        let (result, io) = run_test(
            CodexScenario::PermissionDeny,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("item-1", "fileChange"),
                server_request,
                turn_completed("completed"),
            ],
        );
        let recording = result?;
        assert_eq!(recording.permission_requests, 1);
        assert!(io.outgoing.iter().any(|message| {
            message.get("id") == Some(&Value::from(91))
                && message.pointer("/result/decision") == Some(&Value::from("decline"))
        }));
        Ok(())
    }

    #[test]
    fn unsafe_permission_request_is_not_answered_even_when_scenario_denies(
    ) -> Result<(), Box<dyn Error>> {
        const OUTSIDE_MARKER: &str = "PRIVATE OUTSIDE PERMISSION";
        let sandbox = test_sandbox_path().to_string_lossy().into_owned();
        let server_request = json!({
            "id": "unsafe-server-request",
            "method": "item/commandExecution/requestApproval",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "startedAtMs": 1,
                "cwd": sandbox,
                "command": format!("cargo run -- & type ..\\{OUTSIDE_MARKER}"),
                "commandActions": [{
                    "type": "unknown",
                    "command": format!("cargo run -- & type ..\\{OUTSIDE_MARKER}")
                }]
            }
        });
        let (result, io) = run_test(
            CodexScenario::PermissionDeny,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("item-1", "commandExecution"),
                server_request,
            ],
        );

        assert!(matches!(
            result,
            Err(CodexRecorderError::UnsafePermissionScope)
        ));
        assert!(!io
            .outgoing
            .iter()
            .any(|message| { message.get("id") == Some(&Value::from("unsafe-server-request")) }));
        let serialized = serde_json::to_string(&io.outgoing)?;
        assert!(!serialized.contains(OUTSIDE_MARKER));
        Ok(())
    }

    #[test]
    fn permission_profile_network_request_fails_closed_for_deny() {
        let sandbox = test_sandbox_path().to_string_lossy().into_owned();
        let server_request = json!({
            "id": "network-permission",
            "method": "item/permissions/requestApproval",
            "params": {
                "cwd": sandbox,
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "startedAtMs": 1,
                "permissions": {"network": {"enabled": true}}
            }
        });
        let (result, io) = run_test(
            CodexScenario::PermissionDeny,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("item-1", "commandExecution"),
                server_request,
            ],
        );

        assert!(matches!(
            result,
            Err(CodexRecorderError::UnsafePermissionScope)
        ));
        assert!(!io
            .outgoing
            .iter()
            .any(|message| message.get("id") == Some(&Value::from("network-permission"))));
    }

    #[test]
    fn file_permission_rejects_a_junction_target_before_reply() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let outside = temporary.path().join("outside");
        let linked = sandbox.join("linked");
        fs::create_dir(&sandbox)?;
        fs::create_dir(&outside)?;
        platform::create_test_directory_link(&outside, &linked)?;
        let mut observations = Observations::new(&sandbox);
        observe_notification(
            &json!({
                "method": "item/started",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": {
                        "id": "item-1",
                        "type": "fileChange",
                        "status": "inProgress",
                        "changes": [{
                            "path": "linked/new.txt",
                            "kind": "add",
                            "diff": "@@ -0 +1 @@"
                        }]
                    }
                }
            }),
            METHOD_ITEM_STARTED,
            &mut observations,
        );
        let request = json!({
            "id": "junction-permission",
            "method": "item/fileChange/requestApproval",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "startedAtMs": 1
            }
        });
        let mut io = MemoryIo::default();

        let result = handle_server_request(
            &mut io,
            &request,
            PermissionDecision::Deny,
            &mut observations,
        );

        assert!(matches!(
            result,
            Err(CodexRecorderError::UnsafePermissionScope)
        ));
        assert!(io.outgoing.is_empty());
        Ok(())
    }

    #[test]
    fn permission_profile_denial_grants_empty_profile() -> Result<(), Box<dyn Error>> {
        let sandbox = test_sandbox_path().to_string_lossy().into_owned();
        let server_request = json!({
            "id": "permissions-1",
            "method": "item/permissions/requestApproval",
            "params": {
                "cwd": sandbox,
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "startedAtMs": 1,
                "permissions": {
                    "fileSystem": {
                        "read": ["notes.txt"]
                    }
                }
            }
        });
        let (result, io) = run_test(
            CodexScenario::PermissionDeny,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("item-1", "commandExecution"),
                server_request,
                turn_completed("completed"),
            ],
        );
        let _ = result?;
        assert!(io.outgoing.iter().any(|message| {
            message.get("id") == Some(&Value::from("permissions-1"))
                && message.pointer("/result/permissions") == Some(&json!({}))
                && message.pointer("/result/scope") == Some(&Value::from("turn"))
        }));
        Ok(())
    }

    #[test]
    fn legacy_exec_approval_uses_legacy_review_decision() -> Result<(), Box<dyn Error>> {
        let sandbox = test_sandbox_path().to_string_lossy().into_owned();
        let server_request = json!({
            "id": "legacy-1",
            "method": "execCommandApproval",
            "params": {
                "conversationId": "thread-1",
                "callId": "call-1",
                "command": ["cargo", "run", "--"],
                "cwd": sandbox,
                "parsedCmd": [{
                    "type": "list_files",
                    "cmd": "cargo run --",
                    "path": null
                }]
            }
        });
        let (result, io) = run_test(
            CodexScenario::PermissionApprove,
            [
                initialized(),
                thread_started(),
                turn_started(),
                item_started("call-1", "commandExecution"),
                server_request,
                turn_completed("completed"),
            ],
        );
        let recording = result?;
        assert_eq!(recording.permission_requests, 1);
        assert!(io.outgoing.iter().any(|message| {
            message.get("id") == Some(&Value::from("legacy-1"))
                && message.pointer("/result/decision") == Some(&Value::from("approved"))
        }));
        Ok(())
    }

    #[test]
    fn mcp_elicitation_is_recorded_and_cancelled() -> Result<(), Box<dyn Error>> {
        let server_request = json!({
            "id": "elicit-1",
            "method": "mcpServer/elicitation/request",
            "params": {
                "serverName": "synthetic-unit-test-only",
                "threadId": "thread-1",
                "turnId": "turn-1",
                "mode": "form",
                "message": "synthetic unit-test input",
                "requestedSchema": {
                    "type": "object",
                    "properties": {}
                }
            }
        });
        let (result, io) = run_test(
            CodexScenario::Elicitation,
            [
                initialized(),
                thread_started(),
                turn_started(),
                server_request,
                turn_completed("completed"),
            ],
        );
        let recording = result?;
        assert_eq!(recording.elicitation_requests, 1);
        assert!(io.outgoing.iter().any(|message| {
            message.get("id") == Some(&Value::from("elicit-1"))
                && message.pointer("/result/action") == Some(&Value::from("cancel"))
        }));
        Ok(())
    }

    #[test]
    fn cancel_sends_turn_interrupt_before_waiting_for_completion() -> Result<(), Box<dyn Error>> {
        let (result, io) = run_test(
            CodexScenario::Cancel,
            [
                initialized(),
                thread_started(),
                turn_started(),
                json!({
                    "method": "item/started",
                    "params": {
                        "threadId": "thread-1",
                        "turnId": "turn-1",
                        "startedAtMs": 1,
                        "item": {"id": "item-1", "type": "commandExecution"}
                    }
                }),
                json!({"id": 4, "result": {}}),
                turn_completed("interrupted"),
            ],
        );
        let recording = result?;
        assert_eq!(recording.completion_status.as_deref(), Some("interrupted"));
        assert!(io.outgoing.iter().any(|message| {
            message.get("method") == Some(&Value::from("turn/interrupt"))
                && message.pointer("/params/threadId") == Some(&Value::from("thread-1"))
                && message.pointer("/params/turnId") == Some(&Value::from("turn-1"))
        }));
        Ok(())
    }

    #[test]
    fn cancel_fails_if_command_execution_never_starts() {
        let (result, io) = run_test(
            CodexScenario::Cancel,
            [
                initialized(),
                thread_started(),
                turn_started(),
                turn_completed("completed"),
            ],
        );
        assert!(matches!(
            result,
            Err(CodexRecorderError::CancelToolDidNotStart)
        ));
        assert!(!io
            .outgoing
            .iter()
            .any(|message| { message.get("method") == Some(&Value::from("turn/interrupt")) }));
    }

    #[test]
    fn session_load_lists_then_resumes_selected_thread() -> Result<(), Box<dyn Error>> {
        let scenario = CodexScenario::SessionLoad {
            thread_id: Some("external-thread".to_owned()),
        };
        let cwd = test_sandbox_path().to_string_lossy().into_owned();
        let (result, io) = run_test(
            scenario,
            [
                initialized(),
                json!({"id": 2, "result": {
                    "data": [
                        {"id": "another-thread", "cwd": cwd},
                        {"id": "external-thread", "cwd": cwd}
                    ]
                }}),
                json!({"id": 3, "result": {
                    "thread": {"id": "external-thread"}
                }}),
            ],
        );
        let recording = result?;
        assert_eq!(recording.thread_id, "external-thread");
        assert_eq!(recording.turn_id, None);
        assert_eq!(
            io.outgoing
                .iter()
                .filter_map(|message| message.get("method").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            vec!["initialize", "initialized", "thread/list", "thread/resume"]
        );
        Ok(())
    }

    #[test]
    fn session_load_validates_every_page_before_resuming() -> Result<(), Box<dyn Error>> {
        let cwd = test_sandbox_path().to_string_lossy().into_owned();
        let (result, io) = run_test(
            CodexScenario::SessionLoad {
                thread_id: Some("target-thread".to_owned()),
            },
            [
                initialized(),
                json!({"id": 2, "result": {
                    "data": [{"id": "first-thread", "cwd": cwd}],
                    "nextCursor": "page-2"
                }}),
                json!({"id": 2, "result": {
                    "data": [{"id": "target-thread", "cwd": cwd}],
                    "nextCursor": null
                }}),
                json!({"id": 3, "result": {
                    "thread": {"id": "target-thread"}
                }}),
            ],
        );

        let recording = result?;
        assert_eq!(recording.thread_id, "target-thread");
        assert_eq!(
            io.outgoing
                .iter()
                .filter(|message| message.get("method") == Some(&Value::from("thread/list")))
                .count(),
            2
        );
        assert_eq!(
            io.outgoing
                .last()
                .and_then(|message| message.get("method"))
                .and_then(Value::as_str),
            Some("thread/resume")
        );
        Ok(())
    }

    #[test]
    fn unsafe_thread_list_marker_is_not_written() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let outside = temporary.path().join("real-project");
        fs::create_dir_all(&sandbox)?;
        fs::create_dir_all(&outside)?;
        for cwd in [
            Some(outside.to_string_lossy().into_owned()),
            None,
            Some(sandbox.join("missing").to_string_lossy().into_owned()),
        ] {
            let thread = match cwd {
                Some(cwd) => json!({
                    "id": "outside",
                    "cwd": cwd,
                    "marker": "SESSION_LIST_SENSITIVE_MARKER"
                }),
                None => json!({
                    "id": "missing-cwd",
                    "marker": "SESSION_LIST_SENSITIVE_MARKER"
                }),
            };
            let raw = serde_json::to_string(&json!({
                "id": 2,
                "result": {"data": [thread]}
            }))?;
            let mut fixture = FixtureSink::new(Vec::new(), crate::redact::Redactor::from_pairs([]));

            let result = record_validated_thread_list_raw(&raw, 2, &sandbox, &mut fixture);
            assert!(matches!(result, Err(CodexRecorderError::UnsafeThreadList)));
            let bytes = fixture.into_inner();
            assert!(bytes.is_empty());
            assert!(!bytes
                .windows(b"SESSION_LIST_SENSITIVE_MARKER".len())
                .any(|window| window == b"SESSION_LIST_SENSITIVE_MARKER"));
        }
        Ok(())
    }

    #[test]
    fn unsafe_later_thread_list_page_never_resumes() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let outside = temporary.path().join("real-project");
        fs::create_dir_all(&outside)?;
        let cwd = test_sandbox_path().to_string_lossy().into_owned();
        let (result, io) = run_test(
            CodexScenario::SessionLoad {
                thread_id: Some("safe-thread".to_owned()),
            },
            [
                initialized(),
                json!({"id": 2, "result": {
                    "data": [{"id": "safe-thread", "cwd": cwd}],
                    "nextCursor": "page-2"
                }}),
                json!({"id": 2, "result": {
                    "data": [{
                        "id": "outside-thread",
                        "cwd": outside.to_string_lossy(),
                        "marker": "SESSION_LIST_SENSITIVE_MARKER"
                    }]
                }}),
            ],
        );

        assert!(matches!(result, Err(CodexRecorderError::UnsafeThreadList)));
        assert!(!io
            .outgoing
            .iter()
            .any(|message| message.get("method") == Some(&Value::from("thread/resume"))));
        Ok(())
    }

    #[test]
    fn unknown_server_request_gets_json_rpc_method_not_found() -> Result<(), Box<dyn Error>> {
        let (result, io) = run_test(
            CodexScenario::SimpleTurn,
            [
                initialized(),
                thread_started(),
                turn_started(),
                json!({"id": "unknown-1", "method": "unknown/request", "params": {}}),
                turn_completed("completed"),
            ],
        );
        let _ = result?;
        assert!(io.outgoing.iter().any(|message| {
            message.get("id") == Some(&Value::from("unknown-1"))
                && message.pointer("/error/code") == Some(&Value::from(-32601))
        }));
        Ok(())
    }

    #[test]
    fn wrong_response_id_is_rejected() {
        let (result, _) = run_test(CodexScenario::SimpleTurn, [json!({"id": 99, "result": {}})]);
        assert!(matches!(
            result,
            Err(CodexRecorderError::UnexpectedResponse)
        ));
    }

    #[test]
    fn session_load_without_thread_id_is_rejected_before_initialize() {
        let (result, io) = run_test(
            CodexScenario::SessionLoad { thread_id: None },
            std::iter::empty(),
        );

        assert!(matches!(result, Err(CodexRecorderError::ThreadIdRequired)));
        assert!(io.outgoing.is_empty());
    }

    #[test]
    fn missing_requested_session_is_an_error() {
        let cwd = test_sandbox_path().to_string_lossy().into_owned();
        let (result, _) = run_test(
            CodexScenario::SessionLoad {
                thread_id: Some("missing".to_owned()),
            },
            [
                initialized(),
                json!({"id": 2, "result": {"data": [{"id": "other", "cwd": cwd}]}}),
            ],
        );
        assert!(matches!(result, Err(CodexRecorderError::NoSession)));
    }

    #[test]
    fn upstream_error_details_are_counted_without_being_retained() {
        let mut observations = Observations::new(&test_sandbox_path());
        observe_notification(
            &json!({
                "method": "error",
                "params": {
                    "error": {
                        "codexErrorInfo": {
                            "unknownFutureVariant": {
                                "requestContext": "<sensitive>"
                            }
                        }
                    }
                }
            }),
            "error",
            &mut observations,
        );

        assert_eq!(observations.error_info_count, 1);
    }

    #[test]
    fn sandbox_validation_rejects_other_directories() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        assert!(matches!(
            validated_sandbox(temporary.path()),
            Err(CodexRecorderError::UnsafeSandbox)
        ));
        Ok(())
    }

    #[test]
    fn sandbox_validation_rejects_an_unrelated_matching_suffix() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("tests/fixtures/sandbox");
        fs::create_dir_all(&sandbox)?;
        assert!(matches!(
            validated_sandbox(&sandbox),
            Err(CodexRecorderError::UnsafeSandbox)
        ));
        Ok(())
    }

    #[test]
    fn sandbox_validation_rejects_a_linked_expected_root() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let target = temporary.path().join("real-project");
        let expected = temporary.path().join("tests/fixtures/sandbox");
        fs::create_dir_all(&target)?;
        fs::create_dir_all(
            expected
                .parent()
                .ok_or("linked sandbox must have a parent")?,
        )?;
        platform::create_test_directory_link(&target, &expected)?;

        assert!(matches!(
            validated_sandbox_against(&target, &expected),
            Err(CodexRecorderError::UnsafeSandbox)
        ));
        Ok(())
    }

    #[test]
    fn sandbox_validation_accepts_only_the_repository_fixture_sandbox() -> Result<(), Box<dyn Error>>
    {
        let expected = expected_fixture_sandbox()?;
        assert_eq!(validated_sandbox(&expected)?, expected);
        Ok(())
    }
}
