use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::future::{poll_fn, Future};
use std::io::{self, BufRead, BufReader, Cursor, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use thiserror::Error;

use super::{
    validate_exact_permission_cwd, validate_exact_permission_path, validate_permission_command_as,
    validate_permission_path, CompletedRecording, PermissionCommand, PermissionScopeError,
};
use crate::fixture::{
    http_request_payload, http_response_payload, Direction, FixtureError, FixtureSink, Transport,
};
use crate::platform::{self, ProcessError, ResolvedExecutable};
use crate::sse_tee;

const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
const MAX_START_ATTEMPTS: usize = 3;
const MAX_EVENTS: usize = 500;
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SESSION_LOAD_SEED_TITLE: &str = "KALEIDO SESSION LOAD SEED";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scenario {
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

impl Scenario {
    fn prompt(self) -> Option<&'static str> {
        match self {
            Self::SimpleTurn => {
                Some("Reply with exactly one short sentence: kaleido simple turn completed.")
            }
            Self::ToolCall => Some(
                "Use the file-reading tool to read notes.txt in the current directory, then \
                 summarize it in one sentence. Do not modify any file.",
            ),
            Self::PermissionApprove | Self::PermissionDeny => Some(
                "Use the shell tool to run `cargo run --` in the current directory exactly once. \
                 Do not use any other tool instead.",
            ),
            Self::FileChange => Some(
                "Use the editing tool to replace the complete contents of editable.txt with \
                 exactly `changed by the OpenCode fixture recorder` followed by one newline.",
            ),
            Self::Cancel => Some(
                "Use the shell tool to execute `cargo run -- wait` in the current directory \
                 exactly once, and wait for it to finish.",
            ),
            Self::Error => Some(
                "Use the shell tool to execute `cargo run -- fail` in the current directory \
                 exactly once. Report the failure.",
            ),
            Self::Elicitation => Some(
                "Use the question tool to ask me to choose one color from Red, Green, and Blue. \
                 Do not answer the question yourself.",
            ),
            Self::SessionLoad => None,
        }
    }

    fn session_body(self, sandbox: &Path) -> Result<String, OpenCodeError> {
        let mut permission = vec![
            json!({
                "permission": "*",
                "pattern": "*",
                "action": "deny"
            }),
            json!({
                "permission": "external_directory",
                "pattern": "*",
                "action": "deny"
            }),
        ];
        let exact_rules = match self {
            Self::ToolCall => vec![
                ("read", "notes.txt".to_owned(), "ask"),
                (
                    "read",
                    canonical_permission_pattern(sandbox, "notes.txt")?,
                    "ask",
                ),
            ],
            Self::PermissionApprove | Self::PermissionDeny => {
                vec![("bash", "cargo run --".to_owned(), "ask")]
            }
            Self::FileChange => vec![
                ("edit", "editable.txt".to_owned(), "ask"),
                (
                    "edit",
                    canonical_permission_pattern(sandbox, "editable.txt")?,
                    "ask",
                ),
            ],
            Self::Cancel => vec![("bash", "cargo run -- wait".to_owned(), "ask")],
            Self::Error => vec![("bash", "cargo run -- fail".to_owned(), "ask")],
            Self::Elicitation => vec![("question", "*".to_owned(), "allow")],
            Self::SimpleTurn | Self::SessionLoad => Vec::new(),
        };
        for (permission_name, pattern, action) in exact_rules {
            permission.push(json!({
                "permission": permission_name,
                "pattern": pattern,
                "action": action
            }));
        }
        Ok(json!({
            "title": "OneKaleidoscope T-004 fixture recording",
            "permission": permission
        })
        .to_string())
    }
}

fn canonical_permission_pattern(sandbox: &Path, relative: &str) -> Result<String, OpenCodeError> {
    platform::permission_path_pattern(&sandbox.join(relative)).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            OpenCodeError::NonUtf8SandboxPath
        } else {
            OpenCodeError::ProjectIsolation
        }
    })
}

#[derive(Debug, Eq, PartialEq)]
pub enum Outcome {
    Recorded {
        session_id: Option<String>,
        event_count: usize,
        observations: Vec<String>,
    },
    Unsupported {
        reason: String,
    },
    NotObserved {
        session_id: Option<String>,
        event_count: usize,
        reason: String,
        observations: Vec<String>,
    },
}

#[derive(Debug, Error)]
pub enum OpenCodeError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    #[error("OpenCode HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("OpenCode returned HTTP {status} for {method} {path}")]
    HttpStatus {
        method: &'static str,
        path: String,
        status: StatusCode,
    },
    #[error("OpenCode returned non-UTF-8 response content")]
    NonUtf8Response,
    #[error("invalid OpenCode JSON response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OpenCode response did not have the required shape: {0}")]
    Protocol(&'static str),
    #[error(
        "OpenCode session-load preflight could not prove that every listed session belongs to \
         the fixture sandbox"
    )]
    SessionLoadIsolation,
    #[error("OpenCode session-load preflight found no session titled `KALEIDO SESSION LOAD SEED`")]
    SessionLoadSeedMissing,
    #[error("OpenCode created a session outside the fixture sandbox")]
    SessionCreateIsolation,
    #[error("OpenCode session-load detail did not match the selected sandbox seed")]
    SessionLoadDetail,
    #[error("OpenCode session-load messages were empty or belonged to another session")]
    SessionLoadMessages,
    #[error("OpenCode prompt response did not belong to the sandbox session")]
    PromptResponseIsolation,
    #[error("fixture sandbox does not have a reliable isolated project root")]
    ProjectIsolation,
    #[error("canonical fixture sandbox path is not valid UTF-8")]
    NonUtf8SandboxPath,
    #[error("refusing to record outside the repository tests/fixtures/sandbox")]
    InvalidSandbox,
    #[error(transparent)]
    Sse(#[from] sse_tee::SseError),
    #[error("failed to perform recorder I/O: {0}")]
    Io(#[from] io::Error),
    #[error("OpenCode server exited during startup with {0}")]
    ServerExited(std::process::ExitStatus),
    #[error(
        "OpenCode did not become ready after {attempts} loopback-port attempt(s); \
         a released dynamic port can be claimed by another process: {last_error}"
    )]
    ServerNotReady { attempts: usize, last_error: String },
    #[error("OpenCode prompt worker closed without a response")]
    PromptWorkerClosed,
    #[error("OpenCode recording was interrupted by Ctrl-C")]
    Interrupted,
    #[error("git init for the fixture sandbox exited with {0}")]
    GitInitFailed(ExitStatus),
    #[error("failed to remove the recorder-created fixture .git directory: {0}")]
    TemporaryGitCleanup(#[source] io::Error),
    #[error(
        "OpenCode operation failed ({source}); removing the recorder-created fixture .git \
         directory also failed: {cleanup}"
    )]
    TemporaryGitCleanupAfterError {
        #[source]
        source: Box<OpenCodeError>,
        cleanup: io::Error,
    },
    #[error(
        "OpenCode permission request could not be proven safe inside the canonical fixture sandbox"
    )]
    UnsafePermissionScope,
    #[error("OpenCode operation failed ({source}); server cleanup also failed: {cleanup}")]
    CleanupAfterError {
        #[source]
        source: Box<OpenCodeError>,
        cleanup: ProcessError,
    },
}

#[derive(Clone, Debug, Default)]
struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn check(&self) -> Result<(), OpenCodeError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(OpenCodeError::Interrupted)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitRootOrigin {
    RecorderCreated,
    PreExisting,
}

impl GitRootOrigin {
    const fn diagnostic(self) -> &'static str {
        match self {
            Self::RecorderCreated => {
                "OpenCode fixture project root: .git=recorder-created; cleanup=RAII"
            }
            Self::PreExisting => {
                "OpenCode fixture project root: .git=pre-existing; cleanup=preserved"
            }
        }
    }
}

#[derive(Debug)]
struct TemporaryGitRoot {
    owned_git: Option<PathBuf>,
    origin: GitRootOrigin,
}

impl TemporaryGitRoot {
    fn prepare(sandbox: &Path) -> Result<Self, OpenCodeError> {
        let git = sandbox.join(".git");
        match fs::symlink_metadata(&git) {
            Ok(_) => {
                validate_prompt_project_isolation(sandbox)?;
                let root = Self {
                    owned_git: None,
                    origin: GitRootOrigin::PreExisting,
                };
                eprintln!("kaleido-recorder: {}", root.origin.diagnostic());
                Ok(root)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let root = Self {
                    owned_git: Some(git),
                    origin: GitRootOrigin::RecorderCreated,
                };
                root.initialize(sandbox)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn initialize(self, sandbox: &Path) -> Result<Self, OpenCodeError> {
        let git = platform::resolve(OsStr::new("git")).map_err(ProcessError::from)?;
        let arguments = [
            OsString::from("-c"),
            OsString::from("init.templateDir="),
            OsString::from("init"),
            OsString::from("--quiet"),
            OsString::from("--initial-branch=kaleido-fixture"),
        ];
        let child = platform::spawn_fixture(&git, &arguments, sandbox)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(OpenCodeError::GitInitFailed(output.status));
        }
        validate_prompt_project_isolation(sandbox)?;
        eprintln!("kaleido-recorder: {}", self.origin.diagnostic());
        Ok(self)
    }

    fn finish<T>(mut self, result: Result<T, OpenCodeError>) -> Result<T, OpenCodeError> {
        let cleanup = self.cleanup();
        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(cleanup)) => Err(OpenCodeError::TemporaryGitCleanup(cleanup)),
            (Err(source), Ok(())) => Err(source),
            (Err(source), Err(cleanup)) => Err(OpenCodeError::TemporaryGitCleanupAfterError {
                source: Box::new(source),
                cleanup,
            }),
        }
    }

    fn cleanup(&mut self) -> io::Result<()> {
        let Some(git) = self.owned_git.as_ref() else {
            return Ok(());
        };
        remove_git_path(git)?;
        self.owned_git = None;
        Ok(())
    }
}

impl Drop for TemporaryGitRoot {
    fn drop(&mut self) {
        let _cleanup_result = self.cleanup();
    }
}

fn remove_git_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn start_interrupt_listener(cancellation: CancellationToken) -> Result<(), OpenCodeError> {
    start_interrupt_listener_with(cancellation, tokio::signal::ctrl_c())
}

fn start_interrupt_listener_with<F>(
    cancellation: CancellationToken,
    signal: F,
) -> Result<(), OpenCodeError>
where
    F: Future<Output = io::Result<()>> + Send + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let listener = thread::Builder::new()
        .name("kaleido-opencode-ctrl-c".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                let mut signal = pin!(signal);
                let mut ready_sender = Some(ready_sender);
                let signal_result = poll_fn(|context| {
                    let result = signal.as_mut().poll(context);
                    if let Some(sender) = ready_sender.take() {
                        let registration = match &result {
                            Poll::Ready(Err(error)) => Err(error.kind()),
                            Poll::Pending | Poll::Ready(Ok(())) => Ok(()),
                        };
                        let _send_result = sender.send(registration);
                    }
                    result
                })
                .await;
                if signal_result.is_ok() {
                    cancellation.cancel();
                }
            });
        })?;
    drop(listener);
    match ready_receiver.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(kind)) => {
            Err(io::Error::new(kind, "could not register the OpenCode Ctrl-C listener").into())
        }
        Err(_) => {
            Err(io::Error::other("OpenCode Ctrl-C listener stopped before registering").into())
        }
    }
}

pub fn record<W: Write>(
    executable: &ResolvedExecutable,
    sandbox: &Path,
    scenario: Scenario,
    fixture: &mut FixtureSink<W>,
    timeout: Duration,
) -> Result<CompletedRecording<Outcome>, OpenCodeError> {
    let sandbox = validate_fixture_sandbox(sandbox)?;
    let cancellation = CancellationToken::default();
    start_interrupt_listener(cancellation.clone())?;
    cancellation.check()?;
    let temporary_git = TemporaryGitRoot::prepare(&sandbox)?;
    cancellation.check()?;
    let client = Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(timeout)
        .build()?;
    let mut server = start_server(executable, &sandbox, &client, timeout, &cancellation)?;
    let result = validate_current_project_root(&client, server.base_url(), &sandbox, &cancellation)
        .and_then(|()| {
            if scenario == Scenario::SessionLoad {
                record_session_load(&client, server.base_url(), &sandbox, fixture, &cancellation)
            } else {
                record_prompt_scenario(
                    &client,
                    server.base_url(),
                    scenario,
                    &sandbox,
                    fixture,
                    timeout,
                    &cancellation,
                )
            }
        });
    let result = finish_recording(result, || server.stop());
    temporary_git.finish(result)
}

fn finish_recording(
    result: Result<Outcome, OpenCodeError>,
    cleanup: impl FnOnce() -> Result<(), ProcessError>,
) -> Result<CompletedRecording<Outcome>, OpenCodeError> {
    let cleanup = cleanup();
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => Err(OpenCodeError::CleanupAfterError {
            source: Box::new(error),
            cleanup,
        }),
        (Err(error), Ok(())) => Err(error),
        (Ok(outcome), cleanup) => Ok(CompletedRecording::with_cleanup_result(outcome, cleanup)),
    }
}

struct Server {
    child: Option<Child>,
    base_url: String,
}

impl Server {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn stop(&mut self) -> Result<(), ProcessError> {
        if let Some(mut child) = self.child.take() {
            match platform::terminate_tree(&mut child) {
                Ok(_) => {}
                Err(error) => {
                    self.child = Some(child);
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = platform::terminate_tree(&mut child);
        }
    }
}

fn start_server(
    executable: &ResolvedExecutable,
    sandbox: &Path,
    client: &Client,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<Server, OpenCodeError> {
    retry_server_startup(
        MAX_START_ATTEMPTS,
        |_| {
            cancellation.check()?;
            let port = reserve_then_release_port()?;
            let arguments = [
                "serve",
                "--pure",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--log-level",
                "ERROR",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
            let mut child = platform::spawn_fixture(executable, &arguments, sandbox)?;
            drain_child_output(&mut child);
            let base_url = format!("http://127.0.0.1:{port}");
            match wait_until_ready(&mut child, client, &base_url, timeout, cancellation) {
                Ok(()) => Ok(StartupAttempt::Ready(Server {
                    child: Some(child),
                    base_url,
                })),
                Err(error) => Ok(StartupAttempt::Retry { error, child }),
            }
        },
        |child| platform::terminate_tree(child).map(drop),
    )
}

enum StartupAttempt<T, C> {
    Ready(T),
    Retry { error: OpenCodeError, child: C },
}

fn retry_server_startup<T, C>(
    max_attempts: usize,
    mut attempt: impl FnMut(usize) -> Result<StartupAttempt<T, C>, OpenCodeError>,
    mut cleanup: impl FnMut(&mut C) -> Result<(), ProcessError>,
) -> Result<T, OpenCodeError> {
    let mut last_error = String::from("no startup attempt was made");
    for attempt_number in 1..=max_attempts {
        match attempt(attempt_number)? {
            StartupAttempt::Ready(value) => return Ok(value),
            StartupAttempt::Retry { error, mut child } => {
                last_error = error.to_string();
                if let Err(cleanup) = cleanup(&mut child) {
                    return Err(OpenCodeError::CleanupAfterError {
                        source: Box::new(error),
                        cleanup,
                    });
                }
            }
        }
    }
    Err(OpenCodeError::ServerNotReady {
        attempts: max_attempts,
        last_error,
    })
}

fn reserve_then_release_port() -> io::Result<u16> {
    let listener = TcpListener::bind(SocketAddrV4::new(LOOPBACK, 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn drain_child_output(child: &mut Child) {
    if let Some(stdout) = child.stdout.take() {
        let _stdout_drain = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let _ = io::copy(&mut reader, &mut io::sink());
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let _stderr_drain = thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let _ = io::copy(&mut reader, &mut io::sink());
        });
    }
}

fn wait_until_ready(
    child: &mut Child,
    client: &Client,
    base_url: &str,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<(), OpenCodeError> {
    let deadline = Instant::now() + timeout.min(Duration::from_secs(15));
    let health_url = format!("{base_url}/global/health");
    let mut last_error = String::from("health endpoint did not respond");
    while Instant::now() < deadline {
        cancellation.check()?;
        if let Some(status) = child.try_wait()? {
            return Err(OpenCodeError::ServerExited(status));
        }
        match client
            .get(&health_url)
            .timeout(Duration::from_secs(1))
            .send()
        {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                last_error = format!("health endpoint returned {}", response.status());
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    cancellation.check()?;
    Err(OpenCodeError::ServerNotReady {
        attempts: 1,
        last_error,
    })
}

fn validate_current_project_root(
    client: &Client,
    base_url: &str,
    sandbox: &Path,
    cancellation: &CancellationToken,
) -> Result<(), OpenCodeError> {
    cancellation.check()?;
    let response =
        request_json_unrecorded(client, base_url, Method::GET, "/project/current", None)?;
    cancellation.check()?;
    let project: Value = serde_json::from_str(&response.body)?;
    if canonical_json_path(project.get("worktree"), sandbox) {
        Ok(())
    } else {
        Err(OpenCodeError::ProjectIsolation)
    }
}

fn record_session_load<W: Write>(
    client: &Client,
    base_url: &str,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
    cancellation: &CancellationToken,
) -> Result<Outcome, OpenCodeError> {
    cancellation.check()?;
    let (sessions, sessions_value) = fetch_session_list_unrecorded(client, base_url)?;
    cancellation.check()?;
    let session_id = validate_session_load_preflight(&sessions_value, sandbox)?.to_owned();
    record_session_list_exchange(&sessions, fixture)?;
    let session_path = format!("/session/{session_id}");
    let detail = request_json_unrecorded(client, base_url, Method::GET, &session_path, None)?;
    cancellation.check()?;
    let detail_value: Value = serde_json::from_str(&detail.body)?;
    validate_session_load_detail(&detail_value, &session_id, sandbox)?;
    record_json_exchange(Method::GET, &session_path, None, &detail, fixture)?;
    let messages_path = format!("/session/{session_id}/message");
    let messages = request_json_unrecorded(client, base_url, Method::GET, &messages_path, None)?;
    cancellation.check()?;
    let messages_value: Value = serde_json::from_str(&messages.body)?;
    validate_session_load_messages(&messages_value, &session_id)?;
    record_json_exchange(Method::GET, &messages_path, None, &messages, fixture)?;
    Ok(Outcome::Recorded {
        session_id: Some(session_id),
        event_count: 0,
        observations: vec![
            "session.list".to_owned(),
            "session.get".to_owned(),
            "session.messages".to_owned(),
        ],
    })
}

fn fetch_session_list_unrecorded(
    client: &Client,
    base_url: &str,
) -> Result<(RawResponse, Value), OpenCodeError> {
    let response = request_json_unrecorded(client, base_url, Method::GET, "/session", None)?;
    let sessions = serde_json::from_str(&response.body)?;
    Ok((response, sessions))
}

fn validate_session_load_detail(
    detail: &Value,
    session_id: &str,
    sandbox: &Path,
) -> Result<(), OpenCodeError> {
    let matches = detail.get("id").and_then(Value::as_str) == Some(session_id)
        && detail.get("title").and_then(Value::as_str) == Some(SESSION_LOAD_SEED_TITLE)
        && canonical_json_path(detail.get("directory"), sandbox);
    if matches {
        Ok(())
    } else {
        Err(OpenCodeError::SessionLoadDetail)
    }
}

fn validate_session_load_messages(messages: &Value, session_id: &str) -> Result<(), OpenCodeError> {
    let valid = messages.as_array().is_some_and(|messages| {
        !messages.is_empty()
            && messages.iter().all(|message| {
                message.pointer("/info/sessionID").and_then(Value::as_str) == Some(session_id)
            })
    });
    if valid {
        Ok(())
    } else {
        Err(OpenCodeError::SessionLoadMessages)
    }
}

fn record_session_list_exchange<W: Write>(
    response: &RawResponse,
    fixture: &mut FixtureSink<W>,
) -> Result<(), OpenCodeError> {
    let path = "/session";
    let request_payload = http_request_payload("GET", path, "", "null")?;
    fixture.record(Direction::C2s, Transport::Http, &request_payload)?;
    record_raw_response("GET", path, response, fixture)
}

fn validate_session_load_preflight<'a>(
    sessions: &'a Value,
    sandbox: &Path,
) -> Result<&'a str, OpenCodeError> {
    let sessions = sessions.as_array().ok_or(OpenCodeError::Protocol(
        "GET /session response must be an array",
    ))?;
    let mut seed = None;
    for session in sessions {
        let directory = session
            .get("directory")
            .and_then(Value::as_str)
            .ok_or(OpenCodeError::SessionLoadIsolation)?;
        let canonical_directory = Path::new(directory)
            .canonicalize()
            .map_err(|_| OpenCodeError::SessionLoadIsolation)?;
        if canonical_directory != sandbox {
            return Err(OpenCodeError::SessionLoadIsolation);
        }
        if session.get("title").and_then(Value::as_str) == Some(SESSION_LOAD_SEED_TITLE)
            && seed.is_none()
        {
            seed = Some(required_string(session, "id")?);
        }
    }
    seed.ok_or(OpenCodeError::SessionLoadSeedMissing)
}

fn create_session<W: Write>(
    client: &Client,
    base_url: &str,
    scenario: Scenario,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<Value, OpenCodeError> {
    let body = scenario.session_body(sandbox)?;
    let response =
        request_json_unrecorded(client, base_url, Method::POST, "/session", Some(&body))?;
    let session: Value = serde_json::from_str(&response.body)?;
    if !canonical_json_path(session.get("directory"), sandbox)
        || session.get("id").and_then(Value::as_str).is_none()
    {
        return Err(OpenCodeError::SessionCreateIsolation);
    }
    record_json_exchange(Method::POST, "/session", Some(&body), &response, fixture)?;
    Ok(session)
}

fn validate_prompt_response(
    response: &Value,
    session_id: &str,
    sandbox: &Path,
) -> Result<(), OpenCodeError> {
    let valid = response.pointer("/info/sessionID").and_then(Value::as_str) == Some(session_id)
        && canonical_json_path(response.pointer("/info/path/cwd"), sandbox)
        && canonical_json_path(response.pointer("/info/path/root"), sandbox);
    if valid {
        Ok(())
    } else {
        Err(OpenCodeError::PromptResponseIsolation)
    }
}

fn validate_and_record_prompt_response<W: Write>(
    response: &RawResponse,
    prompt_path: &str,
    session_id: &str,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<Value, OpenCodeError> {
    let value: Value = serde_json::from_str(&response.body)?;
    validate_prompt_response(&value, session_id, sandbox)?;
    record_raw_response("POST", prompt_path, response, fixture)?;
    Ok(value)
}

fn record_prompt_scenario<W: Write>(
    client: &Client,
    base_url: &str,
    scenario: Scenario,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<Outcome, OpenCodeError> {
    cancellation.check()?;
    let editable_path = sandbox.join("editable.txt");
    let editable_before = (scenario == Scenario::FileChange)
        .then(|| fs::read(&editable_path))
        .transpose()?;
    let session_value = create_session(client, base_url, scenario, sandbox, fixture)?;
    let session_id = required_string(&session_value, "id")?.to_owned();

    let event_path = "/event";
    let request_payload = http_request_payload("GET", event_path, "", "null")?;
    fixture.record(Direction::C2s, Transport::Http, &request_payload)?;
    let event_response = client
        .get(format!("{base_url}{event_path}"))
        .header(ACCEPT, "text/event-stream")
        .send()?;
    if !event_response.status().is_success() {
        let status = event_response.status();
        record_http_response("GET", event_path, event_response, fixture)?;
        return Err(OpenCodeError::HttpStatus {
            method: "GET",
            path: event_path.to_owned(),
            status,
        });
    }

    let prompt = scenario.prompt().ok_or(OpenCodeError::Protocol(
        "prompt scenario did not have a prompt",
    ))?;
    let prompt_path = format!("/session/{session_id}/message");
    let prompt_body = json!({"parts": [{"type": "text", "text": prompt}]}).to_string();
    let prompt_request =
        http_request_payload("POST", &prompt_path, "application/json", &prompt_body)?;
    fixture.record(Direction::C2s, Transport::Http, &prompt_request)?;
    let prompt_worker = spawn_prompt_worker(
        client.clone(),
        format!("{base_url}{prompt_path}"),
        prompt_body,
    );

    let event_receiver = spawn_sse_reader(event_response);
    let mut state = ObservationState::default();
    let deadline = Instant::now() + timeout;
    while state.event_count < MAX_EVENTS {
        let frame = match recv_with_cancellation(&event_receiver, deadline, cancellation)? {
            CancellableReceive::Item(SseReaderEvent::Frame(frame)) => frame,
            CancellableReceive::Item(SseReaderEvent::Closed)
            | CancellableReceive::TimedOut
            | CancellableReceive::Disconnected => break,
            CancellableReceive::Item(SseReaderEvent::Error(error)) if is_timeout(&error) => break,
            CancellableReceive::Item(SseReaderEvent::Error(error)) => return Err(error.into()),
        };
        cancellation.check()?;
        let Some(event) =
            record_current_session_frame(&frame, &session_id, scenario, sandbox, fixture)?
        else {
            continue;
        };
        state.event_count += 1;
        observe_event(&event, &mut state);
        respond_to_control_event(
            ControlRequestContext {
                client,
                base_url,
                scenario,
                session_id: &session_id,
                sandbox,
            },
            &event,
            fixture,
            &mut state,
        )?;
        if state.is_complete(scenario)
            || (scenario == Scenario::FileChange
                && state.permission_target_has_terminal(ToolTerminal::Succeeded)
                && state.file_diff_evidence
                && state.idle)
        {
            break;
        }
    }

    let prompt_response = match recv_with_cancellation(&prompt_worker, deadline, cancellation)? {
        CancellableReceive::Item(response) => response?,
        CancellableReceive::TimedOut => {
            return Ok(prompt_timeout_outcome(state, session_id));
        }
        CancellableReceive::Disconnected => return Err(OpenCodeError::PromptWorkerClosed),
    };
    if !prompt_response.status.is_success() {
        return Err(OpenCodeError::HttpStatus {
            method: "POST",
            path: prompt_path,
            status: prompt_response.status,
        });
    }
    let prompt_value = validate_and_record_prompt_response(
        &prompt_response,
        &prompt_path,
        &session_id,
        sandbox,
        fixture,
    )?;
    if scenario == Scenario::Cancel {
        observe_prompt_abort(&prompt_value, &mut state);
    }

    if scenario == Scenario::FileChange {
        let diff_path = format!("/session/{session_id}/diff");
        let diff = send_json(client, base_url, Method::GET, &diff_path, None, fixture)?;
        let diff_value: Value = serde_json::from_str(&diff.body)?;
        if diff_has_actual_changes(&diff_value) {
            state.file_diff_evidence = true;
            state.add("session.diff");
        }
        state.file_changed_on_disk =
            file_changed_since(editable_before.as_deref(), &editable_path)?;
        if state.file_changed_on_disk {
            state.add("editable.txt.changed");
        }
    }

    Ok(state.outcome(scenario, session_id))
}

enum CancellableReceive<T> {
    Item(T),
    TimedOut,
    Disconnected,
}

fn recv_with_cancellation<T>(
    receiver: &Receiver<T>,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<CancellableReceive<T>, OpenCodeError> {
    loop {
        cancellation.check()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(CancellableReceive::TimedOut);
        }
        match receiver.recv_timeout(remaining.min(INTERRUPT_POLL_INTERVAL)) {
            Ok(value) => return Ok(CancellableReceive::Item(value)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Ok(CancellableReceive::Disconnected);
            }
        }
    }
}

fn prompt_timeout_outcome(state: ObservationState, session_id: String) -> Outcome {
    state.not_observed(
        session_id,
        "the prompt POST timed out before its sandbox session response could be validated",
    )
}

#[derive(Debug)]
struct RawResponse {
    status: StatusCode,
    content_type: String,
    body: String,
}

fn spawn_prompt_worker(
    client: Client,
    url: String,
    body: String,
) -> Receiver<Result<RawResponse, OpenCodeError>> {
    let (sender, receiver) = mpsc::channel();
    drop(thread::spawn(move || {
        let result = client
            .post(url)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .map_err(OpenCodeError::from)
            .and_then(raw_response);
        let _ = sender.send(result);
    }));
    receiver
}

enum SseReaderEvent {
    Frame(SseFrame),
    Error(io::Error),
    Closed,
}

fn spawn_sse_reader(response: Response) -> Receiver<SseReaderEvent> {
    let (sender, receiver) = mpsc::channel();
    drop(thread::spawn(move || {
        let mut stream = BufReader::new(response);
        loop {
            let event = match read_sse_frame(&mut stream) {
                Ok(Some(frame)) => SseReaderEvent::Frame(frame),
                Ok(None) => SseReaderEvent::Closed,
                Err(error) => SseReaderEvent::Error(error),
            };
            let terminal = matches!(event, SseReaderEvent::Error(_) | SseReaderEvent::Closed);
            if sender.send(event).is_err() || terminal {
                return;
            }
        }
    }));
    receiver
}

#[derive(Clone, Copy)]
struct ControlRequestContext<'a> {
    client: &'a Client,
    base_url: &'a str,
    scenario: Scenario,
    session_id: &'a str,
    sandbox: &'a Path,
}

fn respond_to_control_event<W: Write>(
    context: ControlRequestContext<'_>,
    event: &Value,
    fixture: &mut FixtureSink<W>,
    state: &mut ObservationState,
) -> Result<(), OpenCodeError> {
    let event_type = event.get("type").and_then(Value::as_str);
    match (context.scenario, event_type) {
        (
            Scenario::ToolCall
            | Scenario::PermissionApprove
            | Scenario::PermissionDeny
            | Scenario::FileChange
            | Scenario::Cancel
            | Scenario::Error,
            Some("permission.asked" | "permission.v2.asked"),
        ) => {
            validate_opencode_permission_scope(event, context.sandbox, context.scenario)
                .map_err(|_| OpenCodeError::UnsafePermissionScope)?;
            let Some((protocol, request_id)) = state.permission_reply_target(event) else {
                return Ok(());
            };
            let reply = if context.scenario == Scenario::PermissionDeny {
                "reject"
            } else {
                "once"
            };
            let path = match protocol {
                PermissionProtocol::Legacy => format!("/permission/{request_id}/reply"),
                PermissionProtocol::V2 => {
                    format!(
                        "/api/session/{}/permission/{request_id}/reply",
                        context.session_id
                    )
                }
            };
            let response = send_json(
                context.client,
                context.base_url,
                Method::POST,
                &path,
                Some(json!({"reply": reply}).to_string()),
                fixture,
            )?;
            if protocol == PermissionProtocol::Legacy {
                let accepted: Value = serde_json::from_str(&response.body)?;
                if accepted.as_bool() != Some(true) {
                    return Err(OpenCodeError::Protocol(
                        "legacy permission reply endpoint did not accept the reply",
                    ));
                }
            }
            state.mark_permission_reply_sent(&request_id, reply);
        }
        (Scenario::Cancel, _) if state.prepare_cancel_target() => {
            let path = format!("/session/{}/abort", context.session_id);
            let response = send_json(
                context.client,
                context.base_url,
                Method::POST,
                &path,
                None,
                fixture,
            )?;
            let accepted: Value = serde_json::from_str(&response.body)?;
            state.abort_sent = accepted.as_bool() == Some(true);
            if state.abort_sent {
                state.add("session.abort.accepted");
            }
        }
        (Scenario::Elicitation, Some("question.asked" | "question.v2.asked"))
            if !state.question_replied =>
        {
            let Some(request_id) = event.pointer("/properties/id").and_then(Value::as_str) else {
                return Ok(());
            };
            let Some(answer) = first_question_option(event) else {
                return Ok(());
            };
            let path = if event_type == Some("question.v2.asked") {
                format!(
                    "/api/session/{}/question/{request_id}/reply",
                    context.session_id
                )
            } else {
                format!("/question/{request_id}/reply")
            };
            let response = send_json(
                context.client,
                context.base_url,
                Method::POST,
                &path,
                Some(json!({"answers": [[answer]]}).to_string()),
                fixture,
            )?;
            if event_type == Some("question.asked") {
                let accepted: Value = serde_json::from_str(&response.body)?;
                if accepted.as_bool() != Some(true) {
                    return Err(OpenCodeError::Protocol(
                        "legacy question reply endpoint did not accept the reply",
                    ));
                }
            }
            state.question_replied = true;
            state.add("question.reply");
        }
        _ => {}
    }
    Ok(())
}

fn first_question_option(event: &Value) -> Option<&str> {
    event
        .pointer("/properties/questions/0/options/0/label")
        .and_then(Value::as_str)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolTerminal {
    Succeeded,
    Failed,
}

#[derive(Debug)]
struct ToolLifecycle {
    message_id: String,
    part_id: Option<String>,
    updated: bool,
    terminal: Option<ToolTerminal>,
    cancelled: bool,
}

impl ToolLifecycle {
    fn new(message_id: &str, part_id: Option<&str>) -> Self {
        Self {
            message_id: message_id.to_owned(),
            part_id: part_id.map(str::to_owned),
            updated: false,
            terminal: None,
            cancelled: false,
        }
    }

    fn accepts(&mut self, message_id: &str, part_id: Option<&str>) -> bool {
        if self.message_id != message_id {
            return false;
        }
        match (self.part_id.as_deref(), part_id) {
            (Some(existing), Some(incoming)) => existing == incoming,
            (None, Some(incoming)) => {
                self.part_id = Some(incoming.to_owned());
                true
            }
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PermissionProtocol {
    Legacy,
    V2,
}

#[derive(Debug)]
struct PermissionFlow {
    protocol: PermissionProtocol,
    request_id: String,
    message_id: String,
    call_id: String,
    expected_reply: Option<String>,
    reply_sent: bool,
    reply_confirmed: bool,
}

#[derive(Debug, Default)]
struct ObservationState {
    event_count: usize,
    idle: bool,
    assistant_message_ids: BTreeSet<String>,
    assistant_text_ids: BTreeSet<String>,
    tool_calls: BTreeMap<String, ToolLifecycle>,
    permission: Option<PermissionFlow>,
    file_diff_evidence: bool,
    file_changed_on_disk: bool,
    cancel_target_call_id: Option<String>,
    abort_sent: bool,
    question_asked: bool,
    question_replied: bool,
    unverified_tool_event: bool,
    unverified_permission_event: bool,
    observations: Vec<String>,
}

impl ObservationState {
    fn add(&mut self, observation: &str) {
        if !self
            .observations
            .iter()
            .any(|existing| existing == observation)
        {
            self.observations.push(observation.to_owned());
        }
    }

    fn observe_tool_start(
        &mut self,
        call_id: &str,
        message_id: &str,
        part_id: Option<&str>,
    ) -> bool {
        if let Some(lifecycle) = self.tool_calls.get_mut(call_id) {
            return lifecycle.terminal.is_none() && lifecycle.accepts(message_id, part_id);
        }
        self.tool_calls
            .insert(call_id.to_owned(), ToolLifecycle::new(message_id, part_id));
        true
    }

    fn observe_tool_terminal(
        &mut self,
        call_id: &str,
        message_id: &str,
        part_id: Option<&str>,
        terminal: ToolTerminal,
    ) -> bool {
        let Some(lifecycle) = self.tool_calls.get_mut(call_id) else {
            return false;
        };
        if lifecycle.terminal.is_some() || !lifecycle.accepts(message_id, part_id) {
            return false;
        }
        lifecycle.terminal = Some(terminal);
        true
    }

    fn observe_tool_update(
        &mut self,
        call_id: &str,
        message_id: &str,
        part_id: Option<&str>,
    ) -> bool {
        let Some(lifecycle) = self.tool_calls.get_mut(call_id) else {
            return false;
        };
        if lifecycle.terminal.is_some() || !lifecycle.accepts(message_id, part_id) {
            return false;
        }
        lifecycle.updated = true;
        true
    }

    fn observe_permission_asked(
        &mut self,
        protocol: PermissionProtocol,
        request_id: &str,
        message_id: &str,
        call_id: &str,
    ) -> bool {
        let valid_target = self.tool_calls.get(call_id).is_some_and(|lifecycle| {
            lifecycle.message_id == message_id && lifecycle.terminal.is_none()
        });
        if !valid_target {
            return false;
        }
        match &self.permission {
            Some(existing) => {
                existing.protocol == protocol
                    && existing.request_id == request_id
                    && existing.message_id == message_id
                    && existing.call_id == call_id
            }
            None => {
                self.permission = Some(PermissionFlow {
                    protocol,
                    request_id: request_id.to_owned(),
                    message_id: message_id.to_owned(),
                    call_id: call_id.to_owned(),
                    expected_reply: None,
                    reply_sent: false,
                    reply_confirmed: false,
                });
                true
            }
        }
    }

    fn permission_reply_target(&self, event: &Value) -> Option<(PermissionProtocol, String)> {
        let event_type = event.get("type").and_then(Value::as_str)?;
        let request_id = event.pointer("/properties/id").and_then(Value::as_str)?;
        let flow = self.permission.as_ref()?;
        let event_protocol = permission_protocol(event_type)?;
        (flow.protocol == event_protocol && flow.request_id == request_id && !flow.reply_sent)
            .then(|| (flow.protocol, flow.request_id.clone()))
    }

    fn mark_permission_reply_sent(&mut self, request_id: &str, reply: &str) {
        let Some(flow) = self.permission.as_mut() else {
            return;
        };
        if flow.request_id != request_id || flow.reply_sent {
            return;
        }
        flow.expected_reply = Some(reply.to_owned());
        flow.reply_sent = true;
        self.add(if reply == "once" {
            "permission.reply.once.sent"
        } else {
            "permission.reply.reject.sent"
        });
    }

    fn observe_permission_replied(
        &mut self,
        protocol: PermissionProtocol,
        request_id: &str,
        reply: &str,
    ) -> bool {
        let Some(flow) = self.permission.as_mut() else {
            return false;
        };
        let confirmed = flow.protocol == protocol
            && flow.request_id == request_id
            && flow.reply_sent
            && flow.expected_reply.as_deref() == Some(reply);
        if confirmed {
            flow.reply_confirmed = true;
        }
        confirmed
    }

    fn permission_target_has_terminal(&self, terminal: ToolTerminal) -> bool {
        let Some(flow) = &self.permission else {
            return false;
        };
        !self.unverified_tool_event
            && !self.unverified_permission_event
            && self.tool_calls.len() == 1
            && self.tool_calls.contains_key(&flow.call_id)
            && flow.reply_confirmed
            && self.tool_calls.get(&flow.call_id).is_some_and(|lifecycle| {
                lifecycle.message_id == flow.message_id
                    && lifecycle.updated
                    && lifecycle.terminal == Some(terminal)
            })
    }

    fn permission_target_has_terminal_and_text(&self, terminal: ToolTerminal) -> bool {
        let Some(flow) = &self.permission else {
            return false;
        };
        self.permission_target_has_terminal(terminal)
            && self.assistant_text_ids.contains(&flow.message_id)
    }

    fn prepare_cancel_target(&mut self) -> bool {
        if self.abort_sent {
            return false;
        }
        if self.cancel_target_call_id.is_none() {
            self.cancel_target_call_id = self
                .tool_calls
                .iter()
                .find(|(_, lifecycle)| lifecycle.updated && lifecycle.terminal.is_none())
                .map(|(call_id, _)| call_id.clone());
        }
        self.cancel_target_call_id.is_some()
    }

    fn mark_cancelled_message(&mut self, message_id: &str) -> bool {
        if !self.abort_sent {
            return false;
        }
        let Some(call_id) = self.cancel_target_call_id.as_deref() else {
            return false;
        };
        let Some(lifecycle) = self.tool_calls.get_mut(call_id) else {
            return false;
        };
        if lifecycle.message_id != message_id {
            return false;
        }
        lifecycle.cancelled = true;
        true
    }

    fn mark_cancelled_tool(&mut self, call_id: &str, detail: Option<&str>) -> bool {
        let cancelled_detail = detail.is_some_and(|detail| {
            let detail = detail.to_ascii_lowercase();
            detail.contains("abort") || detail.contains("cancel")
        });
        if !self.abort_sent
            || self.cancel_target_call_id.as_deref() != Some(call_id)
            || !cancelled_detail
        {
            return false;
        }
        let Some(lifecycle) = self.tool_calls.get_mut(call_id) else {
            return false;
        };
        lifecycle.cancelled = true;
        true
    }

    fn cancel_observed(&self) -> bool {
        let Some(flow) = &self.permission else {
            return false;
        };
        self.abort_sent
            && self.tool_calls.len() == 1
            && self.tool_calls.contains_key(&flow.call_id)
            && flow.reply_confirmed
            && flow.expected_reply.as_deref() == Some("once")
            && self.cancel_target_call_id.as_deref() == Some(flow.call_id.as_str())
            && self.tool_calls.get(&flow.call_id).is_some_and(|lifecycle| {
                lifecycle.message_id == flow.message_id && lifecycle.updated && lifecycle.cancelled
            })
    }

    fn is_complete(&self, scenario: Scenario) -> bool {
        if self.unverified_tool_event || self.unverified_permission_event {
            return false;
        }
        match scenario {
            Scenario::SimpleTurn => {
                self.tool_calls.is_empty() && !self.assistant_text_ids.is_empty() && self.idle
            }
            Scenario::ToolCall => {
                self.permission_target_has_terminal_and_text(ToolTerminal::Succeeded) && self.idle
            }
            Scenario::PermissionApprove => {
                self.permission_target_has_terminal(ToolTerminal::Succeeded) && self.idle
            }
            Scenario::PermissionDeny => {
                self.permission_target_has_terminal(ToolTerminal::Failed) && self.idle
            }
            Scenario::FileChange => {
                self.permission_target_has_terminal(ToolTerminal::Succeeded)
                    && self.file_diff_evidence
                    && self.file_changed_on_disk
                    && self.idle
            }
            Scenario::Cancel => self.cancel_observed(),
            Scenario::Error => {
                self.permission_target_has_terminal(ToolTerminal::Failed) && self.idle
            }
            Scenario::Elicitation => {
                self.tool_calls.is_empty()
                    && self.permission.is_none()
                    && self.question_asked
                    && self.question_replied
                    && self.idle
            }
            Scenario::SessionLoad => false,
        }
    }

    fn outcome(self, scenario: Scenario, session_id: String) -> Outcome {
        if self.is_complete(scenario) {
            return Outcome::Recorded {
                session_id: Some(session_id),
                event_count: self.event_count,
                observations: self.observations,
            };
        }
        Outcome::NotObserved {
            session_id: Some(session_id),
            event_count: self.event_count,
            reason: missing_observation_reason(scenario, &self),
            observations: self.observations,
        }
    }

    fn not_observed(self, session_id: String, reason: &str) -> Outcome {
        Outcome::NotObserved {
            session_id: Some(session_id),
            event_count: self.event_count,
            reason: reason.to_owned(),
            observations: self.observations,
        }
    }
}

fn observe_event(event: &Value, state: &mut ObservationState) {
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return;
    };
    match event_type {
        "session.idle" => {
            state.idle = true;
            state.add("session.idle");
        }
        "session.status"
            if event
                .pointer("/properties/status/type")
                .and_then(Value::as_str)
                == Some("idle") =>
        {
            state.idle = true;
            state.add("session.status.idle");
        }
        "session.status"
            if event
                .pointer("/properties/status/type")
                .and_then(Value::as_str)
                == Some("busy") =>
        {
            state.idle = false;
            state.add("session.status.busy");
        }
        "message.updated"
            if event
                .pointer("/properties/info/role")
                .and_then(Value::as_str)
                == Some("assistant") =>
        {
            let message_id = event.pointer("/properties/info/id").and_then(Value::as_str);
            if event
                .pointer("/properties/info/error/name")
                .and_then(Value::as_str)
                == Some("MessageAbortedError")
                && message_id.is_some_and(|message_id| state.mark_cancelled_message(message_id))
            {
                state.add("message.aborted.correlated");
            }
            if let Some(message_id) = message_id {
                state.assistant_message_ids.insert(message_id.to_owned());
            }
            state.idle = false;
            state.add("message.updated.assistant");
        }
        "message.part.updated" => {
            let part_type = event
                .pointer("/properties/part/type")
                .and_then(Value::as_str);
            let message_id = event
                .pointer("/properties/part/messageID")
                .and_then(Value::as_str);
            let belongs_to_assistant = message_id
                .is_some_and(|message_id| state.assistant_message_ids.contains(message_id));
            let has_text = event
                .pointer("/properties/part/text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty());
            if part_type == Some("text") && belongs_to_assistant && has_text {
                state.idle = false;
                if let Some(message_id) = message_id {
                    state.assistant_text_ids.insert(message_id.to_owned());
                }
                state.add("message.part.text");
            } else if part_type == Some("tool") {
                let part_id = event.pointer("/properties/part/id").and_then(Value::as_str);
                let call_id = event
                    .pointer("/properties/part/callID")
                    .and_then(Value::as_str);
                let status = event
                    .pointer("/properties/part/state/status")
                    .and_then(Value::as_str);
                let Some((part_id, call_id, message_id)) = part_id
                    .zip(call_id)
                    .zip(message_id)
                    .map(|((a, b), c)| (a, b, c))
                else {
                    state.unverified_tool_event = true;
                    return;
                };
                if !belongs_to_assistant {
                    state.unverified_tool_event = true;
                    return;
                }
                let observed = match status {
                    Some("pending") => state.observe_tool_start(call_id, message_id, Some(part_id)),
                    Some("running") => {
                        let started = state.observe_tool_start(call_id, message_id, Some(part_id));
                        let updated = legacy_tool_update_has_content(event)
                            && state.observe_tool_update(call_id, message_id, Some(part_id));
                        if updated {
                            state.add("message.part.tool.updated");
                        }
                        started || updated
                    }
                    Some("completed") => state.observe_tool_terminal(
                        call_id,
                        message_id,
                        Some(part_id),
                        ToolTerminal::Succeeded,
                    ),
                    Some("error") => {
                        let observed = state.observe_tool_terminal(
                            call_id,
                            message_id,
                            Some(part_id),
                            ToolTerminal::Failed,
                        );
                        let error = event
                            .pointer("/properties/part/state/error")
                            .and_then(Value::as_str);
                        if state.mark_cancelled_tool(call_id, error) {
                            state.add("tool.cancelled.correlated");
                        }
                        observed
                    }
                    _ => false,
                };
                if observed {
                    state.idle = false;
                    state.add(match status {
                        Some("completed") => "message.part.tool.completed.correlated",
                        Some("error") => "message.part.tool.error.correlated",
                        _ => "message.part.tool.started",
                    });
                } else {
                    state.unverified_tool_event = true;
                }
            }
        }
        "session.next.text.delta" => {
            let has_text = ["/properties/delta", "/properties/text"]
                .into_iter()
                .filter_map(|path| event.pointer(path).and_then(Value::as_str))
                .any(|text| !text.is_empty());
            if has_text {
                if let Some(message_id) = event
                    .pointer("/properties/assistantMessageID")
                    .and_then(Value::as_str)
                {
                    state.assistant_message_ids.insert(message_id.to_owned());
                    state.assistant_text_ids.insert(message_id.to_owned());
                    state.idle = false;
                    state.add(event_type);
                }
            }
        }
        "session.next.tool.called" => {
            if let Some((call_id, message_id)) = next_tool_ids(event) {
                state.assistant_message_ids.insert(message_id.to_owned());
                if state.observe_tool_start(call_id, message_id, None) {
                    state.idle = false;
                    state.add("session.next.tool.called.correlated");
                } else {
                    state.unverified_tool_event = true;
                }
            } else {
                state.unverified_tool_event = true;
            }
        }
        "session.next.tool.input.delta" | "session.next.tool.progress" => {
            if let Some((call_id, message_id)) = next_tool_ids(event) {
                if next_tool_update_has_content(event) {
                    if state.observe_tool_update(call_id, message_id, None) {
                        state.idle = false;
                        state.add(if event_type == "session.next.tool.input.delta" {
                            "session.next.tool.input.delta.correlated"
                        } else {
                            "session.next.tool.progress.correlated"
                        });
                    } else {
                        state.unverified_tool_event = true;
                    }
                }
            } else {
                state.unverified_tool_event = true;
            }
        }
        "session.next.tool.success" => {
            if let Some((call_id, message_id)) = next_tool_ids(event) {
                if state.observe_tool_terminal(call_id, message_id, None, ToolTerminal::Succeeded) {
                    state.add("session.next.tool.success.correlated");
                } else {
                    state.unverified_tool_event = true;
                }
            } else {
                state.unverified_tool_event = true;
            }
        }
        "session.next.tool.failed" => {
            if let Some((call_id, message_id)) = next_tool_ids(event) {
                let observed =
                    state.observe_tool_terminal(call_id, message_id, None, ToolTerminal::Failed);
                if observed {
                    state.add("session.next.tool.failed.correlated");
                } else {
                    state.unverified_tool_event = true;
                }
                let error = event
                    .pointer("/properties/error/message")
                    .and_then(Value::as_str);
                if state.mark_cancelled_tool(call_id, error) {
                    state.add("tool.cancelled.correlated");
                }
            } else {
                state.unverified_tool_event = true;
            }
        }
        "session.error" => {
            state.idle = false;
            state.add(event_type);
        }
        "permission.asked" | "permission.v2.asked" => {
            if let Some((protocol, request_id, message_id, call_id)) =
                permission_asked_target(event_type, event)
            {
                if state.observe_permission_asked(protocol, request_id, message_id, call_id) {
                    state.idle = false;
                    state.add(if protocol == PermissionProtocol::Legacy {
                        "permission.asked.correlated"
                    } else {
                        "permission.v2.asked.correlated"
                    });
                } else {
                    state.unverified_permission_event = true;
                }
            } else {
                state.unverified_permission_event = true;
            }
        }
        "permission.replied" | "permission.v2.replied" => {
            let protocol = permission_protocol(event_type);
            let request_id = event
                .pointer("/properties/requestID")
                .and_then(Value::as_str);
            let reply = event.pointer("/properties/reply").and_then(Value::as_str);
            if let Some((protocol, request_id, reply)) = protocol
                .zip(request_id)
                .zip(reply)
                .map(|((a, b), c)| (a, b, c))
            {
                if state.observe_permission_replied(protocol, request_id, reply) {
                    state.add(if protocol == PermissionProtocol::Legacy {
                        "permission.replied.correlated"
                    } else {
                        "permission.v2.replied.correlated"
                    });
                } else {
                    state.unverified_permission_event = true;
                }
            } else {
                state.unverified_permission_event = true;
            }
        }
        "session.diff" => {
            if diff_has_actual_changes(event.pointer("/properties/diff").unwrap_or(&Value::Null)) {
                state.file_diff_evidence = true;
                state.add("session.diff.actual-change");
            }
        }
        "question.asked" | "question.v2.asked" => {
            state.idle = false;
            state.question_asked = true;
            state.add(event_type);
        }
        _ => {}
    }
}

fn next_tool_ids(event: &Value) -> Option<(&str, &str)> {
    event
        .pointer("/properties/callID")
        .and_then(Value::as_str)
        .zip(
            event
                .pointer("/properties/assistantMessageID")
                .and_then(Value::as_str),
        )
}

fn legacy_tool_update_has_content(event: &Value) -> bool {
    [
        "/properties/part/state/input",
        "/properties/part/state/title",
        "/properties/part/state/metadata",
    ]
    .into_iter()
    .filter_map(|path| event.pointer(path))
    .any(value_is_substantive)
}

fn next_tool_update_has_content(event: &Value) -> bool {
    match event.get("type").and_then(Value::as_str) {
        Some("session.next.tool.input.delta") => event
            .pointer("/properties/delta")
            .is_some_and(value_is_substantive),
        Some("session.next.tool.progress") => ["/properties/content", "/properties/structured"]
            .into_iter()
            .filter_map(|path| event.pointer(path))
            .any(value_is_substantive),
        _ => false,
    }
}

fn value_is_substantive(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn permission_protocol(event_type: &str) -> Option<PermissionProtocol> {
    match event_type {
        "permission.asked" | "permission.replied" => Some(PermissionProtocol::Legacy),
        "permission.v2.asked" | "permission.v2.replied" => Some(PermissionProtocol::V2),
        _ => None,
    }
}

fn permission_asked_target<'a>(
    event_type: &str,
    event: &'a Value,
) -> Option<(PermissionProtocol, &'a str, &'a str, &'a str)> {
    let protocol = permission_protocol(event_type)?;
    let request_id = event.pointer("/properties/id").and_then(Value::as_str)?;
    let source = match protocol {
        PermissionProtocol::Legacy => event.pointer("/properties/tool")?,
        PermissionProtocol::V2 => {
            let source = event.pointer("/properties/source")?;
            if source.get("type").and_then(Value::as_str) != Some("tool") {
                return None;
            }
            source
        }
    };
    let message_id = source.get("messageID").and_then(Value::as_str)?;
    let call_id = source.get("callID").and_then(Value::as_str)?;
    Some((protocol, request_id, message_id, call_id))
}

fn observe_prompt_abort(response: &Value, state: &mut ObservationState) {
    let aborted =
        response.pointer("/info/error/name").and_then(Value::as_str) == Some("MessageAbortedError");
    let message_id = response.pointer("/info/id").and_then(Value::as_str);
    if aborted && message_id.is_some_and(|message_id| state.mark_cancelled_message(message_id)) {
        state.add("prompt.message.aborted.correlated");
    }
}

fn diff_has_actual_changes(diff: &Value) -> bool {
    diff.as_array().is_some_and(|items| {
        items.iter().any(|item| {
            let changed_lines = item.get("additions").and_then(Value::as_u64).unwrap_or(0) > 0
                || item.get("deletions").and_then(Value::as_u64).unwrap_or(0) > 0;
            let nonempty_patch = item
                .get("patch")
                .and_then(Value::as_str)
                .is_some_and(|patch| !patch.is_empty());
            changed_lines && nonempty_patch
        })
    })
}

fn file_changed_since(before: Option<&[u8]>, path: &Path) -> io::Result<bool> {
    let after = fs::read(path)?;
    Ok(before.is_some_and(|before| before != after))
}

fn missing_observation_reason(scenario: Scenario, state: &ObservationState) -> String {
    let requirement = match scenario {
        Scenario::SimpleTurn => "assistant text followed by session idle",
        Scenario::ToolCall => {
            "one exact notes.txt read permission flow with a correlated tool success, assistant \
             text, and session idle"
        }
        Scenario::PermissionApprove => {
            "one permission request ID with a tool source, matching `once` replied event, \
             success of that same tool/call ID, and session idle"
        }
        Scenario::PermissionDeny => {
            "one permission request ID with a tool source, matching `reject` replied event, \
             failure of that same tool/call ID, and session idle"
        }
        Scenario::FileChange => {
            "one exact editable.txt edit permission flow, a correlated tool success, a diff with \
             actual additions/deletions/patch, changed editable.txt bytes, and session idle"
        }
        Scenario::Cancel => {
            "one exact `cargo run -- wait` permission flow, a successful POST \
             /session/{id}/abort, and aborted evidence for that same tool/call ID"
        }
        Scenario::Error => {
            "one exact `cargo run -- fail` permission flow with failure of that same tool/call ID, \
             then idle"
        }
        Scenario::Elicitation => "question.asked and a matching question reply followed by idle",
        Scenario::SessionLoad => "a pre-existing session",
    };
    format!(
        "the real event stream ended or timed out before observing {requirement}; \
         {} SSE event(s) were recorded",
        state.event_count
    )
}

fn event_session_id(event: &Value) -> Option<&str> {
    event
        .pointer("/properties/sessionID")
        .or_else(|| event.get("sessionID"))
        .and_then(Value::as_str)
}

fn record_current_session_frame<W: Write>(
    frame: &SseFrame,
    session_id: &str,
    scenario: Scenario,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<Option<Value>, OpenCodeError> {
    let event: Value = serde_json::from_str(&frame.data)?;
    if event_session_id(&event) != Some(session_id) {
        return Ok(None);
    }
    if matches!(
        event.get("type").and_then(Value::as_str),
        Some("permission.asked" | "permission.v2.asked")
    ) {
        validate_opencode_permission_scope(&event, sandbox, scenario)
            .map_err(|_| OpenCodeError::UnsafePermissionScope)?;
    }
    let recorded = sse_tee::record_stream(Cursor::new(frame.raw.as_bytes()), fixture, Some(1))?;
    if recorded != 1 {
        return Err(OpenCodeError::Protocol(
            "accepted SSE frame did not contain exactly one event",
        ));
    }
    Ok(Some(event))
}

fn validate_opencode_permission_scope(
    event: &Value,
    sandbox: &Path,
    scenario: Scenario,
) -> Result<(), PermissionScopeError> {
    let expected =
        opencode_permission_scope(scenario).ok_or(PermissionScopeError::UnsafeCommand)?;
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    let properties = event
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    match event_type {
        "permission.asked" => {
            if properties.get("permission").and_then(Value::as_str) != Some(expected.name()) {
                return Err(PermissionScopeError::UnsafeCommand);
            }
            validate_opencode_targets(
                properties
                    .get("patterns")
                    .ok_or(PermissionScopeError::UnsafeCommand)?,
                sandbox,
                expected,
            )?;
            if let Some(always) = properties.get("always") {
                if !always
                    .as_array()
                    .is_some_and(|patterns| patterns.is_empty())
                {
                    validate_opencode_targets(always, sandbox, expected)?;
                }
            }
        }
        "permission.v2.asked" => {
            if properties.get("action").and_then(Value::as_str) != Some(expected.name()) {
                return Err(PermissionScopeError::UnsafeCommand);
            }
            validate_opencode_targets(
                properties
                    .get("resources")
                    .ok_or(PermissionScopeError::UnsafeCommand)?,
                sandbox,
                expected,
            )?;
            if let Some(save) = properties.get("save") {
                if !save
                    .as_array()
                    .is_some_and(|resources| resources.is_empty())
                {
                    validate_opencode_targets(save, sandbox, expected)?;
                }
            }
        }
        _ => return Err(PermissionScopeError::UnsafeCommand),
    }
    if let Some(metadata) = properties.get("metadata") {
        validate_permission_metadata(metadata, sandbox, expected, None)?;
    }
    let _ =
        permission_asked_target(event_type, event).ok_or(PermissionScopeError::UnsafeCommand)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpenCodePermissionScope {
    Command(PermissionCommand),
    Read(&'static str),
    Edit(&'static str),
}

impl OpenCodePermissionScope {
    const fn name(self) -> &'static str {
        match self {
            Self::Command(_) => "bash",
            Self::Read(_) => "read",
            Self::Edit(_) => "edit",
        }
    }
}

const fn opencode_permission_scope(scenario: Scenario) -> Option<OpenCodePermissionScope> {
    match scenario {
        Scenario::ToolCall => Some(OpenCodePermissionScope::Read("notes.txt")),
        Scenario::PermissionApprove | Scenario::PermissionDeny => {
            Some(OpenCodePermissionScope::Command(PermissionCommand::Run))
        }
        Scenario::FileChange => Some(OpenCodePermissionScope::Edit("editable.txt")),
        Scenario::Cancel => Some(OpenCodePermissionScope::Command(PermissionCommand::Wait)),
        Scenario::Error => Some(OpenCodePermissionScope::Command(PermissionCommand::Fail)),
        Scenario::SimpleTurn | Scenario::SessionLoad | Scenario::Elicitation => None,
    }
}

fn validate_opencode_targets(
    value: &Value,
    sandbox: &Path,
    expected: OpenCodePermissionScope,
) -> Result<(), PermissionScopeError> {
    let targets = value
        .as_array()
        .filter(|targets| !targets.is_empty())
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    for target in targets {
        let target = target.as_str().ok_or(PermissionScopeError::UnsafeCommand)?;
        match expected {
            OpenCodePermissionScope::Command(command) => {
                validate_permission_command_as(target, command)?;
            }
            OpenCodePermissionScope::Read(relative) | OpenCodePermissionScope::Edit(relative) => {
                validate_exact_permission_path(sandbox, target, Path::new(relative))?;
            }
        }
    }
    Ok(())
}

fn validate_permission_metadata(
    value: &Value,
    sandbox: &Path,
    expected: OpenCodePermissionScope,
    key: Option<&str>,
) -> Result<(), PermissionScopeError> {
    match value {
        Value::Object(fields) => {
            for (field, value) in fields {
                validate_permission_metadata(value, sandbox, expected, Some(field))?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_permission_metadata(value, sandbox, expected, key)?;
            }
        }
        Value::String(value) => match key {
            Some("command" | "cmd") => match expected {
                OpenCodePermissionScope::Command(command) => {
                    validate_permission_command_as(value, command)?;
                }
                OpenCodePermissionScope::Read(_) | OpenCodePermissionScope::Edit(_) => {
                    return Err(PermissionScopeError::UnsafeCommand);
                }
            },
            Some("cwd" | "directory" | "workdir" | "root") => {
                validate_exact_permission_cwd(sandbox, value)?
            }
            Some("path" | "file" | "filePath" | "file_path" | "target" | "filename") => {
                validate_opencode_metadata_path(sandbox, value, expected)?;
            }
            _ if looks_like_permission_path(value) => {
                validate_opencode_metadata_path(sandbox, value, expected)?;
            }
            _ => {}
        },
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn validate_opencode_metadata_path(
    sandbox: &Path,
    path: &str,
    expected: OpenCodePermissionScope,
) -> Result<(), PermissionScopeError> {
    match expected {
        OpenCodePermissionScope::Command(_) => validate_permission_path(sandbox, path),
        OpenCodePermissionScope::Read(relative) | OpenCodePermissionScope::Edit(relative) => {
            validate_exact_permission_path(sandbox, path, Path::new(relative))
        }
    }
}

fn looks_like_permission_path(value: &str) -> bool {
    value.contains("<OUTSIDE_PATH>")
        || value.starts_with('.')
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.as_bytes().get(1) == Some(&b':')
}

struct SseFrame {
    raw: String,
    data: String,
}

fn read_sse_frame(reader: &mut impl BufRead) -> io::Result<Option<SseFrame>> {
    let mut raw = String::new();
    let mut data = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            if raw.is_empty() {
                return Ok(None);
            }
            if data.is_empty() {
                return Ok(None);
            }
            return Ok(Some(SseFrame {
                raw,
                data: data.join("\n"),
            }));
        }
        raw.push_str(&line);
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if data.is_empty() {
                raw.clear();
                continue;
            }
            return Ok(Some(SseFrame {
                raw,
                data: data.join("\n"),
            }));
        }
        if let Some(value) = trimmed.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) || error.to_string().to_ascii_lowercase().contains("timed out")
}

fn request_json_unrecorded(
    client: &Client,
    base_url: &str,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> Result<RawResponse, OpenCodeError> {
    let method_name = if method == Method::GET { "GET" } else { "POST" };
    let mut request = client
        .request(method, format!("{base_url}{path}"))
        .header(ACCEPT, "application/json");
    if let Some(body) = body {
        request = request
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_owned());
    }
    let response = raw_response(request.send()?)?;
    if !response.status.is_success() {
        return Err(OpenCodeError::HttpStatus {
            method: method_name,
            path: path.to_owned(),
            status: response.status,
        });
    }
    Ok(response)
}

fn record_json_exchange<W: Write>(
    method: Method,
    path: &str,
    body: Option<&str>,
    response: &RawResponse,
    fixture: &mut FixtureSink<W>,
) -> Result<(), OpenCodeError> {
    let method_name = if method == Method::GET { "GET" } else { "POST" };
    let request_payload = http_request_payload(
        method_name,
        path,
        if body.is_some() {
            "application/json"
        } else {
            ""
        },
        body.unwrap_or("null"),
    )?;
    fixture.record(Direction::C2s, Transport::Http, &request_payload)?;
    record_raw_response(method_name, path, response, fixture)
}

fn send_json<W: Write>(
    client: &Client,
    base_url: &str,
    method: Method,
    path: &str,
    body: Option<String>,
    fixture: &mut FixtureSink<W>,
) -> Result<RawResponse, OpenCodeError> {
    let method_name = if method == Method::GET { "GET" } else { "POST" };
    let request_content_type = if body.is_some() {
        "application/json"
    } else {
        ""
    };
    let fixture_body = body.as_deref().unwrap_or("null");
    let request_payload =
        http_request_payload(method_name, path, request_content_type, fixture_body)?;
    fixture.record(Direction::C2s, Transport::Http, &request_payload)?;

    let mut request = client
        .request(method, format!("{base_url}{path}"))
        .header(ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.header(CONTENT_TYPE, "application/json").body(body);
    }
    let response = raw_response(request.send()?)?;
    record_raw_response(method_name, path, &response, fixture)?;
    if !response.status.is_success() {
        return Err(OpenCodeError::HttpStatus {
            method: method_name,
            path: path.to_owned(),
            status: response.status,
        });
    }
    Ok(response)
}

fn raw_response(response: Response) -> Result<RawResponse, OpenCodeError> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = response.bytes()?;
    let body = std::str::from_utf8(&body)
        .map_err(|_| OpenCodeError::NonUtf8Response)?
        .to_owned();
    Ok(RawResponse {
        status,
        content_type,
        body,
    })
}

fn record_http_response<W: Write>(
    method: &'static str,
    path: &str,
    response: Response,
    fixture: &mut FixtureSink<W>,
) -> Result<(), OpenCodeError> {
    let response = raw_response(response)?;
    record_raw_response(method, path, &response, fixture)
}

fn record_raw_response<W: Write>(
    method: &str,
    path: &str,
    response: &RawResponse,
    fixture: &mut FixtureSink<W>,
) -> Result<(), OpenCodeError> {
    let body = if response.body.is_empty() {
        "null"
    } else {
        &response.body
    };
    let response_payload = http_response_payload(
        method,
        path,
        response.status.as_u16(),
        &response.content_type,
        body,
    )?;
    fixture.record(Direction::S2c, Transport::Http, &response_payload)?;
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &'static str) -> Result<&'a str, OpenCodeError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(OpenCodeError::Protocol(field))
}

fn canonical_json_path(value: Option<&Value>, expected: &Path) -> bool {
    value
        .and_then(Value::as_str)
        .and_then(|path| Path::new(path).canonicalize().ok())
        .is_some_and(|path| path == expected)
}

fn validate_prompt_project_isolation(sandbox: &Path) -> Result<(), OpenCodeError> {
    let git = sandbox.join(".git");
    match platform::validate_fixture_sandbox_root(&git, &git) {
        Ok(Some(_)) => Ok(()),
        Ok(None) | Err(_) => Err(OpenCodeError::ProjectIsolation),
    }
}

fn validate_fixture_sandbox(path: &Path) -> Result<PathBuf, OpenCodeError> {
    let expected = expected_fixture_sandbox_path()?;
    validate_fixture_sandbox_against(path, &expected)
}

fn validate_fixture_sandbox_against(
    path: &Path,
    expected: &Path,
) -> Result<PathBuf, OpenCodeError> {
    platform::validate_fixture_sandbox_root(path, expected)?.ok_or(OpenCodeError::InvalidSandbox)
}

fn expected_fixture_sandbox_path() -> Result<PathBuf, OpenCodeError> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or(OpenCodeError::InvalidSandbox)?
        .join("tests")
        .join("fixtures")
        .join("sandbox"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::Redactor;

    fn test_sandbox_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("sandbox")
    }
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn completed_protocol_recording_survives_cleanup_failure(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let outcome = Outcome::Recorded {
            session_id: Some("session-test".to_owned()),
            event_count: 3,
            observations: vec!["session.idle".to_owned()],
        };
        let completed = finish_recording(Ok(outcome), || {
            Err(ProcessError::IncompleteCleanup {
                root_pid: 7,
                unconfirmed_pids: vec![42],
                detail: "forced cleanup failure".to_owned(),
            })
        })?;

        assert!(matches!(
            completed.outcome,
            Outcome::Recorded { event_count: 3, .. }
        ));
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
    fn protocol_and_cleanup_failures_are_both_reported() -> Result<(), Box<dyn std::error::Error>> {
        let protocol = OpenCodeError::Protocol("forced protocol failure");
        let cleanup = ProcessError::Terminate(io::Error::other("forced cleanup failure"));
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
            OpenCodeError::CleanupAfterError {
                source,
                cleanup: ProcessError::Terminate(_),
            } if matches!(source.as_ref(), OpenCodeError::Protocol("forced protocol failure"))
        ));
        let message = error.to_string();
        assert!(message.contains("forced protocol failure"));
        assert!(message.contains("forced cleanup failure"));
        Ok(())
    }

    #[test]
    fn failed_startup_cleanup_stops_retries_and_reports_both_errors(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut attempts = 0;
        let result = retry_server_startup::<(), ()>(
            MAX_START_ATTEMPTS,
            |_| {
                attempts += 1;
                Ok(StartupAttempt::Retry {
                    error: OpenCodeError::Protocol("forced readiness failure"),
                    child: (),
                })
            },
            |_| {
                Err(ProcessError::Terminate(io::Error::other(
                    "forced retry cleanup failure",
                )))
            },
        );

        assert_eq!(attempts, 1, "cleanup failure must prevent another spawn");
        let error = match result {
            Err(error) => error,
            Ok(()) => return Err(io::Error::other("cleanup failure did not abort startup").into()),
        };
        assert!(matches!(
            &error,
            OpenCodeError::CleanupAfterError {
                source,
                cleanup: ProcessError::Terminate(_),
            } if matches!(source.as_ref(), OpenCodeError::Protocol("forced readiness failure"))
        ));
        let message = error.to_string();
        assert!(message.contains("forced readiness failure"));
        assert!(message.contains("forced retry cleanup failure"));
        Ok(())
    }

    #[test]
    fn non_success_response_is_recorded_before_error() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let address = listener.local_addr()?;
        let server = thread::spawn(move || -> io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            read_request(&mut stream)?;
            stream.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\n\
                      Content-Type: application/json\r\n\
                      Content-Length: 18\r\n\
                      Connection: close\r\n\r\n\
                      {\"error\":\"forced\"}",
            )?;
            Ok(())
        });
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));
        let response = send_json(
            &client,
            &format!("http://{address}"),
            Method::POST,
            "/session",
            Some("{}".to_owned()),
            &mut fixture,
        );
        let error = match response {
            Err(error) => error,
            Ok(_) => return Err(io::Error::other("503 response unexpectedly succeeded").into()),
        };
        server
            .join()
            .map_err(|_| io::Error::other("mock server thread panicked"))??;
        assert!(matches!(
            error,
            OpenCodeError::HttpStatus {
                status: StatusCode::SERVICE_UNAVAILABLE,
                ..
            }
        ));
        let output = String::from_utf8(fixture.into_inner())?;
        assert!(output.contains("\"status\":503"));
        assert!(output.contains("\"body\":{\"error\":\"forced\"}"));
        Ok(())
    }

    #[test]
    fn session_load_skips_first_session_and_loads_only_exact_sandbox_seed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let sessions = json!([
            {
                "id": "ses_first",
                "title": "not the session-load seed",
                "directory": sandbox
            },
            {
                "id": "ses_seed",
                "title": SESSION_LOAD_SEED_TITLE,
                "directory": sandbox.join(".")
            }
        ])
        .to_string();
        let detail = json!({
            "id": "ses_seed",
            "title": SESSION_LOAD_SEED_TITLE,
            "directory": sandbox
        })
        .to_string();
        let messages = json!([
            {
                "info": {"sessionID": "ses_seed"},
                "parts": []
            }
        ])
        .to_string();
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let address = listener.local_addr()?;
        let server = serve_json_responses(listener, vec![sessions, detail, messages]);
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let outcome = record_session_load(
            &client,
            &format!("http://{address}"),
            &sandbox,
            &mut fixture,
            &CancellationToken::default(),
        )?;
        let paths = server
            .join()
            .map_err(|_| io::Error::other("mock server thread panicked"))??;

        assert!(matches!(
            outcome,
            Outcome::Recorded {
                session_id: Some(session_id),
                ..
            } if session_id == "ses_seed"
        ));
        assert_eq!(
            paths,
            ["/session", "/session/ses_seed", "/session/ses_seed/message"]
        );
        assert_eq!(String::from_utf8(fixture.into_inner())?.lines().count(), 6);
        Ok(())
    }

    #[test]
    fn session_load_rejects_wrong_directory_and_inexact_title(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const OUTSIDE_METADATA_MARKER: &str = "PRIVATE OUTSIDE SESSION METADATA";
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let unrelated = temporary.path().join("unrelated");
        std::fs::create_dir_all(&sandbox)?;
        std::fs::create_dir_all(&unrelated)?;
        let sandbox = sandbox.canonicalize()?;
        let unrelated = unrelated.canonicalize()?;
        let sessions = json!([
            {
                "id": "ses_wrong_directory",
                "title": SESSION_LOAD_SEED_TITLE,
                "directory": unrelated,
                "metadata": {"private": OUTSIDE_METADATA_MARKER}
            },
            {
                "id": "ses_wrong_title",
                "title": format!("{SESSION_LOAD_SEED_TITLE} "),
                "directory": sandbox
            }
        ])
        .to_string();
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let address = listener.local_addr()?;
        let server = serve_json_responses(listener, vec![sessions]);
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = record_session_load(
            &client,
            &format!("http://{address}"),
            &sandbox,
            &mut fixture,
            &CancellationToken::default(),
        );
        let paths = server
            .join()
            .map_err(|_| io::Error::other("mock server thread panicked"))??;

        assert!(matches!(result, Err(OpenCodeError::SessionLoadIsolation)));
        assert_eq!(paths, ["/session"]);
        let output = String::from_utf8(fixture.into_inner())?;
        assert!(output.is_empty());
        assert!(!output.contains(OUTSIDE_METADATA_MARKER));
        Ok(())
    }

    #[test]
    fn session_load_preflight_failures_leave_fixture_empty(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let missing = temporary.path().join("missing-directory");
        let cases = [
            (
                "non-array",
                json!({"not": "an array"}),
                "GET /session response must be an array",
            ),
            (
                "unresolved-directory",
                json!([{
                    "id": "ses_seed",
                    "title": SESSION_LOAD_SEED_TITLE,
                    "directory": missing
                }]),
                "isolation",
            ),
            (
                "missing-seed",
                json!([{
                    "id": "ses_other",
                    "title": "not the seed",
                    "directory": sandbox
                }]),
                "seed",
            ),
        ];

        for (case, sessions, expected) in cases {
            let listener = TcpListener::bind((LOOPBACK, 0))?;
            let address = listener.local_addr()?;
            let server = serve_json_responses(listener, vec![sessions.to_string()]);
            let client = Client::builder().no_proxy().build()?;
            let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

            let result = record_session_load(
                &client,
                &format!("http://{address}"),
                &sandbox,
                &mut fixture,
                &CancellationToken::default(),
            );
            let paths = server
                .join()
                .map_err(|_| io::Error::other("mock server thread panicked"))??;
            let matched = match expected {
                "GET /session response must be an array" => matches!(
                    result,
                    Err(OpenCodeError::Protocol(
                        "GET /session response must be an array"
                    ))
                ),
                "isolation" => matches!(result, Err(OpenCodeError::SessionLoadIsolation)),
                "seed" => matches!(result, Err(OpenCodeError::SessionLoadSeedMissing)),
                _ => false,
            };
            assert!(matched, "unexpected result for {case}: {result:?}");
            assert_eq!(paths, ["/session"]);
            assert!(
                fixture.into_inner().is_empty(),
                "preflight failure `{case}` wrote fixture bytes"
            );
        }
        Ok(())
    }

    #[test]
    fn session_load_rejects_mismatched_detail_before_recording_it(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const DETAIL_MARKER: &str = "PRIVATE FOREIGN DETAIL";
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let sessions = json!([{
            "id": "ses_seed",
            "title": SESSION_LOAD_SEED_TITLE,
            "directory": sandbox
        }])
        .to_string();
        let detail = json!({
            "id": "ses_foreign",
            "title": SESSION_LOAD_SEED_TITLE,
            "directory": sandbox,
            "private": DETAIL_MARKER
        })
        .to_string();
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let address = listener.local_addr()?;
        let server = serve_json_responses(listener, vec![sessions, detail]);
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = record_session_load(
            &client,
            &format!("http://{address}"),
            &sandbox,
            &mut fixture,
            &CancellationToken::default(),
        );
        let paths = server
            .join()
            .map_err(|_| io::Error::other("mock server thread panicked"))??;

        assert!(matches!(result, Err(OpenCodeError::SessionLoadDetail)));
        assert_eq!(paths, ["/session", "/session/ses_seed"]);
        let output = String::from_utf8(fixture.into_inner())?;
        assert_eq!(output.lines().count(), 2);
        assert!(!output.contains(DETAIL_MARKER));
        Ok(())
    }

    #[test]
    fn session_load_detail_checks_id_title_and_directory_independently(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&sandbox)?;
        std::fs::create_dir_all(&outside)?;
        let sandbox = sandbox.canonicalize()?;
        let outside = outside.canonicalize()?;
        let valid = json!({
            "id": "ses_seed",
            "title": SESSION_LOAD_SEED_TITLE,
            "directory": sandbox
        });
        assert!(validate_session_load_detail(&valid, "ses_seed", &sandbox).is_ok());

        for invalid in [
            json!({
                "id": "ses_foreign",
                "title": SESSION_LOAD_SEED_TITLE,
                "directory": sandbox
            }),
            json!({
                "id": "ses_seed",
                "title": format!("{SESSION_LOAD_SEED_TITLE} "),
                "directory": sandbox
            }),
            json!({
                "id": "ses_seed",
                "title": SESSION_LOAD_SEED_TITLE,
                "directory": outside
            }),
        ] {
            assert!(matches!(
                validate_session_load_detail(&invalid, "ses_seed", &sandbox),
                Err(OpenCodeError::SessionLoadDetail)
            ));
        }
        Ok(())
    }

    #[test]
    fn session_load_rejects_empty_or_foreign_messages_before_recording_them(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const MESSAGE_MARKER: &str = "PRIVATE FOREIGN MESSAGE";
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let sessions = json!([{
            "id": "ses_seed",
            "title": SESSION_LOAD_SEED_TITLE,
            "directory": sandbox
        }])
        .to_string();
        let detail = json!({
            "id": "ses_seed",
            "title": SESSION_LOAD_SEED_TITLE,
            "directory": sandbox
        })
        .to_string();
        let invalid_messages = [
            json!([]),
            json!([{
                "info": {
                    "sessionID": "ses_foreign",
                    "private": MESSAGE_MARKER
                },
                "parts": []
            }]),
        ];

        for invalid in invalid_messages {
            let listener = TcpListener::bind((LOOPBACK, 0))?;
            let address = listener.local_addr()?;
            let server = serve_json_responses(
                listener,
                vec![sessions.clone(), detail.clone(), invalid.to_string()],
            );
            let client = Client::builder().no_proxy().build()?;
            let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

            let result = record_session_load(
                &client,
                &format!("http://{address}"),
                &sandbox,
                &mut fixture,
                &CancellationToken::default(),
            );
            server
                .join()
                .map_err(|_| io::Error::other("mock server thread panicked"))??;

            assert!(matches!(result, Err(OpenCodeError::SessionLoadMessages)));
            let output = String::from_utf8(fixture.into_inner())?;
            assert_eq!(output.lines().count(), 4);
            assert!(!output.contains(MESSAGE_MARKER));
        }
        Ok(())
    }

    #[test]
    fn session_create_rejects_foreign_directory_before_writing_exchange(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const CREATE_MARKER: &str = "PRIVATE FOREIGN CREATED SESSION";
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&sandbox)?;
        std::fs::create_dir_all(&outside)?;
        let sandbox = sandbox.canonicalize()?;
        let outside = outside.canonicalize()?;
        let response = json!({
            "id": "ses_foreign",
            "directory": outside,
            "private": CREATE_MARKER
        })
        .to_string();
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let address = listener.local_addr()?;
        let server = serve_json_responses(listener, vec![response]);
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = create_session(
            &client,
            &format!("http://{address}"),
            Scenario::SimpleTurn,
            &sandbox,
            &mut fixture,
        );
        server
            .join()
            .map_err(|_| io::Error::other("mock server thread panicked"))??;

        assert!(matches!(result, Err(OpenCodeError::SessionCreateIsolation)));
        let output = String::from_utf8(fixture.into_inner())?;
        assert!(output.is_empty());
        assert!(!output.contains(CREATE_MARKER));
        Ok(())
    }

    #[test]
    fn session_create_requires_an_id_before_writing_exchange(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const CREATE_MARKER: &str = "PRIVATE IDLESS CREATED SESSION";
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let response = json!({
            "directory": sandbox,
            "private": CREATE_MARKER
        })
        .to_string();
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let address = listener.local_addr()?;
        let server = serve_json_responses(listener, vec![response]);
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = create_session(
            &client,
            &format!("http://{address}"),
            Scenario::SimpleTurn,
            &sandbox,
            &mut fixture,
        );
        server
            .join()
            .map_err(|_| io::Error::other("mock server thread panicked"))??;

        assert!(matches!(result, Err(OpenCodeError::SessionCreateIsolation)));
        let output = String::from_utf8(fixture.into_inner())?;
        assert!(output.is_empty());
        assert!(!output.contains(CREATE_MARKER));
        Ok(())
    }

    #[test]
    fn foreign_prompt_response_is_not_written() -> Result<(), Box<dyn std::error::Error>> {
        const PROMPT_MARKER: &str = "PRIVATE FOREIGN PROMPT RESPONSE";
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let response = RawResponse {
            status: StatusCode::OK,
            content_type: "application/json".to_owned(),
            body: json!({
                "info": {
                    "id": "msg_foreign",
                    "sessionID": "ses_foreign",
                    "path": {"cwd": sandbox, "root": sandbox}
                },
                "parts": [],
                "private": PROMPT_MARKER
            })
            .to_string(),
        };
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = validate_and_record_prompt_response(
            &response,
            "/session/ses_current/message",
            "ses_current",
            &sandbox,
            &mut fixture,
        );

        assert!(matches!(
            result,
            Err(OpenCodeError::PromptResponseIsolation)
        ));
        let output = String::from_utf8(fixture.into_inner())?;
        assert!(output.is_empty());
        assert!(!output.contains(PROMPT_MARKER));
        Ok(())
    }

    #[test]
    fn prompt_response_checks_session_cwd_and_root_independently(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&sandbox)?;
        std::fs::create_dir_all(&outside)?;
        let sandbox = sandbox.canonicalize()?;
        let outside = outside.canonicalize()?;
        let valid = json!({
            "info": {
                "id": "msg_current",
                "sessionID": "ses_current",
                "path": {"cwd": sandbox, "root": sandbox}
            },
            "parts": []
        });
        assert!(validate_prompt_response(&valid, "ses_current", &sandbox).is_ok());

        for invalid in [
            json!({
                "info": {
                    "id": "msg_current",
                    "sessionID": "ses_foreign",
                    "path": {"cwd": sandbox, "root": sandbox}
                }
            }),
            json!({
                "info": {
                    "id": "msg_current",
                    "sessionID": "ses_current",
                    "path": {"cwd": outside, "root": sandbox}
                }
            }),
            json!({
                "info": {
                    "id": "msg_current",
                    "sessionID": "ses_current",
                    "path": {"cwd": sandbox, "root": outside}
                }
            }),
        ] {
            assert!(matches!(
                validate_prompt_response(&invalid, "ses_current", &sandbox),
                Err(OpenCodeError::PromptResponseIsolation)
            ));
        }
        Ok(())
    }

    #[test]
    fn global_sse_filters_foreign_unscoped_and_invalid_frames_before_writing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const FOREIGN_MARKER: &str = "PRIVATE FOREIGN SSE";
        const UNSCOPED_MARKER: &str = "PRIVATE UNSCOPED SSE";
        let foreign = sse_frame(json!({
            "type": "session.idle",
            "properties": {
                "sessionID": "ses_foreign",
                "private": FOREIGN_MARKER
            }
        }));
        let unscoped = sse_frame(json!({
            "type": "server.connected",
            "properties": {"private": UNSCOPED_MARKER}
        }));
        let invalid = SseFrame {
            raw: "data: {not-json}\n\n".to_owned(),
            data: "{not-json}".to_owned(),
        };
        let current = sse_frame(json!({
            "type": "session.idle",
            "properties": {"sessionID": "ses_current"}
        }));
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir(&sandbox)?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        assert!(record_current_session_frame(
            &foreign,
            "ses_current",
            Scenario::SimpleTurn,
            &sandbox,
            &mut fixture
        )?
        .is_none());
        assert!(record_current_session_frame(
            &unscoped,
            "ses_current",
            Scenario::SimpleTurn,
            &sandbox,
            &mut fixture
        )?
        .is_none());
        assert!(matches!(
            record_current_session_frame(
                &invalid,
                "ses_current",
                Scenario::SimpleTurn,
                &sandbox,
                &mut fixture
            ),
            Err(OpenCodeError::Json(_))
        ));
        assert!(record_current_session_frame(
            &current,
            "ses_current",
            Scenario::SimpleTurn,
            &sandbox,
            &mut fixture
        )?
        .is_some());

        let output = String::from_utf8(fixture.into_inner())?;
        assert!(!output.contains(FOREIGN_MARKER));
        assert!(!output.contains(UNSCOPED_MARKER));
        assert!(output.contains("ses_current"));
        Ok(())
    }

    #[test]
    fn sse_frame_preserves_raw_json_for_existing_parser() -> Result<(), Box<dyn std::error::Error>>
    {
        let input = b"event: message\r\ndata: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"ses_test\"}}\r\n\r\n";
        let mut reader = BufReader::new(&input[..]);
        let frame = read_sse_frame(&mut reader)?
            .ok_or_else(|| io::Error::other("SSE frame was not returned"))?;
        assert_eq!(
            frame.data,
            "{\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"ses_test\"}}"
        );
        assert_eq!(frame.raw.as_bytes(), input);
        Ok(())
    }

    #[test]
    fn permission_session_body_matches_pinned_openapi_shape(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let sandbox = test_sandbox_path().canonicalize()?;
        for scenario in [
            Scenario::SimpleTurn,
            Scenario::ToolCall,
            Scenario::PermissionApprove,
            Scenario::PermissionDeny,
            Scenario::FileChange,
            Scenario::Cancel,
            Scenario::Error,
            Scenario::Elicitation,
        ] {
            let body: Value = serde_json::from_str(&scenario.session_body(&sandbox)?)?;
            let rules = body
                .get("permission")
                .and_then(Value::as_array)
                .ok_or_else(|| io::Error::other("permission rules were absent"))?;
            assert_eq!(
                rules.first(),
                Some(&json!({
                    "permission": "*",
                    "pattern": "*",
                    "action": "deny"
                }))
            );
            assert_eq!(
                rules.get(1),
                Some(&json!({
                    "permission": "external_directory",
                    "pattern": "*",
                    "action": "deny"
                }))
            );
            for rule in rules {
                if matches!(
                    rule.get("action").and_then(Value::as_str),
                    Some("ask" | "allow")
                ) {
                    let pattern = rule
                        .get("pattern")
                        .and_then(Value::as_str)
                        .ok_or_else(|| io::Error::other("permission pattern was absent"))?;
                    let is_question_allow = rule.get("permission").and_then(Value::as_str)
                        == Some("question")
                        && rule.get("action").and_then(Value::as_str) == Some("allow")
                        && pattern == "*";
                    if !is_question_allow {
                        assert!(!pattern.contains('*'));
                        assert!(!pattern.contains('?'));
                    }
                }
            }
            assert_eq!(
                body.get("title").and_then(Value::as_str),
                Some("OneKaleidoscope T-004 fixture recording")
            );
        }
        let read: Value = serde_json::from_str(&Scenario::ToolCall.session_body(&sandbox)?)?;
        assert_eq!(
            read.pointer("/permission/2"),
            Some(&json!({
                "permission": "read",
                "pattern": "notes.txt",
                "action": "ask"
            }))
        );
        assert_eq!(
            read.pointer("/permission/3"),
            Some(&json!({
                "permission": "read",
                "pattern": canonical_permission_pattern(&sandbox, "notes.txt")?,
                "action": "ask"
            }))
        );
        let edit: Value = serde_json::from_str(&Scenario::FileChange.session_body(&sandbox)?)?;
        assert_eq!(
            edit.pointer("/permission/2"),
            Some(&json!({
                "permission": "edit",
                "pattern": "editable.txt",
                "action": "ask"
            }))
        );
        assert_eq!(
            edit.pointer("/permission/3"),
            Some(&json!({
                "permission": "edit",
                "pattern": canonical_permission_pattern(&sandbox, "editable.txt")?,
                "action": "ask"
            }))
        );
        let elicitation: Value =
            serde_json::from_str(&Scenario::Elicitation.session_body(&sandbox)?)?;
        assert_eq!(
            elicitation.pointer("/permission/2"),
            Some(&json!({
                "permission": "question",
                "pattern": "*",
                "action": "allow"
            }))
        );
        Ok(())
    }

    #[test]
    fn unsafe_permission_sse_is_rejected_before_fixture_write(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const OUTSIDE_MARKER: &str = "PRIVATE OUTSIDE PERMISSION";
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir(&sandbox)?;
        let event = json!({
            "type": "permission.v2.asked",
            "properties": {
                "id": "per_unsafe",
                "sessionID": "ses_current",
                "action": "bash",
                "resources": [format!("cargo run -- & type ..\\{OUTSIDE_MARKER}")],
                "metadata": {},
                "save": [],
                "source": {
                    "type": "tool",
                    "messageID": "msg_a",
                    "callID": "call_a"
                }
            }
        });
        let frame = sse_frame(event);
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = record_current_session_frame(
            &frame,
            "ses_current",
            Scenario::PermissionDeny,
            &sandbox,
            &mut fixture,
        );

        assert!(matches!(result, Err(OpenCodeError::UnsafePermissionScope)));
        let output = String::from_utf8(fixture.into_inner())?;
        assert!(output.is_empty());
        assert!(!output.contains(OUTSIDE_MARKER));
        Ok(())
    }

    #[test]
    fn read_and_edit_permission_sse_accept_only_the_exact_scenario_file(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let sandbox = test_sandbox_path();
        for (scenario, action, filename) in [
            (Scenario::ToolCall, "read", "notes.txt"),
            (Scenario::FileChange, "edit", "editable.txt"),
        ] {
            for target in [
                filename.to_owned(),
                platform::permission_path_pattern(&sandbox.join(filename))?,
            ] {
                let event = json!({
                    "type": "permission.v2.asked",
                    "properties": {
                        "id": format!("per_{action}"),
                        "sessionID": "ses_current",
                        "action": action,
                        "resources": [target],
                        "metadata": {"path": target},
                        "save": [],
                        "source": {
                            "type": "tool",
                            "messageID": "msg_a",
                            "callID": "call_a"
                        }
                    }
                });
                let frame = sse_frame(event);
                let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

                assert!(record_current_session_frame(
                    &frame,
                    "ses_current",
                    scenario,
                    &sandbox,
                    &mut fixture,
                )?
                .is_some());
                assert!(!fixture.into_inner().is_empty());
            }
        }

        let unrelated = json!({
            "type": "permission.v2.asked",
            "properties": {
                "id": "per_unrelated",
                "sessionID": "ses_current",
                "action": "read",
                "resources": ["editable.txt"],
                "metadata": {"path": "editable.txt"},
                "save": [],
                "source": {
                    "type": "tool",
                    "messageID": "msg_a",
                    "callID": "call_a"
                }
            }
        });
        let frame = sse_frame(unrelated);
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));
        assert!(matches!(
            record_current_session_frame(
                &frame,
                "ses_current",
                Scenario::ToolCall,
                &sandbox,
                &mut fixture,
            ),
            Err(OpenCodeError::UnsafePermissionScope)
        ));
        assert!(fixture.into_inner().is_empty());
        Ok(())
    }

    #[test]
    fn read_and_edit_permission_requests_receive_once_reply(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let sandbox = test_sandbox_path();
        for (scenario, action, filename) in [
            (Scenario::ToolCall, "read", "notes.txt"),
            (Scenario::FileChange, "edit", "editable.txt"),
        ] {
            let asked = json!({
                "type": "permission.v2.asked",
                "properties": {
                    "id": format!("per_{action}"),
                    "sessionID": "ses_test",
                    "action": action,
                    "resources": [filename],
                    "metadata": {"path": filename},
                    "save": [],
                    "source": {
                        "type": "tool",
                        "messageID": "msg_a",
                        "callID": "call_a"
                    }
                }
            });
            let mut state = ObservationState::default();
            observe_next_tool_called(&mut state, "call_a", "msg_a");
            observe_next_tool_update(&mut state, "call_a", "msg_a", "input");
            observe_event(&asked, &mut state);
            let listener = TcpListener::bind((LOOPBACK, 0))?;
            let address = listener.local_addr()?;
            let server = serve_permission_reply(listener, PermissionProtocol::V2);
            let client = Client::builder().no_proxy().build()?;
            let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

            respond_to_control_event(
                ControlRequestContext {
                    client: &client,
                    base_url: &format!("http://{address}"),
                    scenario,
                    session_id: "ses_test",
                    sandbox: &sandbox,
                },
                &asked,
                &mut fixture,
                &mut state,
            )?;

            let path = server
                .join()
                .map_err(|_| io::Error::other("mock server thread panicked"))??;
            assert_eq!(
                path,
                format!("/api/session/ses_test/permission/per_{action}/reply")
            );
            assert!(state.permission.as_ref().is_some_and(|flow| {
                flow.reply_sent && flow.expected_reply.as_deref() == Some("once")
            }));
        }
        Ok(())
    }

    #[test]
    fn unsafe_permission_is_never_replied_to_even_for_deny(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir(&sandbox)?;
        let asked = json!({
            "type": "permission.asked",
            "properties": {
                "id": "per_unsafe",
                "sessionID": "ses_test",
                "permission": "bash",
                "patterns": ["cargo run; type C:\\private.txt"],
                "metadata": {},
                "always": [],
                "tool": {"messageID": "msg_a", "callID": "call_a"}
            }
        });
        for scenario in [Scenario::PermissionApprove, Scenario::PermissionDeny] {
            let mut state = ObservationState::default();
            observe_next_tool_called(&mut state, "call_a", "msg_a");
            observe_event(&asked, &mut state);
            let client = Client::builder().no_proxy().build()?;
            let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

            let result = respond_to_control_event(
                ControlRequestContext {
                    client: &client,
                    base_url: "http://127.0.0.1:1",
                    scenario,
                    session_id: "ses_test",
                    sandbox: &sandbox,
                },
                &asked,
                &mut fixture,
                &mut state,
            );

            assert!(matches!(result, Err(OpenCodeError::UnsafePermissionScope)));
            assert!(fixture.into_inner().is_empty());
            assert!(!state
                .permission
                .as_ref()
                .is_some_and(|flow| flow.reply_sent));
        }
        Ok(())
    }

    #[test]
    fn prompt_timeout_cannot_promote_complete_sse_state_to_recorded() {
        let state = ObservationState {
            event_count: 2,
            idle: true,
            assistant_text_ids: ["msg_a".to_owned()].into_iter().collect(),
            ..ObservationState::default()
        };
        assert!(state.is_complete(Scenario::SimpleTurn));

        let outcome = prompt_timeout_outcome(state, "ses_test".to_owned());

        assert!(matches!(
            outcome,
            Outcome::NotObserved { reason, .. } if reason.contains("timed out")
        ));
    }

    #[test]
    fn empty_assistant_text_never_completes_a_turn() {
        let mut part_state = ObservationState::default();
        observe_event(
            &json!({
                "type": "message.updated",
                "properties": {
                    "sessionID": "ses_test",
                    "info": {"id": "msg_assistant", "role": "assistant"}
                }
            }),
            &mut part_state,
        );
        observe_event(
            &json!({
                "type": "message.part.updated",
                "properties": {
                    "sessionID": "ses_test",
                    "part": {
                        "id": "prt_text",
                        "sessionID": "ses_test",
                        "messageID": "msg_assistant",
                        "type": "text",
                        "text": ""
                    }
                }
            }),
            &mut part_state,
        );
        observe_idle(&mut part_state);
        assert!(!part_state.is_complete(Scenario::SimpleTurn));

        let mut delta_state = ObservationState::default();
        observe_event(
            &json!({
                "type": "session.next.text.delta",
                "properties": {
                    "sessionID": "ses_test",
                    "assistantMessageID": "msg_assistant",
                    "delta": ""
                }
            }),
            &mut delta_state,
        );
        observe_idle(&mut delta_state);
        assert!(!delta_state.is_complete(Scenario::SimpleTurn));

        observe_event(
            &json!({
                "type": "session.next.text.delta",
                "properties": {
                    "sessionID": "ses_test",
                    "assistantMessageID": "msg_assistant",
                    "delta": "non-empty"
                }
            }),
            &mut delta_state,
        );
        observe_idle(&mut delta_state);
        assert!(delta_state.is_complete(Scenario::SimpleTurn));
    }

    #[test]
    fn next_tool_lifecycle_requires_same_ids_and_nonempty_update() {
        let mut missing_update = ObservationState::default();
        observe_next_tool_called(&mut missing_update, "call_a", "msg_a");
        observe_next_tool_terminal(
            &mut missing_update,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_next_text(&mut missing_update, "msg_a", "summary");
        observe_idle(&mut missing_update);
        assert!(!missing_update.is_complete(Scenario::ToolCall));

        let mut foreign_update = ObservationState::default();
        observe_next_tool_called(&mut foreign_update, "call_a", "msg_a");
        observe_next_tool_update(&mut foreign_update, "call_b", "msg_a", "input");
        observe_next_tool_terminal(
            &mut foreign_update,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_next_text(&mut foreign_update, "msg_a", "summary");
        observe_idle(&mut foreign_update);
        assert!(!foreign_update.is_complete(Scenario::ToolCall));

        let mut complete = ObservationState::default();
        observe_next_tool_called(&mut complete, "call_a", "msg_a");
        observe_next_tool_update(&mut complete, "call_a", "msg_a", "input");
        observe_next_tool_terminal(
            &mut complete,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_next_text(&mut complete, "msg_a", "summary");
        observe_idle(&mut complete);
        assert!(
            !complete.is_complete(Scenario::ToolCall),
            "a lifecycle without a validated permission scope must not be recorded"
        );
        confirm_once_permission(&mut complete, "call_a", "msg_a");
        assert!(complete.is_complete(Scenario::ToolCall));

        let mut mismatched_part = ObservationState::default();
        observe_assistant_message(&mut mismatched_part, "msg_a");
        observe_legacy_tool_part(
            &mut mismatched_part,
            "prt_a",
            "call_a",
            "msg_a",
            "running",
            json!({"title": "reading notes.txt"}),
        );
        observe_legacy_tool_part(
            &mut mismatched_part,
            "prt_b",
            "call_a",
            "msg_a",
            "completed",
            json!({}),
        );
        observe_next_text(&mut mismatched_part, "msg_a", "summary");
        observe_idle(&mut mismatched_part);
        assert!(!mismatched_part.is_complete(Scenario::ToolCall));
    }

    #[test]
    fn completion_rejects_extra_or_unmatched_tool_lifecycles() {
        let mut simple = ObservationState::default();
        observe_next_text(&mut simple, "msg_text", "answer");
        observe_next_tool_called(&mut simple, "call_extra", "msg_tool");
        observe_next_tool_update(&mut simple, "call_extra", "msg_tool", "input");
        observe_next_tool_terminal(
            &mut simple,
            "session.next.tool.success",
            "call_extra",
            "msg_tool",
            None,
        );
        observe_idle(&mut simple);
        assert!(!simple.is_complete(Scenario::SimpleTurn));

        let mut scoped = ObservationState::default();
        observe_next_tool_called(&mut scoped, "call_a", "msg_a");
        observe_next_tool_update(&mut scoped, "call_a", "msg_a", "input");
        confirm_once_permission(&mut scoped, "call_a", "msg_a");
        observe_next_tool_terminal(
            &mut scoped,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_next_text(&mut scoped, "msg_a", "summary");
        observe_next_tool_called(&mut scoped, "call_extra", "msg_extra");
        observe_next_tool_update(&mut scoped, "call_extra", "msg_extra", "input");
        observe_next_tool_terminal(
            &mut scoped,
            "session.next.tool.success",
            "call_extra",
            "msg_extra",
            None,
        );
        observe_idle(&mut scoped);
        assert!(!scoped.is_complete(Scenario::ToolCall));

        let mut orphan = ObservationState::default();
        observe_next_text(&mut orphan, "msg_text", "answer");
        observe_next_tool_terminal(
            &mut orphan,
            "session.next.tool.success",
            "call_orphan",
            "msg_orphan",
            None,
        );
        observe_idle(&mut orphan);
        assert!(orphan.unverified_tool_event);
        assert!(!orphan.is_complete(Scenario::SimpleTurn));
    }

    #[test]
    fn legacy_tool_lifecycle_requires_substantive_running_update() {
        let mut empty = ObservationState::default();
        observe_assistant_message(&mut empty, "msg_a");
        observe_legacy_tool_part(
            &mut empty,
            "prt_a",
            "call_a",
            "msg_a",
            "running",
            json!({"input": {}, "title": "", "metadata": {}}),
        );
        observe_legacy_tool_part(
            &mut empty,
            "prt_a",
            "call_a",
            "msg_a",
            "completed",
            json!({}),
        );
        observe_next_text(&mut empty, "msg_a", "summary");
        observe_idle(&mut empty);
        assert!(!empty.is_complete(Scenario::ToolCall));

        let mut complete = ObservationState::default();
        observe_assistant_message(&mut complete, "msg_a");
        observe_legacy_tool_part(
            &mut complete,
            "prt_a",
            "call_a",
            "msg_a",
            "running",
            json!({"input": {"path": "notes.txt"}, "title": "", "metadata": {}}),
        );
        observe_legacy_tool_part(
            &mut complete,
            "prt_a",
            "call_a",
            "msg_a",
            "completed",
            json!({}),
        );
        observe_next_text(&mut complete, "msg_a", "summary");
        observe_idle(&mut complete);
        assert!(
            !complete.is_complete(Scenario::ToolCall),
            "legacy lifecycle evidence cannot bypass permission scope validation"
        );
        confirm_once_permission(&mut complete, "call_a", "msg_a");
        assert!(complete.is_complete(Scenario::ToolCall));
    }

    #[test]
    fn error_and_file_change_require_one_complete_correlated_tool_lifecycle() {
        let mut error = ObservationState::default();
        observe_next_tool_called(&mut error, "call_a", "msg_a");
        observe_next_tool_update(&mut error, "call_b", "msg_a", "input");
        observe_next_tool_terminal(
            &mut error,
            "session.next.tool.failed",
            "call_a",
            "msg_a",
            Some("forced failure"),
        );
        observe_idle(&mut error);
        assert!(!error.is_complete(Scenario::Error));

        let mut complete_error = ObservationState::default();
        observe_next_tool_called(&mut complete_error, "call_a", "msg_a");
        observe_next_tool_update(&mut complete_error, "call_a", "msg_a", "input");
        observe_next_tool_terminal(
            &mut complete_error,
            "session.next.tool.failed",
            "call_a",
            "msg_a",
            Some("forced failure"),
        );
        observe_idle(&mut complete_error);
        assert!(
            !complete_error.is_complete(Scenario::Error),
            "a failed tool without validated command scope must not satisfy error"
        );
        confirm_once_permission(&mut complete_error, "call_a", "msg_a");
        assert!(complete_error.is_complete(Scenario::Error));

        let mut file_change = ObservationState {
            file_diff_evidence: true,
            file_changed_on_disk: true,
            idle: true,
            ..ObservationState::default()
        };
        observe_next_tool_called(&mut file_change, "call_a", "msg_a");
        observe_next_tool_terminal(
            &mut file_change,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        assert!(!file_change.is_complete(Scenario::FileChange));

        let mut complete_file_change = ObservationState {
            file_diff_evidence: true,
            file_changed_on_disk: true,
            idle: true,
            ..ObservationState::default()
        };
        observe_next_tool_called(&mut complete_file_change, "call_a", "msg_a");
        observe_next_tool_update(&mut complete_file_change, "call_a", "msg_a", "input");
        observe_next_tool_terminal(
            &mut complete_file_change,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_idle(&mut complete_file_change);
        assert!(
            !complete_file_change.is_complete(Scenario::FileChange),
            "a changed file without validated edit scope must not satisfy file-change"
        );
        confirm_once_permission(&mut complete_file_change, "call_a", "msg_a");
        assert!(complete_file_change.is_complete(Scenario::FileChange));
        assert!(diff_has_actual_changes(&json!([{
            "additions": 1,
            "deletions": 0,
            "patch": "@@ -1 +1 @@"
        }])));
        assert!(!diff_has_actual_changes(&json!([{
            "additions": 0,
            "deletions": 0,
            "patch": "@@ -1 +1 @@"
        }])));
    }

    #[test]
    fn file_change_compares_real_before_and_after_bytes() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let editable = temporary.path().join("editable.txt");
        std::fs::write(&editable, b"before\n")?;
        let before = std::fs::read(&editable)?;

        assert!(!file_changed_since(Some(&before), &editable)?);
        std::fs::write(&editable, b"after\n")?;
        assert!(file_changed_since(Some(&before), &editable)?);
        assert!(!file_changed_since(None, &editable)?);
        Ok(())
    }

    #[test]
    fn permission_v2_without_tool_source_is_not_answered_or_completed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut state = ObservationState::default();
        observe_next_tool_called(&mut state, "call_a", "msg_a");
        observe_next_tool_update(&mut state, "call_a", "msg_a", "input");
        let asked = json!({
            "type": "permission.v2.asked",
            "properties": {
                "id": "per_missing_source",
                "sessionID": "ses_test",
                "action": "bash",
                "resources": ["cargo run --"]
            }
        });
        observe_event(&asked, &mut state);
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = respond_to_control_event(
            ControlRequestContext {
                client: &client,
                base_url: "http://127.0.0.1:1",
                scenario: Scenario::PermissionApprove,
                session_id: "ses_test",
                sandbox: &test_sandbox_path(),
            },
            &asked,
            &mut fixture,
            &mut state,
        );
        assert!(matches!(result, Err(OpenCodeError::UnsafePermissionScope)));

        observe_next_tool_terminal(
            &mut state,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_idle(&mut state);
        let outcome = state.outcome(Scenario::PermissionApprove, "ses_test".to_owned());
        assert!(matches!(
            outcome,
            Outcome::NotObserved { reason, .. } if reason.contains("tool source")
        ));
        assert!(fixture.into_inner().is_empty());
        Ok(())
    }

    #[test]
    fn legacy_and_v2_permission_replies_use_their_pinned_routes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (protocol, event_type, expected_path) in [
            (
                PermissionProtocol::Legacy,
                "permission.asked",
                "/permission/per_route/reply",
            ),
            (
                PermissionProtocol::V2,
                "permission.v2.asked",
                "/api/session/ses_test/permission/per_route/reply",
            ),
        ] {
            let mut state = ObservationState::default();
            observe_next_tool_called(&mut state, "call_a", "msg_a");
            observe_next_tool_update(&mut state, "call_a", "msg_a", "input");
            let source_key = if protocol == PermissionProtocol::Legacy {
                "tool"
            } else {
                "source"
            };
            let mut source = json!({"messageID": "msg_a", "callID": "call_a"});
            if protocol == PermissionProtocol::V2 {
                source
                    .as_object_mut()
                    .ok_or_else(|| io::Error::other("source was not an object"))?
                    .insert("type".to_owned(), json!("tool"));
            }
            let mut properties = if protocol == PermissionProtocol::Legacy {
                json!({
                    "id": "per_route",
                    "sessionID": "ses_test",
                    "permission": "bash",
                    "patterns": ["cargo run --"],
                    "metadata": {},
                    "always": []
                })
            } else {
                json!({
                    "id": "per_route",
                    "sessionID": "ses_test",
                    "action": "bash",
                    "resources": ["cargo run --"],
                    "metadata": {},
                    "save": []
                })
            };
            properties
                .as_object_mut()
                .ok_or_else(|| io::Error::other("properties was not an object"))?
                .insert(source_key.to_owned(), source);
            let asked = json!({"type": event_type, "properties": properties});
            observe_event(&asked, &mut state);

            let listener = TcpListener::bind((LOOPBACK, 0))?;
            let address = listener.local_addr()?;
            let server = serve_permission_reply(listener, protocol);
            let client = Client::builder().no_proxy().build()?;
            let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));
            respond_to_control_event(
                ControlRequestContext {
                    client: &client,
                    base_url: &format!("http://{address}"),
                    scenario: Scenario::PermissionApprove,
                    session_id: "ses_test",
                    sandbox: &test_sandbox_path(),
                },
                &asked,
                &mut fixture,
                &mut state,
            )?;
            let path = server
                .join()
                .map_err(|_| io::Error::other("mock server thread panicked"))??;

            assert_eq!(path, expected_path);
            assert!(state
                .permission
                .as_ref()
                .is_some_and(|flow| flow.reply_sent));
            assert_eq!(String::from_utf8(fixture.into_inner())?.lines().count(), 2);
        }
        Ok(())
    }

    #[test]
    fn permission_completion_requires_matching_request_reply_and_target_terminal() {
        let mut state = ObservationState::default();
        observe_next_tool_called(&mut state, "call_a", "msg_a");
        observe_next_tool_update(&mut state, "call_a", "msg_a", "input");
        observe_event(
            &json!({
                "type": "permission.v2.asked",
                "properties": {
                    "id": "per_a",
                    "sessionID": "ses_test",
                    "source": {
                        "type": "tool",
                        "messageID": "msg_a",
                        "callID": "call_a"
                    }
                }
            }),
            &mut state,
        );
        state.mark_permission_reply_sent("per_a", "once");
        observe_event(
            &json!({
                "type": "permission.v2.replied",
                "properties": {
                    "sessionID": "ses_test",
                    "requestID": "per_b",
                    "reply": "once"
                }
            }),
            &mut state,
        );
        observe_next_tool_terminal(
            &mut state,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_idle(&mut state);
        assert!(!state.is_complete(Scenario::PermissionApprove));

        observe_event(
            &json!({
                "type": "permission.v2.replied",
                "properties": {
                    "sessionID": "ses_test",
                    "requestID": "per_a",
                    "reply": "once"
                }
            }),
            &mut state,
        );
        assert!(
            !state.is_complete(Scenario::PermissionApprove),
            "an unverified permission reply must permanently taint the observation"
        );

        let mut clean = ObservationState::default();
        observe_next_tool_called(&mut clean, "call_a", "msg_a");
        observe_next_tool_update(&mut clean, "call_a", "msg_a", "input");
        observe_event(
            &json!({
                "type": "permission.v2.asked",
                "properties": {
                    "id": "per_a",
                    "sessionID": "ses_test",
                    "source": {
                        "type": "tool",
                        "messageID": "msg_a",
                        "callID": "call_a"
                    }
                }
            }),
            &mut clean,
        );
        clean.mark_permission_reply_sent("per_a", "once");
        observe_event(
            &json!({
                "type": "permission.v2.replied",
                "properties": {
                    "sessionID": "ses_test",
                    "requestID": "per_a",
                    "reply": "once"
                }
            }),
            &mut clean,
        );
        observe_next_tool_terminal(
            &mut clean,
            "session.next.tool.success",
            "call_a",
            "msg_a",
            None,
        );
        observe_idle(&mut clean);
        assert!(clean.is_complete(Scenario::PermissionApprove));
    }

    #[test]
    fn question_legacy_and_v2_replies_use_distinct_routes() -> Result<(), Box<dyn std::error::Error>>
    {
        for (event_type, expected_path) in [
            ("question.asked", "/question/que_route/reply"),
            (
                "question.v2.asked",
                "/api/session/ses_test/question/que_route/reply",
            ),
        ] {
            let event = json!({
                "type": event_type,
                "properties": {
                    "id": "que_route",
                    "sessionID": "ses_test",
                    "questions": [{
                        "options": [{"label": "Red"}]
                    }]
                }
            });
            let mut state = ObservationState::default();
            observe_event(&event, &mut state);
            let listener = TcpListener::bind((LOOPBACK, 0))?;
            let address = listener.local_addr()?;
            let server = serve_json_responses(listener, vec!["true".to_owned()]);
            let client = Client::builder().no_proxy().build()?;
            let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

            respond_to_control_event(
                ControlRequestContext {
                    client: &client,
                    base_url: &format!("http://{address}"),
                    scenario: Scenario::Elicitation,
                    session_id: "ses_test",
                    sandbox: &test_sandbox_path(),
                },
                &event,
                &mut fixture,
                &mut state,
            )?;
            let paths = server
                .join()
                .map_err(|_| io::Error::other("mock server thread panicked"))??;

            assert_eq!(paths, [expected_path]);
            assert!(state.question_replied);
        }
        Ok(())
    }

    #[test]
    fn rejected_legacy_question_reply_does_not_mark_the_question_answered(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let event = json!({
            "type": "question.asked",
            "properties": {
                "id": "que_rejected",
                "sessionID": "ses_test",
                "questions": [{
                    "options": [{"label": "Red"}]
                }]
            }
        });
        let mut state = ObservationState::default();
        observe_event(&event, &mut state);
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let address = listener.local_addr()?;
        let server = serve_json_responses(listener, vec!["false".to_owned()]);
        let client = Client::builder().no_proxy().build()?;
        let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let result = respond_to_control_event(
            ControlRequestContext {
                client: &client,
                base_url: &format!("http://{address}"),
                scenario: Scenario::Elicitation,
                session_id: "ses_test",
                sandbox: &test_sandbox_path(),
            },
            &event,
            &mut fixture,
            &mut state,
        );
        server
            .join()
            .map_err(|_| io::Error::other("mock server thread panicked"))??;

        assert!(matches!(
            result,
            Err(OpenCodeError::Protocol(
                "legacy question reply endpoint did not accept the reply"
            ))
        ));
        assert!(!state.question_replied);
        Ok(())
    }

    #[test]
    fn user_text_does_not_complete_simple_turn() {
        let mut state = ObservationState::default();
        observe_event(
            &json!({
                "type": "message.updated",
                "properties": {
                    "sessionID": "ses_test",
                    "info": {"id": "msg_user", "role": "user"}
                }
            }),
            &mut state,
        );
        observe_event(
            &json!({
                "type": "message.part.updated",
                "properties": {
                    "sessionID": "ses_test",
                    "part": {
                        "id": "prt_user",
                        "sessionID": "ses_test",
                        "messageID": "msg_user",
                        "type": "text",
                        "text": "the prompt"
                    },
                    "time": 1
                }
            }),
            &mut state,
        );
        observe_event(
            &json!({
                "type": "session.idle",
                "properties": {"sessionID": "ses_test"}
            }),
            &mut state,
        );
        assert!(matches!(
            state.outcome(Scenario::SimpleTurn, "ses_test".to_owned()),
            Outcome::NotObserved { .. }
        ));
    }

    #[test]
    fn provider_error_without_tool_call_does_not_complete_error_scenario() {
        let mut state = ObservationState::default();
        observe_event(
            &json!({
                "type": "session.error",
                "properties": {
                    "sessionID": "ses_test",
                    "error": {"name": "ProviderAuthError", "data": {}}
                }
            }),
            &mut state,
        );
        observe_event(
            &json!({
                "type": "session.idle",
                "properties": {"sessionID": "ses_test"}
            }),
            &mut state,
        );
        assert!(matches!(
            state.outcome(Scenario::Error, "ses_test".to_owned()),
            Outcome::NotObserved { .. }
        ));
    }

    #[test]
    fn accepted_abort_without_terminal_evidence_does_not_complete_cancel() {
        let mut state = ObservationState::default();
        assert!(state.observe_tool_start("call_target", "msg_target", None));
        assert!(state.observe_tool_update("call_target", "msg_target", None));
        assert!(state.prepare_cancel_target());
        state.abort_sent = true;
        assert!(matches!(
            state.outcome(Scenario::Cancel, "ses_test".to_owned()),
            Outcome::NotObserved { .. }
        ));
    }

    #[test]
    fn cancel_requires_aborted_evidence_for_the_selected_tool_message() {
        let mut state = ObservationState::default();
        assert!(state.observe_tool_start("call_target", "msg_target", None));
        assert!(state.observe_tool_update("call_target", "msg_target", None));
        assert!(state.prepare_cancel_target());
        state.abort_sent = true;

        observe_prompt_abort(
            &json!({
                "info": {
                    "id": "msg_foreign",
                    "error": {"name": "MessageAbortedError"}
                }
            }),
            &mut state,
        );
        observe_event(
            &json!({
                "type": "session.error",
                "properties": {
                    "sessionID": "ses_test",
                    "error": {"name": "MessageAbortedError"}
                }
            }),
            &mut state,
        );
        assert!(!state.is_complete(Scenario::Cancel));

        confirm_once_permission(&mut state, "call_target", "msg_target");
        observe_prompt_abort(
            &json!({
                "info": {
                    "id": "msg_target",
                    "error": {"name": "MessageAbortedError"}
                }
            }),
            &mut state,
        );
        assert!(state.is_complete(Scenario::Cancel));

        assert!(state.observe_tool_start("call_extra", "msg_extra", None));
        assert!(state.observe_tool_update("call_extra", "msg_extra", None));
        assert!(state.observe_tool_terminal(
            "call_extra",
            "msg_extra",
            None,
            ToolTerminal::Succeeded
        ));
        assert!(
            !state.is_complete(Scenario::Cancel),
            "a second lifecycle must invalidate the selected cancel target"
        );
    }

    #[test]
    fn temporary_git_root_is_created_and_removed_by_scope() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let git = sandbox.join(".git");

        {
            let root = TemporaryGitRoot::prepare(&sandbox)?;
            assert_eq!(root.origin, GitRootOrigin::RecorderCreated);
            assert!(git.is_dir());
        }

        assert!(!git.exists());
        Ok(())
    }

    #[test]
    fn temporary_git_root_is_removed_after_injected_recording_failure(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let git = sandbox.join(".git");
        let root = TemporaryGitRoot::prepare(&sandbox)?;

        let result = root.finish::<()>(Err(OpenCodeError::Protocol("injected recording failure")));

        assert!(matches!(
            result,
            Err(OpenCodeError::Protocol("injected recording failure"))
        ));
        assert!(!git.exists());
        Ok(())
    }

    #[test]
    fn temporary_git_root_is_removed_while_unwinding() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let git = sandbox.join(".git");
        let root = TemporaryGitRoot::prepare(&sandbox)?;

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _root = root;
            std::panic::resume_unwind(Box::new("injected unwind"));
        }));

        assert!(unwind.is_err());
        assert!(!git.exists());
        Ok(())
    }

    #[test]
    fn pre_existing_git_root_is_diagnosed_and_preserved() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let git = sandbox.join(".git");
        std::fs::create_dir_all(&git)?;
        std::fs::write(git.join("preserved.marker"), b"pre-existing\n")?;
        let sandbox = sandbox.canonicalize()?;

        {
            let root = TemporaryGitRoot::prepare(&sandbox)?;
            assert_eq!(root.origin, GitRootOrigin::PreExisting);
            assert_eq!(
                root.origin.diagnostic(),
                "OpenCode fixture project root: .git=pre-existing; cleanup=preserved"
            );
        }

        assert_eq!(
            std::fs::read(git.join("preserved.marker"))?,
            b"pre-existing\n"
        );
        Ok(())
    }

    #[test]
    fn current_project_preflight_accepts_only_the_exact_sandbox_root(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&sandbox)?;
        std::fs::create_dir_all(&outside)?;
        let sandbox = sandbox.canonicalize()?;
        let outside = outside.canonicalize()?;

        for (reported, accepted) in [(&sandbox, true), (&outside, false)] {
            let listener = TcpListener::bind((LOOPBACK, 0))?;
            let address = listener.local_addr()?;
            let server = serve_json_responses(
                listener,
                vec![json!({"id": "project-test", "worktree": reported}).to_string()],
            );
            let client = Client::builder().no_proxy().build()?;

            let result = validate_current_project_root(
                &client,
                &format!("http://{address}"),
                &sandbox,
                &CancellationToken::default(),
            );
            let paths = server
                .join()
                .map_err(|_| io::Error::other("mock server thread panicked"))??;

            assert_eq!(paths, ["/project/current"]);
            assert_eq!(result.is_ok(), accepted);
            if !accepted {
                assert!(matches!(result, Err(OpenCodeError::ProjectIsolation)));
            }
        }
        Ok(())
    }

    #[test]
    fn interrupt_listener_bridge_sets_the_cancellation_token(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cancellation = CancellationToken::default();
        start_interrupt_listener_with(cancellation.clone(), std::future::ready(Ok(())))?;
        let deadline = Instant::now() + Duration::from_secs(1);
        while cancellation.check().is_ok() && Instant::now() < deadline {
            thread::yield_now();
        }

        assert!(matches!(
            cancellation.check(),
            Err(OpenCodeError::Interrupted)
        ));
        Ok(())
    }

    #[test]
    fn cancellation_poll_unwinds_and_removes_the_temporary_git_root(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;
        let git = sandbox.join(".git");
        let root = TemporaryGitRoot::prepare(&sandbox)?;
        let (sender, receiver) = mpsc::channel::<()>();
        let cancellation = CancellationToken::default();
        let trigger = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            trigger.cancel();
        });

        let receive = recv_with_cancellation(
            &receiver,
            Instant::now() + Duration::from_secs(1),
            &cancellation,
        );
        drop(sender);
        canceller
            .join()
            .map_err(|_| io::Error::other("cancellation thread panicked"))?;
        let result = root.finish(receive.map(drop));

        assert!(matches!(result, Err(OpenCodeError::Interrupted)));
        assert!(!git.exists());
        Ok(())
    }

    #[test]
    fn prompt_project_isolation_requires_an_ordinary_dot_git_directory(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        std::fs::create_dir_all(&sandbox)?;
        let sandbox = sandbox.canonicalize()?;

        assert!(matches!(
            validate_prompt_project_isolation(&sandbox),
            Err(OpenCodeError::ProjectIsolation)
        ));
        std::fs::write(sandbox.join(".git"), "gitdir: outside")?;
        assert!(matches!(
            validate_prompt_project_isolation(&sandbox),
            Err(OpenCodeError::ProjectIsolation)
        ));
        std::fs::remove_file(sandbox.join(".git"))?;
        std::fs::create_dir(sandbox.join(".git"))?;
        assert!(validate_prompt_project_isolation(&sandbox).is_ok());
        Ok(())
    }

    #[test]
    fn prompt_project_isolation_rejects_a_reparse_dot_git() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let target = temporary.path().join("foreign-git");
        std::fs::create_dir_all(&sandbox)?;
        std::fs::create_dir_all(&target)?;
        platform::create_test_directory_link(&target, &sandbox.join(".git"))?;
        let sandbox = sandbox.canonicalize()?;

        assert!(matches!(
            validate_prompt_project_isolation(&sandbox),
            Err(OpenCodeError::ProjectIsolation)
        ));
        Ok(())
    }

    #[test]
    fn sandbox_validation_rejects_an_unrelated_matching_suffix(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("tests/fixtures/sandbox");
        std::fs::create_dir_all(&sandbox)?;

        assert!(matches!(
            validate_fixture_sandbox(&sandbox),
            Err(OpenCodeError::InvalidSandbox)
        ));
        Ok(())
    }

    #[test]
    fn sandbox_validation_rejects_a_linked_expected_root() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let target = temporary.path().join("real-project");
        let expected = temporary.path().join("tests/fixtures/sandbox");
        std::fs::create_dir_all(&target)?;
        std::fs::create_dir_all(
            expected
                .parent()
                .ok_or("linked sandbox must have a parent")?,
        )?;
        platform::create_test_directory_link(&target, &expected)?;

        assert!(matches!(
            validate_fixture_sandbox_against(&target, &expected),
            Err(OpenCodeError::InvalidSandbox)
        ));
        Ok(())
    }

    #[test]
    fn sandbox_validation_accepts_only_the_repository_fixture_sandbox(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("workspace root missing"))?
            .join("tests")
            .join("fixtures")
            .join("sandbox")
            .canonicalize()?;

        assert_eq!(validate_fixture_sandbox(&expected)?, expected);
        Ok(())
    }

    fn read_request(stream: &mut TcpStream) -> io::Result<()> {
        read_request_path(stream).map(|_| ())
    }

    fn observe_idle(state: &mut ObservationState) {
        observe_event(
            &json!({
                "type": "session.idle",
                "properties": {"sessionID": "ses_test"}
            }),
            state,
        );
    }

    fn confirm_once_permission(state: &mut ObservationState, call_id: &str, message_id: &str) {
        state.permission = Some(PermissionFlow {
            protocol: PermissionProtocol::V2,
            request_id: "per_test".to_owned(),
            message_id: message_id.to_owned(),
            call_id: call_id.to_owned(),
            expected_reply: Some("once".to_owned()),
            reply_sent: true,
            reply_confirmed: true,
        });
    }

    fn observe_assistant_message(state: &mut ObservationState, message_id: &str) {
        observe_event(
            &json!({
                "type": "message.updated",
                "properties": {
                    "sessionID": "ses_test",
                    "info": {"id": message_id, "role": "assistant"}
                }
            }),
            state,
        );
    }

    fn observe_next_text(state: &mut ObservationState, message_id: &str, delta: &str) {
        observe_event(
            &json!({
                "type": "session.next.text.delta",
                "properties": {
                    "sessionID": "ses_test",
                    "assistantMessageID": message_id,
                    "delta": delta
                }
            }),
            state,
        );
    }

    fn observe_next_tool_called(state: &mut ObservationState, call_id: &str, message_id: &str) {
        observe_event(
            &json!({
                "type": "session.next.tool.called",
                "properties": {
                    "sessionID": "ses_test",
                    "assistantMessageID": message_id,
                    "callID": call_id
                }
            }),
            state,
        );
    }

    fn observe_next_tool_update(
        state: &mut ObservationState,
        call_id: &str,
        message_id: &str,
        delta: &str,
    ) {
        observe_event(
            &json!({
                "type": "session.next.tool.input.delta",
                "properties": {
                    "sessionID": "ses_test",
                    "assistantMessageID": message_id,
                    "callID": call_id,
                    "delta": delta
                }
            }),
            state,
        );
    }

    fn observe_next_tool_terminal(
        state: &mut ObservationState,
        event_type: &str,
        call_id: &str,
        message_id: &str,
        error: Option<&str>,
    ) {
        let properties = error.map_or_else(
            || {
                json!({
                    "sessionID": "ses_test",
                    "assistantMessageID": message_id,
                    "callID": call_id
                })
            },
            |error| {
                json!({
                    "sessionID": "ses_test",
                    "assistantMessageID": message_id,
                    "callID": call_id,
                    "error": {"message": error}
                })
            },
        );
        observe_event(
            &json!({"type": event_type, "properties": properties}),
            state,
        );
    }

    fn observe_legacy_tool_part(
        state: &mut ObservationState,
        part_id: &str,
        call_id: &str,
        message_id: &str,
        status: &str,
        tool_state: Value,
    ) {
        let tool_state = match tool_state {
            Value::Object(mut fields) => {
                fields.insert("status".to_owned(), json!(status));
                Value::Object(fields)
            }
            _ => json!({"status": status}),
        };
        observe_event(
            &json!({
                "type": "message.part.updated",
                "properties": {
                    "sessionID": "ses_test",
                    "part": {
                        "id": part_id,
                        "sessionID": "ses_test",
                        "messageID": message_id,
                        "callID": call_id,
                        "type": "tool",
                        "state": tool_state
                    }
                }
            }),
            state,
        );
    }

    fn sse_frame(value: Value) -> SseFrame {
        let data = value.to_string();
        SseFrame {
            raw: format!("data: {data}\n\n"),
            data,
        }
    }

    fn read_request_path(stream: &mut TcpStream) -> io::Result<String> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer)?;
        let request = std::str::from_utf8(
            buffer
                .get(..read)
                .ok_or_else(|| io::Error::other("mock request length was invalid"))?,
        )
        .map_err(|_| io::Error::other("mock request was not UTF-8"))?;
        request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("mock request line did not contain a path"))
    }

    fn serve_permission_reply(
        listener: TcpListener,
        protocol: PermissionProtocol,
    ) -> thread::JoinHandle<io::Result<String>> {
        thread::spawn(move || {
            let (mut stream, _) = listener.accept()?;
            let path = read_request_path(&mut stream)?;
            let response = if protocol == PermissionProtocol::Legacy {
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: 4\r\n\
                 Connection: close\r\n\r\n\
                 true"
            } else {
                "HTTP/1.1 204 No Content\r\n\
                 Content-Length: 0\r\n\
                 Connection: close\r\n\r\n"
            };
            stream.write_all(response.as_bytes())?;
            Ok(path)
        })
    }

    fn serve_json_responses(
        listener: TcpListener,
        responses: Vec<String>,
    ) -> thread::JoinHandle<io::Result<Vec<String>>> {
        thread::spawn(move || {
            let mut paths = Vec::new();
            for body in responses {
                let (mut stream, _) = listener.accept()?;
                paths.push(read_request_path(&mut stream)?);
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n\
                     {body}",
                    body.len()
                );
                stream.write_all(response.as_bytes())?;
            }
            Ok(paths)
        })
    }
}
