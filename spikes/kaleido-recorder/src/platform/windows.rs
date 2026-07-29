use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use directories::BaseDirs;

use super::{
    read_user_npm_prefix, resolved, wait_for_spawned_process_family_exit, Candidate,
    CandidateStatus, DiscoveryLayer, DiscoveryTarget, Launcher, LayerReport, ProbeStatus,
    ProcessEntry, ProcessError, ResolvedExecutable, SpawnedProcessFamily,
};

const FALLBACK_EXTENSIONS: [&str; 3] = [".CMD", ".EXE", ".BAT"];
const USER_ENVIRONMENT_KEY: &str = r"HKCU\Environment";
const MACHINE_ENVIRONMENT_KEY: &str =
    r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const NODE_MACHINE_KEY: &str = r"HKLM\SOFTWARE\Node.js";
const NVM_HOME_VALUE: &str = "NVM_HOME";
const NVM_SYMLINK_VALUE: &str = "NVM_SYMLINK";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// npm adapters can take several seconds to finish native descendants after their launcher is
// reaped. Keep the verification bounded, but do not report a false residue while the exact
// captured PID family is still completing forced shutdown.
const PROCESS_FAMILY_EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const SANDBOX_RENAME_RETRY_TIMEOUT: Duration = Duration::from_secs(2);
const SANDBOX_RENAME_RETRY_INTERVAL: Duration = Duration::from_millis(20);
const ERROR_SHARING_VIOLATION: i32 = 32;
const DESCENDANT_FORCE_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

// Toolhelp32 is the only FFI boundary in the recorder. It enumerates numeric PID/PPID
// relationships and intentionally never reads or compares executable names.
#[allow(unsafe_code)]
mod process_snapshot_ffi {
    use std::ffi::c_void;
    use std::io;
    use std::mem;

    use super::ProcessEntry;

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const ERROR_NO_MORE_FILES: i32 = 18;
    const MAX_PATH: usize = 260;
    const INVALID_HANDLE_VALUE: *mut c_void = -1_isize as *mut c_void;

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        thread_count: u32,
        parent_process_id: u32,
        base_priority: i32,
        flags: u32,
        executable_file: [u16; MAX_PATH],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    struct SnapshotHandle(*mut c_void);

    impl Drop for SnapshotHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a live snapshot handle returned by
            // `CreateToolhelp32Snapshot`; this owner closes it exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn snapshot_processes() -> io::Result<Vec<ProcessEntry>> {
        // SAFETY: The flags request a system process snapshot and the PID argument is
        // required to be zero for this snapshot kind. No borrowed pointers are passed.
        let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw_snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = SnapshotHandle(raw_snapshot);
        let mut raw_entry = ProcessEntry32W {
            size: mem::size_of::<ProcessEntry32W>() as u32,
            usage: 0,
            process_id: 0,
            default_heap_id: 0,
            module_id: 0,
            thread_count: 0,
            parent_process_id: 0,
            base_priority: 0,
            flags: 0,
            executable_file: [0; MAX_PATH],
        };
        // SAFETY: `snapshot.0` remains live for this call and `raw_entry` points to a
        // writable, correctly sized `ProcessEntry32W`.
        if unsafe { Process32FirstW(snapshot.0, &mut raw_entry) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut processes = Vec::new();
        loop {
            processes.push(ProcessEntry {
                pid: raw_entry.process_id,
                parent_pid: raw_entry.parent_process_id,
            });
            // SAFETY: The same live snapshot and valid writable entry are retained for
            // the complete enumeration.
            if unsafe { Process32NextW(snapshot.0, &mut raw_entry) } != 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                break;
            }
            return Err(error);
        }
        Ok(processes)
    }
}

// These process handles bind cleanup to the exact PIDs captured while the recorder-owned
// launcher is still alive. Holding the handles prevents PID reuse from redirecting cleanup to an
// unrelated process. No executable names or command lines cross this FFI boundary.
#[allow(unsafe_code)]
mod process_handle_ffi {
    use std::ffi::c_void;
    use std::io;
    use std::ptr;
    use std::time::{Duration, Instant};

    const PROCESS_TERMINATE: u32 = 0x0000_0001;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    const WAIT_FAILED: u32 = 0xffff_ffff;
    const ERROR_INVALID_PARAMETER: i32 = 87;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn TerminateProcess(process: *mut c_void, exit_code: u32) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    #[derive(Debug)]
    struct OwnedProcessHandle {
        raw: *mut c_void,
    }

    impl OwnedProcessHandle {
        fn open(process_id: u32) -> io::Result<Option<Self>> {
            // SAFETY: OpenProcess is called with a numeric PID from Toolhelp32 and no inherited
            // handle. A non-null handle is owned by this value and closed exactly once in Drop.
            let raw = unsafe { OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, process_id) };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
                    return Ok(None);
                }
                return Err(error);
            }
            Ok(Some(Self { raw }))
        }

        fn is_exited(&self) -> io::Result<bool> {
            // SAFETY: self.raw remains a valid owned process handle until Drop.
            match unsafe { WaitForSingleObject(self.raw, 0) } {
                WAIT_OBJECT_0 => Ok(true),
                WAIT_TIMEOUT => Ok(false),
                WAIT_FAILED => Err(io::Error::last_os_error()),
                outcome => Err(io::Error::other(format!(
                    "unexpected process wait result {outcome}"
                ))),
            }
        }

        fn terminate(&self) -> io::Result<()> {
            if self.is_exited()? {
                return Ok(());
            }
            // SAFETY: self.raw is an owned handle opened with PROCESS_TERMINATE.
            if unsafe { TerminateProcess(self.raw, 1) } == 0 {
                if self.is_exited()? {
                    return Ok(());
                }
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        fn wait_for_exit(&self, timeout: Duration) -> io::Result<()> {
            let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: self.raw remains valid for the duration of this bounded wait.
            match unsafe { WaitForSingleObject(self.raw, milliseconds) } {
                WAIT_OBJECT_0 => Ok(()),
                WAIT_TIMEOUT => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "captured descendant did not exit after forced termination",
                )),
                WAIT_FAILED => Err(io::Error::last_os_error()),
                outcome => Err(io::Error::other(format!(
                    "unexpected process wait result {outcome}"
                ))),
            }
        }
    }

    impl Drop for OwnedProcessHandle {
        fn drop(&mut self) {
            if !self.raw.is_null() {
                // SAFETY: self.raw is owned by this value and has not been closed elsewhere.
                let _close_result = unsafe { CloseHandle(self.raw) };
                self.raw = ptr::null_mut();
            }
        }
    }

    #[derive(Debug)]
    pub(super) struct CapturedDescendants {
        handles: Vec<OwnedProcessHandle>,
    }

    impl CapturedDescendants {
        pub(super) fn capture(process_ids: impl Iterator<Item = u32>) -> io::Result<Self> {
            let mut handles = Vec::new();
            for process_id in process_ids {
                if let Some(handle) = OwnedProcessHandle::open(process_id)? {
                    handles.push(handle);
                }
            }
            Ok(Self { handles })
        }

        pub(super) fn terminate_and_wait(&self, timeout: Duration) -> io::Result<()> {
            for handle in &self.handles {
                handle.terminate()?;
            }
            let started = Instant::now();
            for handle in &self.handles {
                let remaining = timeout.saturating_sub(started.elapsed());
                handle.wait_for_exit(remaining)?;
            }
            Ok(())
        }
    }
}

fn configure_child(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

pub(super) fn fixture_sandbox_root_is_link(path: &Path) -> io::Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

pub(super) fn quarantine_sandbox_working_copy(source: &Path, quarantine: &Path) -> io::Result<()> {
    let started = Instant::now();
    retry_directory_rename_after_sharing_violation(
        || fs::rename(source, quarantine),
        || started.elapsed() < SANDBOX_RENAME_RETRY_TIMEOUT,
        || thread::sleep(SANDBOX_RENAME_RETRY_INTERVAL),
    )
}

fn retry_directory_rename_after_sharing_violation(
    mut operation: impl FnMut() -> io::Result<()>,
    mut retry_available: impl FnMut() -> bool,
    mut wait: impl FnMut(),
) -> io::Result<()> {
    loop {
        match operation() {
            Err(error)
                if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) && retry_available() =>
            {
                wait();
            }
            outcome => return outcome,
        }
    }
}

pub(super) fn canonical_permission_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    let value = canonical
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "path is not valid UTF-8"))?;
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        return Ok(PathBuf::from(format!(r"\\{unc}")));
    }
    Ok(PathBuf::from(value.strip_prefix(r"\\?\").unwrap_or(value)))
}

pub(super) fn permission_path_pattern(path: &Path) -> io::Result<String> {
    canonical_permission_path(path)?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "path is not valid UTF-8"))
}

pub(super) fn create_test_directory_link(target: &Path, link: &Path) -> io::Result<()> {
    let link = link.to_string_lossy().replace('/', "\\");
    let target = target.to_string_lossy().replace('/', "\\");
    let mut command = Command::new("cmd.exe");
    command
        .args(["/d", "/c", "mklink", "/J", &link, &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_child(&mut command);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "test junction creation failed with {status}"
        )))
    }
}

pub(super) fn set_test_preserved_file_metadata(path: &Path, marked: bool) -> io::Result<()> {
    let flags = if marked { ["+H", "+R"] } else { ["-H", "-R"] };
    let mut command = Command::new("attrib.exe");
    command
        .args(flags)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_child(&mut command);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("attrib.exe failed with {status}")))
    }
}

pub(super) fn test_preserved_file_metadata(path: &Path) -> io::Result<u32> {
    const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
    const PRESERVED_ATTRIBUTES: u32 = FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN;

    Ok(fs::metadata(path)?.file_attributes() & PRESERVED_ATTRIBUTES)
}

pub(super) fn create_test_node_probe(directory: &Path) -> io::Result<PathBuf> {
    let path = directory.join("node.cmd");
    fs::write(
        &path,
        "@echo off\r\nif \"%~1\"==\"--version\" (\r\n  echo v22.13.0\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"--kaleido-explicit-node\" exit /b 0\r\nexit /b 23\r\n",
    )?;
    Ok(path)
}

pub(super) fn create_test_acp_probe_requiring_node(
    directory: &Path,
    version: &str,
) -> io::Result<PathBuf> {
    let path = directory.join("claude-agent-acp.cmd");
    fs::write(
        &path,
        format!(
            "@echo off\r\ncall node --kaleido-explicit-node >nul 2>&1\r\nif errorlevel 1 exit /b 29\r\necho {version}\r\n"
        ),
    )?;
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

    let Some(launcher) = launcher_for_path(path) else {
        return LayerReport {
            layer,
            status: ProbeStatus::InvalidConfiguration(
                "Windows executable must end in .cmd, .exe, or .bat".to_owned(),
            ),
            candidates: vec![Candidate {
                path: path.to_path_buf(),
                status: CandidateStatus::UnsupportedExtension,
            }],
            diagnostics: Vec::new(),
        };
    };
    let candidate = inspect_candidate(path);
    let status = if candidate.status == CandidateStatus::File {
        ProbeStatus::Found(resolved(path.to_path_buf(), launcher))
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
    include_where: bool,
) -> LayerReport {
    let path_ext = env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".CMD;.EXE;.BAT"));
    probe_in(
        layer,
        program,
        search_path,
        &path_ext,
        include_where.then(|| where_output(program)),
    )
}

fn probe_in(
    layer: DiscoveryLayer,
    program: &OsStr,
    search_path: &OsStr,
    path_ext: &OsStr,
    where_diagnostic: Option<String>,
) -> LayerReport {
    let extensions = executable_extensions(path_ext);
    let mut diagnostics = where_diagnostic
        .map(|output| vec![format!("where.exe output: {output}")])
        .unwrap_or_default();
    let directories: Vec<PathBuf> = env::split_paths(search_path)
        .filter(|directory| {
            if directory.as_os_str().is_empty() {
                diagnostics.push("ignored empty PATH entry".to_owned());
                false
            } else if !directory.is_absolute() {
                diagnostics.push("ignored non-absolute PATH entry".to_owned());
                false
            } else {
                true
            }
        })
        .collect();
    let mut candidates = Vec::new();
    let mut selected = None;

    for directory in &directories {
        for extension in &extensions {
            let mut file_name = program.to_os_string();
            file_name.push(extension);
            let path = directory.join(file_name);
            let candidate = inspect_candidate(&path);
            if selected.is_none() && candidate.status == CandidateStatus::File {
                selected = Some(resolved(path, launcher_for_supported_extension(extension)));
            }
            candidates.push(candidate);
        }
    }

    let status = selected.map_or(ProbeStatus::NotFound, ProbeStatus::Found);
    LayerReport {
        layer,
        status,
        candidates,
        diagnostics,
    }
}

pub(super) fn probe_persistent(program: &OsStr) -> LayerReport {
    let mut paths = Vec::new();
    let mut diagnostics = Vec::new();
    for (label, key) in [
        ("user", USER_ENVIRONMENT_KEY),
        ("system", MACHINE_ENVIRONMENT_KEY),
    ] {
        match query_registry_path(key) {
            Ok(path) => {
                diagnostics.push(format!(
                    "{label} persistent PATH={}",
                    path.to_string_lossy()
                ));
                paths.extend(env::split_paths(&path));
            }
            Err(reason) => diagnostics.push(format!("{label} persistent PATH query: {reason}")),
        }
    }

    let joined = match env::join_paths(paths) {
        Ok(path) => path,
        Err(error) => {
            return LayerReport {
                layer: DiscoveryLayer::PersistentPath,
                status: ProbeStatus::InvalidConfiguration(
                    "persistent PATH contains an unrepresentable entry".to_owned(),
                ),
                candidates: Vec::new(),
                diagnostics: {
                    diagnostics.push(format!("join failure: {error}"));
                    diagnostics
                },
            };
        }
    };
    let mut report = probe_in(
        DiscoveryLayer::PersistentPath,
        program,
        &joined,
        &env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".CMD;.EXE;.BAT")),
        None,
    );
    diagnostics.append(&mut report.diagnostics);
    report.diagnostics = diagnostics;
    report
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
    let mut directories = vec![
        base.data_local_dir().join("Microsoft").join("WindowsApps"),
        base.home_dir().join(".local").join("bin"),
    ];
    directories.extend(known_npm_prefixes(
        base.home_dir(),
        base.data_dir(),
        env::var_os("NPM_CONFIG_PREFIX"),
        &mut diagnostics,
    ));
    append_nvm_locations(&mut directories, &mut diagnostics);
    match target {
        DiscoveryTarget::Codex => {
            let root = base.data_local_dir().join("OpenAI").join("Codex");
            collect_subdirectories(&root, 3, &mut directories, &mut diagnostics);
        }
        DiscoveryTarget::ClaudeAcp | DiscoveryTarget::Node => {
            match query_registry_value(NODE_MACHINE_KEY, "InstallPath") {
                Ok(path) => {
                    diagnostics.push(format!("Node.js registry InstallPath={path}"));
                    directories.push(PathBuf::from(path));
                }
                Err(reason) => diagnostics.push(format!("Node.js registry query: {reason}")),
            }
            let root = base.data_local_dir().join("nvm");
            collect_subdirectories(&root, 2, &mut directories, &mut diagnostics);
        }
        DiscoveryTarget::ClaudeCli | DiscoveryTarget::OpenCode => {}
    }
    directories.sort();
    directories.dedup();
    let search_path = match env::join_paths(&directories) {
        Ok(path) => path,
        Err(error) => {
            diagnostics.push(format!("known-location PATH join failed: {error}"));
            return LayerReport {
                layer: DiscoveryLayer::KnownLocation,
                status: ProbeStatus::NotFound,
                candidates: Vec::new(),
                diagnostics,
            };
        }
    };
    let mut report = probe_in(
        DiscoveryLayer::KnownLocation,
        OsStr::new(target.program()),
        &search_path,
        &env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".CMD;.EXE;.BAT")),
        None,
    );
    append_gui_directory_evidence(target, &base, &mut report.candidates, &mut diagnostics);
    diagnostics.append(&mut report.diagnostics);
    report.diagnostics = diagnostics;
    report
}

fn known_npm_prefixes(
    home_directory: &Path,
    data_directory: &Path,
    configured_prefix: Option<OsString>,
    diagnostics: &mut Vec<String>,
) -> Vec<PathBuf> {
    let default_prefix = data_directory.join("npm");
    diagnostics.push(format!(
        "known npm data location={}",
        default_prefix.to_string_lossy()
    ));
    let mut prefixes = vec![default_prefix];
    match configured_prefix.map(PathBuf::from) {
        Some(prefix) if prefix.is_absolute() => {
            diagnostics.push(format!("NPM_CONFIG_PREFIX={}", prefix.to_string_lossy()));
            prefixes.push(prefix.clone());
            prefixes.push(prefix.join("bin"));
        }
        Some(_) => diagnostics.push("ignored non-absolute NPM_CONFIG_PREFIX".to_owned()),
        None => diagnostics.push("NPM_CONFIG_PREFIX is not set".to_owned()),
    }
    if let Some(prefix) = read_user_npm_prefix(home_directory, data_directory, diagnostics) {
        prefixes.push(prefix.clone());
        prefixes.push(prefix.join("bin"));
    }
    diagnostics.push(
        "dynamic `npm prefix --global` probe not executed; deterministic default, environment, \
         and user .npmrc prefixes were inspected without starting a child process"
            .to_owned(),
    );
    prefixes
}

fn append_nvm_locations(directories: &mut Vec<PathBuf>, diagnostics: &mut Vec<String>) {
    for value_name in [NVM_SYMLINK_VALUE, NVM_HOME_VALUE] {
        for (scope, key) in [
            ("user", USER_ENVIRONMENT_KEY),
            ("system", MACHINE_ENVIRONMENT_KEY),
        ] {
            match query_registry_value(key, value_name) {
                Ok(raw) => {
                    let expanded = expand_environment_variables(&raw, persistent_environment_value);
                    let path = PathBuf::from(&expanded);
                    if path.is_absolute() {
                        diagnostics.push(format!("{scope} {value_name}={expanded}"));
                        if value_name == NVM_HOME_VALUE {
                            collect_subdirectories(&path, 2, directories, diagnostics);
                        } else {
                            directories.push(path);
                        }
                    } else {
                        diagnostics.push(format!("{scope} {value_name} was not an absolute path"));
                    }
                }
                Err(reason) => {
                    diagnostics.push(format!("{scope} {value_name} registry query: {reason}"));
                }
            }
        }
    }
}

fn append_gui_directory_evidence(
    target: DiscoveryTarget,
    base: &BaseDirs,
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    let roots: Vec<PathBuf> = match target {
        DiscoveryTarget::Codex => {
            vec![base.data_local_dir().join("OpenAI").join("Codex")]
        }
        DiscoveryTarget::ClaudeCli => {
            matching_top_level_directories(base.data_local_dir(), &["claude", "anthropic"])
        }
        DiscoveryTarget::OpenCode => {
            matching_top_level_directories(base.data_local_dir(), &["opencode"])
        }
        DiscoveryTarget::ClaudeAcp | DiscoveryTarget::Node => Vec::new(),
    };

    for root in roots {
        match fs::metadata(&root) {
            Ok(metadata) if metadata.is_dir() => {
                diagnostics.push(format!(
                    "GUI/installation directory evidence={} (directory alone does not prove a runnable CLI)",
                    root.to_string_lossy()
                ));
                candidates.push(Candidate {
                    path: root,
                    status: CandidateStatus::DirectoryEvidence,
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => diagnostics.push(format!(
                "GUI/installation directory evidence inaccessible ({:?})",
                error.kind()
            )),
        }
    }
}

fn matching_top_level_directories(root: &Path, needles: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if needles.iter().any(|needle| name.contains(needle))
            && entry.file_type().is_ok_and(|file_type| file_type.is_dir())
        {
            matches.push(entry.path());
        }
    }
    matches.sort();
    matches.dedup();
    matches
}

pub(super) fn command(
    executable: &ResolvedExecutable,
    arguments: &[OsString],
) -> Result<Command, ProcessError> {
    let mut command = match executable.launcher() {
        Launcher::Native => {
            let mut native = Command::new(executable.path());
            native.args(arguments);
            native
        }
        Launcher::CmdScript | Launcher::BatchScript => {
            let command_line = command_script_line(executable.path().as_os_str(), arguments)?;
            let command_interpreter =
                env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe"));
            let mut script = Command::new(command_interpreter);
            script.args(["/D", "/E:ON", "/V:OFF", "/S", "/C"]);
            script.raw_arg(command_line);
            script
        }
    };
    augment_child_path(&mut command, executable.path());
    configure_child(&mut command);
    Ok(command)
}

fn augment_child_path(command: &mut Command, executable: &Path) {
    let mut directories = Vec::new();
    if let Some(parent) = executable.parent() {
        directories.push(parent.to_path_buf());
    }
    if let Ok(node_install) = query_registry_value(NODE_MACHINE_KEY, "InstallPath") {
        let node_install = PathBuf::from(node_install);
        if node_install.is_absolute() {
            directories.push(node_install);
        }
    }
    directories.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    let mut unique = Vec::new();
    for directory in directories {
        if !unique.iter().any(|known: &PathBuf| {
            known
                .to_string_lossy()
                .eq_ignore_ascii_case(&directory.to_string_lossy())
        }) {
            unique.push(directory);
        }
    }
    if let Ok(path) = env::join_paths(unique) {
        command.env("PATH", path);
    }
}

fn command_script_line(
    executable: &OsStr,
    arguments: &[OsString],
) -> Result<OsString, ProcessError> {
    if contains_cmd_metacharacter(executable)
        || arguments
            .iter()
            .any(|argument| contains_cmd_metacharacter(argument))
    {
        return Err(ProcessError::UnsafeScriptArgument);
    }

    let mut line = OsString::from("\"\"");
    line.push(executable);
    line.push("\"");
    for argument in arguments {
        line.push(" \"");
        line.push(argument);
        line.push("\"");
    }
    line.push("\"");
    Ok(line)
}

fn contains_cmd_metacharacter(value: &OsStr) -> bool {
    value.to_string_lossy().chars().any(|character| {
        matches!(
            character,
            '"' | '%' | '!' | '^' | '&' | '|' | '<' | '>' | '\r' | '\n'
        )
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

fn launcher_for_path(path: &Path) -> Option<Launcher> {
    let extension = path.extension()?;
    if extension.eq_ignore_ascii_case(OsStr::new("CMD")) {
        Some(Launcher::CmdScript)
    } else if extension.eq_ignore_ascii_case(OsStr::new("BAT")) {
        Some(Launcher::BatchScript)
    } else if extension.eq_ignore_ascii_case(OsStr::new("EXE")) {
        Some(Launcher::Native)
    } else {
        None
    }
}

pub(super) fn resolve_candidate(path: &Path) -> Option<ResolvedExecutable> {
    launcher_for_path(path).map(|launcher| resolved(path.to_path_buf(), launcher))
}

fn launcher_for_supported_extension(extension: &OsStr) -> Launcher {
    if extension.eq_ignore_ascii_case(OsStr::new(".CMD")) {
        Launcher::CmdScript
    } else if extension.eq_ignore_ascii_case(OsStr::new(".BAT")) {
        Launcher::BatchScript
    } else {
        Launcher::Native
    }
}

fn inspect_candidate(path: &Path) -> Candidate {
    let status = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => CandidateStatus::File,
        Ok(_) => CandidateStatus::NotFile,
        Err(error) if error.kind() == io::ErrorKind::NotFound => CandidateStatus::Missing,
        Err(error) => CandidateStatus::Inaccessible(error.kind()),
    };
    Candidate {
        path: path.to_path_buf(),
        status,
    }
}

fn collect_subdirectories(
    root: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<String>,
) {
    output.push(root.to_path_buf());
    if depth == 0 {
        return;
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            diagnostics.push(format!(
                "could not inspect known location {}: {:?}",
                root.display(),
                error.kind()
            ));
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry) => match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {
                    collect_subdirectories(&entry.path(), depth - 1, output, diagnostics);
                }
                Ok(_) => {}
                Err(error) => diagnostics.push(format!(
                    "could not inspect known-location entry type: {:?}",
                    error.kind()
                )),
            },
            Err(error) => diagnostics.push(format!(
                "could not enumerate known-location entry: {:?}",
                error.kind()
            )),
        }
    }
}

fn query_registry_path(key: &str) -> Result<OsString, String> {
    let value = query_registry_value(key, "Path")?;
    Ok(expand_environment_variables(&value, persistent_environment_value).into())
}

fn persistent_environment_value(name: &str) -> Option<String> {
    prefer_persistent_environment_value(
        query_registry_value(USER_ENVIRONMENT_KEY, name).ok(),
        query_registry_value(MACHINE_ENVIRONMENT_KEY, name).ok(),
        env::var_os(name).map(|value| value.to_string_lossy().into_owned()),
    )
}

fn prefer_persistent_environment_value(
    user: Option<String>,
    system: Option<String>,
    process: Option<String>,
) -> Option<String> {
    user.or(system).or(process)
}

fn query_registry_value(key: &str, value_name: &str) -> Result<String, String> {
    let command_line = utf8_registry_query_command_line(key, value_name)
        .map_err(|error| format!("reg.exe command construction failed: {error}"))?;
    let output = utf8_command_interpreter(command_line)
        .output()
        .map_err(|error| format!("reg.exe failed to start ({:?})", error.kind()))?;
    let stdout = decode_diagnostic_bytes(&output.stdout);
    if !output.status.success() {
        let stderr = decode_diagnostic_bytes(&output.stderr);
        return Err(format!(
            "reg.exe exited {}: {}{}",
            output.status,
            stdout.trim(),
            stderr.trim()
        ));
    }
    parse_registry_value(&stdout).ok_or_else(|| format!("registry value `{value_name}` was absent"))
}

fn parse_registry_value(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        ["REG_EXPAND_SZ", "REG_SZ"].iter().find_map(|kind| {
            line.find(kind)
                .map(|offset| line[offset + kind.len()..].trim().to_owned())
                .filter(|value| !value.is_empty())
        })
    })
}

fn expand_environment_variables(
    input: &str,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> String {
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.find('%') {
        output.push_str(&remainder[..start]);
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('%') else {
            output.push_str(&remainder[start..]);
            return output;
        };
        let name = &after_start[..end];
        if let Some(value) = lookup(name) {
            output.push_str(&value);
        } else {
            output.push('%');
            output.push_str(name);
            output.push('%');
        }
        remainder = &after_start[end + 1..];
    }
    output.push_str(remainder);
    output
}

fn where_output(program: &OsStr) -> String {
    let command_line = match utf8_where_command_line(program) {
        Ok(command_line) => command_line,
        Err(ProcessError::UnsafeScriptArgument) => {
            return "where.exe was not run for an unsafe program name".to_owned();
        }
        Err(_) => return "where.exe command construction failed".to_owned(),
    };
    match utf8_command_interpreter(command_line).output() {
        Ok(output) => {
            let mut combined = decode_diagnostic_bytes(&output.stdout);
            let stderr = decode_diagnostic_bytes(&output.stderr);
            if !combined.is_empty()
                && !stderr.is_empty()
                && !combined.ends_with('\n')
                && !combined.ends_with('\r')
            {
                combined.push('\n');
            }
            combined.push_str(&stderr);
            format!("exit={}; {combined}", output.status)
        }
        Err(error) => format!("where.exe failed to start ({:?})", error.kind()),
    }
}

fn utf8_command_interpreter(command_line: OsString) -> Command {
    let command_interpreter = env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe"));
    let mut command = Command::new(command_interpreter);
    command.args(["/D", "/E:ON", "/V:OFF", "/S", "/C"]);
    command.raw_arg(command_line);
    configure_child(&mut command);
    command
}

fn utf8_where_command_line(program: &OsStr) -> Result<OsString, ProcessError> {
    if contains_cmd_metacharacter(program) {
        return Err(ProcessError::UnsafeScriptArgument);
    }

    let mut line = OsString::from("\"chcp 65001 >NUL & where.exe \"");
    line.push(program);
    line.push("\"\"");
    Ok(line)
}

fn utf8_registry_query_command_line(key: &str, value_name: &str) -> Result<OsString, ProcessError> {
    if contains_cmd_metacharacter(OsStr::new(key))
        || contains_cmd_metacharacter(OsStr::new(value_name))
    {
        return Err(ProcessError::UnsafeScriptArgument);
    }

    let mut line = OsString::from("\"chcp 65001 >NUL & reg.exe query \"");
    line.push(key);
    line.push("\" /v \"");
    line.push(value_name);
    line.push("\"\"");
    Ok(line)
}

fn decode_diagnostic_bytes(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(text) => text,
        Err(error) => {
            let bytes = error.into_bytes();
            let mut encoded = String::from("non-UTF-8 bytes (hex):");
            for byte in bytes {
                if let Some(high) = char::from_digit(u32::from(byte >> 4), 16) {
                    encoded.push(high);
                }
                if let Some(low) = char::from_digit(u32::from(byte & 0x0f), 16) {
                    encoded.push(low);
                }
            }
            encoded
        }
    }
}

pub(super) fn terminate_tree(child: &mut Child) -> Result<ExitStatus, ProcessError> {
    let root_pid = child.id();
    let snapshot = process_snapshot_ffi::snapshot_processes().map_err(|error| {
        ProcessError::Terminate(io::Error::new(
            error.kind(),
            format!("could not capture spawned root PID {root_pid} family: {error}"),
        ))
    })?;
    let family = SpawnedProcessFamily::capture(root_pid, &snapshot);
    let descendants = process_handle_ffi::CapturedDescendants::capture(family.descendant_pids())
        .map_err(|error| {
            ProcessError::Terminate(io::Error::new(
                error.kind(),
                format!(
                    "could not retain exact handles for spawned root PID {root_pid} descendants: \
                     {error}"
                ),
            ))
        });
    let root_termination = terminate_tree_with(child, |process_id| {
        taskkill_command(process_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    });
    let descendant_termination = descendants.and_then(|descendants| {
        descendants
            .terminate_and_wait(DESCENDANT_FORCE_EXIT_TIMEOUT)
            .map_err(|error| {
                ProcessError::Terminate(io::Error::new(
                    error.kind(),
                    format!(
                        "could not terminate exact descendants of spawned root PID {root_pid}: \
                         {error}"
                    ),
                ))
            })
    });
    let status = combine_family_termination(root_termination, descendant_termination)?;
    wait_for_spawned_process_family_exit(
        &family,
        PROCESS_FAMILY_EXIT_TIMEOUT,
        process_snapshot_ffi::snapshot_processes,
    )?;
    Ok(status)
}

fn combine_family_termination(
    root: Result<ExitStatus, ProcessError>,
    descendants: Result<(), ProcessError>,
) -> Result<ExitStatus, ProcessError> {
    match (root, descendants) {
        (Ok(status), Ok(())) => Ok(status),
        (Err(root), Ok(())) => Err(root),
        (Ok(_), Err(descendants)) => Err(descendants),
        (Err(root), Err(descendants)) => Err(ProcessError::Terminate(io::Error::other(format!(
            "spawned root cleanup failed ({root}); exact descendant cleanup also failed \
                 ({descendants})"
        )))),
    }
}

#[cfg(test)]
fn terminate_tree_with_tracking(
    child: &mut Child,
    run_taskkill: impl FnOnce(u32) -> io::Result<ExitStatus>,
    mut snapshot_processes: impl FnMut() -> io::Result<Vec<ProcessEntry>>,
) -> Result<ExitStatus, ProcessError> {
    let root_pid = child.id();
    let family =
        snapshot_processes().map(|snapshot| SpawnedProcessFamily::capture(root_pid, &snapshot));
    let termination = terminate_tree_with(child, run_taskkill);
    let status = termination?;
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

trait TerminationTarget {
    fn process_id(&self) -> u32;
    fn try_wait_now(&mut self) -> io::Result<Option<ExitStatus>>;
    fn poll_exit(&mut self, timeout: Duration) -> Result<Option<ExitStatus>, ProcessError>;
    fn kill_direct(&mut self) -> io::Result<()>;
}

impl TerminationTarget for Child {
    fn process_id(&self) -> u32 {
        self.id()
    }

    fn try_wait_now(&mut self) -> io::Result<Option<ExitStatus>> {
        self.try_wait()
    }

    fn poll_exit(&mut self, timeout: Duration) -> Result<Option<ExitStatus>, ProcessError> {
        wait_for_exit(self, timeout)
    }

    fn kill_direct(&mut self) -> io::Result<()> {
        self.kill()
    }
}

fn terminate_tree_with(
    child: &mut impl TerminationTarget,
    run_taskkill: impl FnOnce(u32) -> io::Result<ExitStatus>,
) -> Result<ExitStatus, ProcessError> {
    if let Some(status) = child.try_wait_now().map_err(ProcessError::Inspect)? {
        return Ok(status);
    }

    let taskkill_result = run_taskkill(child.process_id());
    finish_termination(child, taskkill_result)
}

fn finish_termination(
    child: &mut impl TerminationTarget,
    taskkill_result: io::Result<ExitStatus>,
) -> Result<ExitStatus, ProcessError> {
    let mut taskkill_wait_error = None;
    if taskkill_result.as_ref().is_ok_and(ExitStatus::success) {
        match child.poll_exit(Duration::from_secs(3)) {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => taskkill_wait_error = Some(error),
        }
    }

    let direct_kill_result = child.kill_direct();
    match child.poll_exit(Duration::from_secs(3)) {
        Ok(Some(status)) => Ok(status),
        Err(error) => Err(error),
        Ok(None) => {
            if let Err(error) = direct_kill_result {
                return Err(ProcessError::Terminate(error));
            }
            if let Some(error) = taskkill_wait_error {
                return Err(error);
            }
            Err(ProcessError::Terminate(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "child did not exit after {} and direct kill",
                    taskkill_outcome(&taskkill_result)
                ),
            )))
        }
    }
}

fn taskkill_outcome(result: &io::Result<ExitStatus>) -> String {
    match result {
        Ok(status) => format!("taskkill exited {status}"),
        Err(error) => format!("taskkill failed to start ({:?})", error.kind()),
    }
}

fn taskkill_command(process_id: u32) -> Command {
    let mut taskkill = Command::new("taskkill.exe");
    configure_child(&mut taskkill);
    taskkill.args(["/PID", &process_id.to_string(), "/T", "/F"]);
    taskkill
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>, ProcessError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(ProcessError::Inspect)? {
            return Ok(Some(status));
        }
        if started.elapsed() >= timeout {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::os::windows::process::ExitStatusExt;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{
        command_script_line, decode_diagnostic_bytes, executable_extensions,
        expand_environment_variables, parse_registry_value, prefer_persistent_environment_value,
        probe_in, query_registry_value, retry_directory_rename_after_sharing_violation,
        taskkill_command, terminate_tree_with, terminate_tree_with_tracking,
        utf8_registry_query_command_line, utf8_where_command_line, where_output, TerminationTarget,
        ERROR_SHARING_VIOLATION, USER_ENVIRONMENT_KEY,
    };
    use crate::platform::{CandidateStatus, DiscoveryLayer, Launcher, ProbeStatus, ProcessError};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn sandbox_quarantine_retries_transient_windows_sharing_violations() -> TestResult {
        let attempts = Cell::new(0);
        let waits = Cell::new(0);

        retry_directory_rename_after_sharing_violation(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    Err(io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION))
                } else {
                    Ok(())
                }
            },
            || true,
            || waits.set(waits.get() + 1),
        )?;

        assert_eq!(attempts.get(), 3);
        assert_eq!(waits.get(), 2);
        Ok(())
    }

    #[test]
    fn sandbox_quarantine_does_not_retry_other_windows_errors() -> TestResult {
        let attempts = Cell::new(0);
        let retry_checks = Cell::new(0);
        let waits = Cell::new(0);

        let result = retry_directory_rename_after_sharing_violation(
            || {
                attempts.set(attempts.get() + 1);
                Err(io::Error::from_raw_os_error(5))
            },
            || {
                retry_checks.set(retry_checks.get() + 1);
                true
            },
            || waits.set(waits.get() + 1),
        );
        let error = match result {
            Err(error) => error,
            Ok(()) => return Err("access denied unexpectedly succeeded".into()),
        };

        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(attempts.get(), 1);
        assert_eq!(retry_checks.get(), 0);
        assert_eq!(waits.get(), 0);
        Ok(())
    }

    #[test]
    fn ignores_extensionless_shim_and_selects_cmd() -> TestResult {
        let directory = tempdir()?;
        fs::write(directory.path().join("codex"), "#!/bin/sh\n")?;
        fs::write(directory.path().join("codex.cmd"), "@echo off\r\n")?;
        let search_path = std::env::join_paths([directory.path()])?;

        let report = probe_in(
            DiscoveryLayer::InheritedPath,
            OsStr::new("codex"),
            &search_path,
            OsStr::new(".CMD;.EXE;.BAT"),
            None,
        );

        let ProbeStatus::Found(executable) = report.status else {
            return Err(io::Error::other("cmd shim must resolve").into());
        };
        assert!(executable
            .path()
            .to_string_lossy()
            .eq_ignore_ascii_case(&directory.path().join("codex.cmd").to_string_lossy()));
        assert_eq!(executable.launcher(), Launcher::CmdScript);
        assert!(report
            .candidates
            .iter()
            .all(|candidate| candidate.path.file_name() != Some(OsStr::new("codex"))));
        Ok(())
    }

    #[test]
    fn honors_pathext_order_for_supported_launchers() -> TestResult {
        let directory = tempdir()?;
        fs::write(directory.path().join("agent.cmd"), "@echo off\r\n")?;
        fs::write(directory.path().join("agent.exe"), b"MZ")?;
        let search_path = std::env::join_paths([directory.path()])?;

        let report = probe_in(
            DiscoveryLayer::InheritedPath,
            OsStr::new("agent"),
            &search_path,
            OsStr::new(".EXE;.CMD;.BAT"),
            None,
        );

        let ProbeStatus::Found(executable) = report.status else {
            return Err(io::Error::other("exe must resolve first").into());
        };
        assert!(executable
            .path()
            .to_string_lossy()
            .eq_ignore_ascii_case(&directory.path().join("agent.exe").to_string_lossy()));
        assert_eq!(executable.launcher(), Launcher::Native);
        Ok(())
    }

    #[test]
    fn pathext_falls_back_to_cmd_exe_and_bat_without_accepting_other_shims() {
        assert_eq!(
            executable_extensions(OsStr::new(".PS1;.COM;.CMD;.cmd")),
            [".CMD", ".EXE", ".BAT"]
        );
    }

    #[test]
    fn missing_program_reports_every_supported_candidate() -> TestResult {
        let directory = tempdir()?;
        fs::write(directory.path().join("agent"), "#!/bin/sh\n")?;
        let search_path = std::env::join_paths([directory.path()])?;

        let report = probe_in(
            DiscoveryLayer::InheritedPath,
            OsStr::new("agent"),
            &search_path,
            OsStr::new(".CMD;.EXE;.BAT"),
            Some("INFO: no files".to_owned()),
        );

        assert_eq!(report.status, ProbeStatus::NotFound);
        let names: Vec<OsString> = report
            .candidates
            .iter()
            .map(|candidate| candidate.path.file_name().map(OsStr::to_os_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| io::Error::other("every candidate must have a file name"))?;
        assert_eq!(names, ["agent.CMD", "agent.EXE", "agent.BAT"]);
        assert!(report
            .candidates
            .iter()
            .all(|candidate| candidate.status == CandidateStatus::Missing));
        assert_eq!(report.diagnostics, ["where.exe output: INFO: no files"]);
        Ok(())
    }

    #[test]
    fn ignores_empty_and_relative_path_entries() -> TestResult {
        let directory = tempdir()?;
        fs::write(directory.path().join("agent.cmd"), "@echo off\r\n")?;
        let search_path =
            OsString::from(format!(";relative;{};", directory.path().to_string_lossy()));

        let report = probe_in(
            DiscoveryLayer::InheritedPath,
            OsStr::new("agent"),
            &search_path,
            OsStr::new(".CMD;.EXE;.BAT"),
            None,
        );

        assert!(report
            .candidates
            .iter()
            .all(|candidate| candidate.path.is_absolute()));
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "ignored empty PATH entry"));
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "ignored non-absolute PATH entry"));
        Ok(())
    }

    #[test]
    fn explicit_extensionless_shim_is_rejected() -> TestResult {
        let directory = tempdir()?;
        let shim = directory.path().join("codex");
        fs::write(&shim, "#!/bin/sh\n")?;

        let report = super::probe_explicit(DiscoveryLayer::Explicit, &shim);

        assert!(matches!(
            report.status,
            ProbeStatus::InvalidConfiguration(_)
        ));
        assert_eq!(
            report.candidates,
            [crate::platform::Candidate {
                path: shim,
                status: CandidateStatus::UnsupportedExtension,
            }]
        );
        Ok(())
    }

    #[test]
    fn cmd_script_is_launched_through_command_interpreter() -> TestResult {
        let directory = tempdir()?;
        let script = directory.path().join("probe.cmd");
        fs::write(&script, "@echo off\r\necho [%~1]\r\n")?;
        let report = super::probe_explicit(DiscoveryLayer::Explicit, &script);
        let ProbeStatus::Found(executable) = report.status else {
            return Err(io::Error::other("cmd script must resolve").into());
        };

        let output = super::command(&executable, &[OsString::from("hello world")])
            .map_err(|error| io::Error::other(error.to_string()))?
            .output()?;

        assert!(
            output.status.success(),
            "cmd script exited with {}",
            output.status
        );
        assert_eq!(String::from_utf8(output.stdout)?.trim(), "[hello world]");
        Ok(())
    }

    #[test]
    fn cmd_script_can_resolve_a_sibling_from_augmented_path() -> TestResult {
        let directory = tempdir()?;
        let script = directory.path().join("parent.cmd");
        let sibling = directory.path().join("sibling.cmd");
        fs::write(&script, "@echo off\r\nsibling.cmd\r\n")?;
        fs::write(&sibling, "@echo off\r\necho sibling-ran\r\n")?;
        let report = super::probe_explicit(DiscoveryLayer::Explicit, &script);
        let ProbeStatus::Found(executable) = report.status else {
            return Err(io::Error::other("cmd script must resolve").into());
        };

        let output = super::command(&executable, &[])
            .map_err(|error| io::Error::other(error.to_string()))?
            .output()?;

        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout)?.trim(), "sibling-ran");
        Ok(())
    }

    #[test]
    fn known_npm_locations_are_enumerated_without_starting_a_cmd_shim() -> TestResult {
        let directory = tempdir()?;
        let data_directory = directory.path().join("data");
        let npm_directory = data_directory.join("npm");
        let configured_directory = directory.path().join("custom-npm");
        let marker = directory.path().join("npm-ran");
        fs::create_dir_all(&npm_directory)?;
        fs::write(
            directory.path().join(".npmrc"),
            format!("prefix={}\n", configured_directory.display()),
        )?;
        fs::write(
            npm_directory.join("npm.cmd"),
            format!(
                "@echo off\r\necho should-not-run>\"{}\"\r\n",
                marker.display()
            ),
        )?;
        let mut diagnostics = Vec::new();

        let prefixes =
            super::known_npm_prefixes(directory.path(), &data_directory, None, &mut diagnostics);

        assert!(
            prefixes.iter().any(|prefix| prefix == &npm_directory),
            "the deterministic npm data location was omitted"
        );
        assert!(prefixes
            .iter()
            .any(|prefix| prefix == &configured_directory));
        assert!(prefixes
            .iter()
            .any(|prefix| prefix == &configured_directory.join("bin")));
        assert!(!marker.exists());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("not executed")));
        Ok(())
    }

    #[test]
    fn relative_npm_config_prefix_is_rejected_without_execution() -> TestResult {
        let directory = tempdir()?;
        let mut diagnostics = Vec::new();

        let prefixes = super::known_npm_prefixes(
            directory.path(),
            directory.path(),
            Some(OsString::from("relative-prefix")),
            &mut diagnostics,
        );

        assert_eq!(prefixes.len(), 1);
        assert!(prefixes
            .iter()
            .all(|prefix| prefix == &directory.path().join("npm")));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "ignored non-absolute NPM_CONFIG_PREFIX"));
        Ok(())
    }

    #[test]
    fn cmd_script_rejects_shell_metacharacters() {
        let result = command_script_line(
            OsStr::new(r"C:\safe\agent.cmd"),
            &[OsString::from("safe & whoami")],
        );
        assert!(matches!(result, Err(ProcessError::UnsafeScriptArgument)));
    }

    #[test]
    fn where_diagnostic_rejects_command_metacharacters() {
        assert!(matches!(
            utf8_where_command_line(OsStr::new("safe & whoami")),
            Err(ProcessError::UnsafeScriptArgument)
        ));
        assert!(matches!(
            utf8_registry_query_command_line(USER_ENVIRONMENT_KEY, "safe & whoami"),
            Err(ProcessError::UnsafeScriptArgument)
        ));
    }

    #[test]
    fn invalid_utf8_diagnostics_are_preserved_as_exact_hex() {
        let gbk_error_prefix = [0xd0, 0xc5, 0xcf, 0xa2];

        assert_eq!(
            decode_diagnostic_bytes(&gbk_error_prefix),
            "non-UTF-8 bytes (hex):d0c5cfa2"
        );
    }

    #[test]
    fn where_output_is_utf8_and_never_inserts_replacement_characters() {
        let output = where_output(OsStr::new("definitely-not-a-real-kaleido-recorder-command"));

        assert!(output.starts_with("exit="));
        assert!(!output.contains('\u{fffd}'));
        assert!(!output.ends_with("; "));
    }

    #[test]
    fn registry_errors_are_utf8_and_never_insert_replacement_characters() -> TestResult {
        let error = query_registry_value(
            USER_ENVIRONMENT_KEY,
            "definitely-not-a-real-kaleido-recorder-value",
        )
        .err()
        .ok_or_else(|| io::Error::other("the deliberately absent registry value must fail"))?;

        assert!(!error.contains('\u{fffd}'));
        Ok(())
    }

    #[test]
    fn taskkill_command_forces_the_entire_process_tree() {
        let command = taskkill_command(42);
        let arguments: Vec<OsString> = command.get_args().map(OsStr::to_os_string).collect();

        assert_eq!(command.get_program(), OsStr::new("taskkill.exe"));
        assert_eq!(arguments, ["/PID", "42", "/T", "/F"]);
    }

    #[test]
    fn toolhelp_snapshot_contains_the_current_process_by_pid() -> TestResult {
        let processes = super::process_snapshot_ffi::snapshot_processes()?;

        assert!(processes
            .iter()
            .any(|process| process.pid == std::process::id()));
        Ok(())
    }

    #[test]
    fn captured_process_handle_terminates_only_the_exact_pid() -> TestResult {
        fn sleeping_process() -> io::Result<Child> {
            let mut command = Command::new("powershell.exe");
            command
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[Threading.Thread]::Sleep(30000)",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            super::configure_child(&mut command);
            command.spawn()
        }

        let mut target = sleeping_process()?;
        let mut unrelated = match sleeping_process() {
            Ok(child) => child,
            Err(error) => {
                let _ = target.kill();
                let _ = target.wait();
                return Err(error.into());
            }
        };
        let result = (|| -> TestResult {
            let handles =
                super::process_handle_ffi::CapturedDescendants::capture([target.id()].into_iter())?;
            handles.terminate_and_wait(Duration::from_secs(5))?;

            assert!(target.wait()?.code().is_some());
            assert!(
                unrelated.try_wait()?.is_none(),
                "a process whose PID was not captured must remain untouched"
            );
            Ok(())
        })();
        let _ = target.kill();
        let _ = target.wait();
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        result
    }

    struct RecordingTerminationTarget {
        calls: Vec<&'static str>,
        exit_status: Option<ExitStatus>,
    }

    impl TerminationTarget for RecordingTerminationTarget {
        fn process_id(&self) -> u32 {
            42
        }

        fn try_wait_now(&mut self) -> io::Result<Option<ExitStatus>> {
            self.calls.push("try-wait");
            Ok(None)
        }

        fn poll_exit(&mut self, _timeout: Duration) -> Result<Option<ExitStatus>, ProcessError> {
            self.calls.push("wait");
            Ok(self.exit_status.take())
        }

        fn kill_direct(&mut self) -> io::Result<()> {
            self.calls.push("kill");
            Ok(())
        }
    }

    #[test]
    fn taskkill_spawn_failure_reaps_root_for_outer_family_verification() -> TestResult {
        let taskkill_process_id = Cell::new(0);
        let mut child = RecordingTerminationTarget {
            calls: Vec::new(),
            exit_status: Some(ExitStatus::from_raw(0)),
        };

        let result = terminate_tree_with(&mut child, |process_id| {
            taskkill_process_id.set(process_id);
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "deliberately missing taskkill",
            ))
        });

        let status = result?;
        assert_eq!(status.code(), Some(0));
        assert_eq!(taskkill_process_id.get(), 42);
        assert_eq!(child.calls, ["try-wait", "kill", "wait"]);
        Ok(())
    }

    #[test]
    fn taskkill_nonzero_exit_reaps_root_for_outer_family_verification() -> TestResult {
        let mut child = RecordingTerminationTarget {
            calls: Vec::new(),
            exit_status: Some(ExitStatus::from_raw(0)),
        };

        let result = terminate_tree_with(&mut child, |_| Ok(ExitStatus::from_raw(5)));

        let status = result?;
        assert_eq!(status.code(), Some(0));
        assert_eq!(child.calls, ["try-wait", "kill", "wait"]);
        Ok(())
    }

    #[test]
    fn real_child_with_nonzero_taskkill_reaches_pid_family_verification() -> TestResult {
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Threading.Thread]::Sleep(30000)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        super::configure_child(&mut command);
        let mut child = command.spawn()?;
        let snapshot_calls = Cell::new(0_usize);

        let result = terminate_tree_with_tracking(
            &mut child,
            |_| Ok(ExitStatus::from_raw(1)),
            || {
                snapshot_calls.set(snapshot_calls.get() + 1);
                super::process_snapshot_ffi::snapshot_processes()
            },
        );

        let status = match result {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        assert!(status.code().is_some());
        assert!(
            snapshot_calls.get() >= 2,
            "outer PID-family verification was skipped"
        );
        assert!(child.try_wait()?.is_some());
        Ok(())
    }

    #[test]
    fn persistent_values_win_over_inherited_process_values() {
        assert_eq!(
            prefer_persistent_environment_value(
                Some("user".to_owned()),
                Some("system".to_owned()),
                Some("process".to_owned())
            ),
            Some("user".to_owned())
        );
        assert_eq!(
            prefer_persistent_environment_value(
                None,
                Some("system".to_owned()),
                Some("process".to_owned())
            ),
            Some("system".to_owned())
        );
        assert_eq!(
            prefer_persistent_environment_value(None, None, Some("process".to_owned())),
            Some("process".to_owned())
        );
    }

    #[test]
    fn parses_and_expands_persistent_path() -> TestResult {
        let raw = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    %ROOT%\\bin;D:\\tools\r\n";
        let value = parse_registry_value(raw)
            .ok_or_else(|| io::Error::other("registry PATH must parse"))?;
        let expanded = expand_environment_variables(&value, |name| {
            (name == "ROOT").then(|| r"C:\runtime".to_owned())
        });

        assert_eq!(expanded, r"C:\runtime\bin;D:\tools");
        Ok(())
    }
}
