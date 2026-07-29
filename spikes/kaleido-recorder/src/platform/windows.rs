use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use super::{
    configure_child, resolved, Candidate, Launcher, ProcessError, ResolutionFailure,
    ResolvedExecutable,
};

const FALLBACK_EXTENSIONS: [&str; 3] = [".CMD", ".EXE", ".BAT"];

pub(super) fn resolve(program: &OsStr) -> Result<ResolvedExecutable, ResolutionFailure> {
    let path = env::var_os("PATH").unwrap_or_default();
    let path_ext = env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".CMD;.EXE;.BAT"));
    let where_output = where_output(program);
    resolve_in(program, &path, &path_ext, where_output)
}

fn resolve_in(
    program: &OsStr,
    search_path: &OsStr,
    path_ext: &OsStr,
    where_output: String,
) -> Result<ResolvedExecutable, ResolutionFailure> {
    let extensions = executable_extensions(path_ext);
    let directories: Vec<PathBuf> = env::split_paths(search_path).collect();
    let mut candidates = Vec::new();

    for directory in &directories {
        for extension in &extensions {
            let mut file_name = program.to_os_string();
            file_name.push(extension);
            let candidate = directory.join(&file_name);
            let found = is_regular_file(&candidate);
            candidates.push(Candidate {
                file_name: file_name.clone(),
                found,
            });
            if found {
                return Ok(resolved(
                    candidate,
                    launcher_for_extension(extension),
                    candidates,
                ));
            }
        }
    }

    Err(ResolutionFailure {
        program: program.to_os_string(),
        candidates,
        where_output,
    })
}

fn executable_extensions(path_ext: &OsStr) -> Vec<OsString> {
    let mut extensions = Vec::new();
    for extension in path_ext.to_string_lossy().split(';') {
        let upper = extension.trim().to_ascii_uppercase();
        if FALLBACK_EXTENSIONS.contains(&upper.as_str())
            && !extensions
                .iter()
                .any(|known: &OsString| known.eq_ignore_ascii_case(OsStr::new(&upper)))
        {
            extensions.push(OsString::from(upper));
        }
    }
    for fallback in FALLBACK_EXTENSIONS {
        if !extensions
            .iter()
            .any(|known| known.eq_ignore_ascii_case(OsStr::new(fallback)))
        {
            extensions.push(OsString::from(fallback));
        }
    }
    extensions
}

fn launcher_for_extension(extension: &OsStr) -> Launcher {
    if extension.eq_ignore_ascii_case(OsStr::new(".CMD")) {
        Launcher::CmdScript
    } else if extension.eq_ignore_ascii_case(OsStr::new(".BAT")) {
        Launcher::BatchScript
    } else {
        Launcher::Native
    }
}

fn is_regular_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn where_output(program: &OsStr) -> String {
    let mut command = Command::new("where.exe");
    configure_child(&mut command);
    match command.arg(program).output() {
        Ok(output) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            combined
        }
        Err(error) => format!("where.exe failed: {error}"),
    }
}

pub(super) fn terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    if let Some(status) = child.try_wait().map_err(ProcessError::Inspect)? {
        return Ok(status);
    }

    let mut taskkill = Command::new("taskkill.exe");
    configure_child(&mut taskkill);
    let status = taskkill
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(ProcessError::Terminate)?;
    let child_status = child.wait().map_err(ProcessError::Terminate)?;
    if status.success() || child_status.success() {
        Ok(child_status)
    } else {
        Err(ProcessError::Terminate(io::Error::other(format!(
            "taskkill exited with {status}; child exited with {child_status}"
        ))))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;

    use tempfile::tempdir;

    use super::resolve_in;
    use crate::platform::Launcher;

    #[test]
    fn ignores_extensionless_shim_and_selects_cmd() {
        let directory = tempdir().expect("temporary PATH directory must be created");
        fs::write(directory.path().join("codex"), "#!/bin/sh\n")
            .expect("extensionless shim must be written");
        fs::write(directory.path().join("codex.cmd"), "@echo off\r\n")
            .expect("cmd shim must be written");
        let search_path =
            std::env::join_paths([directory.path()]).expect("temporary PATH must be joined");

        let resolved = resolve_in(
            OsStr::new("codex"),
            &search_path,
            OsStr::new(".CMD;.EXE;.BAT"),
            String::new(),
        )
        .expect("cmd shim must resolve");

        assert_eq!(resolved.path(), directory.path().join("codex.cmd"));
        assert_eq!(resolved.launcher(), Launcher::CmdScript);
    }

    #[test]
    fn honors_pathext_order_for_supported_launchers() {
        let directory = tempdir().expect("temporary PATH directory must be created");
        fs::write(directory.path().join("agent.cmd"), "@echo off\r\n")
            .expect("cmd shim must be written");
        fs::write(directory.path().join("agent.exe"), b"MZ")
            .expect("exe candidate must be written");
        let search_path =
            std::env::join_paths([directory.path()]).expect("temporary PATH must be joined");

        let resolved = resolve_in(
            OsStr::new("agent"),
            &search_path,
            OsStr::new(".EXE;.CMD;.BAT"),
            String::new(),
        )
        .expect("exe must resolve first");

        assert_eq!(resolved.path(), directory.path().join("agent.exe"));
        assert_eq!(resolved.launcher(), Launcher::Native);
    }

    #[test]
    fn missing_program_reports_every_supported_candidate() {
        let directory = tempdir().expect("temporary PATH directory must be created");
        fs::write(directory.path().join("agent"), "#!/bin/sh\n")
            .expect("extensionless shim must be written");
        let search_path =
            std::env::join_paths([directory.path()]).expect("temporary PATH must be joined");

        let failure = resolve_in(
            OsStr::new("agent"),
            &search_path,
            OsStr::new(".CMD;.EXE;.BAT"),
            "INFO: no files".to_owned(),
        )
        .expect_err("extensionless shim must not count as executable");
        let names: Vec<String> = failure
            .candidates
            .iter()
            .map(|candidate| candidate.file_name.to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, ["agent.CMD", "agent.EXE", "agent.BAT"]);
        assert!(failure.candidates.iter().all(|candidate| !candidate.found));
        assert_eq!(failure.where_output, "INFO: no files");
    }
}
