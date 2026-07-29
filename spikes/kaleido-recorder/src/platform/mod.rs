use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use directories::BaseDirs;
use thiserror::Error;

use crate::redact::Redactor;

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Launcher {
    Native,
    CmdScript,
    BatchScript,
}

impl fmt::Display for Launcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native => formatter.write_str("native"),
            Self::CmdScript => formatter.write_str("cmd"),
            Self::BatchScript => formatter.write_str("bat"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryTarget {
    Codex,
    ClaudeAcp,
    ClaudeCli,
    OpenCode,
    Node,
}

impl DiscoveryTarget {
    pub const fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeAcp => "claude-agent-acp",
            Self::ClaudeCli => "claude",
            Self::OpenCode => "opencode",
            Self::Node => "node",
        }
    }
}

impl fmt::Display for DiscoveryTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codex => formatter.write_str("codex"),
            Self::ClaudeAcp => formatter.write_str("claude-acp"),
            Self::ClaudeCli => formatter.write_str("claude-cli"),
            Self::OpenCode => formatter.write_str("opencode"),
            Self::Node => formatter.write_str("node"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryLayer {
    Explicit,
    InheritedPath,
    PersistentPath,
    KnownLocation,
    Bundled,
}

impl fmt::Display for DiscoveryLayer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => formatter.write_str("1-explicit"),
            Self::InheritedPath => formatter.write_str("2-inherited-path"),
            Self::PersistentPath => formatter.write_str("3-persistent-path"),
            Self::KnownLocation => formatter.write_str("4-known-location"),
            Self::Bundled => formatter.write_str("5-bundled"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateStatus {
    File,
    DirectoryEvidence,
    InstallationArtifactEvidence,
    Missing,
    NotFile,
    Inaccessible(io::ErrorKind),
    UnsupportedExtension,
}

impl fmt::Display for CandidateStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => formatter.write_str("file"),
            Self::DirectoryEvidence => formatter.write_str("directory-evidence"),
            Self::InstallationArtifactEvidence => {
                formatter.write_str("installation-artifact-evidence")
            }
            Self::Missing => formatter.write_str("missing"),
            Self::NotFile => formatter.write_str("not-a-file"),
            Self::Inaccessible(kind) => write!(formatter, "inaccessible:{kind:?}"),
            Self::UnsupportedExtension => formatter.write_str("unsupported-extension"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    pub status: CandidateStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedExecutable {
    path: PathBuf,
    launcher: Launcher,
    child_path_entries: Vec<PathBuf>,
}

impl ResolvedExecutable {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn launcher(&self) -> Launcher {
        self.launcher
    }

    pub fn command(&self, arguments: &[OsString]) -> Result<Command, ProcessError> {
        let mut command = platform_command(self, arguments)?;
        apply_child_path_entries(&mut command, &self.child_path_entries)?;
        Ok(command)
    }

    pub fn with_child_path_entry(mut self, directory: &Path) -> io::Result<Self> {
        if !directory.is_absolute() || !directory.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child PATH entry must be an existing absolute directory",
            ));
        }
        if !self
            .child_path_entries
            .iter()
            .any(|known| known == directory)
        {
            self.child_path_entries.push(directory.to_path_buf());
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeStatus {
    Found(ResolvedExecutable),
    NotFound,
    NotConfigured,
    NotApplicable,
    InvalidConfiguration(String),
}

impl fmt::Display for ProbeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Found(executable) => write!(
                formatter,
                "found ({}, {})",
                executable.launcher(),
                executable.path().display()
            ),
            Self::NotFound => formatter.write_str("not-found"),
            Self::NotConfigured => formatter.write_str("not-configured"),
            Self::NotApplicable => formatter.write_str("not-applicable"),
            Self::InvalidConfiguration(reason) => write!(formatter, "invalid: {reason}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerReport {
    pub layer: DiscoveryLayer,
    pub status: ProbeStatus,
    pub candidates: Vec<Candidate>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryReport {
    pub target: DiscoveryTarget,
    pub process_path: OsString,
    pub layers: Vec<LayerReport>,
    selected: Option<(DiscoveryLayer, ResolvedExecutable)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallationAssessment {
    RunnableLauncher,
    InstalledButLaunchFailed,
    InstallationEvidenceWithoutResolvedCli,
    NotObservedInFiveSources,
}

impl fmt::Display for InstallationAssessment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunnableLauncher => formatter.write_str("runtime-launcher-confirmed"),
            Self::InstalledButLaunchFailed => {
                formatter.write_str("installed-but-resolution-or-launch-failed")
            }
            Self::InstallationEvidenceWithoutResolvedCli => {
                formatter.write_str("installation-evidence-without-resolved-launcher")
            }
            Self::NotObservedInFiveSources => formatter.write_str(
                "not-observed-in-five-sources (not equivalent to an installation verdict)",
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeProbe {
    NotResolved,
    Runnable {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    NonZero {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    SpawnFailed(io::ErrorKind),
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCandidateProbe {
    pub layer: DiscoveryLayer,
    pub executable: ResolvedExecutable,
    pub outcome: RuntimeProbe,
}

impl RuntimeProbe {
    pub fn write_redacted(&self, mut writer: impl Write, redactor: &Redactor) -> io::Result<()> {
        match self {
            Self::NotResolved => writeln!(writer, "runtime_probe=not-run (no resolved candidate)"),
            Self::Runnable {
                exit_code,
                stdout,
                stderr,
            } => {
                writeln!(writer, "runtime_probe=runnable exit_code={exit_code:?}")?;
                write_probe_stream(&mut writer, "stdout", stdout, redactor)?;
                write_probe_stream(&mut writer, "stderr", stderr, redactor)
            }
            Self::NonZero {
                exit_code,
                stdout,
                stderr,
            } => {
                writeln!(writer, "runtime_probe=nonzero exit_code={exit_code:?}")?;
                write_probe_stream(&mut writer, "stdout", stdout, redactor)?;
                write_probe_stream(&mut writer, "stderr", stderr, redactor)
            }
            Self::SpawnFailed(kind) => {
                writeln!(writer, "runtime_probe=spawn-failed kind={kind:?}")
            }
            Self::TimedOut => writeln!(writer, "runtime_probe=timed-out"),
        }
    }
}

impl DiscoveryReport {
    pub fn selected(&self) -> Option<(DiscoveryLayer, &ResolvedExecutable)> {
        self.selected
            .as_ref()
            .map(|(layer, executable)| (*layer, executable))
    }

    pub fn write_redacted(&self, mut writer: impl Write, redactor: &Redactor) -> io::Result<()> {
        writeln!(writer, "agent={}", self.target)?;
        writeln!(
            writer,
            "process_PATH={}",
            redactor.redact(&self.process_path.to_string_lossy())
        )?;
        for layer in &self.layers {
            let status = match &layer.status {
                ProbeStatus::Found(executable) => format!(
                    "found ({}, {})",
                    executable.launcher(),
                    redactor.redact(&executable.path().to_string_lossy())
                ),
                other => other.to_string(),
            };
            writeln!(writer, "{}={status}", layer.layer)?;
            for candidate in &layer.candidates {
                writeln!(
                    writer,
                    "  candidate={} [{}]",
                    redactor.redact(&candidate.path.to_string_lossy()),
                    candidate.status
                )?;
            }
            for diagnostic in &layer.diagnostics {
                writeln!(writer, "  diagnostic={}", redactor.redact(diagnostic))?;
            }
        }
        if let Some((layer, executable)) = &self.selected {
            writeln!(
                writer,
                "selected={} ({}, {})",
                layer,
                executable.launcher(),
                redactor.redact(&executable.path().to_string_lossy())
            )?;
        } else {
            writeln!(writer, "selected=none (no installation inference made)")?;
        }
        Ok(())
    }

    pub fn probe_runtime(&self, timeout: Duration) -> RuntimeProbe {
        let probes = self.probe_runtimes(timeout);
        probes
            .iter()
            .find(|probe| matches!(probe.outcome, RuntimeProbe::Runnable { .. }))
            .or_else(|| probes.first())
            .map(|probe| probe.outcome.clone())
            .unwrap_or(RuntimeProbe::NotResolved)
    }

    pub fn probe_runtimes(&self, timeout: Duration) -> Vec<RuntimeCandidateProbe> {
        self.probe_runtimes_until(timeout, |outcome| {
            matches!(outcome, RuntimeProbe::Runnable { .. })
        })
    }

    pub fn probe_runtimes_until(
        &self,
        timeout: Duration,
        mut accepts: impl FnMut(&RuntimeProbe) -> bool,
    ) -> Vec<RuntimeCandidateProbe> {
        let mut probes = Vec::new();
        for (layer, executable) in self.resolved_candidates() {
            let outcome = probe_executable(self.target, &executable, timeout);
            let accepted = accepts(&outcome);
            probes.push(RuntimeCandidateProbe {
                layer,
                executable,
                outcome,
            });
            if accepted {
                break;
            }
        }
        probes
    }

    pub fn probe_runtimes_until_with_child_path_entry(
        &self,
        timeout: Duration,
        child_path_entry: &Path,
        mut accepts: impl FnMut(&RuntimeProbe) -> bool,
    ) -> io::Result<Vec<RuntimeCandidateProbe>> {
        let mut probes = Vec::new();
        for (layer, executable) in self.resolved_candidates() {
            let executable = executable.with_child_path_entry(child_path_entry)?;
            let outcome = probe_executable(self.target, &executable, timeout);
            let accepted = accepts(&outcome);
            probes.push(RuntimeCandidateProbe {
                layer,
                executable,
                outcome,
            });
            if accepted {
                break;
            }
        }
        Ok(probes)
    }

    pub fn resolved_candidates(&self) -> Vec<(DiscoveryLayer, ResolvedExecutable)> {
        let mut resolved = Vec::new();
        for layer in &self.layers {
            for candidate in &layer.candidates {
                if candidate.status != CandidateStatus::File {
                    continue;
                }
                let Some(executable) = platform_resolve_candidate(&candidate.path) else {
                    continue;
                };
                if resolved
                    .iter()
                    .any(|(_, known): &(DiscoveryLayer, ResolvedExecutable)| {
                        known.path() == executable.path()
                    })
                {
                    continue;
                }
                resolved.push((layer.layer, executable));
            }
        }
        resolved
    }

    pub fn first_runnable(
        &self,
        timeout: Duration,
    ) -> (Option<ResolvedExecutable>, Vec<RuntimeCandidateProbe>) {
        let probes = self.probe_runtimes(timeout);
        let executable = probes
            .iter()
            .find(|probe| matches!(probe.outcome, RuntimeProbe::Runnable { .. }))
            .map(|probe| probe.executable.clone());
        (executable, probes)
    }

    pub fn installation_assessment_from_probes(
        &self,
        probes: &[RuntimeCandidateProbe],
    ) -> InstallationAssessment {
        if probes
            .iter()
            .any(|probe| matches!(probe.outcome, RuntimeProbe::Runnable { .. }))
        {
            return InstallationAssessment::RunnableLauncher;
        }
        if !probes.is_empty() {
            return InstallationAssessment::InstalledButLaunchFailed;
        }
        self.installation_assessment(&RuntimeProbe::NotResolved)
    }

    pub fn installation_assessment(&self, runtime: &RuntimeProbe) -> InstallationAssessment {
        match runtime {
            RuntimeProbe::Runnable { .. } => InstallationAssessment::RunnableLauncher,
            RuntimeProbe::NonZero { .. }
            | RuntimeProbe::SpawnFailed(_)
            | RuntimeProbe::TimedOut => InstallationAssessment::InstalledButLaunchFailed,
            RuntimeProbe::NotResolved
                if self.layers.iter().any(|layer| {
                    layer.candidates.iter().any(|candidate| {
                        matches!(
                            candidate.status,
                            CandidateStatus::File
                                | CandidateStatus::DirectoryEvidence
                                | CandidateStatus::InstallationArtifactEvidence
                                | CandidateStatus::Inaccessible(_)
                        )
                    })
                }) =>
            {
                InstallationAssessment::InstallationEvidenceWithoutResolvedCli
            }
            RuntimeProbe::NotResolved => InstallationAssessment::NotObservedInFiveSources,
        }
    }
}

fn probe_executable(
    target: DiscoveryTarget,
    executable: &ResolvedExecutable,
    timeout: Duration,
) -> RuntimeProbe {
    let arguments = [OsString::from("--version")];
    let mut command = match executable.command(&arguments) {
        Ok(command) => command,
        Err(error) => return runtime_probe_from_process_error(&error),
    };
    let _isolation = if target == DiscoveryTarget::OpenCode {
        match tempfile::tempdir() {
            Ok(directory) => {
                apply_opencode_probe_isolation(&mut command, directory.path());
                Some(directory)
            }
            Err(error) => return RuntimeProbe::SpawnFailed(error.kind()),
        }
    } else {
        None
    };
    probe_prepared_command(command, timeout)
}

pub fn probe_command(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    timeout: Duration,
) -> RuntimeProbe {
    let command = match executable.command(arguments) {
        Ok(command) => command,
        Err(error) => return runtime_probe_from_process_error(&error),
    };
    probe_prepared_command(command, timeout)
}

fn apply_opencode_probe_isolation(command: &mut Command, root: &Path) {
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"));
}

pub(super) fn read_user_npm_prefix(
    home_directory: &Path,
    data_directory: &Path,
    diagnostics: &mut Vec<String>,
) -> Option<PathBuf> {
    let npmrc = home_directory.join(".npmrc");
    let contents = match fs::read_to_string(&npmrc) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            diagnostics.push("user .npmrc is not present".to_owned());
            return None;
        }
        Err(error) => {
            diagnostics.push(format!(
                "user .npmrc could not be read ({:?}); its contents were not logged",
                error.kind()
            ));
            return None;
        }
    };
    let Some(raw_prefix) = parse_npmrc_prefix(&contents) else {
        diagnostics.push("user .npmrc contains no prefix setting".to_owned());
        return None;
    };
    let Some(prefix) = resolve_npmrc_prefix(&raw_prefix, home_directory, data_directory)
        .filter(|p| p.is_absolute())
    else {
        diagnostics.push(
            "user .npmrc prefix is not an absolute path or supported home/data expansion"
                .to_owned(),
        );
        return None;
    };
    diagnostics.push(format!("user .npmrc prefix={}", prefix.to_string_lossy()));
    Some(prefix)
}

fn parse_npmrc_prefix(contents: &str) -> Option<String> {
    contents.lines().rev().find_map(parse_npmrc_prefix_line)
}

fn parse_npmrc_prefix_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    if !key.trim().eq_ignore_ascii_case("prefix") {
        return None;
    }
    let value = value.trim();
    let unquoted = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim();
    (!unquoted.is_empty()).then(|| unquoted.to_owned())
}

fn resolve_npmrc_prefix(
    value: &str,
    home_directory: &Path,
    data_directory: &Path,
) -> Option<PathBuf> {
    for (marker, base) in [
        ("${HOME}", home_directory),
        ("${USERPROFILE}", home_directory),
        ("${APPDATA}", data_directory),
    ] {
        if value == marker {
            return Some(base.to_path_buf());
        }
        if let Some(relative) = value
            .strip_prefix(marker)
            .and_then(|suffix| suffix.strip_prefix(['/', '\\']))
        {
            return Some(base.join(relative));
        }
    }
    if value == "~" {
        return Some(home_directory.to_path_buf());
    }
    if let Some(relative) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return Some(home_directory.join(relative));
    }
    Some(PathBuf::from(value))
}

fn probe_prepared_command(mut command: Command, timeout: Duration) -> RuntimeProbe {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return RuntimeProbe::SpawnFailed(error.kind()),
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return match child.wait_with_output() {
                    Ok(output) => runtime_probe_from_output(output),
                    Err(error) => RuntimeProbe::SpawnFailed(error.kind()),
                };
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                return match platform_terminate_tree(&mut child) {
                    Ok(_) => RuntimeProbe::TimedOut,
                    Err(error) => RuntimeProbe::SpawnFailed(process_error_kind(&error)),
                };
            }
            Err(error) => return RuntimeProbe::SpawnFailed(error.kind()),
        }
    }
}

fn runtime_probe_from_process_error(error: &ProcessError) -> RuntimeProbe {
    RuntimeProbe::SpawnFailed(process_error_kind(error))
}

fn process_error_kind(error: &ProcessError) -> io::ErrorKind {
    match error {
        ProcessError::Spawn(error)
        | ProcessError::Inspect(error)
        | ProcessError::Terminate(error) => error.kind(),
        ProcessError::Resolve(_) => io::ErrorKind::NotFound,
        ProcessError::UnsafeScriptArgument => io::ErrorKind::InvalidInput,
    }
}

fn runtime_probe_from_output(output: std::process::Output) -> RuntimeProbe {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        RuntimeProbe::Runnable {
            exit_code: output.status.code(),
            stdout,
            stderr,
        }
    } else {
        RuntimeProbe::NonZero {
            exit_code: output.status.code(),
            stdout,
            stderr,
        }
    }
}

fn write_probe_stream(
    writer: &mut impl Write,
    name: &str,
    value: &str,
    redactor: &Redactor,
) -> io::Result<()> {
    if value.is_empty() {
        writeln!(writer, "  {name}=<empty>")
    } else {
        for line in value.lines() {
            writeln!(writer, "  {name}={}", redactor.redact(line))?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionFailure {
    pub program: OsString,
    pub report: Box<LayerReport>,
}

impl fmt::Display for ResolutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not resolve {:?}; {} candidate(s) checked",
            self.program,
            self.report.candidates.len()
        )
    }
}

impl std::error::Error for ResolutionFailure {}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error(transparent)]
    Resolve(#[from] ResolutionFailure),
    #[error("failed to start child process: {0}")]
    Spawn(#[source] io::Error),
    #[error("failed to inspect child process: {0}")]
    Inspect(#[source] io::Error),
    #[error("failed to terminate child process tree: {0}")]
    Terminate(#[source] io::Error),
    #[error("refusing unsafe command-script argument")]
    UnsafeScriptArgument,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessEntry {
    pub(super) pid: u32,
    pub(super) parent_pid: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SpawnedProcessFamily {
    root_pid: u32,
    members: BTreeSet<u32>,
}

impl SpawnedProcessFamily {
    pub(super) fn capture(root_pid: u32, snapshot: &[ProcessEntry]) -> Self {
        let mut members = BTreeSet::from([root_pid]);
        loop {
            let before = members.len();
            for process in snapshot {
                if members.contains(&process.parent_pid) {
                    members.insert(process.pid);
                }
            }
            if members.len() == before {
                break;
            }
        }
        Self { root_pid, members }
    }

    fn remaining<'a>(&self, snapshot: &'a [ProcessEntry]) -> Vec<&'a ProcessEntry> {
        snapshot
            .iter()
            .filter(|process| self.members.contains(&process.pid))
            .collect()
    }

    #[cfg(windows)]
    fn descendant_pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.members
            .iter()
            .copied()
            .filter(|process_id| *process_id != self.root_pid)
    }
}

pub(super) fn wait_for_spawned_process_family_exit(
    family: &SpawnedProcessFamily,
    timeout: Duration,
    mut snapshot: impl FnMut() -> io::Result<Vec<ProcessEntry>>,
) -> Result<(), ProcessError> {
    let started = Instant::now();
    loop {
        let current = snapshot().map_err(|error| {
            ProcessError::Terminate(io::Error::new(
                error.kind(),
                format!(
                    "could not verify cleanup of spawned root PID {}: {error}",
                    family.root_pid
                ),
            ))
        })?;
        let remaining = family.remaining(&current);
        if remaining.is_empty() {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(ProcessError::Terminate(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{} member(s) of spawned root PID {} family remained after cleanup",
                    remaining.len(),
                    family.root_pid
                ),
            )));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn resolve(program: &OsStr) -> Result<ResolvedExecutable, ResolutionFailure> {
    let report = platform_probe_path(
        DiscoveryLayer::InheritedPath,
        program,
        &env::var_os("PATH").unwrap_or_default(),
        true,
    );
    match &report.status {
        ProbeStatus::Found(executable) => Ok(executable.clone()),
        _ => Err(ResolutionFailure {
            program: program.to_os_string(),
            report: Box::new(report),
        }),
    }
}

pub fn discover(
    target: DiscoveryTarget,
    explicit: Option<&Path>,
    bundled: Option<&Path>,
) -> DiscoveryReport {
    let program = OsStr::new(target.program());
    let mut layers = Vec::with_capacity(5);
    layers.push(match explicit {
        Some(path) => platform_probe_explicit(DiscoveryLayer::Explicit, path),
        None => empty_layer(DiscoveryLayer::Explicit, ProbeStatus::NotConfigured),
    });
    layers.push(platform_probe_path(
        DiscoveryLayer::InheritedPath,
        program,
        &env::var_os("PATH").unwrap_or_default(),
        true,
    ));
    layers.push(platform_probe_persistent(program));
    layers.push(platform_probe_known(target));
    layers.push(if target == DiscoveryTarget::ClaudeAcp {
        match bundled {
            Some(path) => platform_probe_explicit(DiscoveryLayer::Bundled, path),
            None => empty_layer(DiscoveryLayer::Bundled, ProbeStatus::NotConfigured),
        }
    } else {
        empty_layer(DiscoveryLayer::Bundled, ProbeStatus::NotApplicable)
    });

    let selected = layers.iter().find_map(|layer| {
        if let ProbeStatus::Found(executable) = &layer.status {
            Some((layer.layer, executable.clone()))
        } else {
            None
        }
    });

    DiscoveryReport {
        target,
        process_path: env::var_os("PATH").unwrap_or_default(),
        layers,
        selected,
    }
}

pub fn spawn(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    cwd: &Path,
) -> Result<Child, ProcessError> {
    let child = spawn_command(executable, arguments, cwd)?
        .spawn()
        .map_err(ProcessError::Spawn)?;
    Ok(child)
}

pub fn spawn_fixture(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    sandbox: &Path,
) -> Result<Child, ProcessError> {
    let mut command = spawn_command(executable, arguments, sandbox)?;
    apply_fixture_isolation(&mut command, sandbox);
    command.spawn().map_err(ProcessError::Spawn)
}

pub fn validate_fixture_sandbox_root(
    sandbox: &Path,
    expected: &Path,
) -> io::Result<Option<PathBuf>> {
    if !sandbox.is_absolute() || !expected.is_absolute() {
        return Ok(None);
    }
    if platform_fixture_sandbox_root_is_link(sandbox)?
        || platform_fixture_sandbox_root_is_link(expected)?
    {
        return Ok(None);
    }
    let canonical = sandbox.canonicalize()?;
    let canonical_expected = expected.canonicalize()?;
    Ok((canonical.is_dir() && canonical == canonical_expected).then_some(canonical))
}

pub fn path_is_link_or_reparse(path: &Path) -> io::Result<bool> {
    platform_fixture_sandbox_root_is_link(path)
}

pub fn quarantine_sandbox_working_copy(source: &Path, quarantine: &Path) -> io::Result<()> {
    platform_quarantine_sandbox_working_copy(source, quarantine)
}

pub fn canonical_permission_path(path: &Path) -> io::Result<PathBuf> {
    platform_canonical_permission_path(path)
}

pub fn permission_path_pattern(path: &Path) -> io::Result<String> {
    platform_permission_path_pattern(path)
}

#[doc(hidden)]
pub fn create_test_directory_link(target: &Path, link: &Path) -> io::Result<()> {
    platform_create_test_directory_link(target, link)
}

#[doc(hidden)]
pub fn set_test_preserved_file_metadata(path: &Path, marked: bool) -> io::Result<()> {
    platform_set_test_preserved_file_metadata(path, marked)
}

#[doc(hidden)]
pub fn test_preserved_file_metadata(path: &Path) -> io::Result<u32> {
    platform_test_preserved_file_metadata(path)
}

#[doc(hidden)]
pub fn create_test_node_probe(directory: &Path) -> io::Result<PathBuf> {
    platform_create_test_node_probe(directory)
}

#[doc(hidden)]
pub fn create_test_acp_probe_requiring_node(
    directory: &Path,
    version: &str,
) -> io::Result<PathBuf> {
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "test probe version contains unsupported characters",
        ));
    }
    platform_create_test_acp_probe_requiring_node(directory, version)
}

fn apply_fixture_isolation(command: &mut Command, sandbox: &Path) {
    if let Some(ceiling) = sandbox.parent() {
        command.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
}

fn apply_child_path_entries(
    command: &mut Command,
    prepended_directories: &[PathBuf],
) -> Result<(), ProcessError> {
    if prepended_directories.is_empty() {
        return Ok(());
    }

    let command_path = command
        .get_envs()
        .find(|(name, _)| *name == OsStr::new("PATH"))
        .map(|(_, value)| value.map(OsStr::to_os_string));
    let inherited_path = match command_path {
        Some(Some(path)) => path,
        Some(None) => OsString::new(),
        None => env::var_os("PATH").unwrap_or_default(),
    };
    let joined = env::join_paths(
        prepended_directories
            .iter()
            .cloned()
            .chain(env::split_paths(&inherited_path)),
    )
    .map_err(|error| {
        ProcessError::Spawn(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("could not construct child PATH: {error}"),
        ))
    })?;
    command.env("PATH", joined);
    Ok(())
}

fn spawn_command(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    cwd: &Path,
) -> Result<Command, ProcessError> {
    let mut command = executable.command(arguments)?;
    command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command)
}

pub fn terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    platform_terminate_tree(child)
}

#[cfg(windows)]
fn platform_probe_explicit(layer: DiscoveryLayer, path: &Path) -> LayerReport {
    windows::probe_explicit(layer, path)
}

#[cfg(unix)]
fn platform_probe_explicit(layer: DiscoveryLayer, path: &Path) -> LayerReport {
    unix::probe_explicit(layer, path)
}

#[cfg(windows)]
fn platform_probe_path(
    layer: DiscoveryLayer,
    program: &OsStr,
    search_path: &OsStr,
    include_where: bool,
) -> LayerReport {
    windows::probe_path(layer, program, search_path, include_where)
}

#[cfg(unix)]
fn platform_probe_path(
    layer: DiscoveryLayer,
    program: &OsStr,
    search_path: &OsStr,
    include_where: bool,
) -> LayerReport {
    unix::probe_path(layer, program, search_path, include_where)
}

#[cfg(windows)]
fn platform_probe_persistent(program: &OsStr) -> LayerReport {
    windows::probe_persistent(program)
}

#[cfg(unix)]
fn platform_probe_persistent(program: &OsStr) -> LayerReport {
    unix::probe_persistent(program)
}

#[cfg(windows)]
fn platform_probe_known(target: DiscoveryTarget) -> LayerReport {
    windows::probe_known(target, BaseDirs::new())
}

#[cfg(unix)]
fn platform_probe_known(target: DiscoveryTarget) -> LayerReport {
    unix::probe_known(target, BaseDirs::new())
}

#[cfg(target_os = "macos")]
fn platform_extend_unix_known_executable_directories(directories: &mut Vec<PathBuf>) {
    macos::extend_known_executable_directories(directories);
}

#[cfg(target_os = "linux")]
fn platform_extend_unix_known_executable_directories(directories: &mut Vec<PathBuf>) {
    linux::extend_known_executable_directories(directories);
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn platform_extend_unix_known_executable_directories(_directories: &mut Vec<PathBuf>) {}

#[cfg(target_os = "macos")]
fn platform_append_unix_installation_evidence(
    target: DiscoveryTarget,
    home_directory: &Path,
    _data_directory: &Path,
    _executable_directories: &[PathBuf],
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    macos::append_installation_evidence(target, home_directory, candidates, diagnostics);
}

#[cfg(target_os = "linux")]
fn platform_append_unix_installation_evidence(
    target: DiscoveryTarget,
    _home_directory: &Path,
    data_directory: &Path,
    executable_directories: &[PathBuf],
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    linux::append_installation_evidence(
        target,
        data_directory,
        executable_directories,
        candidates,
        diagnostics,
    );
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn platform_append_unix_installation_evidence(
    target: DiscoveryTarget,
    _home_directory: &Path,
    _data_directory: &Path,
    _executable_directories: &[PathBuf],
    _candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    diagnostics.push(format!(
        "no documented GUI installation mapping is enumerated for {target} on this Unix platform"
    ));
}

#[cfg(windows)]
fn platform_command(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
) -> Result<Command, ProcessError> {
    windows::command(executable, arguments)
}

#[cfg(windows)]
fn platform_resolve_candidate(path: &Path) -> Option<ResolvedExecutable> {
    windows::resolve_candidate(path)
}

#[cfg(unix)]
fn platform_resolve_candidate(path: &Path) -> Option<ResolvedExecutable> {
    unix::resolve_candidate(path)
}

#[cfg(unix)]
fn platform_command(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
) -> Result<Command, ProcessError> {
    Ok(unix::command(executable, arguments))
}

#[cfg(windows)]
fn platform_terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    windows::terminate_tree(child)
}

#[cfg(unix)]
fn platform_terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    unix::terminate_tree(child)
}

#[cfg(windows)]
fn platform_fixture_sandbox_root_is_link(path: &Path) -> io::Result<bool> {
    windows::fixture_sandbox_root_is_link(path)
}

#[cfg(unix)]
fn platform_fixture_sandbox_root_is_link(path: &Path) -> io::Result<bool> {
    unix::fixture_sandbox_root_is_link(path)
}

#[cfg(windows)]
fn platform_quarantine_sandbox_working_copy(source: &Path, quarantine: &Path) -> io::Result<()> {
    windows::quarantine_sandbox_working_copy(source, quarantine)
}

#[cfg(unix)]
fn platform_quarantine_sandbox_working_copy(source: &Path, quarantine: &Path) -> io::Result<()> {
    unix::quarantine_sandbox_working_copy(source, quarantine)
}

#[cfg(windows)]
fn platform_canonical_permission_path(path: &Path) -> io::Result<PathBuf> {
    windows::canonical_permission_path(path)
}

#[cfg(unix)]
fn platform_canonical_permission_path(path: &Path) -> io::Result<PathBuf> {
    unix::canonical_permission_path(path)
}

#[cfg(windows)]
fn platform_permission_path_pattern(path: &Path) -> io::Result<String> {
    windows::permission_path_pattern(path)
}

#[cfg(unix)]
fn platform_permission_path_pattern(path: &Path) -> io::Result<String> {
    unix::permission_path_pattern(path)
}

#[cfg(windows)]
fn platform_create_test_directory_link(target: &Path, link: &Path) -> io::Result<()> {
    windows::create_test_directory_link(target, link)
}

#[cfg(unix)]
fn platform_create_test_directory_link(target: &Path, link: &Path) -> io::Result<()> {
    unix::create_test_directory_link(target, link)
}

#[cfg(windows)]
fn platform_set_test_preserved_file_metadata(path: &Path, marked: bool) -> io::Result<()> {
    windows::set_test_preserved_file_metadata(path, marked)
}

#[cfg(unix)]
fn platform_set_test_preserved_file_metadata(path: &Path, marked: bool) -> io::Result<()> {
    unix::set_test_preserved_file_metadata(path, marked)
}

#[cfg(windows)]
fn platform_test_preserved_file_metadata(path: &Path) -> io::Result<u32> {
    windows::test_preserved_file_metadata(path)
}

#[cfg(unix)]
fn platform_test_preserved_file_metadata(path: &Path) -> io::Result<u32> {
    unix::test_preserved_file_metadata(path)
}

#[cfg(windows)]
fn platform_create_test_node_probe(directory: &Path) -> io::Result<PathBuf> {
    windows::create_test_node_probe(directory)
}

#[cfg(unix)]
fn platform_create_test_node_probe(directory: &Path) -> io::Result<PathBuf> {
    unix::create_test_node_probe(directory)
}

#[cfg(windows)]
fn platform_create_test_acp_probe_requiring_node(
    directory: &Path,
    version: &str,
) -> io::Result<PathBuf> {
    windows::create_test_acp_probe_requiring_node(directory, version)
}

#[cfg(unix)]
fn platform_create_test_acp_probe_requiring_node(
    directory: &Path,
    version: &str,
) -> io::Result<PathBuf> {
    unix::create_test_acp_probe_requiring_node(directory, version)
}

pub(super) fn empty_layer(layer: DiscoveryLayer, status: ProbeStatus) -> LayerReport {
    LayerReport {
        layer,
        status,
        candidates: Vec::new(),
        diagnostics: Vec::new(),
    }
}

pub(super) fn resolved(path: PathBuf, launcher: Launcher) -> ResolvedExecutable {
    ResolvedExecutable {
        path,
        launcher,
        child_path_entries: Vec::new(),
    }
}

#[cfg(test)]
mod process_family_tests {
    use std::io;
    use std::time::Duration;

    use super::{
        wait_for_spawned_process_family_exit, ProcessEntry, ProcessError, SpawnedProcessFamily,
    };

    #[test]
    fn unrelated_same_name_gui_process_is_not_part_of_the_spawned_pid_family() {
        let spawned_cli = (
            ProcessEntry {
                pid: 100,
                parent_pid: 10,
            },
            "claude.exe",
        );
        let spawned_descendant = ProcessEntry {
            pid: 101,
            parent_pid: spawned_cli.0.pid,
        };
        let unrelated_same_name_gui = (
            ProcessEntry {
                pid: 900,
                parent_pid: 9,
            },
            "claude.exe",
        );
        let family = SpawnedProcessFamily::capture(
            spawned_cli.0.pid,
            &[spawned_cli.0, spawned_descendant, unrelated_same_name_gui.0],
        );

        let result = wait_for_spawned_process_family_exit(&family, Duration::ZERO, || {
            Ok(vec![unrelated_same_name_gui.0])
        });

        assert_eq!(spawned_cli.1, unrelated_same_name_gui.1);
        assert!(result.is_ok());
    }

    #[test]
    fn remaining_spawned_descendant_is_reported_as_cleanup_failure(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = ProcessEntry {
            pid: 100,
            parent_pid: 10,
        };
        let descendant = ProcessEntry {
            pid: 101,
            parent_pid: root.pid,
        };
        let family = SpawnedProcessFamily::capture(root.pid, &[root, descendant]);

        let Err(error) =
            wait_for_spawned_process_family_exit(&family, Duration::ZERO, || Ok(vec![descendant]))
        else {
            return Err(io::Error::other("spawned descendant residual was not detected").into());
        };

        assert!(matches!(error, ProcessError::Terminate(_)));
        assert!(error.to_string().contains("1 member(s)"));
        assert!(error.to_string().contains("root PID 100"));
        Ok(())
    }

    #[test]
    fn process_snapshot_failure_is_reported_instead_of_claiming_cleanup(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let family = SpawnedProcessFamily::capture(100, &[]);

        let Err(error) = wait_for_spawned_process_family_exit(&family, Duration::ZERO, || {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "forced"))
        }) else {
            return Err(io::Error::other("snapshot failure was not detected").into());
        };

        assert!(matches!(error, ProcessError::Terminate(_)));
        assert!(error.to_string().contains("could not verify cleanup"));
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{
        apply_fixture_isolation, apply_opencode_probe_isolation, platform_probe_explicit,
        read_user_npm_prefix, Candidate, CandidateStatus, DiscoveryLayer, DiscoveryReport,
        DiscoveryTarget, InstallationAssessment, LayerReport, ProbeStatus, RuntimeProbe,
    };

    #[test]
    fn fixture_spawn_sets_parent_git_ceiling() {
        let sandbox = PathBuf::from(r"D:\repo\tests\fixtures\sandbox");
        let mut command = Command::new("fixture-test");

        apply_fixture_isolation(&mut command, &sandbox);

        let ceiling = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new("GIT_CEILING_DIRECTORIES"))
            .and_then(|(_, value)| value);
        assert_eq!(ceiling, Some(OsStr::new(r"D:\repo\tests\fixtures")));
    }

    #[test]
    fn opencode_version_probe_uses_only_an_isolated_xdg_root() {
        let root = PathBuf::from(r"D:\isolated-opencode-probe");
        let mut command = Command::new("opencode");

        apply_opencode_probe_isolation(&mut command, &root);

        let environments = command
            .get_envs()
            .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value.to_owned())))
            .collect::<Vec<_>>();
        assert_eq!(
            environments,
            [
                (
                    OsString::from("XDG_CACHE_HOME"),
                    root.join("cache").into_os_string(),
                ),
                (
                    OsString::from("XDG_CONFIG_HOME"),
                    root.join("config").into_os_string(),
                ),
                (
                    OsString::from("XDG_DATA_HOME"),
                    root.join("data").into_os_string(),
                ),
                (
                    OsString::from("XDG_STATE_HOME"),
                    root.join("state").into_os_string(),
                ),
            ]
        );
    }

    #[test]
    fn user_npmrc_uses_last_exact_prefix_without_logging_other_settings(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let home = directory.path().join("home");
        let data = directory.path().join("data");
        let first = directory.path().join("first");
        let selected = directory.path().join("selected prefix");
        fs::create_dir_all(&home)?;
        fs::write(
            home.join(".npmrc"),
            format!(
                "//registry.npmjs.org/:_authToken=do-not-log\nprefix={}\nnot-prefix=ignored\nPREFIX=\"{}\"\n",
                first.display(),
                selected.display()
            ),
        )?;
        let mut diagnostics = Vec::new();

        let prefix = read_user_npm_prefix(&home, &data, &mut diagnostics);

        assert_eq!(prefix.as_deref(), Some(selected.as_path()));
        let report = diagnostics.join("\n");
        assert!(!report.contains("do-not-log"));
        assert!(!report.contains("_authToken"));
        assert!(report.contains(&selected.to_string_lossy().to_string()));
        Ok(())
    }

    #[test]
    fn user_npmrc_expands_supported_home_and_data_variables(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let home = directory.path().join("home");
        let data = directory.path().join("data");
        fs::create_dir_all(&home)?;
        let mut diagnostics = Vec::new();
        let home_prefix = home.join("npm-tools");
        let data_prefix = data.join("npm-tools");

        fs::write(home.join(".npmrc"), "prefix=${HOME}/npm-tools\n")?;
        assert_eq!(
            read_user_npm_prefix(&home, &data, &mut diagnostics).as_deref(),
            Some(home_prefix.as_path())
        );

        fs::write(home.join(".npmrc"), "prefix=${APPDATA}\\npm-tools\n")?;
        assert_eq!(
            read_user_npm_prefix(&home, &data, &mut diagnostics).as_deref(),
            Some(data_prefix.as_path())
        );
        Ok(())
    }

    #[test]
    fn runtime_probe_falls_back_after_an_earlier_candidate_fails(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let failing = directory.path().join("failing.cmd");
        let runnable = directory.path().join("runnable.cmd");
        fs::write(&failing, "@echo off\r\nexit /b 7\r\n")?;
        fs::write(&runnable, "@echo off\r\necho 1.2.3\r\n")?;
        let explicit = platform_probe_explicit(DiscoveryLayer::Explicit, &failing);
        let inherited = platform_probe_explicit(DiscoveryLayer::InheritedPath, &runnable);
        let selected = match &explicit.status {
            ProbeStatus::Found(executable) => Some((DiscoveryLayer::Explicit, executable.clone())),
            _ => None,
        };
        let report = DiscoveryReport {
            target: DiscoveryTarget::Codex,
            process_path: OsString::new(),
            layers: vec![explicit, inherited],
            selected,
        };

        let (selected, probes) = report.first_runnable(Duration::from_secs(2));

        assert_eq!(probes.len(), 2);
        let mut outcomes = probes.iter().map(|probe| &probe.outcome);
        assert!(matches!(
            outcomes.next(),
            Some(RuntimeProbe::NonZero { .. })
        ));
        assert!(matches!(
            outcomes.next(),
            Some(RuntimeProbe::Runnable { .. })
        ));
        assert_eq!(
            selected
                .ok_or("second candidate must be selected")?
                .path()
                .file_name(),
            runnable.file_name()
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn compatible_explicit_candidate_prevents_bundled_fallback_execution(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let explicit_path = directory.path().join("explicit.cmd");
        let bundled_path = directory.path().join("bundled.cmd");
        let bundled_marker = directory.path().join("bundled-ran");
        fs::write(&explicit_path, "@echo off\r\necho 0.63.0\r\n")?;
        fs::write(
            &bundled_path,
            format!(
                "@echo off\r\necho bundled>\"{}\"\r\necho 0.63.0\r\n",
                bundled_marker.display()
            ),
        )?;
        let explicit = platform_probe_explicit(DiscoveryLayer::Explicit, &explicit_path);
        let bundled = platform_probe_explicit(DiscoveryLayer::Bundled, &bundled_path);
        let selected = match &explicit.status {
            ProbeStatus::Found(executable) => Some((DiscoveryLayer::Explicit, executable.clone())),
            _ => None,
        };
        let report = DiscoveryReport {
            target: DiscoveryTarget::ClaudeAcp,
            process_path: OsString::new(),
            layers: vec![explicit, bundled],
            selected,
        };

        let probes = report.probe_runtimes_until(Duration::from_secs(2), |outcome| {
            matches!(
                outcome,
                RuntimeProbe::Runnable { stdout, .. } if stdout == "0.63.0"
            )
        });

        assert_eq!(probes.len(), 1);
        assert_eq!(
            probes.first().map(|probe| probe.layer),
            Some(DiscoveryLayer::Explicit)
        );
        assert!(!bundled_marker.exists());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn runtime_probe_stops_before_a_lower_priority_side_effect_after_compatible_failover(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let incompatible_path = directory.path().join("incompatible.cmd");
        let compatible_path = directory.path().join("compatible.cmd");
        let lower_path = directory.path().join("lower.cmd");
        let incompatible_count = directory.path().join("incompatible-count");
        let compatible_count = directory.path().join("compatible-count");
        let lower_side_effect = directory.path().join("lower-side-effect");
        fs::write(
            &incompatible_path,
            format!(
                "@echo off\r\necho called>>\"{}\"\r\necho 0.62.0\r\n",
                incompatible_count.display()
            ),
        )?;
        fs::write(
            &compatible_path,
            format!(
                "@echo off\r\necho called>>\"{}\"\r\necho 0.63.0\r\n",
                compatible_count.display()
            ),
        )?;
        fs::write(
            &lower_path,
            format!(
                "@echo off\r\necho should-not-run>\"{}\"\r\necho 0.63.0\r\n",
                lower_side_effect.display()
            ),
        )?;
        let incompatible = platform_probe_explicit(DiscoveryLayer::Explicit, &incompatible_path);
        let compatible = platform_probe_explicit(DiscoveryLayer::InheritedPath, &compatible_path);
        let lower = platform_probe_explicit(DiscoveryLayer::Bundled, &lower_path);
        let report = DiscoveryReport {
            target: DiscoveryTarget::ClaudeAcp,
            process_path: OsString::new(),
            layers: vec![incompatible, compatible, lower],
            selected: None,
        };

        let probes = report.probe_runtimes_until(Duration::from_secs(2), |outcome| {
            matches!(
                outcome,
                RuntimeProbe::Runnable { stdout, .. } if stdout == "0.63.0"
            )
        });

        assert_eq!(probes.len(), 2);
        assert_eq!(fs::read_to_string(incompatible_count)?.lines().count(), 1);
        assert_eq!(fs::read_to_string(compatible_count)?.lines().count(), 1);
        assert!(!lower_side_effect.exists());
        Ok(())
    }

    #[test]
    fn claude_acp_discovers_the_adapter_instead_of_the_npx_launcher() {
        assert_eq!(DiscoveryTarget::ClaudeAcp.program(), "claude-agent-acp");
    }

    #[test]
    fn unresolved_file_or_inaccessible_candidate_remains_installation_evidence() {
        for status in [
            CandidateStatus::File,
            CandidateStatus::Inaccessible(io::ErrorKind::PermissionDenied),
        ] {
            let report = DiscoveryReport {
                target: DiscoveryTarget::OpenCode,
                process_path: OsString::new(),
                layers: vec![LayerReport {
                    layer: DiscoveryLayer::KnownLocation,
                    status: ProbeStatus::NotFound,
                    candidates: vec![Candidate {
                        path: PathBuf::from(r"C:\observed\opencode.exe"),
                        status,
                    }],
                    diagnostics: Vec::new(),
                }],
                selected: None,
            };

            assert_eq!(
                report.installation_assessment(&RuntimeProbe::NotResolved),
                InstallationAssessment::InstallationEvidenceWithoutResolvedCli
            );
        }
    }

    #[test]
    fn five_empty_sources_do_not_claim_that_an_agent_is_uninstalled() {
        let layers = [
            (DiscoveryLayer::Explicit, ProbeStatus::NotConfigured),
            (DiscoveryLayer::InheritedPath, ProbeStatus::NotFound),
            (DiscoveryLayer::PersistentPath, ProbeStatus::NotFound),
            (DiscoveryLayer::KnownLocation, ProbeStatus::NotFound),
            (DiscoveryLayer::Bundled, ProbeStatus::NotApplicable),
        ]
        .into_iter()
        .map(|(layer, status)| LayerReport {
            layer,
            status,
            candidates: Vec::new(),
            diagnostics: Vec::new(),
        })
        .collect();
        let report = DiscoveryReport {
            target: DiscoveryTarget::OpenCode,
            process_path: OsString::new(),
            layers,
            selected: None,
        };

        assert_eq!(
            report.installation_assessment(&RuntimeProbe::NotResolved),
            InstallationAssessment::NotObservedInFiveSources
        );
        assert!(report
            .installation_assessment(&RuntimeProbe::NotResolved)
            .to_string()
            .contains("not equivalent to an installation verdict"));
    }
}

#[cfg(test)]
mod fixture_path_tests {
    use std::env;
    use std::ffi::OsStr;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{apply_child_path_entries, resolved, Launcher};

    #[test]
    fn explicit_node_directory_precedes_an_impoverished_fixture_path(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let node_directory = directory
            .path()
            .join("explicit-node-outside-persistent-path");
        let impoverished = directory.path().join("impoverished-path");
        std::fs::create_dir(&node_directory)?;
        std::fs::create_dir(&impoverished)?;
        let adapter = resolved(directory.path().join("adapter"), Launcher::Native)
            .with_child_path_entry(&node_directory)?;
        let mut command = Command::new("fixture-child");
        command.env("PATH", &impoverished);

        apply_child_path_entries(&mut command, &adapter.child_path_entries)?;

        let child_path = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .ok_or("fixture child PATH must be set")?;
        assert_eq!(
            env::split_paths(child_path).collect::<Vec<_>>(),
            [node_directory, impoverished.clone()]
        );

        let unrelated_agent = resolved(directory.path().join("unrelated-agent"), Launcher::Native);
        let mut unrelated_command = Command::new("unrelated-fixture-child");
        unrelated_command.env("PATH", &impoverished);
        apply_child_path_entries(&mut unrelated_command, &unrelated_agent.child_path_entries)?;
        let unrelated_path = unrelated_command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .ok_or("unrelated fixture child PATH must remain set")?;
        assert_eq!(
            env::split_paths(unrelated_path).collect::<Vec<_>>(),
            [impoverished]
        );
        Ok(())
    }
}

#[cfg(test)]
mod installation_evidence_tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::{
        Candidate, CandidateStatus, DiscoveryLayer, DiscoveryReport, DiscoveryTarget,
        InstallationAssessment, LayerReport, ProbeStatus, RuntimeProbe,
    };

    #[test]
    fn evidence_never_becomes_a_resolved_protocol_launcher() {
        let evidence = vec![
            Candidate {
                path: PathBuf::from("observed-installation-directory"),
                status: CandidateStatus::DirectoryEvidence,
            },
            Candidate {
                path: PathBuf::from("observed-installation-artifact"),
                status: CandidateStatus::InstallationArtifactEvidence,
            },
        ];
        let report = DiscoveryReport {
            target: DiscoveryTarget::ClaudeCli,
            process_path: OsStr::new("").to_os_string(),
            layers: vec![LayerReport {
                layer: DiscoveryLayer::KnownLocation,
                status: ProbeStatus::NotFound,
                candidates: evidence,
                diagnostics: Vec::new(),
            }],
            selected: None,
        };

        assert!(report.resolved_candidates().is_empty());
        assert_eq!(
            report.installation_assessment(&RuntimeProbe::NotResolved),
            InstallationAssessment::InstallationEvidenceWithoutResolvedCli
        );
    }
}
