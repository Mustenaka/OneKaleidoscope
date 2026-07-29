use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, ExitStatus};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::{json, Value};

use super::super::{
    validate_exact_permission_cwd, validate_permission_argv_as, validate_permission_path,
    PermissionCommand,
};
use super::{AcpError, AcpScenario};
use crate::platform;

// These names and wire shapes are dispatched dynamically so the recorder keeps using its
// schema-validated JSONL pipeline. They are copied from the read-only, pinned
// `schemas/acp/meta.json` and `schemas/acp/schema.json`; no parallel Rust protocol types live here.
pub(super) const FS_READ_TEXT_FILE_METHOD: &str = "fs/read_text_file";
pub(super) const FS_WRITE_TEXT_FILE_METHOD: &str = "fs/write_text_file";
pub(super) const TERMINAL_CREATE_METHOD: &str = "terminal/create";
pub(super) const TERMINAL_OUTPUT_METHOD: &str = "terminal/output";
pub(super) const TERMINAL_RELEASE_METHOD: &str = "terminal/release";
pub(super) const TERMINAL_WAIT_FOR_EXIT_METHOD: &str = "terminal/wait_for_exit";
pub(super) const TERMINAL_KILL_METHOD: &str = "terminal/kill";

const MAX_FILE_BYTES: usize = 1_048_576;
const MAX_TERMINAL_OUTPUT_BYTES: u64 = 1_048_576;
const MAX_ACTIVE_TERMINALS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RpcFailure {
    Internal,
    ResourceNotFound,
}

impl RpcFailure {
    pub(super) const fn code(self) -> i32 {
        match self {
            Self::Internal => -32603,
            Self::ResourceNotFound => -32002,
        }
    }

    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::Internal => "Internal error",
            Self::ResourceNotFound => "Resource not found",
        }
    }
}

#[derive(Debug)]
pub(super) enum ClientMethodOutcome {
    Success(Value),
    Failure(RpcFailure),
}

#[derive(Debug)]
pub(super) struct AcpClientServices {
    sandbox: PathBuf,
    scenario: AcpScenario,
    terminals: Vec<ManagedTerminal>,
    next_terminal_id: u64,
}

impl AcpClientServices {
    pub(super) fn new(sandbox: PathBuf, scenario: AcpScenario) -> Self {
        Self {
            sandbox,
            scenario,
            terminals: Vec::new(),
            next_terminal_id: 1,
        }
    }

    pub(super) fn validates(method: &str) -> bool {
        matches!(
            method,
            FS_READ_TEXT_FILE_METHOD
                | FS_WRITE_TEXT_FILE_METHOD
                | TERMINAL_CREATE_METHOD
                | TERMINAL_OUTPUT_METHOD
                | TERMINAL_RELEASE_METHOD
                | TERMINAL_WAIT_FOR_EXIT_METHOD
                | TERMINAL_KILL_METHOD
        )
    }

    pub(super) fn validate_request(
        &self,
        message: &Value,
        active_session_id: Option<&str>,
    ) -> Result<(), AcpError> {
        if message.get("id").is_none() {
            return Err(AcpError::MessageShape(
                "ACP client method request id is missing",
            ));
        }
        let method =
            message
                .get("method")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "ACP client method request method is missing",
                ))?;
        let params = request_params(message)?;
        validate_session_id(params, active_session_id)?;
        match method {
            FS_READ_TEXT_FILE_METHOD => {
                let path = required_absolute_path(params)?;
                validate_permission_path(&self.sandbox, path)
                    .map_err(|_| AcpError::UnsafeClientMethodScope)?;
                validate_optional_u32(params, "line")?;
                validate_optional_u32(params, "limit")
            }
            FS_WRITE_TEXT_FILE_METHOD => {
                let path = required_absolute_path(params)?;
                validate_permission_path(&self.sandbox, path)
                    .map_err(|_| AcpError::UnsafeClientMethodScope)?;
                let content =
                    params
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or(AcpError::MessageShape(
                            "fs/write_text_file params.content is missing",
                        ))?;
                if content.len() > MAX_FILE_BYTES {
                    return Err(AcpError::UnsafeClientMethodScope);
                }
                Ok(())
            }
            TERMINAL_CREATE_METHOD => self.validate_terminal_create(params),
            TERMINAL_OUTPUT_METHOD
            | TERMINAL_RELEASE_METHOD
            | TERMINAL_WAIT_FOR_EXIT_METHOD
            | TERMINAL_KILL_METHOD => {
                let _ = required_terminal_id(params)?;
                Ok(())
            }
            _ => Err(AcpError::InvalidState(
                "unknown method reached ACP client service validation",
            )),
        }
    }

    pub(super) fn handle(&mut self, message: &Value) -> Result<ClientMethodOutcome, AcpError> {
        let method =
            message
                .get("method")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "ACP client method request method is missing",
                ))?;
        let params = request_params(message)?;
        match method {
            FS_READ_TEXT_FILE_METHOD => self.read_text_file(params),
            FS_WRITE_TEXT_FILE_METHOD => self.write_text_file(params),
            TERMINAL_CREATE_METHOD => self.create_terminal(params),
            TERMINAL_OUTPUT_METHOD => self.terminal_output(params),
            TERMINAL_RELEASE_METHOD => self.release_terminal(params),
            TERMINAL_WAIT_FOR_EXIT_METHOD => self.wait_for_terminal_exit(params),
            TERMINAL_KILL_METHOD => self.kill_terminal(params),
            _ => Err(AcpError::InvalidState(
                "unknown method reached ACP client service dispatch",
            )),
        }
    }

    pub(super) fn stop_all(&mut self) -> Result<(), AcpError> {
        let mut first_error = None;
        for mut terminal in std::mem::take(&mut self.terminals) {
            if let Err(error) = terminal.stop() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn validate_terminal_create(&self, params: &Value) -> Result<(), AcpError> {
        let command =
            params
                .get("command")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "terminal/create params.command is missing",
                ))?;
        if command.is_empty() || command.chars().any(char::is_whitespace) {
            return Err(AcpError::UnsafeClientMethodScope);
        }
        let arguments = optional_string_array(params, "args")?;
        let mut command_line = Vec::with_capacity(arguments.len().saturating_add(1));
        command_line.push(command.to_owned());
        command_line.extend(arguments);
        let expected =
            expected_terminal_command(self.scenario).ok_or(AcpError::UnsafeClientMethodScope)?;
        validate_permission_argv_as(&command_line, expected)
            .map_err(|_| AcpError::UnsafeClientMethodScope)?;

        let environment = optional_environment(params)?;
        if !environment.is_empty() {
            return Err(AcpError::UnsafeClientMethodScope);
        }

        let cwd = optional_string(params, "cwd")?
            .map(PathBuf::from)
            .unwrap_or_else(|| self.sandbox.clone());
        let cwd_text = cwd.to_str().ok_or(AcpError::UnsafeClientMethodScope)?;
        if !cwd.is_absolute() {
            return Err(AcpError::UnsafeClientMethodScope);
        }
        validate_exact_permission_cwd(&self.sandbox, cwd_text)
            .map_err(|_| AcpError::UnsafeClientMethodScope)?;

        if optional_u64(params, "outputByteLimit")?
            .is_some_and(|limit| limit > MAX_TERMINAL_OUTPUT_BYTES)
        {
            return Err(AcpError::UnsafeClientMethodScope);
        }
        Ok(())
    }

    fn read_text_file(&self, params: &Value) -> Result<ClientMethodOutcome, AcpError> {
        let path = Path::new(required_absolute_path(params)?);
        validate_permission_path(&self.sandbox, path.to_string_lossy().as_ref())
            .map_err(|_| AcpError::UnsafeClientMethodScope)?;
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ClientMethodOutcome::Failure(RpcFailure::ResourceNotFound));
            }
            Err(_) => return Ok(ClientMethodOutcome::Failure(RpcFailure::Internal)),
        };
        if content.len() > MAX_FILE_BYTES {
            return Ok(ClientMethodOutcome::Failure(RpcFailure::Internal));
        }
        let line = optional_u32(params, "line")?.unwrap_or(1);
        let limit = optional_u32(params, "limit")?;
        let content = select_lines(&content, line, limit);
        Ok(ClientMethodOutcome::Success(json!({ "content": content })))
    }

    fn write_text_file(&self, params: &Value) -> Result<ClientMethodOutcome, AcpError> {
        let path = Path::new(required_absolute_path(params)?);
        validate_permission_path(&self.sandbox, path.to_string_lossy().as_ref())
            .map_err(|_| AcpError::UnsafeClientMethodScope)?;
        let content =
            params
                .get("content")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "fs/write_text_file params.content is missing",
                ))?;
        match fs::write(path, content) {
            Ok(()) => Ok(ClientMethodOutcome::Success(json!({}))),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(ClientMethodOutcome::Failure(RpcFailure::ResourceNotFound))
            }
            Err(_) => Ok(ClientMethodOutcome::Failure(RpcFailure::Internal)),
        }
    }

    fn create_terminal(&mut self, params: &Value) -> Result<ClientMethodOutcome, AcpError> {
        if self.terminals.len() >= MAX_ACTIVE_TERMINALS {
            return Ok(ClientMethodOutcome::Failure(RpcFailure::Internal));
        }
        let command =
            params
                .get("command")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "terminal/create params.command is missing",
                ))?;
        let arguments = optional_string_array(params, "args")?;
        let arguments = arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let cwd = optional_string(params, "cwd")?
            .map(PathBuf::from)
            .unwrap_or_else(|| self.sandbox.clone());
        let output_limit =
            optional_u64(params, "outputByteLimit")?.unwrap_or(MAX_TERMINAL_OUTPUT_BYTES);
        let output_limit =
            usize::try_from(output_limit).map_err(|_| AcpError::UnsafeClientMethodScope)?;
        let executable = match platform::resolve(OsStr::new(command)) {
            Ok(executable) => executable,
            Err(_) => {
                return Ok(ClientMethodOutcome::Failure(RpcFailure::ResourceNotFound));
            }
        };
        let mut child = platform::spawn_fixture(&executable, &arguments, &cwd)?;
        drop(child.stdin.take());
        let stdout = require_terminal_pipe(
            child.stdout.take(),
            &mut child,
            AcpError::ClientTerminalPipe("stdout"),
        )?;
        let stderr = require_terminal_pipe(
            child.stderr.take(),
            &mut child,
            AcpError::ClientTerminalPipe("stderr"),
        )?;
        let terminal_id = format!("terminal-{}", self.next_terminal_id);
        self.next_terminal_id = self
            .next_terminal_id
            .checked_add(1)
            .ok_or(AcpError::CounterOverflow)?;
        self.terminals.push(ManagedTerminal::new(
            terminal_id.clone(),
            child,
            stdout,
            stderr,
            output_limit,
        ));
        Ok(ClientMethodOutcome::Success(
            json!({ "terminalId": terminal_id }),
        ))
    }

    fn terminal_output(&mut self, params: &Value) -> Result<ClientMethodOutcome, AcpError> {
        let terminal_id = required_terminal_id(params)?;
        let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|terminal| terminal.id == terminal_id)
        else {
            return Ok(ClientMethodOutcome::Failure(RpcFailure::ResourceNotFound));
        };
        let exit_status = terminal.refresh_exit_status()?;
        let (output, truncated) = terminal.output_snapshot()?;
        let mut result = json!({
            "output": output,
            "truncated": truncated
        });
        if let Some(status) = exit_status {
            result
                .as_object_mut()
                .ok_or(AcpError::InvalidState(
                    "terminal output result was not an object",
                ))?
                .insert("exitStatus".to_owned(), exit_status_value(status));
        }
        Ok(ClientMethodOutcome::Success(result))
    }

    fn release_terminal(&mut self, params: &Value) -> Result<ClientMethodOutcome, AcpError> {
        let terminal_id = required_terminal_id(params)?;
        let Some(index) = self
            .terminals
            .iter()
            .position(|terminal| terminal.id == terminal_id)
        else {
            return Ok(ClientMethodOutcome::Failure(RpcFailure::ResourceNotFound));
        };
        let mut terminal = self.terminals.remove(index);
        terminal.stop()?;
        Ok(ClientMethodOutcome::Success(json!({})))
    }

    fn wait_for_terminal_exit(&mut self, params: &Value) -> Result<ClientMethodOutcome, AcpError> {
        let terminal_id = required_terminal_id(params)?;
        let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|terminal| terminal.id == terminal_id)
        else {
            return Ok(ClientMethodOutcome::Failure(RpcFailure::ResourceNotFound));
        };
        let status = terminal.wait_for_exit()?;
        Ok(ClientMethodOutcome::Success(exit_status_value(status)))
    }

    fn kill_terminal(&mut self, params: &Value) -> Result<ClientMethodOutcome, AcpError> {
        let terminal_id = required_terminal_id(params)?;
        let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|terminal| terminal.id == terminal_id)
        else {
            return Ok(ClientMethodOutcome::Failure(RpcFailure::ResourceNotFound));
        };
        terminal.kill()?;
        Ok(ClientMethodOutcome::Success(json!({})))
    }
}

impl Drop for AcpClientServices {
    fn drop(&mut self) {
        let _ = self.stop_all();
    }
}

#[derive(Debug)]
struct ManagedTerminal {
    id: String,
    child: Option<Child>,
    exit_status: Option<ExitStatus>,
    output: Arc<Mutex<CapturedOutput>>,
    readers: Vec<JoinHandle<()>>,
}

impl ManagedTerminal {
    fn new(
        id: String,
        child: Child,
        stdout: ChildStdout,
        stderr: ChildStderr,
        output_limit: usize,
    ) -> Self {
        let output = Arc::new(Mutex::new(CapturedOutput::new(output_limit)));
        let stdout_reader = spawn_output_reader(stdout, Arc::clone(&output));
        let stderr_reader = spawn_output_reader(stderr, Arc::clone(&output));
        Self {
            id,
            child: Some(child),
            exit_status: None,
            output,
            readers: vec![stdout_reader, stderr_reader],
        }
    }

    fn refresh_exit_status(&mut self) -> Result<Option<ExitStatus>, AcpError> {
        if self.exit_status.is_none() {
            let child = self
                .child
                .as_mut()
                .ok_or(AcpError::InvalidState("active terminal child is missing"))?;
            self.exit_status = child.try_wait()?;
            if self.exit_status.is_some() {
                self.join_readers()?;
            }
        }
        Ok(self.exit_status)
    }

    fn wait_for_exit(&mut self) -> Result<ExitStatus, AcpError> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        let child = self
            .child
            .as_mut()
            .ok_or(AcpError::InvalidState("active terminal child is missing"))?;
        let status = child.wait()?;
        self.exit_status = Some(status);
        self.join_readers()?;
        Ok(status)
    }

    fn kill(&mut self) -> Result<(), AcpError> {
        if self.exit_status.is_none() {
            let child = self
                .child
                .as_mut()
                .ok_or(AcpError::InvalidState("active terminal child is missing"))?;
            self.exit_status = Some(platform::terminate_tree(child)?);
        }
        self.join_readers()
    }

    fn stop(&mut self) -> Result<(), AcpError> {
        self.kill()?;
        self.child.take();
        Ok(())
    }

    fn join_readers(&mut self) -> Result<(), AcpError> {
        for reader in std::mem::take(&mut self.readers) {
            if reader.join().is_err() {
                return Err(AcpError::ClientTerminalReaderPanicked);
            }
        }
        Ok(())
    }

    fn output_snapshot(&self) -> Result<(String, bool), AcpError> {
        let output = self
            .output
            .lock()
            .map_err(|_| AcpError::ClientTerminalOutputUnavailable)?;
        if output.failed {
            return Err(AcpError::ClientTerminalOutputUnavailable);
        }
        Ok((output.text.clone(), output.truncated))
    }
}

impl Drop for ManagedTerminal {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Debug)]
struct CapturedOutput {
    text: String,
    limit: usize,
    truncated: bool,
    failed: bool,
}

impl CapturedOutput {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
            truncated: false,
            failed: false,
        }
    }

    fn append(&mut self, bytes: Vec<u8>) {
        let Ok(text) = String::from_utf8(bytes) else {
            self.failed = true;
            return;
        };
        self.text.push_str(&text);
        if self.text.len() <= self.limit {
            return;
        }
        self.truncated = true;
        if self.limit == 0 {
            self.text.clear();
            return;
        }
        let minimum_start = self.text.len().saturating_sub(self.limit);
        let boundary = (minimum_start..=self.text.len())
            .find(|offset| self.text.is_char_boundary(*offset))
            .unwrap_or(self.text.len());
        self.text.drain(..boundary);
    }
}

fn spawn_output_reader<R>(reader: R, output: Arc<Mutex<CapturedOutput>>) -> JoinHandle<()>
where
    R: io::Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut bytes = Vec::new();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => return,
                Ok(_) => {
                    let Ok(mut output) = output.lock() else {
                        return;
                    };
                    output.append(bytes);
                }
                Err(_) => {
                    if let Ok(mut output) = output.lock() {
                        output.failed = true;
                    }
                    return;
                }
            }
        }
    })
}

fn require_terminal_pipe<T>(
    pipe: Option<T>,
    child: &mut Child,
    missing: AcpError,
) -> Result<T, AcpError> {
    match pipe {
        Some(pipe) => Ok(pipe),
        None => match platform::terminate_tree(child) {
            Ok(_) => Err(missing),
            Err(cleanup) => Err(AcpError::ClientTerminalCleanupAfterError {
                source: Box::new(missing),
                cleanup,
            }),
        },
    }
}

fn request_params(message: &Value) -> Result<&Value, AcpError> {
    message
        .get("params")
        .filter(|params| params.is_object())
        .ok_or(AcpError::MessageShape(
            "ACP client method request params is missing",
        ))
}

fn validate_session_id(params: &Value, active_session_id: Option<&str>) -> Result<(), AcpError> {
    let session_id =
        params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or(AcpError::MessageShape(
                "ACP client method params.sessionId is missing",
            ))?;
    if active_session_id == Some(session_id) {
        Ok(())
    } else {
        Err(AcpError::SessionIdMismatch)
    }
}

fn required_absolute_path(params: &Value) -> Result<&str, AcpError> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or(AcpError::MessageShape(
            "ACP filesystem params.path is missing",
        ))?;
    if Path::new(path).is_absolute() {
        Ok(path)
    } else {
        Err(AcpError::UnsafeClientMethodScope)
    }
}

fn required_terminal_id(params: &Value) -> Result<&str, AcpError> {
    params
        .get("terminalId")
        .and_then(Value::as_str)
        .filter(|terminal_id| !terminal_id.is_empty())
        .ok_or(AcpError::MessageShape(
            "ACP terminal params.terminalId is missing",
        ))
}

fn optional_string<'a>(params: &'a Value, name: &str) -> Result<Option<&'a str>, AcpError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(AcpError::MessageShape(
            "ACP client method optional string has the wrong type",
        )),
    }
}

fn optional_u64(params: &Value, name: &str) -> Result<Option<u64>, AcpError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value.as_u64().map(Some).ok_or(AcpError::MessageShape(
            "ACP client method optional integer has the wrong type",
        )),
        Some(_) => Err(AcpError::MessageShape(
            "ACP client method optional integer has the wrong type",
        )),
    }
}

fn optional_u32(params: &Value, name: &str) -> Result<Option<u32>, AcpError> {
    optional_u64(params, name)?
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                AcpError::MessageShape("ACP client method integer exceeds the schema range")
            })
        })
        .transpose()
}

fn validate_optional_u32(params: &Value, name: &str) -> Result<(), AcpError> {
    let _ = optional_u32(params, name)?;
    Ok(())
}

fn optional_string_array(params: &Value, name: &str) -> Result<Vec<String>, AcpError> {
    let Some(value) = params.get(name) else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or(AcpError::MessageShape(
        "ACP client method string array has the wrong type",
    ))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(AcpError::MessageShape(
                    "ACP client method string array contains a non-string",
                ))
        })
        .collect()
}

fn optional_environment(params: &Value) -> Result<Vec<(&str, &str)>, AcpError> {
    let Some(value) = params.get("env") else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or(AcpError::MessageShape(
        "terminal/create params.env has the wrong type",
    ))?;
    entries
        .iter()
        .map(|entry| {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or(AcpError::MessageShape(
                    "terminal/create env.name is missing",
                ))?;
            let value =
                entry
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or(AcpError::MessageShape(
                        "terminal/create env.value is missing",
                    ))?;
            Ok((name, value))
        })
        .collect()
}

fn expected_terminal_command(scenario: AcpScenario) -> Option<PermissionCommand> {
    match scenario {
        AcpScenario::PermissionApprove | AcpScenario::PermissionDeny => {
            Some(PermissionCommand::Run)
        }
        AcpScenario::Cancel => Some(PermissionCommand::Wait),
        AcpScenario::Error => Some(PermissionCommand::Fail),
        AcpScenario::SimpleTurn
        | AcpScenario::ToolCall
        | AcpScenario::FileChange
        | AcpScenario::SessionLoad
        | AcpScenario::Elicitation => None,
    }
}

fn select_lines(content: &str, line: u32, limit: Option<u32>) -> String {
    let start = usize::try_from(line.saturating_sub(1)).unwrap_or(usize::MAX);
    let take = limit
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(usize::MAX);
    content
        .split_inclusive('\n')
        .skip(start)
        .take(take)
        .collect()
}

fn exit_status_value(status: ExitStatus) -> Value {
    match status.code().and_then(|code| u32::try_from(code).ok()) {
        Some(exit_code) => json!({ "exitCode": exit_code }),
        None => json!({ "signal": "terminated" }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::Value;

    use super::{
        select_lines, CapturedOutput, FS_READ_TEXT_FILE_METHOD, FS_WRITE_TEXT_FILE_METHOD,
        TERMINAL_CREATE_METHOD, TERMINAL_KILL_METHOD, TERMINAL_OUTPUT_METHOD,
        TERMINAL_RELEASE_METHOD, TERMINAL_WAIT_FOR_EXIT_METHOD,
    };

    #[test]
    fn method_names_match_the_read_only_pinned_acp_v1_metadata(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("schemas")
            .join("acp")
            .join("meta.json");
        let metadata: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        for (field, expected) in [
            ("fs_read_text_file", FS_READ_TEXT_FILE_METHOD),
            ("fs_write_text_file", FS_WRITE_TEXT_FILE_METHOD),
            ("terminal_create", TERMINAL_CREATE_METHOD),
            ("terminal_output", TERMINAL_OUTPUT_METHOD),
            ("terminal_release", TERMINAL_RELEASE_METHOD),
            ("terminal_wait_for_exit", TERMINAL_WAIT_FOR_EXIT_METHOD),
            ("terminal_kill", TERMINAL_KILL_METHOD),
        ] {
            assert_eq!(
                metadata
                    .pointer(&format!("/clientMethods/{field}"))
                    .and_then(Value::as_str),
                Some(expected)
            );
        }
        Ok(())
    }

    #[test]
    fn line_selection_uses_one_based_offsets_and_honors_zero_limit() {
        let content = "first\nsecond\nthird\n";
        assert_eq!(select_lines(content, 1, None), content);
        assert_eq!(select_lines(content, 2, Some(1)), "second\n");
        assert_eq!(select_lines(content, 0, Some(1)), "first\n");
        assert_eq!(select_lines(content, 9, None), "");
        assert_eq!(select_lines(content, 1, Some(0)), "");
    }

    #[test]
    fn terminal_output_truncates_at_a_utf8_character_boundary() {
        let mut output = CapturedOutput::new(5);
        output.append("aé".as_bytes().to_vec());
        output.append("界z".as_bytes().to_vec());

        assert_eq!(output.text, "界z");
        assert!(output.truncated);
        assert!(output.text.len() <= 5);
    }

    #[test]
    fn invalid_utf8_marks_terminal_output_unavailable() {
        let mut output = CapturedOutput::new(32);
        output.append(vec![0xff]);

        assert!(output.failed);
        assert!(output.text.is_empty());
    }
}
