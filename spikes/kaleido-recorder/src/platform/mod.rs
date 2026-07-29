use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use thiserror::Error;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub file_name: OsString,
    pub found: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedExecutable {
    path: PathBuf,
    launcher: Launcher,
    candidates: Vec<Candidate>,
}

impl ResolvedExecutable {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn launcher(&self) -> Launcher {
        self.launcher
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.path);
        configure_child(&mut command);
        command
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionFailure {
    pub program: OsString,
    pub candidates: Vec<Candidate>,
    pub where_output: String,
}

impl fmt::Display for ResolutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not resolve {:?}; {} candidate(s) checked",
            self.program,
            self.candidates.len()
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
}

pub fn resolve(program: &OsStr) -> Result<ResolvedExecutable, ResolutionFailure> {
    platform_resolve(program)
}

pub fn spawn(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
    cwd: &Path,
) -> Result<Child, ProcessError> {
    let child = executable
        .command()
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ProcessError::Spawn)?;
    Ok(child)
}

pub fn terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    platform_terminate_tree(child)
}

#[cfg(windows)]
fn platform_resolve(program: &OsStr) -> Result<ResolvedExecutable, ResolutionFailure> {
    windows::resolve(program)
}

#[cfg(unix)]
fn platform_resolve(program: &OsStr) -> Result<ResolvedExecutable, ResolutionFailure> {
    unix::resolve(program)
}

#[cfg(windows)]
fn platform_terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    windows::terminate_tree(child)
}

#[cfg(unix)]
fn platform_terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    unix::terminate_tree(child)
}

fn configure_child(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

fn resolved(
    path: PathBuf,
    launcher: Launcher,
    candidates: Vec<Candidate>,
) -> ResolvedExecutable {
    ResolvedExecutable {
        path,
        launcher,
        candidates,
    }
}
