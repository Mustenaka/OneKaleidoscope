use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use super::{resolved, Candidate, Launcher, ProcessError, ResolutionFailure, ResolvedExecutable};

pub(super) fn resolve(program: &OsStr) -> Result<ResolvedExecutable, ResolutionFailure> {
    let path = env::var_os("PATH").unwrap_or_default();
    let mut candidates = Vec::new();
    for directory in env::split_paths(&path) {
        let candidate = directory.join(program);
        let found = is_executable(&candidate);
        candidates.push(Candidate {
            file_name: program.to_os_string(),
            found,
        });
        if found {
            return Ok(resolved(candidate, Launcher::Native, candidates));
        }
    }
    Err(ResolutionFailure {
        program: program.to_os_string(),
        candidates,
        where_output: String::new(),
    })
}

fn is_executable(path: &PathBuf) -> bool {
    fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    })
}

pub(super) fn terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    if let Some(status) = child.try_wait().map_err(ProcessError::Inspect)? {
        return Ok(status);
    }

    let process_group = format!("-{}", child.id());
    let terminate = Command::new("kill")
        .args(["-TERM", "--", &process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(ProcessError::Terminate)?;
    thread::sleep(Duration::from_millis(250));
    let kill = Command::new("kill")
        .args(["-KILL", "--", &process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(ProcessError::Terminate)?;
    let child_status = child.wait().map_err(ProcessError::Terminate)?;
    if terminate.success() || kill.success() || child_status.success() {
        Ok(child_status)
    } else {
        Err(ProcessError::Terminate(io::Error::other(format!(
            "process-group termination exited with {terminate} and {kill}; child exited with {child_status}"
        ))))
    }
}

pub(crate) fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}
