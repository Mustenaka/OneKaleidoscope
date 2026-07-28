mod forbidden;

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

use forbidden::scan_repository;

#[derive(Debug)]
enum Task {
    Ci,
    Fmt,
    Clippy,
    Test,
    LintForbidden,
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
}

impl fmt::Display for XtaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                formatter,
                "usage: cargo xtask <ci|fmt|clippy|test|lint-forbidden>"
            ),
            Self::WorkspaceRoot => write!(formatter, "could not resolve the workspace root"),
            Self::Io(error) => write!(formatter, "I/O failure: {error}"),
            Self::StepFailed { step, status } => {
                write!(formatter, "step `{step}` failed with status {status}")
            }
            Self::ForbiddenFound(count) => {
                write!(formatter, "lint-forbidden found {count} violation(s)")
            }
        }
    }
}

impl std::error::Error for XtaskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for XtaskError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), XtaskError> {
    let task = parse_task()?;
    let root = workspace_root()?;

    match task {
        Task::Ci => {
            run_cargo_step(&root, "fmt-check", &["fmt", "--all", "--", "--check"])?;
            run_cargo_step(
                &root,
                "clippy",
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            )?;
            run_cargo_step(&root, "test", &["test", "--workspace"])?;
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
    }
}

fn parse_task() -> Result<Task, XtaskError> {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Err(XtaskError::Usage);
    };
    if arguments.next().is_some() {
        return Err(XtaskError::Usage);
    }

    match command.to_str() {
        Some("ci") => Ok(Task::Ci),
        Some("fmt") => Ok(Task::Fmt),
        Some("clippy") => Ok(Task::Clippy),
        Some("test") => Ok(Task::Test),
        Some("lint-forbidden") => Ok(Task::LintForbidden),
        _ => Err(XtaskError::Usage),
    }
}

fn workspace_root() -> Result<PathBuf, XtaskError> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or(XtaskError::WorkspaceRoot)
}

fn run_cargo_step(root: &Path, step: &'static str, arguments: &[&str]) -> Result<(), XtaskError> {
    announce(step)?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let status = Command::new(cargo)
        .args(arguments)
        .current_dir(root)
        .status()?;
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
