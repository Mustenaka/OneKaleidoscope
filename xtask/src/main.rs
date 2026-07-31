use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

use xtask::antipattern::{self, scan_repository};
use xtask::schema::{self, SchemaCommand};
use xtask::{deps, fixtures};

#[derive(Debug)]
enum Task {
    Ci,
    Fmt,
    Clippy,
    Test,
    CheckDeps,
    LintForbidden,
    FixturesVerify,
    Schema(SchemaCommand),
}

#[derive(Debug)]
enum XtaskError {
    Usage,
    WorkspaceRoot,
    Io(io::Error),
    StepFailed {
        step: &'static str,
        status: ExitStatus,
    },
    ForbiddenFound(usize),
    Dependencies(deps::DependencyCheckError),
    Antipattern(antipattern::AntipatternError),
    Fixtures(fixtures::FixtureVerifyError),
    Schema(schema::SchemaError),
}

impl fmt::Display for XtaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                formatter,
                "usage: cargo xtask <ci|fmt|clippy|test|check-deps|lint-forbidden|fixtures verify|schema <refresh|diff|history <tool> <entry-id>>>"
            ),
            Self::WorkspaceRoot => write!(formatter, "could not resolve the workspace root"),
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
            Self::Schema(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for XtaskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Dependencies(error) => Some(error),
            Self::Antipattern(error) => Some(error),
            Self::Fixtures(error) => Some(error),
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
            let target = root.join("target").join("xtask-ci");
            run_cargo_step_in_target(
                &root,
                &target,
                "fmt-check",
                &["fmt", "--all", "--", "--check"],
            )?;
            run_deps_step(&root)?;
            run_forbidden_step(&root)?;
            run_cargo_step_in_target(
                &root,
                &target,
                "clippy",
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            )?;
            run_test_step(&root, Some(&target))?;
            run_fixtures_step(&root)
        }
        Task::Fmt => run_cargo_step(&root, "fmt-check", &["fmt", "--all", "--", "--check"]),
        Task::Clippy => run_cargo_step(
            &root,
            "clippy",
            &["clippy", "--all-targets", "--", "-D", "warnings"],
        ),
        Task::Test => {
            let target = root.join("target").join("xtask-test");
            run_test_step(&root, Some(&target))
        }
        Task::CheckDeps => run_deps_step(&root),
        Task::LintForbidden => run_forbidden_step(&root),
        Task::FixturesVerify => run_fixtures_step(&root),
        Task::Schema(command) => schema::run(command, &root).map_err(Into::into),
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

fn run_cargo_step(root: &Path, step: &'static str, arguments: &[&str]) -> Result<(), XtaskError> {
    run_cargo_step_with_target(root, None, step, arguments)
}

fn run_cargo_step_in_target(
    root: &Path,
    target: &Path,
    step: &'static str,
    arguments: &[&str],
) -> Result<(), XtaskError> {
    run_cargo_step_with_target(root, Some(target), step, arguments)
}

#[derive(Debug, Eq, PartialEq)]
struct TestScope {
    arguments: &'static [&'static str],
    exclusion_notice: &'static str,
}

fn test_scope() -> TestScope {
    TestScope {
        arguments: &["test", "--workspace", "--exclude", "kaleido-recorder"],
        exclusion_notice: "test: kaleido-recorder excluded on all platforms (ADR-0016)",
    }
}

fn run_test_step(root: &Path, target: Option<&Path>) -> Result<(), XtaskError> {
    let scope = test_scope();
    println!("{}", scope.exclusion_notice);
    run_cargo_step_with_target(root, target, "test", scope.arguments)
}

fn run_cargo_step_with_target(
    root: &Path,
    target: Option<&Path>,
    step: &'static str,
    arguments: &[&str],
) -> Result<(), XtaskError> {
    announce(step)?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command.args(arguments).current_dir(root);
    if let Some(target) = target {
        command.env("CARGO_TARGET_DIR", target);
    }
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

fn announce(step: &str) -> Result<(), XtaskError> {
    println!("==> {step}");
    io::stdout().flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{test_scope, TestScope};

    #[test]
    fn test_scope_excludes_the_recorder_on_all_platforms_with_an_explicit_notice() {
        assert_eq!(
            test_scope(),
            TestScope {
                arguments: &["test", "--workspace", "--exclude", "kaleido-recorder"],
                exclusion_notice: "test: kaleido-recorder excluded on all platforms (ADR-0016)",
            }
        );
    }
}
