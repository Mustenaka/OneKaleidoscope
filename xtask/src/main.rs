use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

use xtask::antipattern::{self, scan_repository};
use xtask::build::{self, BuildRootError};
use xtask::schema::{self, SchemaCommand};
use xtask::test_gate::test_gate_plan;
use xtask::{deps, fixtures, sidecar};

#[derive(Debug)]
enum Task {
    Ci,
    Fmt,
    Clippy,
    Test,
    CheckDeps,
    LintForbidden,
    ClaudeSidecar,
    FixturesVerify,
    Schema(SchemaCommand),
}

#[derive(Debug)]
enum XtaskError {
    Usage,
    WorkspaceRoot,
    BuildRoot(BuildRootError),
    Io(io::Error),
    StepFailed {
        step: &'static str,
        status: ExitStatus,
    },
    ForbiddenFound(usize),
    Dependencies(deps::DependencyCheckError),
    Antipattern(antipattern::AntipatternError),
    Fixtures(fixtures::FixtureVerifyError),
    Sidecar(sidecar::SidecarCheckError),
    Schema(schema::SchemaError),
}

impl fmt::Display for XtaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                formatter,
                "usage: cargo xtask <ci|fmt|clippy|test|check-deps|lint-forbidden|claude-sidecar|fixtures verify|schema <refresh|diff|history <tool> <entry-id>>>"
            ),
            Self::WorkspaceRoot => write!(formatter, "could not resolve the workspace root"),
            Self::BuildRoot(error) => write!(formatter, "build artifact root: {error}"),
            Self::Io(error) => write!(formatter, "I/O failure: {error}"),
            Self::StepFailed { step, status } => {
                write!(formatter, "step `{step}` failed with status {status}")
            }
            Self::ForbiddenFound(count) => {
                write!(formatter, "lint-forbidden found {count} violation(s)")
            }
            Self::Dependencies(error) => write!(formatter, "{error}"),
            Self::Antipattern(error) => write!(formatter, "{error}"),
            Self::Fixtures(error) => write!(formatter, "{error}"),
            Self::Sidecar(error) => write!(formatter, "{error}"),
            Self::Schema(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for XtaskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::BuildRoot(error) => Some(error),
            Self::Dependencies(error) => Some(error),
            Self::Antipattern(error) => Some(error),
            Self::Fixtures(error) => Some(error),
            Self::Sidecar(error) => Some(error),
            Self::Schema(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for XtaskError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<BuildRootError> for XtaskError {
    fn from(error: BuildRootError) -> Self {
        Self::BuildRoot(error)
    }
}

impl From<schema::SchemaError> for XtaskError {
    fn from(error: schema::SchemaError) -> Self {
        Self::Schema(error)
    }
}

impl From<deps::DependencyCheckError> for XtaskError {
    fn from(error: deps::DependencyCheckError) -> Self {
        Self::Dependencies(error)
    }
}

impl From<antipattern::AntipatternError> for XtaskError {
    fn from(error: antipattern::AntipatternError) -> Self {
        Self::Antipattern(error)
    }
}

impl From<fixtures::FixtureVerifyError> for XtaskError {
    fn from(error: fixtures::FixtureVerifyError) -> Self {
        Self::Fixtures(error)
    }
}

impl From<sidecar::SidecarCheckError> for XtaskError {
    fn from(error: sidecar::SidecarCheckError) -> Self {
        Self::Sidecar(error)
    }
}

impl XtaskError {
    const fn exit_code(&self) -> u8 {
        match self {
            Self::Schema(error) => error.exit_code(),
            _ => 1,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

fn run() -> Result<(), XtaskError> {
    let task = parse_task()?;
    let root = workspace_root()?;

    match task {
        Task::Ci => {
            let target = build::cargo_target_dir(&root)?;
            run_cargo_step_with_target(
                &root,
                &target,
                "fmt-check",
                &["fmt", "--all", "--", "--check"],
            )?;
            run_deps_step(&root)?;
            run_forbidden_step(&root)?;
            run_cargo_step_with_target(
                &root,
                &target,
                "clippy",
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            )?;
            run_claude_sidecar_step(&root)?;
            run_test_step(&root, &target)?;
            run_fixtures_step(&root)
        }
        Task::Fmt => {
            let target = build::cargo_target_dir(&root)?;
            run_cargo_step_with_target(
                &root,
                &target,
                "fmt-check",
                &["fmt", "--all", "--", "--check"],
            )
        }
        Task::Clippy => {
            let target = build::cargo_target_dir(&root)?;
            run_cargo_step_with_target(
                &root,
                &target,
                "clippy",
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            )
        }
        Task::Test => {
            let target = build::cargo_target_dir(&root)?;
            run_test_step(&root, &target)
        }
        Task::CheckDeps => run_deps_step(&root),
        Task::LintForbidden => run_forbidden_step(&root),
        Task::ClaudeSidecar => run_claude_sidecar_step(&root),
        Task::FixturesVerify => run_fixtures_step(&root),
        Task::Schema(command) => {
            let target = build::cargo_target_dir(&root)?;
            schema::run(command, &root, &target).map_err(Into::into)
        }
    }
}

fn parse_task() -> Result<Task, XtaskError> {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Err(XtaskError::Usage);
    };

    match command.to_str() {
        Some("ci") => parse_without_extra_arguments(Task::Ci, arguments),
        Some("fmt") => parse_without_extra_arguments(Task::Fmt, arguments),
        Some("clippy") => parse_without_extra_arguments(Task::Clippy, arguments),
        Some("test") => parse_without_extra_arguments(Task::Test, arguments),
        Some("check-deps") => parse_without_extra_arguments(Task::CheckDeps, arguments),
        Some("lint-forbidden") => parse_without_extra_arguments(Task::LintForbidden, arguments),
        Some("claude-sidecar") => parse_without_extra_arguments(Task::ClaudeSidecar, arguments),
        Some("fixtures") => {
            let Some(subcommand) = arguments.next() else {
                return Err(XtaskError::Usage);
            };
            match subcommand.to_str() {
                Some("verify") => parse_without_extra_arguments(Task::FixturesVerify, arguments),
                _ => Err(XtaskError::Usage),
            }
        }
        Some("schema") => {
            let Some(subcommand) = arguments.next() else {
                return Err(XtaskError::Usage);
            };
            match subcommand.to_str() {
                Some("refresh") => {
                    parse_without_extra_arguments(Task::Schema(SchemaCommand::Refresh), arguments)
                }
                Some("diff") => {
                    parse_without_extra_arguments(Task::Schema(SchemaCommand::Diff), arguments)
                }
                Some("history") => {
                    let Some(tool) = arguments.next().and_then(|value| value.into_string().ok())
                    else {
                        return Err(XtaskError::Usage);
                    };
                    let Some(entry_id) =
                        arguments.next().and_then(|value| value.into_string().ok())
                    else {
                        return Err(XtaskError::Usage);
                    };
                    parse_without_extra_arguments(
                        Task::Schema(SchemaCommand::History { tool, entry_id }),
                        arguments,
                    )
                }
                _ => Err(XtaskError::Usage),
            }
        }
        _ => Err(XtaskError::Usage),
    }
}

fn parse_without_extra_arguments(
    task: Task,
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<Task, XtaskError> {
    if arguments.next().is_some() {
        Err(XtaskError::Usage)
    } else {
        Ok(task)
    }
}

fn workspace_root() -> Result<PathBuf, XtaskError> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or(XtaskError::WorkspaceRoot)
}

fn run_test_step(root: &Path, target: &Path) -> Result<(), XtaskError> {
    for invocation in test_gate_plan() {
        println!("{}", invocation.notice);
        run_cargo_step_with_target(root, target, "test", invocation.cargo_arguments)?;
    }
    Ok(())
}

fn run_cargo_step_with_target(
    root: &Path,
    target: &Path,
    step: &'static str,
    arguments: &[&str],
) -> Result<(), XtaskError> {
    announce(step)?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command.args(arguments).current_dir(root);
    command.env("CARGO_TARGET_DIR", target);
    let status = command.status()?;
    if !status.success() {
        return Err(XtaskError::StepFailed { step, status });
    }
    println!("<== {step}: ok");
    Ok(())
}

fn run_forbidden_step(root: &Path) -> Result<(), XtaskError> {
    const STEP: &str = "lint-forbidden";
    announce(STEP)?;
    let report = scan_repository(root)?;
    println!(
        "lint-forbidden: A-2 agent-name-branch exemptions={}",
        report.agent_name_branch_exemptions
    );
    println!(
        "lint-forbidden: A-11 version-branch exemptions={}",
        report.version_branch_exemptions
    );
    if !report.violations.is_empty() {
        for violation in &report.violations {
            eprintln!("{violation}");
        }
        return Err(XtaskError::ForbiddenFound(report.violations.len()));
    }
    println!("<== {STEP}: ok");
    Ok(())
}

fn run_deps_step(root: &Path) -> Result<(), XtaskError> {
    const STEP: &str = "check-deps";
    announce(STEP)?;
    let report = deps::check_workspace(root)?;
    println!("<== {STEP}: ok; {report}");
    Ok(())
}

fn run_fixtures_step(root: &Path) -> Result<(), XtaskError> {
    const STEP: &str = "fixtures-verify";
    announce(STEP)?;
    let summary = fixtures::verify_workspace(root)?;
    if summary.files == 0 {
        println!("<== {STEP}: ok; 0 files, 0 records (no fixture files found)");
    } else {
        println!("<== {STEP}: ok; {summary}");
    }
    Ok(())
}

fn run_claude_sidecar_step(root: &Path) -> Result<(), XtaskError> {
    const STEP: &str = "claude-sidecar";
    announce(STEP)?;
    sidecar::check_workspace(root)?;
    println!("<== {STEP}: ok; npm ci --ignore-scripts + npm run typecheck");
    Ok(())
}

fn announce(step: &str) -> Result<(), XtaskError> {
    println!("==> {step}");
    io::stdout().flush()?;
    Ok(())
}
