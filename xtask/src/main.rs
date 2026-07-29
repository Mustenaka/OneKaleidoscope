mod forbidden;

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

use forbidden::scan_repository;
use xtask::schema::{self, SchemaCommand};

#[derive(Debug)]
enum Task {
    Ci,
    Fmt,
    Clippy,
    Test,
    LintForbidden,
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
    Schema(schema::SchemaError),
}

impl fmt::Display for XtaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                formatter,
                "usage: cargo xtask <ci|fmt|clippy|test|lint-forbidden|schema <refresh|diff>>"
            ),
            Self::WorkspaceRoot => write!(formatter, "could not resolve the workspace root"),
            Self::Io(error) => write!(formatter, "I/O failure: {error}"),
            Self::StepFailed { step, status } => {
                write!(formatter, "step `{step}` failed with status {status}")
            }
            Self::ForbiddenFound(count) => {
                write!(formatter, "lint-forbidden found {count} violation(s)")
            }
            Self::Schema(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for XtaskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
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
            run_cargo_step_in_target(
                &root,
                &target,
                "clippy",
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            )?;
            run_cargo_step_in_target(&root, &target, "test", &["test", "--workspace"])?;
            run_forbidden_step(&root)
        }
        Task::Fmt => run_cargo_step(&root, "fmt-check", &["fmt", "--all", "--", "--check"]),
        Task::Clippy => run_cargo_step(
            &root,
            "clippy",
            &["clippy", "--all-targets", "--", "-D", "warnings"],
        ),
        Task::Test => run_cargo_step(&root, "test", &["test", "--workspace"]),
        Task::LintForbidden => run_forbidden_step(&root),
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
        Some("lint-forbidden") => parse_without_extra_arguments(Task::LintForbidden, arguments),
        Some("schema") => {
            let Some(subcommand) = arguments.next() else {
                return Err(XtaskError::Usage);
            };
            let schema_command = match subcommand.to_str() {
                Some("refresh") => SchemaCommand::Refresh,
                Some("diff") => SchemaCommand::Diff,
                _ => return Err(XtaskError::Usage),
            };
            parse_without_extra_arguments(Task::Schema(schema_command), arguments)
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
    let violations = scan_repository(root)?;
    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("{violation}");
        }
        return Err(XtaskError::ForbiddenFound(violations.len()));
    }
    println!("<== {STEP}: ok");
    Ok(())
}

fn announce(step: &str) -> Result<(), XtaskError> {
    println!("==> {step}");
    io::stdout().flush()?;
    Ok(())
}
