use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use directories::BaseDirs;

use super::{
    platform_append_unix_installation_evidence, platform_extend_unix_known_executable_directories,
    read_user_npm_prefix, resolved, wait_for_spawned_process_family_exit, Candidate,
    CandidateStatus, DiscoveryLayer, DiscoveryTarget, Launcher, LayerReport, ProbeStatus,
    ProcessEntry, ProcessError, ResolvedExecutable, SpawnedProcessFamily,
};

const PROCESS_FAMILY_EXIT_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) fn fixture_sandbox_root_is_link(path: &Path) -> io::Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_type().is_symlink())
}

pub(super) fn quarantine_sandbox_working_copy(source: &Path, quarantine: &Path) -> io::Result<()> {
    fs::rename(source, quarantine)
}

pub(super) fn canonical_permission_path(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
}

pub(super) fn permission_path_pattern(path: &Path) -> io::Result<String> {
    canonical_permission_path(path)?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "path is not valid UTF-8"))
}

pub(super) fn create_test_directory_link(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

pub(super) fn set_test_preserved_file_metadata(path: &Path, marked: bool) -> io::Result<()> {
    let mode = if marked { 0o751 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

pub(super) fn test_preserved_file_metadata(path: &Path) -> io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

pub(super) fn create_test_node_probe(directory: &Path) -> io::Result<PathBuf> {
    let path = directory.join("node");
    fs::write(
        &path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' 'v22.13.0'\n  exit 0\nfi\nif [ \"$1\" = \"--kaleido-explicit-node\" ]; then\n  exit 0\nfi\nexit 23\n",
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

pub(super) fn create_test_acp_probe_requiring_node(
    directory: &Path,
    version: &str,
) -> io::Result<PathBuf> {
    let path = directory.join("claude-agent-acp");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nnode --kaleido-explicit-node >/dev/null 2>&1 || exit 29\nprintf '%s\\n' '{version}'\n"
        ),
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

pub(super) fn probe_explicit(layer: DiscoveryLayer, path: &Path) -> LayerReport {
    if !path.is_absolute() {
        return LayerReport {
            layer,
            status: ProbeStatus::InvalidConfiguration(
                "explicit executable path must be absolute".to_owned(),
            ),
            candidates: vec![Candidate {
                path: path.to_path_buf(),
                status: CandidateStatus::Missing,
            }],
            diagnostics: Vec::new(),
        };
    }
    let candidate = inspect_candidate(path);
    let status = if candidate.status == CandidateStatus::File {
        ProbeStatus::Found(resolved(path.to_path_buf(), Launcher::Native))
    } else {
        ProbeStatus::NotFound
    };
    LayerReport {
        layer,
        status,
        candidates: vec![candidate],
        diagnostics: Vec::new(),
    }
}

pub(super) fn probe_path(
    layer: DiscoveryLayer,
    program: &OsStr,
    search_path: &OsStr,
    _include_where: bool,
) -> LayerReport {
    let mut candidates = Vec::new();
    let mut selected = None;
    let mut diagnostics = Vec::new();
    for directory in env::split_paths(search_path) {
        if directory.as_os_str().is_empty() {
            diagnostics.push("ignored empty PATH entry".to_owned());
            continue;
        }
        if !directory.is_absolute() {
            diagnostics.push("ignored non-absolute PATH entry".to_owned());
            continue;
        }
        let path = directory.join(program);
        let candidate = inspect_candidate(&path);
        if selected.is_none() && candidate.status == CandidateStatus::File {
            selected = Some(resolved(path, Launcher::Native));
        }
        candidates.push(candidate);
    }
    LayerReport {
        layer,
        status: selected.map_or(ProbeStatus::NotFound, ProbeStatus::Found),
        candidates,
        diagnostics,
    }
}

pub(super) fn probe_persistent(_program: &OsStr) -> LayerReport {
    LayerReport {
        layer: DiscoveryLayer::PersistentPath,
        status: ProbeStatus::NotApplicable,
        candidates: Vec::new(),
        diagnostics: vec![
            "this platform has no standard process-independent persistent PATH API".to_owned(),
        ],
    }
}

pub(super) fn probe_known(
    target: DiscoveryTarget,
    base_directories: Option<BaseDirs>,
) -> LayerReport {
    let Some(base) = base_directories else {
        return LayerReport {
            layer: DiscoveryLayer::KnownLocation,
            status: ProbeStatus::NotFound,
            candidates: Vec::new(),
            diagnostics: vec!["standard user directories are unavailable".to_owned()],
        };
    };
    let mut diagnostics = Vec::new();
    let mut paths = vec![
        base.home_dir().join(".local").join("bin"),
        base.home_dir().join(".npm-global").join("bin"),
        base.data_dir().join("npm").join("bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ];
    if target == DiscoveryTarget::OpenCode {
        paths.push(base.home_dir().join("bin"));
        paths.push(base.home_dir().join(".opencode").join("bin"));
    }
    if let Some(directory) = base.executable_dir() {
        paths.push(directory.to_path_buf());
    }
    match env::var_os("NPM_CONFIG_PREFIX").map(PathBuf::from) {
        Some(prefix) if prefix.is_absolute() => {
            diagnostics.push(format!("NPM_CONFIG_PREFIX={}", prefix.to_string_lossy()));
            paths.push(prefix.clone());
            paths.push(prefix.join("bin"));
        }
        Some(_) => diagnostics.push("ignored non-absolute NPM_CONFIG_PREFIX".to_owned()),
        None => diagnostics.push("NPM_CONFIG_PREFIX is not set".to_owned()),
    }
    if let Some(prefix) = read_user_npm_prefix(base.home_dir(), base.data_dir(), &mut diagnostics) {
        paths.push(prefix.clone());
        paths.push(prefix.join("bin"));
    }
    platform_extend_unix_known_executable_directories(&mut paths);
    paths.sort();
    paths.dedup();
    let joined = match env::join_paths(&paths) {
        Ok(path) => path,
        Err(error) => {
            diagnostics.push(format!("join failure: {error}"));
            return LayerReport {
                layer: DiscoveryLayer::KnownLocation,
                status: ProbeStatus::InvalidConfiguration(
                    "known locations contain an unrepresentable entry".to_owned(),
                ),
                candidates: Vec::new(),
                diagnostics,
            };
        }
    };
    let mut report = probe_path(
        DiscoveryLayer::KnownLocation,
        OsStr::new(target.program()),
        &joined,
        false,
    );
    platform_append_unix_installation_evidence(
        target,
        base.home_dir(),
        base.data_dir(),
        &paths,
        &mut report.candidates,
        &mut diagnostics,
    );
    diagnostics.append(&mut report.diagnostics);
    report.diagnostics = diagnostics;
    report
}

pub(super) fn command(executable: &ResolvedExecutable, arguments: &[OsString]) -> Command {
    let mut command = Command::new(executable.path());
    command.args(arguments);
    configure_child(&mut command);
    command
}

pub(super) fn resolve_candidate(path: &Path) -> Option<ResolvedExecutable> {
    Some(resolved(path.to_path_buf(), Launcher::Native))
}

fn inspect_candidate(path: &Path) -> Candidate {
    let status = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 => {
            CandidateStatus::File
        }
        Ok(metadata) if metadata.is_file() => {
            CandidateStatus::Inaccessible(io::ErrorKind::PermissionDenied)
        }
        Ok(_) => CandidateStatus::NotFile,
        Err(error) if error.kind() == io::ErrorKind::NotFound => CandidateStatus::Missing,
        Err(error) => CandidateStatus::Inaccessible(error.kind()),
    };
    Candidate {
        path: path.to_path_buf(),
        status,
    }
}

pub(super) fn terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    let root_pid = child.id();
    let family =
        snapshot_processes().map(|snapshot| SpawnedProcessFamily::capture(root_pid, &snapshot));
    if let Some(status) = child.try_wait().map_err(ProcessError::Inspect)? {
        return verify_spawned_process_family(status, root_pid, family);
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
        verify_spawned_process_family(child_status, root_pid, family)
    } else {
        Err(ProcessError::Terminate(io::Error::other(format!(
            "process-group termination exited with {terminate} and {kill}; child exited with {child_status}"
        ))))
    }
}

fn verify_spawned_process_family(
    status: ExitStatus,
    root_pid: u32,
    family: io::Result<SpawnedProcessFamily>,
) -> Result<ExitStatus, ProcessError> {
    let family = family.map_err(|error| {
        ProcessError::Terminate(io::Error::new(
            error.kind(),
            format!(
                "spawned root PID {root_pid} was reaped, but its process family could not be \
                 captured before cleanup: {error}"
            ),
        ))
    })?;
    wait_for_spawned_process_family_exit(&family, PROCESS_FAMILY_EXIT_TIMEOUT, snapshot_processes)?;
    Ok(status)
}

fn snapshot_processes() -> io::Result<Vec<ProcessEntry>> {
    let output = Command::new("ps")
        .args(["-e", "-o", "pid=", "-o", "ppid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ps process snapshot exited with {}",
            output.status
        )));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ps output was not UTF-8"))?;
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_process_entry)
        .collect()
}

fn parse_process_entry(line: &str) -> io::Result<ProcessEntry> {
    let mut fields = line.split_ascii_whitespace();
    let pid = fields
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ps row omitted PID"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ps PID was not numeric"))?;
    let parent_pid = fields
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ps row omitted parent PID"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ps parent PID was not numeric"))?;
    if fields.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ps row contained unexpected fields",
        ));
    }
    Ok(ProcessEntry { pid, parent_pid })
}

fn configure_child(command: &mut Command) {
    command.process_group(0);
}
