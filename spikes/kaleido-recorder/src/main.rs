use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use kaleido_recorder::agents::acp::{self, AcpScenario, ScenarioOutcome as AcpOutcome};
use kaleido_recorder::agents::codex::{self, CodexRecording, CodexScenario};
use kaleido_recorder::agents::opencode::{
    self, Outcome as OpenCodeOutcome, Scenario as OpenCodeScenario,
};
use kaleido_recorder::auth;
use kaleido_recorder::fixture::FixtureSink;
use kaleido_recorder::platform::{
    self, DiscoveryLayer, DiscoveryTarget, ProbeStatus, ResolvedExecutable, RuntimeCandidateProbe,
    RuntimeProbe,
};
use kaleido_recorder::redact::Redactor;
use tempfile::{Builder as TempDirBuilder, NamedTempFile, TempDir};

const DISCOVER_USAGE: &str = "kaleido-recorder discover \
    [--codex <absolute-path>] [--opencode <absolute-path>] \
    [--claude <absolute-path>] [--claude-acp <absolute-path>] \
    [--node <absolute-path>] [--bundled-claude-acp <absolute-path>] \
    [--bundled-claude <absolute-path>]";
const RECORD_USAGE: &str = "kaleido-recorder <codex|acp|opencode> <scenario> \
    [--executable <absolute-path>] [--bundled-executable <absolute-path>] \
    [--timeout-secs <seconds>] \
    [--thread-id <codex-thread-id>] [--session-id <acp-session-id>]\n\
    scenarios: simple-turn, tool-call, permission-approve, permission-deny, \
    file-change, cancel, error, session-load, elicitation";

#[derive(Debug)]
enum RecorderError {
    Usage(String),
    WorkspaceRoot,
    Io(io::Error),
    Agent(Box<dyn Error>),
    Discovery(String),
    ExistingFixture(String),
    NotObserved(String),
}

impl fmt::Display for RecorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(reason) => {
                write!(formatter, "{reason}\n\n{DISCOVER_USAGE}\n{RECORD_USAGE}")
            }
            Self::WorkspaceRoot => formatter.write_str("could not resolve workspace root"),
            Self::Io(error) => write!(formatter, "I/O failure: {error}"),
            Self::Agent(error) => write!(formatter, "agent protocol attempt failed: {error}"),
            Self::Discovery(reason) => write!(formatter, "agent discovery failed: {reason}"),
            Self::ExistingFixture(path) => {
                write!(formatter, "refusing to overwrite existing fixture {path}")
            }
            Self::NotObserved(reason) => {
                write!(
                    formatter,
                    "scenario was attempted but not recorded: {reason}"
                )
            }
        }
    }
}

impl Error for RecorderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Agent(error) => Some(error.as_ref()),
            Self::Usage(_)
            | Self::WorkspaceRoot
            | Self::Discovery(_)
            | Self::ExistingFixture(_)
            | Self::NotObserved(_) => None,
        }
    }
}

impl From<io::Error> for RecorderError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentKind {
    Codex,
    Acp,
    OpenCode,
}

impl AgentKind {
    fn parse(value: &str) -> Result<Self, RecorderError> {
        match value {
            "codex" => Ok(Self::Codex),
            "acp" | "acp-claude" => Ok(Self::Acp),
            "opencode" => Ok(Self::OpenCode),
            _ => Err(RecorderError::Usage(format!("unknown agent `{value}`"))),
        }
    }

    const fn target(self) -> DiscoveryTarget {
        match self {
            Self::Codex => DiscoveryTarget::Codex,
            Self::Acp => DiscoveryTarget::ClaudeAcp,
            Self::OpenCode => DiscoveryTarget::OpenCode,
        }
    }

    const fn fixture_directory(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Acp => "acp-claude",
            Self::OpenCode => "opencode",
        }
    }

    const fn executable_environment_variable(self) -> &'static str {
        match self {
            Self::Codex => "KALEIDO_CODEX_EXECUTABLE",
            Self::Acp => "KALEIDO_CLAUDE_ACP_EXECUTABLE",
            Self::OpenCode => "KALEIDO_OPENCODE_EXECUTABLE",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scenario {
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
    fn parse(value: &str) -> Result<Self, RecorderError> {
        let without_prefix = value
            .strip_prefix("01-")
            .or_else(|| value.strip_prefix("02-"))
            .or_else(|| value.strip_prefix("03-"))
            .or_else(|| value.strip_prefix("04-"))
            .or_else(|| value.strip_prefix("05-"))
            .or_else(|| value.strip_prefix("06-"))
            .or_else(|| value.strip_prefix("07-"))
            .or_else(|| value.strip_prefix("08-"))
            .or_else(|| value.strip_prefix("09-"))
            .unwrap_or(value);
        let normalized = without_prefix
            .strip_suffix(".jsonl")
            .unwrap_or(without_prefix);
        match normalized {
            "simple-turn" => Ok(Self::SimpleTurn),
            "tool-call" => Ok(Self::ToolCall),
            "permission-approve" => Ok(Self::PermissionApprove),
            "permission-deny" => Ok(Self::PermissionDeny),
            "file-change" => Ok(Self::FileChange),
            "cancel" => Ok(Self::Cancel),
            "error" => Ok(Self::Error),
            "session-load" => Ok(Self::SessionLoad),
            "elicitation" => Ok(Self::Elicitation),
            _ => Err(RecorderError::Usage(format!(
                "unknown recording scenario `{value}`"
            ))),
        }
    }

    const fn file_name(self) -> &'static str {
        match self {
            Self::SimpleTurn => "01-simple-turn.jsonl",
            Self::ToolCall => "02-tool-call.jsonl",
            Self::PermissionApprove => "03-permission-approve.jsonl",
            Self::PermissionDeny => "04-permission-deny.jsonl",
            Self::FileChange => "05-file-change.jsonl",
            Self::Cancel => "06-cancel.jsonl",
            Self::Error => "07-error.jsonl",
            Self::SessionLoad => "08-session-load.jsonl",
            Self::Elicitation => "09-elicitation.jsonl",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::SimpleTurn => "simple-turn",
            Self::ToolCall => "tool-call",
            Self::PermissionApprove => "permission-approve",
            Self::PermissionDeny => "permission-deny",
            Self::FileChange => "file-change",
            Self::Cancel => "cancel",
            Self::Error => "error",
            Self::SessionLoad => "session-load",
            Self::Elicitation => "elicitation",
        }
    }
}

#[derive(Debug, Default)]
struct DiscoverArguments {
    codex: Option<PathBuf>,
    opencode: Option<PathBuf>,
    claude: Option<PathBuf>,
    claude_acp: Option<PathBuf>,
    node: Option<PathBuf>,
    bundled_claude_acp: Option<PathBuf>,
    bundled_claude: Option<PathBuf>,
}

#[derive(Debug)]
struct RecordArguments {
    executable: Option<PathBuf>,
    bundled_executable: Option<PathBuf>,
    scenario: Scenario,
    thread_id: Option<String>,
    session_id: Option<String>,
    timeout: Duration,
}

#[derive(Debug)]
struct SandboxState {
    root: PathBuf,
    baseline: PathBuf,
    guard: Option<TempDir>,
}

impl SandboxState {
    fn capture(sandbox: &Path, expected_canonical_root: &Path) -> io::Result<Self> {
        if platform::path_is_link_or_reparse(sandbox)? {
            return Err(io::Error::other(
                "fixture sandbox root must not be a link or reparse point",
            ));
        }
        let root = sandbox.canonicalize()?;
        if root != expected_canonical_root || platform::path_is_link_or_reparse(sandbox)? {
            return Err(io::Error::other(
                "fixture sandbox root did not resolve to the expected workspace path",
            ));
        }
        validate_sandbox_directory(&root)?;
        let parent = root
            .parent()
            .ok_or_else(|| io::Error::other("fixture sandbox must have a parent directory"))?;
        let guard = TempDirBuilder::new()
            .prefix(".kaleido-sandbox-guard-")
            .tempdir_in(parent)?;
        let baseline = guard.path().join("baseline");
        let working_copy = guard.path().join("working-copy");

        fs::rename(&root, &baseline)?;
        if let Err(copy_error) = copy_sandbox_directory(&baseline, &working_copy) {
            let cleanup_error = remove_tree_if_present(&working_copy).err();
            return rollback_capture(root, baseline, guard, copy_error, cleanup_error);
        }
        if let Err(rename_error) = fs::rename(&working_copy, &root) {
            let cleanup_error = remove_tree_if_present(&working_copy).err();
            return rollback_capture(root, baseline, guard, rename_error, cleanup_error);
        }

        Ok(Self {
            root,
            baseline,
            guard: Some(guard),
        })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn restore(mut self) -> io::Result<()> {
        self.restore_in_place()
    }

    fn restore_in_place(&mut self) -> io::Result<()> {
        let Some(guard) = self.guard.take() else {
            return Ok(());
        };
        let quarantine = guard.path().join("quarantine");

        let quarantined = match fs::rename(&self.root, &quarantine) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                return retain_guard_after_failure(
                    guard,
                    error,
                    "could not quarantine the sandbox working copy",
                );
            }
        };

        if let Err(restore_error) = fs::rename(&self.baseline, &self.root) {
            let rollback_error = if quarantined {
                fs::rename(&quarantine, &self.root).err()
            } else {
                None
            };
            let detail = match rollback_error {
                Some(error) => format!(
                    "could not atomically restore the sandbox baseline; restoring the working \
                     copy also failed: {error}"
                ),
                None => "could not atomically restore the sandbox baseline".to_owned(),
            };
            return retain_guard_after_failure(guard, restore_error, &detail);
        }

        if quarantined {
            // On the supported Windows and Unix targets, std::fs::remove_dir_all does not
            // follow symlinks and its implementation is resistant to symlink TOCTOU races.
            // Never replace this with a metadata-then-recursion implementation.
            if let Err(error) = fs::remove_dir_all(&quarantine) {
                return retain_guard_after_failure(
                    guard,
                    error,
                    "the original sandbox was restored, but quarantine cleanup failed",
                );
            }
        }
        guard.close()
    }
}

impl Drop for SandboxState {
    fn drop(&mut self) {
        if self.guard.is_some() {
            let _restore_result = self.restore_in_place();
        }
    }
}

fn rollback_capture(
    root: PathBuf,
    baseline: PathBuf,
    guard: TempDir,
    operation_error: io::Error,
    cleanup_error: Option<io::Error>,
) -> io::Result<SandboxState> {
    if fs::symlink_metadata(&root).is_ok() {
        let detail = cleanup_error.map_or_else(
            || "sandbox capture failed and its original path was unexpectedly occupied".to_owned(),
            |error| {
                format!(
                    "sandbox capture failed, its original path was unexpectedly occupied, and \
                     staging cleanup failed: {error}"
                )
            },
        );
        return retain_guard_after_failure(guard, operation_error, &detail);
    }

    if let Err(rollback_error) = fs::rename(&baseline, &root) {
        return retain_guard_after_failure(
            guard,
            operation_error,
            &format!("sandbox capture failed and rollback also failed: {rollback_error}"),
        );
    }

    if let Some(cleanup_error) = cleanup_error {
        return retain_guard_after_failure(
            guard,
            operation_error,
            &format!(
                "sandbox capture failed and the original was restored, but staging cleanup \
                 failed: {cleanup_error}"
            ),
        );
    }
    Err(operation_error)
}

fn retain_guard_after_failure<T>(
    guard: TempDir,
    source: io::Error,
    context: &str,
) -> io::Result<T> {
    let _retained_guard = guard.keep();
    Err(io::Error::new(
        source.kind(),
        format!("{context}: {source}; private recovery guard retained"),
    ))
}

fn validate_sandbox_directory(directory: &Path) -> io::Result<()> {
    let mut children = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        if platform::path_is_link_or_reparse(&path)? {
            return Err(io::Error::other(
                "fixture sandbox baseline must not contain links or reparse points",
            ));
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            validate_sandbox_directory(&path)?;
        } else if !metadata.is_file() {
            return Err(io::Error::other(
                "fixture sandbox baseline contains an unsupported entry type",
            ));
        }
    }
    Ok(())
}

fn copy_sandbox_directory(source: &Path, destination: &Path) -> io::Result<()> {
    if platform::path_is_link_or_reparse(source)? {
        return Err(io::Error::other(
            "fixture sandbox baseline changed into a link or reparse point",
        ));
    }
    let source_metadata = fs::symlink_metadata(source)?;
    if !source_metadata.is_dir() {
        return Err(io::Error::other(
            "fixture sandbox baseline directory changed type",
        ));
    }

    fs::create_dir(destination)?;
    let mut children = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let source_path = child.path();
        if platform::path_is_link_or_reparse(&source_path)? {
            return Err(io::Error::other(
                "fixture sandbox baseline changed to contain a link or reparse point",
            ));
        }
        let destination_path = destination.join(child.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.is_dir() {
            copy_sandbox_directory(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path)?;
            if platform::path_is_link_or_reparse(&source_path)? {
                return Err(io::Error::other(
                    "fixture sandbox baseline file changed into a link or reparse point",
                ));
            }
        } else {
            return Err(io::Error::other(
                "fixture sandbox baseline contains an unsupported entry type",
            ));
        }
    }
    fs::set_permissions(destination, source_metadata.permissions())
}

fn remove_tree_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kaleido-recorder: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), RecorderError> {
    let mut arguments = env::args_os().skip(1);
    let command = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| RecorderError::Usage("missing recorder command".to_owned()))?;
    if command == "discover" {
        return run_discover(parse_discover_arguments(arguments)?);
    }
    let agent = if command == "record" {
        let value = arguments
            .next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| RecorderError::Usage("record requires an agent".to_owned()))?;
        AgentKind::parse(&value)?
    } else {
        AgentKind::parse(&command)?
    };
    run_record(agent, parse_record_arguments(arguments)?)
}

fn parse_record_arguments(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<RecordArguments, RecorderError> {
    let scenario = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| RecorderError::Usage("record requires a scenario".to_owned()))?;
    let mut parsed = RecordArguments {
        executable: None,
        bundled_executable: None,
        scenario: Scenario::parse(&scenario)?,
        thread_id: None,
        session_id: None,
        timeout: Duration::from_secs(300),
    };
    while let Some(flag) = arguments.next() {
        let flag = flag
            .into_string()
            .map_err(|_| RecorderError::Usage("option name was not valid Unicode".to_owned()))?;
        let value = arguments
            .next()
            .ok_or_else(|| RecorderError::Usage(format!("option `{flag}` requires a value")))?;
        match flag.as_str() {
            "--executable" => {
                parsed.executable = Some(require_absolute(PathBuf::from(value), "--executable")?);
            }
            "--bundled-executable" => {
                parsed.bundled_executable = Some(require_absolute(
                    PathBuf::from(value),
                    "--bundled-executable",
                )?);
            }
            "--thread-id" => {
                parsed.thread_id = Some(value.into_string().map_err(|_| {
                    RecorderError::Usage("thread id was not valid Unicode".to_owned())
                })?);
            }
            "--session-id" => {
                parsed.session_id = Some(value.into_string().map_err(|_| {
                    RecorderError::Usage("session id was not valid Unicode".to_owned())
                })?);
            }
            "--timeout-secs" => {
                let value = value.into_string().map_err(|_| {
                    RecorderError::Usage("timeout was not valid Unicode".to_owned())
                })?;
                let seconds = value.parse::<u64>().map_err(|_| {
                    RecorderError::Usage("timeout must be an integer number of seconds".to_owned())
                })?;
                if seconds == 0 {
                    return Err(RecorderError::Usage(
                        "timeout must be greater than zero".to_owned(),
                    ));
                }
                parsed.timeout = Duration::from_secs(seconds);
            }
            _ => {
                return Err(RecorderError::Usage(format!(
                    "unknown record option `{flag}`"
                )));
            }
        }
    }
    Ok(parsed)
}

fn parse_discover_arguments(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<DiscoverArguments, RecorderError> {
    let mut parsed = DiscoverArguments {
        codex: absolute_environment_path("KALEIDO_CODEX_EXECUTABLE")?,
        opencode: absolute_environment_path("KALEIDO_OPENCODE_EXECUTABLE")?,
        claude: absolute_environment_path("KALEIDO_CLAUDE_EXECUTABLE")?,
        claude_acp: absolute_environment_path("KALEIDO_CLAUDE_ACP_EXECUTABLE")?,
        node: absolute_environment_path("KALEIDO_NODE_EXECUTABLE")?,
        bundled_claude_acp: absolute_environment_path("KALEIDO_BUNDLED_CLAUDE_ACP")?,
        bundled_claude: absolute_environment_path("KALEIDO_BUNDLED_CLAUDE_EXECUTABLE")?,
    };
    while let Some(flag) = arguments.next() {
        let flag = flag
            .into_string()
            .map_err(|_| RecorderError::Usage("option name was not valid Unicode".to_owned()))?;
        let value = arguments.next().ok_or_else(|| {
            RecorderError::Usage(format!("option `{flag}` requires an absolute path"))
        })?;
        let path = require_absolute(PathBuf::from(value), &flag)?;
        match flag.as_str() {
            "--codex" => parsed.codex = Some(path),
            "--opencode" => parsed.opencode = Some(path),
            "--claude" => parsed.claude = Some(path),
            "--claude-acp" => parsed.claude_acp = Some(path),
            "--node" => parsed.node = Some(path),
            "--bundled-claude-acp" => parsed.bundled_claude_acp = Some(path),
            "--bundled-claude" => parsed.bundled_claude = Some(path),
            _ => {
                return Err(RecorderError::Usage(format!(
                    "unknown discover option `{flag}`"
                )));
            }
        }
    }
    Ok(parsed)
}

fn absolute_environment_path(name: &str) -> Result<Option<PathBuf>, RecorderError> {
    env::var_os(name)
        .map(PathBuf::from)
        .map(|path| require_absolute(path, name))
        .transpose()
}

fn require_absolute(path: PathBuf, source: &str) -> Result<PathBuf, RecorderError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(RecorderError::Usage(format!(
            "`{source}` must name an absolute path"
        )))
    }
}

fn run_discover(arguments: DiscoverArguments) -> Result<(), RecorderError> {
    let root = workspace_root()?;
    let sandbox = root.join("tests").join("fixtures").join("sandbox");
    let redactor = Redactor::for_environment(&sandbox);
    let bundled_claude = configured_bundled_claude(arguments.bundled_claude.as_deref());
    let node_report = platform::discover(DiscoveryTarget::Node, arguments.node.as_deref(), None);
    let node_probes = probe_discovery_report(
        &node_report,
        DiscoveryTarget::Node,
        None,
        Duration::from_secs(5),
    )?;
    let node = select_confirmed_runtime(DiscoveryTarget::Node, &node_probes);
    let node_parent = node
        .as_ref()
        .map(|node| {
            node.path().parent().ok_or_else(|| {
                RecorderError::Discovery(
                    "resolved Node.js executable has no parent directory".to_owned(),
                )
            })
        })
        .transpose()?;
    for (target, explicit, bundled) in [
        (DiscoveryTarget::Codex, arguments.codex.as_deref(), None),
        (
            DiscoveryTarget::ClaudeAcp,
            arguments.claude_acp.as_deref(),
            arguments.bundled_claude_acp.as_deref(),
        ),
        (
            DiscoveryTarget::ClaudeCli,
            arguments.claude.as_deref(),
            None,
        ),
        (
            DiscoveryTarget::OpenCode,
            arguments.opencode.as_deref(),
            None,
        ),
    ] {
        let report = platform::discover(target, explicit, bundled);
        let child_path_entry = (target == DiscoveryTarget::ClaudeAcp)
            .then_some(node_parent)
            .flatten();
        let probes =
            probe_discovery_report(&report, target, child_path_entry, Duration::from_secs(5))?;
        write_discovery_result(target, &report, &probes, bundled_claude.as_ref(), &redactor)?;
    }
    write_discovery_result(
        DiscoveryTarget::Node,
        &node_report,
        &node_probes,
        bundled_claude.as_ref(),
        &redactor,
    )?;
    Ok(())
}

fn probe_discovery_report(
    report: &platform::DiscoveryReport,
    target: DiscoveryTarget,
    child_path_entry: Option<&Path>,
    timeout: Duration,
) -> Result<Vec<RuntimeCandidateProbe>, RecorderError> {
    let accepts = |outcome: &RuntimeProbe| runtime_probe_confirms_target(target, outcome);
    match child_path_entry {
        Some(entry) => {
            Ok(report.probe_runtimes_until_with_child_path_entry(timeout, entry, accepts)?)
        }
        None => Ok(report.probe_runtimes_until(timeout, accepts)),
    }
}

fn write_discovery_result(
    target: DiscoveryTarget,
    report: &platform::DiscoveryReport,
    probes: &[RuntimeCandidateProbe],
    bundled_claude: Option<&ResolvedExecutable>,
    redactor: &Redactor,
) -> Result<(), RecorderError> {
    report.write_redacted(io::stdout().lock(), redactor)?;
    write_runtime_probes(probes, redactor)?;
    println!(
        "installation_assessment={}",
        report.installation_assessment_from_probes(probes)
    );
    if target == DiscoveryTarget::ClaudeAcp {
        write_claude_acp_adapter_status(probes);
    }
    write_auth_probes(target, probes, bundled_claude, redactor);
    println!();
    Ok(())
}

fn configured_bundled_claude(path: Option<&Path>) -> Option<ResolvedExecutable> {
    let path = path?;
    let report = platform::discover(DiscoveryTarget::ClaudeCli, Some(path), None);
    report.layers.iter().find_map(|layer| {
        if layer.layer != DiscoveryLayer::Explicit {
            return None;
        }
        match &layer.status {
            ProbeStatus::Found(executable) => Some(executable.clone()),
            ProbeStatus::NotFound
            | ProbeStatus::NotConfigured
            | ProbeStatus::NotApplicable
            | ProbeStatus::InvalidConfiguration(_) => None,
        }
    })
}

fn write_runtime_probes(
    probes: &[RuntimeCandidateProbe],
    redactor: &Redactor,
) -> Result<(), RecorderError> {
    if probes.is_empty() {
        RuntimeProbe::NotResolved.write_redacted(io::stdout().lock(), redactor)?;
        return Ok(());
    }
    for probe in probes {
        println!(
            "runtime_candidate={} ({}, {})",
            probe.layer,
            probe.executable.launcher(),
            redactor.redact(&probe.executable.path().to_string_lossy())
        );
        probe
            .outcome
            .write_redacted(io::stdout().lock(), redactor)?;
    }
    Ok(())
}

fn write_claude_acp_adapter_status(probes: &[RuntimeCandidateProbe]) {
    if probes
        .iter()
        .any(|probe| runtime_probe_confirms_target(DiscoveryTarget::ClaudeAcp, &probe.outcome))
    {
        println!("adapter_package=confirmed ({})", acp::CLAUDE_ACP_PACKAGE);
    } else {
        println!(
            "adapter_package=not-confirmed (required {})",
            acp::CLAUDE_ACP_PACKAGE
        );
        println!(
            "install_command={} (run only after explicit user confirmation)",
            acp::CLAUDE_ACP_INSTALL_COMMAND
        );
    }
    println!("automatic_install=not-attempted");
}

fn write_auth_probes(
    target: DiscoveryTarget,
    probes: &[RuntimeCandidateProbe],
    bundled_claude: Option<&ResolvedExecutable>,
    redactor: &Redactor,
) {
    if target == DiscoveryTarget::ClaudeAcp || target == DiscoveryTarget::Node {
        let report = auth::probe(target, None, bundled_claude, Duration::from_secs(5));
        println!("auth_state={}", report.state);
        println!("auth_evidence={}", report.evidence);
        return;
    }

    let mut observed = false;
    for probe in probes
        .iter()
        .filter(|probe| runtime_probe_confirms_target(target, &probe.outcome))
    {
        observed = true;
        println!(
            "auth_candidate={} ({}, {})",
            probe.layer,
            probe.executable.launcher(),
            redactor.redact(&probe.executable.path().to_string_lossy())
        );
        let report = auth::probe(
            target,
            Some(&probe.executable),
            bundled_claude,
            Duration::from_secs(5),
        );
        println!("auth_state={}", report.state);
        println!("auth_evidence={}", report.evidence);
        if target == DiscoveryTarget::OpenCode {
            break;
        }
    }
    if !observed {
        let report = auth::probe(target, None, bundled_claude, Duration::from_secs(5));
        println!("auth_state={}", report.state);
        println!("auth_evidence={}", report.evidence);
    }
}

fn runtime_probe_confirms_target(target: DiscoveryTarget, probe: &RuntimeProbe) -> bool {
    match probe {
        RuntimeProbe::Runnable { stdout, .. } if target == DiscoveryTarget::ClaudeAcp => {
            acp::is_pinned_launcher_version(stdout)
        }
        RuntimeProbe::Runnable { .. } => true,
        RuntimeProbe::NotResolved
        | RuntimeProbe::NonZero { .. }
        | RuntimeProbe::SpawnFailed(_)
        | RuntimeProbe::TimedOut => false,
    }
}

fn require_node(redactor: &Redactor) -> Result<ResolvedExecutable, RecorderError> {
    let explicit = absolute_environment_path("KALEIDO_NODE_EXECUTABLE")?;
    let report = platform::discover(DiscoveryTarget::Node, explicit.as_deref(), None);
    println!("ACP hard prerequisite: Node.js");
    report.write_redacted(io::stdout().lock(), redactor)?;
    let probes = report.probe_runtimes_until(Duration::from_secs(5), |outcome| {
        runtime_probe_confirms_target(DiscoveryTarget::Node, outcome)
    });
    write_runtime_probes(&probes, redactor)?;
    select_confirmed_runtime(DiscoveryTarget::Node, &probes).ok_or_else(|| {
        let installation = if cfg!(windows) {
            "`winget install --id OpenJS.NodeJS.LTS --exact`, then set \
                 KALEIDO_NODE_EXECUTABLE to node.exe if it is outside persistent PATH"
        } else {
            "install the current Node.js LTS from https://nodejs.org/en/download, then set \
                 KALEIDO_NODE_EXECUTABLE if it is outside the service environment"
        };
        RecorderError::Discovery(format!(
            "Node.js is a hard prerequisite for ACP Claude; {installation}"
        ))
    })
}

fn select_confirmed_runtime(
    target: DiscoveryTarget,
    probes: &[RuntimeCandidateProbe],
) -> Option<ResolvedExecutable> {
    probes
        .iter()
        .find(|probe| runtime_probe_confirms_target(target, &probe.outcome))
        .map(|probe| probe.executable.clone())
}

fn run_record(agent: AgentKind, arguments: RecordArguments) -> Result<(), RecorderError> {
    validate_record_arguments(agent, &arguments)?;
    let root = workspace_root()?;
    let canonical_root = root.canonicalize()?;
    let fixtures_directory = root.join("tests").join("fixtures");
    let expected_fixtures_directory = canonical_root.join("tests").join("fixtures");
    let canonical_fixtures_directory = require_expected_directory(
        &fixtures_directory,
        &expected_fixtures_directory,
        "fixture root",
    )?;
    let output_directory = fixtures_directory.join(agent.fixture_directory());
    let expected_output_directory = canonical_fixtures_directory.join(agent.fixture_directory());
    let output_directory =
        prepare_expected_directory(&output_directory, &expected_output_directory)?;
    let sandbox_path = root.join("tests").join("fixtures").join("sandbox");
    let expected_sandbox = canonical_fixtures_directory.join("sandbox");
    let redactor = Redactor::for_environment(&expected_sandbox);
    let output = output_directory.join(arguments.scenario.file_name());
    match fs::symlink_metadata(&output) {
        Ok(_) => {
            return Err(RecorderError::ExistingFixture(format!(
                "{}/{}",
                agent.fixture_directory(),
                arguments.scenario.file_name()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let explicit = match arguments.executable {
        Some(path) => Some(path),
        None => absolute_environment_path(agent.executable_environment_variable())?,
    };
    let bundled = if agent == AgentKind::Acp {
        match arguments.bundled_executable {
            Some(path) => Some(path),
            None => absolute_environment_path("KALEIDO_BUNDLED_CLAUDE_ACP")?,
        }
    } else {
        None
    };
    let node = if agent == AgentKind::Acp {
        Some(require_node(&redactor)?)
    } else {
        None
    };
    let node_parent = node
        .as_ref()
        .map(|node| {
            node.path().parent().ok_or_else(|| {
                RecorderError::Discovery(
                    "resolved Node.js executable has no parent directory".to_owned(),
                )
            })
        })
        .transpose()?;
    let (_layer, executable) = resolve_for_record(
        agent.target(),
        explicit.as_deref(),
        bundled.as_deref(),
        node_parent,
        &redactor,
    )?;
    let mut temporary = NamedTempFile::new_in(&output_directory)?;
    let sandbox_state = SandboxState::capture(&sandbox_path, &expected_sandbox)?;
    let sandbox = sandbox_state.root().to_path_buf();
    let attempt = {
        let mut fixture = FixtureSink::new(temporary.as_file_mut(), redactor.clone());
        record_agent(
            AgentRecordArguments {
                agent,
                scenario: arguments.scenario,
                thread_id: arguments.thread_id,
                session_id: arguments.session_id,
                timeout: arguments.timeout,
            },
            &executable,
            &sandbox,
            &mut fixture,
        )
    };
    let restore = sandbox_state.restore();
    restore?;
    let summary = attempt?;
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&output)
        .map_err(|error| RecorderError::Io(error.error))?;
    println!(
        "recorded agent={} scenario={} output={} ({summary})",
        agent.fixture_directory(),
        arguments.scenario.label(),
        redactor.redact(&output.to_string_lossy())
    );
    Ok(())
}

fn require_expected_directory(
    path: &Path,
    expected_canonical_path: &Path,
    label: &str,
) -> io::Result<PathBuf> {
    if platform::path_is_link_or_reparse(path)? {
        return Err(io::Error::other(format!(
            "{label} must not be a link or reparse point"
        )));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(format!("{label} must be a directory")));
    }
    let canonical = path.canonicalize()?;
    if canonical != expected_canonical_path {
        return Err(io::Error::other(format!(
            "{label} resolved outside its expected workspace location"
        )));
    }
    Ok(canonical)
}

fn prepare_expected_directory(path: &Path, expected_canonical_path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path)?,
        Err(error) => return Err(error),
    }
    require_expected_directory(path, expected_canonical_path, "agent fixture directory")
}

fn validate_record_arguments(
    agent: AgentKind,
    arguments: &RecordArguments,
) -> Result<(), RecorderError> {
    if arguments.thread_id.is_some()
        && (agent != AgentKind::Codex || arguments.scenario != Scenario::SessionLoad)
    {
        return Err(RecorderError::Usage(
            "--thread-id is only valid for Codex session-load".to_owned(),
        ));
    }
    if arguments.thread_id.as_deref().is_some_and(str::is_empty)
        || (agent == AgentKind::Codex
            && arguments.scenario == Scenario::SessionLoad
            && arguments.thread_id.is_none())
    {
        return Err(RecorderError::Usage(
            "Codex session-load requires a non-empty --thread-id from `codex exec --json`"
                .to_owned(),
        ));
    }
    if arguments.session_id.is_some()
        && (agent != AgentKind::Acp || arguments.scenario != Scenario::SessionLoad)
    {
        return Err(RecorderError::Usage(
            "--session-id is only valid for ACP session-load".to_owned(),
        ));
    }
    if arguments.session_id.as_deref().is_some_and(str::is_empty)
        || (agent == AgentKind::Acp
            && arguments.scenario == Scenario::SessionLoad
            && arguments.session_id.is_none())
    {
        return Err(RecorderError::Usage(
            "ACP session-load requires a non-empty --session-id from the Claude seed command"
                .to_owned(),
        ));
    }
    if arguments.bundled_executable.is_some() && agent != AgentKind::Acp {
        return Err(RecorderError::Usage(
            "--bundled-executable is only valid for ACP Claude".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_for_record(
    target: DiscoveryTarget,
    explicit: Option<&Path>,
    bundled: Option<&Path>,
    child_path_entry: Option<&Path>,
    redactor: &Redactor,
) -> Result<(DiscoveryLayer, ResolvedExecutable), RecorderError> {
    let report = platform::discover(target, explicit, bundled);
    report.write_redacted(io::stdout().lock(), redactor)?;
    let probes = probe_discovery_report(&report, target, child_path_entry, Duration::from_secs(5))?;
    write_runtime_probes(&probes, redactor)?;
    probes
        .into_iter()
        .find(|probe| runtime_probe_confirms_target(target, &probe.outcome))
        .map(|probe| (probe.layer, probe.executable))
        .ok_or_else(|| RecorderError::Discovery(discovery_failure(target)))
}

fn discovery_failure(target: DiscoveryTarget) -> String {
    if target == DiscoveryTarget::ClaudeAcp {
        format!(
            "no resolved launcher reported the required version {}; no package installation was \
             attempted. After explicit user confirmation, install it with `{}`",
            acp::CLAUDE_ACP_VERSION,
            acp::CLAUDE_ACP_INSTALL_COMMAND
        )
    } else {
        "none of the resolved candidates started successfully; the five-layer report above is \
         not an installation verdict"
            .to_owned()
    }
}

struct AgentRecordArguments {
    agent: AgentKind,
    scenario: Scenario,
    thread_id: Option<String>,
    session_id: Option<String>,
    timeout: Duration,
}

fn record_agent<W: Write>(
    arguments: AgentRecordArguments,
    executable: &ResolvedExecutable,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<String, RecorderError> {
    match arguments.agent {
        AgentKind::Codex => record_codex(
            arguments.scenario,
            arguments.thread_id,
            executable,
            sandbox,
            fixture,
        ),
        AgentKind::Acp => record_acp(
            arguments.scenario,
            arguments.session_id,
            arguments.timeout,
            executable,
            sandbox,
            fixture,
        ),
        AgentKind::OpenCode => record_opencode(
            arguments.scenario,
            arguments.timeout,
            executable,
            sandbox,
            fixture,
        ),
    }
}

fn record_codex<W: Write>(
    scenario: Scenario,
    thread_id: Option<String>,
    executable: &ResolvedExecutable,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<String, RecorderError> {
    let codex_scenario = match scenario {
        Scenario::SimpleTurn => CodexScenario::SimpleTurn,
        Scenario::ToolCall => CodexScenario::ToolCall,
        Scenario::PermissionApprove => CodexScenario::PermissionApprove,
        Scenario::PermissionDeny => CodexScenario::PermissionDeny,
        Scenario::FileChange => CodexScenario::FileChange,
        Scenario::Cancel => CodexScenario::Cancel,
        Scenario::Error => CodexScenario::Error,
        Scenario::SessionLoad => CodexScenario::SessionLoad { thread_id },
        Scenario::Elicitation => CodexScenario::Elicitation,
    };
    let recording =
        codex::record(executable, sandbox, codex_scenario, fixture).map_err(agent_error)?;
    ensure_codex_observed(scenario, &recording)?;
    Ok(format!(
        "{} notification methods, {} server requests",
        recording.notification_methods.len(),
        recording.server_request_methods.len()
    ))
}

fn ensure_codex_observed(
    scenario: Scenario,
    recording: &CodexRecording,
) -> Result<(), RecorderError> {
    let observed = match scenario {
        Scenario::SimpleTurn => {
            recording.completion_status.as_deref() == Some("completed")
                && recording
                    .notification_methods
                    .iter()
                    .any(|method| method == "item/agentMessage/delta")
        }
        Scenario::ToolCall => recording.observed_tool_call(),
        Scenario::PermissionApprove => recording.observed_approved_permission_flow(),
        Scenario::PermissionDeny => recording.observed_denied_permission_flow(),
        Scenario::FileChange => recording.observed_file_change(),
        Scenario::Cancel => recording.completion_status.as_deref() == Some("interrupted"),
        Scenario::Error => recording.observed_failed_command(),
        Scenario::SessionLoad => recording.turn_id.is_none(),
        Scenario::Elicitation => recording.observed_elicitation_request(),
    };
    if observed {
        Ok(())
    } else {
        Err(RecorderError::NotObserved(format!(
            "Codex completed without the required {} protocol evidence \
             (turn_status={:?}, notifications={:?}, item_started={:?}, \
             item_completed={:?}, command_exit_codes={:?}, diff_updates={}, \
             error_info_count={})",
            scenario.label(),
            recording.completion_status,
            recording.notification_methods,
            recording.item_types_started,
            recording.item_types_completed,
            recording.command_exit_codes,
            recording.diff_updates,
            recording.error_info_count
        )))
    }
}

fn record_acp<W: Write>(
    scenario: Scenario,
    session_id: Option<String>,
    timeout: Duration,
    executable: &ResolvedExecutable,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<String, RecorderError> {
    if scenario == Scenario::SessionLoad {
        let session_id = session_id.ok_or_else(|| {
            RecorderError::Usage(
                "ACP session-load requires --session-id from the Claude seed command".to_owned(),
            )
        })?;
        let outcome = acp::record_session_load_with_timeout(
            executable,
            acp::pinned_launcher_arguments(),
            sandbox,
            session_id,
            fixture,
            timeout,
        )
        .map_err(agent_error)?;
        return summarize_acp_outcome(scenario, outcome);
    }

    let acp_scenario = match scenario {
        Scenario::SimpleTurn => AcpScenario::SimpleTurn,
        Scenario::ToolCall => AcpScenario::ToolCall,
        Scenario::PermissionApprove => AcpScenario::PermissionApprove,
        Scenario::PermissionDeny => AcpScenario::PermissionDeny,
        Scenario::FileChange => AcpScenario::FileChange,
        Scenario::Cancel => AcpScenario::Cancel,
        Scenario::Error => AcpScenario::Error,
        Scenario::SessionLoad => {
            return Err(RecorderError::Usage(
                "ACP session-load requires --session-id from the Claude seed command".to_owned(),
            ))
        }
        Scenario::Elicitation => AcpScenario::Elicitation,
    };
    let outcome = acp::record_scenario_with_timeout(
        executable,
        acp::pinned_launcher_arguments(),
        sandbox,
        acp_scenario,
        fixture,
        timeout,
    )
    .map_err(agent_error)?;
    summarize_acp_outcome(scenario, outcome)
}

fn summarize_acp_outcome(scenario: Scenario, outcome: AcpOutcome) -> Result<String, RecorderError> {
    match outcome {
        AcpOutcome::Completed {
            stop_reason,
            observations,
            ..
        } => {
            let observed = match scenario {
                Scenario::SimpleTurn => observations
                    .session_update_kinds
                    .iter()
                    .any(|kind| kind == "agent_message_chunk"),
                Scenario::ToolCall => observations.completed_tool_lifecycle,
                Scenario::FileChange => observations.nonempty_file_diff,
                Scenario::Error => observations.failed_tool_lifecycle,
                Scenario::PermissionApprove => observations.approved_permission_flow,
                Scenario::PermissionDeny => observations.denied_permission_flow,
                Scenario::Cancel => observations.cancel_sent && stop_reason == "cancelled",
                Scenario::SessionLoad | Scenario::Elicitation => false,
            };
            if !observed {
                return Err(RecorderError::NotObserved(format!(
                    "ACP completed with stopReason={stop_reason} but without required {} evidence",
                    scenario.label()
                )));
            }
            Ok(format!(
                "stopReason={stop_reason}, updates={:?}",
                observations.session_update_kinds
            ))
        }
        AcpOutcome::SessionLoaded { observations, .. } => Ok(format!(
            "session loaded, replay updates={:?}",
            observations.session_update_kinds
        )),
        AcpOutcome::Unsupported { reason, .. } => Err(RecorderError::NotObserved(format!(
            "ACP adapter/schema reported unsupported: {reason:?}"
        ))),
        AcpOutcome::AuthenticationRequired { stage, .. } => Err(RecorderError::NotObserved(
            format!("ACP authentication required at {stage:?}"),
        )),
        AcpOutcome::AgentError { stage, code, .. } => Err(RecorderError::NotObserved(format!(
            "ACP agent error at {stage:?}: code={code} (upstream message omitted from logs)"
        ))),
    }
}

fn record_opencode<W: Write>(
    scenario: Scenario,
    timeout: Duration,
    executable: &ResolvedExecutable,
    sandbox: &Path,
    fixture: &mut FixtureSink<W>,
) -> Result<String, RecorderError> {
    let opencode_scenario = match scenario {
        Scenario::SimpleTurn => OpenCodeScenario::SimpleTurn,
        Scenario::ToolCall => OpenCodeScenario::ToolCall,
        Scenario::PermissionApprove => OpenCodeScenario::PermissionApprove,
        Scenario::PermissionDeny => OpenCodeScenario::PermissionDeny,
        Scenario::FileChange => OpenCodeScenario::FileChange,
        Scenario::Cancel => OpenCodeScenario::Cancel,
        Scenario::Error => OpenCodeScenario::Error,
        Scenario::SessionLoad => OpenCodeScenario::SessionLoad,
        Scenario::Elicitation => OpenCodeScenario::Elicitation,
    };
    match opencode::record(executable, sandbox, opencode_scenario, fixture, timeout)
        .map_err(agent_error)?
    {
        OpenCodeOutcome::Recorded {
            event_count,
            observations,
            ..
        } => Ok(format!(
            "{event_count} SSE events, observations={observations:?}"
        )),
        OpenCodeOutcome::Unsupported { reason } => Err(RecorderError::NotObserved(format!(
            "OpenCode reported unsupported: {reason}"
        ))),
        OpenCodeOutcome::NotObserved {
            event_count,
            reason,
            observations,
            ..
        } => Err(RecorderError::NotObserved(format!(
            "OpenCode recorded {event_count} events but required evidence was absent: {reason}; \
             observations={observations:?}"
        ))),
    }
}

fn agent_error<E: Error + 'static>(error: E) -> RecorderError {
    RecorderError::Agent(Box::new(error))
}

fn workspace_root() -> Result<PathBuf, RecorderError> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or(RecorderError::WorkspaceRoot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_arguments(scenario: Scenario) -> RecordArguments {
        RecordArguments {
            executable: None,
            bundled_executable: None,
            scenario,
            thread_id: None,
            session_id: None,
            timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn session_load_requires_the_agent_specific_seed_id() {
        let mut codex = record_arguments(Scenario::SessionLoad);
        assert!(matches!(
            validate_record_arguments(AgentKind::Codex, &codex),
            Err(RecorderError::Usage(_))
        ));
        codex.thread_id = Some("thread-from-seed".to_owned());
        assert!(validate_record_arguments(AgentKind::Codex, &codex).is_ok());

        let mut acp = record_arguments(Scenario::SessionLoad);
        assert!(matches!(
            validate_record_arguments(AgentKind::Acp, &acp),
            Err(RecorderError::Usage(_))
        ));
        acp.session_id = Some("session-from-seed".to_owned());
        assert!(validate_record_arguments(AgentKind::Acp, &acp).is_ok());
    }

    #[test]
    fn seed_ids_are_rejected_for_other_agents_and_scenarios() {
        let mut acp = record_arguments(Scenario::SessionLoad);
        acp.thread_id = Some("wrong-kind".to_owned());
        assert!(matches!(
            validate_record_arguments(AgentKind::Acp, &acp),
            Err(RecorderError::Usage(_))
        ));

        let mut codex = record_arguments(Scenario::SimpleTurn);
        codex.thread_id = Some("wrong-scenario".to_owned());
        assert!(matches!(
            validate_record_arguments(AgentKind::Codex, &codex),
            Err(RecorderError::Usage(_))
        ));

        let mut opencode = record_arguments(Scenario::SessionLoad);
        opencode.session_id = Some("wrong-agent".to_owned());
        assert!(matches!(
            validate_record_arguments(AgentKind::OpenCode, &opencode),
            Err(RecorderError::Usage(_))
        ));
    }

    #[test]
    fn parser_accepts_acp_session_id_option() -> Result<(), RecorderError> {
        let parsed = parse_record_arguments(
            [
                "session-load",
                "--session-id",
                "session-from-seed",
                "--timeout-secs",
                "1",
            ]
            .into_iter()
            .map(OsString::from),
        )?;

        assert_eq!(parsed.scenario, Scenario::SessionLoad);
        assert_eq!(parsed.session_id.as_deref(), Some("session-from-seed"));
        Ok(())
    }

    #[test]
    fn sandbox_capture_rejects_a_lexical_linked_root_before_following_it(
    ) -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside)?;
        fs::write(outside.join("marker.txt"), b"OUTSIDE\n")?;
        let sandbox = temporary.path().join("sandbox");
        platform::create_test_directory_link(&outside, &sandbox)?;
        let expected = temporary.path().canonicalize()?.join("sandbox");

        let error = match SandboxState::capture(&sandbox, &expected) {
            Err(error) => error,
            Ok(_) => return Err("linked sandbox root was accepted".into()),
        };

        assert!(error.to_string().contains("link or reparse point"));
        assert_eq!(fs::read(outside.join("marker.txt"))?, b"OUTSIDE\n");
        Ok(())
    }

    #[test]
    fn fixture_output_directory_rejects_a_linked_destination() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside)?;
        let output = temporary.path().join("opencode");
        platform::create_test_directory_link(&outside, &output)?;
        let expected = temporary.path().canonicalize()?.join("opencode");

        let error = match prepare_expected_directory(&output, &expected) {
            Err(error) => error,
            Ok(_) => return Err("linked fixture output directory was accepted".into()),
        };

        assert!(error.to_string().contains("link or reparse point"));
        Ok(())
    }

    #[test]
    fn sandbox_restore_atomically_returns_the_original_tree_without_following_a_hardlink(
    ) -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir_all(sandbox.join("src"))?;
        fs::write(sandbox.join("Cargo.toml"), b"BASELINE MANIFEST\n")?;
        fs::write(sandbox.join("editable.txt"), b"ORIGINAL\n")?;
        fs::write(sandbox.join("linked-a.txt"), b"BASELINE LINK\n")?;
        fs::hard_link(sandbox.join("linked-a.txt"), sandbox.join("linked-b.txt"))?;
        fs::write(sandbox.join("notes.txt"), b"BASELINE NOTES\n")?;
        fs::write(sandbox.join("src/main.rs"), b"fn main() {}\n")?;
        let sandbox = sandbox.canonicalize()?;
        let state = SandboxState::capture(&sandbox, &sandbox)?;

        fs::write(sandbox.join("linked-a.txt"), b"WORKING COPY\n")?;
        assert_eq!(
            fs::read(sandbox.join("linked-b.txt"))?,
            b"BASELINE LINK\n",
            "working files must not retain baseline hard links"
        );
        let outside = temporary.path().join("outside.txt");
        fs::write(&outside, b"OUTSIDE MUST NOT CHANGE\n")?;
        fs::write(sandbox.join("Cargo.toml"), b"MUTATED\n")?;
        fs::remove_file(sandbox.join("editable.txt"))?;
        fs::hard_link(&outside, sandbox.join("editable.txt"))?;
        fs::remove_file(sandbox.join("notes.txt"))?;
        fs::create_dir_all(sandbox.join("target/private"))?;
        fs::write(sandbox.join("target/private/extra.txt"), b"EXTRA\n")?;

        state.restore()?;

        assert_eq!(fs::read(&outside)?, b"OUTSIDE MUST NOT CHANGE\n");
        assert_eq!(
            fs::read(sandbox.join("Cargo.toml"))?,
            b"BASELINE MANIFEST\n"
        );
        assert_eq!(fs::read(sandbox.join("editable.txt"))?, b"ORIGINAL\n");
        assert_eq!(fs::read(sandbox.join("notes.txt"))?, b"BASELINE NOTES\n");
        assert_eq!(fs::read(sandbox.join("src/main.rs"))?, b"fn main() {}\n");
        assert!(!sandbox.join("target").exists());
        fs::write(sandbox.join("linked-a.txt"), b"RESTORED LINK\n")?;
        assert_eq!(
            fs::read(sandbox.join("linked-b.txt"))?,
            b"RESTORED LINK\n",
            "restoration must retain the original hard-link topology"
        );
        Ok(())
    }

    #[test]
    fn dropping_sandbox_state_still_restores_after_a_failed_recording_scope(
    ) -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::write(sandbox.join("editable.txt"), b"ORIGINAL\n")?;

        let canonical_sandbox = sandbox.canonicalize()?;
        let state = SandboxState::capture(&sandbox, &canonical_sandbox)?;
        fs::write(sandbox.join("editable.txt"), b"MUTATED\n")?;
        drop(state);

        assert_eq!(fs::read(sandbox.join("editable.txt"))?, b"ORIGINAL\n");
        Ok(())
    }

    #[test]
    fn sandbox_restore_quarantines_a_replaced_root_without_following_it(
    ) -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::write(sandbox.join("editable.txt"), b"ORIGINAL\n")?;
        let canonical_sandbox = sandbox.canonicalize()?;
        let state = SandboxState::capture(&sandbox, &canonical_sandbox)?;

        let outside = temporary.path().join("outside");
        fs::create_dir(&outside)?;
        fs::write(outside.join("sentinel.txt"), b"DO NOT DELETE\n")?;
        fs::remove_dir_all(&sandbox)?;
        platform::create_test_directory_link(&outside, &sandbox)?;

        state.restore()?;

        assert_eq!(fs::read(outside.join("sentinel.txt"))?, b"DO NOT DELETE\n");
        assert_eq!(fs::read(sandbox.join("editable.txt"))?, b"ORIGINAL\n");
        Ok(())
    }

    #[test]
    fn sandbox_restore_preserves_platform_file_metadata() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        let metadata_file = sandbox.join("metadata.txt");
        fs::write(&metadata_file, b"ORIGINAL\n")?;
        platform::set_test_preserved_file_metadata(&metadata_file, true)?;
        let before = platform::test_preserved_file_metadata(&metadata_file)?;

        let canonical_sandbox = sandbox.canonicalize()?;
        let state = SandboxState::capture(&sandbox, &canonical_sandbox)?;
        platform::set_test_preserved_file_metadata(&metadata_file, false)?;
        assert_ne!(
            platform::test_preserved_file_metadata(&metadata_file)?,
            before,
            "the test must actually change platform metadata before restoration"
        );
        fs::write(&metadata_file, b"MUTATED\n")?;
        state.restore()?;

        assert_eq!(
            platform::test_preserved_file_metadata(&metadata_file)?,
            before
        );
        assert_eq!(fs::read(&metadata_file)?, b"ORIGINAL\n");
        platform::set_test_preserved_file_metadata(&metadata_file, false)?;
        Ok(())
    }

    #[test]
    fn claude_acp_requires_the_exact_pinned_launcher_version() {
        let exact = RuntimeProbe::Runnable {
            exit_code: Some(0),
            stdout: "0.63.0".to_owned(),
            stderr: String::new(),
        };
        let drifted = RuntimeProbe::Runnable {
            exit_code: Some(0),
            stdout: "0.62.0".to_owned(),
            stderr: String::new(),
        };

        assert!(runtime_probe_confirms_target(
            DiscoveryTarget::ClaudeAcp,
            &exact
        ));
        assert!(!runtime_probe_confirms_target(
            DiscoveryTarget::ClaudeAcp,
            &drifted
        ));
        assert!(runtime_probe_confirms_target(
            DiscoveryTarget::Codex,
            &drifted
        ));
    }

    #[test]
    fn acp_version_probe_uses_explicit_node_outside_the_inherited_path(
    ) -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let node_directory = directory.path().join("explicit-node");
        let adapter_directory = directory.path().join("adapter");
        fs::create_dir(&node_directory)?;
        fs::create_dir(&adapter_directory)?;
        let node = platform::create_test_node_probe(&node_directory)?;
        let adapter = platform::create_test_acp_probe_requiring_node(
            &adapter_directory,
            acp::CLAUDE_ACP_VERSION,
        )?;
        assert!(!env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .any(|entry| entry == node_directory));
        let adapter_without_node =
            platform::discover(DiscoveryTarget::ClaudeAcp, Some(&adapter), None)
                .selected()
                .map(|(_, executable)| executable.clone())
                .ok_or("explicit ACP probe must resolve")?;
        let without_explicit_node = platform::probe_command(
            &adapter_without_node,
            &[OsString::from("--version")],
            Duration::from_secs(2),
        );
        assert!(
            !runtime_probe_confirms_target(DiscoveryTarget::ClaudeAcp, &without_explicit_node),
            "the ACP probe double must require the explicit Node directory"
        );
        let node_report = platform::discover(DiscoveryTarget::Node, Some(&node), None);
        let node_probes = probe_discovery_report(
            &node_report,
            DiscoveryTarget::Node,
            None,
            Duration::from_secs(2),
        )?;
        let selected_node = select_confirmed_runtime(DiscoveryTarget::Node, &node_probes)
            .ok_or("explicit Node must pass its runtime probe")?;
        let node_parent = selected_node
            .path()
            .parent()
            .ok_or("explicit Node must have a parent directory")?;
        let redactor = Redactor::for_environment(directory.path());

        let (layer, executable) = resolve_for_record(
            DiscoveryTarget::ClaudeAcp,
            Some(&adapter),
            None,
            Some(node_parent),
            &redactor,
        )?;

        assert_eq!(layer, DiscoveryLayer::Explicit);
        assert!(executable
            .path()
            .to_string_lossy()
            .eq_ignore_ascii_case(&adapter.to_string_lossy()));
        Ok(())
    }

    #[test]
    fn missing_claude_acp_reports_manual_install_without_npx() {
        let message = discovery_failure(DiscoveryTarget::ClaudeAcp);

        assert!(message.contains(acp::CLAUDE_ACP_INSTALL_COMMAND));
        assert!(message.contains(acp::CLAUDE_ACP_VERSION));
        assert!(message.contains("no package installation was attempted"));
        assert!(!message.contains("npx"));
        assert!(!message.contains("--yes"));
    }
}
