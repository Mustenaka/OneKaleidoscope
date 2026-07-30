use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use reqwest::blocking::Client;
use reqwest::header::ACCEPT;
use semver::Version;
use serde_json::Value;
use sha1_smol::Sha1;
use tempfile::{Builder as TempDirBuilder, TempDir};
use thiserror::Error;

use crate::schema_history::{self, SurfaceHistoryRecord};
use crate::schema_surface::{
    self, RawChangeDisposition, RequiredSurface, SurfaceChange, SurfaceChangeKind,
    SurfaceComparison, SurfaceTool, VersionSupport,
};

pub const ACP_CRATE_VERSION: &str = "1.3.0";
pub const ACP_SCHEMA_VERSION: &str = "1.18.0";
pub const ACP_COMMIT: &str = "48b2abf1ac750fece26e03e92e773ccbd4754f5d";

const CODEX_EXECUTABLE_ENVIRONMENT: &str = "KALEIDO_CODEX_EXECUTABLE";
const OPENCODE_EXECUTABLE_ENVIRONMENT: &str = "KALEIDO_OPENCODE_EXECUTABLE";
const ACP_SNAPSHOT_ENVIRONMENT: &str = "KALEIDO_ACP_SNAPSHOT_DIRECTORY";
const REQUIRED_SURFACE_FILE: &str = "required-surface.toml";
const SURFACE_HISTORY_FILE: &str = "surface-history.jsonl";
const ACP_REPOSITORY: &str = "https://github.com/agentclientprotocol/agent-client-protocol.git";
const ACP_SCHEMA_URL: &str = "https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/schema.json";
const ACP_META_URL: &str = "https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/meta.json";
const ACP_SCHEMA_BLOB: &str = "0a830142717b69fbd1da2e67b5540636fc6e51dc";
const ACP_META_BLOB: &str = "670d27876133a37cc1cc476c1ea685351422e07f";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaCommand {
    Refresh,
    Diff,
    History { tool: String, entry_id: String },
}

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("configured tool `{tool}` in `{variable}` must be an absolute path")]
    ToolConfiguration {
        tool: &'static str,
        variable: &'static str,
    },
    #[error("required tool `{tool}` is unavailable ({detail}); install exactly with: {install}")]
    ToolUnavailable {
        tool: &'static str,
        detail: String,
        install: String,
    },
    #[error("`{tool}` failed with status {status}")]
    ProcessFailed {
        tool: &'static str,
        status: ExitStatus,
    },
    #[error("schema I/O failure: {0}")]
    Io(#[from] io::Error),
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: String,
        source: serde_json::Error,
    },
    #[error("request to {url} failed: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("request to {url} returned HTTP {status}")]
    HttpStatus {
        url: String,
        status: reqwest::StatusCode,
    },
    #[error("invalid schema snapshot: {0}")]
    InvalidSnapshot(String),
    #[error(
        "configured ACP snapshot file `{file}` has Git blob {actual}, expected {expected} from commit {ACP_COMMIT}"
    )]
    SnapshotDigest {
        file: &'static str,
        expected: &'static str,
        actual: String,
    },
    #[error("OpenCode schema server failure: {0}")]
    Server(String),
    #[error("schema drift detected at {} path(s)", .0.len())]
    Drift(Vec<SchemaChange>),
    #[error("required-surface drift detected at {} path(s)", .0.len())]
    RequiredSurfaceDrift(Vec<SurfaceChange>),
    #[error(transparent)]
    RequiredSurface(#[from] schema_surface::RequiredSurfaceError),
    #[error(transparent)]
    SurfaceHistory(#[from] schema_history::SurfaceHistoryError),
}

impl SchemaError {
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::ToolConfiguration { .. }
            | Self::ToolUnavailable { .. }
            | Self::ProcessFailed { .. }
            | Self::Server(_) => 2,
            Self::Drift(_) | Self::RequiredSurfaceDrift(_) => 1,
            _ => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ChangeKind {
    Added,
    Changed,
    Removed,
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Added => formatter.write_str("added"),
            Self::Changed => formatter.write_str("changed"),
            Self::Removed => formatter.write_str("removed"),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SchemaChange {
    file: String,
    pointer: String,
    kind: ChangeKind,
}

impl fmt::Display for SchemaChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{} ({})", self.file, self.pointer, self.kind)
    }
}

#[derive(Debug)]
struct SnapshotVersions {
    codex: String,
    opencode: String,
    acp: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolObservation {
    output: String,
    version: String,
}

#[derive(Debug)]
struct ToolVersions {
    codex: ToolObservation,
    opencode: ToolObservation,
}

#[derive(Debug)]
struct SnapshotStats {
    codex_files: usize,
    opencode_files: usize,
    acp_files: usize,
}

pub fn run(command: SchemaCommand, workspace_root: &Path) -> Result<(), SchemaError> {
    if let SchemaCommand::History { tool, entry_id } = &command {
        return run_history(workspace_root, tool, entry_id);
    }

    let snapshot_root = workspace_root.join("schemas");
    let snapshot_versions = load_snapshot_versions(&snapshot_root.join("VERSIONS.md"))?;
    let surface = schema_surface::load_required_surface(workspace_root)?;
    schema_surface::validate_baseline(&surface, &snapshot_root)?;
    let versions = verify_tool_versions(&snapshot_versions)?;
    print_version_report(&surface, &snapshot_versions, &versions);

    let target = workspace_root.join("target");
    fs::create_dir_all(&target)?;
    let staging = TempDirBuilder::new()
        .prefix("schema-snapshot-")
        .tempdir_in(&target)?;
    let staged_schemas = staging.path().join("schemas");

    println!(
        "schema: fetching Codex {}, OpenCode {}, ACP crate {} / schema {}",
        versions.codex.version, versions.opencode.version, ACP_CRATE_VERSION, snapshot_versions.acp
    );
    let stats = fetch_all(workspace_root, &staged_schemas, &versions)?;
    let comparison =
        schema_surface::compare_required_surface(&surface, &snapshot_root, &staged_schemas)?;

    match command {
        SchemaCommand::Refresh => {
            let raw_changes = semantic_changes(&snapshot_root, &staged_schemas)?;
            print_partitioned_report(&comparison, &raw_changes);
            schema_surface::validate_baseline(&surface, &staged_schemas)?;
            preserve_control_files(&snapshot_root, &staged_schemas)?;
            record_observed_history(
                &staged_schemas.join(SURFACE_HISTORY_FILE),
                &versions,
                &snapshot_versions.acp,
                comparison.observed_digests(),
            )?;
            install_snapshot(&staging, &staged_schemas, &snapshot_root)?;
            println!(
                "schema refresh: wrote {} Codex, {} OpenCode, and {} ACP JSON file(s)",
                stats.codex_files, stats.opencode_files, stats.acp_files
            );
            Ok(())
        }
        SchemaCommand::Diff => {
            let raw_changes = semantic_changes(&snapshot_root, &staged_schemas)?;
            print_partitioned_report(&comparison, &raw_changes);
            record_observed_history(
                &snapshot_root.join(SURFACE_HISTORY_FILE),
                &versions,
                &snapshot_versions.acp,
                comparison.observed_digests(),
            )?;
            let result = required_surface_result(&comparison);
            if result.is_ok() {
                println!(
                    "schema diff: required surface is compatible ({} JSON files compared)",
                    stats.codex_files + stats.opencode_files + stats.acp_files
                );
            }
            result
        }
        SchemaCommand::History { .. } => Err(SchemaError::InvalidSnapshot(
            "history command reached schema acquisition unexpectedly".to_owned(),
        )),
    }
}

fn required_surface_result(comparison: &SurfaceComparison) -> Result<(), SchemaError> {
    if comparison.changes().is_empty() {
        Ok(())
    } else {
        Err(SchemaError::RequiredSurfaceDrift(
            comparison.changes().to_vec(),
        ))
    }
}

fn print_version_report(
    surface: &RequiredSurface,
    snapshot: &SnapshotVersions,
    observed: &ToolVersions,
) {
    for line in version_report_lines(surface, snapshot, observed) {
        println!("{line}");
    }
}

fn version_report_lines(
    surface: &RequiredSurface,
    snapshot: &SnapshotVersions,
    observed: &ToolVersions,
) -> Vec<String> {
    let mut lines = vec![format!(
        "schema: observed codex {} (snapshot {}), opencode {} (snapshot {}), acp {} (snapshot {})",
        observed.codex.version,
        snapshot.codex,
        observed.opencode.version,
        snapshot.opencode,
        snapshot.acp,
        snapshot.acp
    )];
    for (tool, observed_version, snapshot_version) in [
        (
            SurfaceTool::Codex,
            observed.codex.version.as_str(),
            snapshot.codex.as_str(),
        ),
        (
            SurfaceTool::OpenCode,
            observed.opencode.version.as_str(),
            snapshot.opencode.as_str(),
        ),
        (
            SurfaceTool::Acp,
            snapshot.acp.as_str(),
            snapshot.acp.as_str(),
        ),
    ] {
        if observed_version != snapshot_version {
            lines.push(format!(
                "schema: NOTICE {tool} version differs: observed {observed_version}, snapshot {snapshot_version}; comparison will continue"
            ));
        }
        match surface.version_support(tool, observed_version) {
            Some(VersionSupport::Supported) | None => {}
            Some(VersionSupport::Unverified { supported_range }) => lines.push(format!(
                "schema: WARNING unverified version for {tool}: observed {observed_version}, supported range {supported_range}; comparison will continue"
            )),
            Some(VersionSupport::Unparseable { supported_range }) => lines.push(format!(
                "schema: WARNING {tool} version `{observed_version}` cannot be matched against supported range {supported_range}; comparison will continue"
            )),
        }
    }
    lines
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ChangeCounts {
    added: usize,
    changed: usize,
    removed: usize,
}

impl ChangeCounts {
    const fn total(self) -> usize {
        self.added + self.changed + self.removed
    }

    fn record(&mut self, kind: ChangeKind) {
        match kind {
            ChangeKind::Added => self.added += 1,
            ChangeKind::Changed => self.changed += 1,
            ChangeKind::Removed => self.removed += 1,
        }
    }
}

fn print_partitioned_report(comparison: &SurfaceComparison, raw_changes: &[SchemaChange]) {
    let outside = out_of_surface_counts(comparison, raw_changes);
    println!("  in-surface    : {} drift", comparison.changes().len());
    for change in comparison.changes() {
        println!("    {change}");
    }
    println!(
        "  out-of-surface: {} drift ({} added / {} changed / {} removed)",
        outside.total(),
        outside.added,
        outside.changed,
        outside.removed
    );
}

fn out_of_surface_counts(
    comparison: &SurfaceComparison,
    raw_changes: &[SchemaChange],
) -> ChangeCounts {
    let mut outside = ChangeCounts::default();
    for change in raw_changes {
        let kind = match change.kind {
            ChangeKind::Added => SurfaceChangeKind::Added,
            ChangeKind::Changed => SurfaceChangeKind::Changed,
            ChangeKind::Removed => SurfaceChangeKind::Removed,
        };
        if comparison.classify_full_change(&change.file, &change.pointer, kind)
            == RawChangeDisposition::OutOfSurface
        {
            outside.record(change.kind);
        }
    }
    outside
}

fn preserve_control_files(source_root: &Path, target_root: &Path) -> Result<(), SchemaError> {
    for file in [REQUIRED_SURFACE_FILE, SURFACE_HISTORY_FILE] {
        let source = source_root.join(file);
        if !source.is_file() {
            return Err(SchemaError::InvalidSnapshot(format!(
                "schema control file `{file}` is missing"
            )));
        }
        fs::copy(source, target_root.join(file))?;
    }
    Ok(())
}

fn record_observed_history(
    path: &Path,
    versions: &ToolVersions,
    acp_version: &str,
    digests: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<(), SchemaError> {
    let observed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut records = Vec::new();
    for (tool, version) in [
        ("codex", versions.codex.version.as_str()),
        ("opencode", versions.opencode.version.as_str()),
        ("acp", acp_version),
    ] {
        let surface_digests = digests.get(tool).cloned().ok_or_else(|| {
            SchemaError::InvalidSnapshot(format!(
                "required-surface digest output omitted tool `{tool}`"
            ))
        })?;
        records.push(SurfaceHistoryRecord {
            observed_at: observed_at.clone(),
            tool: tool.to_owned(),
            version: version.to_owned(),
            surface_digests,
        });
    }
    let summary = schema_history::append_observations(path, &records)?;
    println!(
        "schema history: appended {} new observation(s), deduplicated {} existing observation(s)",
        summary.appended, summary.deduplicated
    );
    Ok(())
}

fn run_history(workspace_root: &Path, tool: &str, entry_id: &str) -> Result<(), SchemaError> {
    let surface = schema_surface::load_required_surface(workspace_root)?;
    let declared = surface
        .entries()
        .iter()
        .any(|entry| entry.tool.as_str() == tool && entry.id == entry_id);
    if !declared {
        return Err(SchemaError::InvalidSnapshot(format!(
            "`{entry_id}` is not a required-surface entry for tool `{tool}`"
        )));
    }
    let history_path = workspace_root.join("schemas").join(SURFACE_HISTORY_FILE);
    let entries = schema_history::timeline(&history_path, tool, entry_id)?;
    print!(
        "{}",
        schema_history::format_timeline(tool, entry_id, &entries)
    );
    Ok(())
}

pub fn verify_semantic_match(expected: &Path, actual: &Path) -> Result<(), SchemaError> {
    let changes = semantic_changes(expected, actual)?;
    if changes.is_empty() {
        Ok(())
    } else {
        Err(SchemaError::Drift(changes))
    }
}

pub fn semantic_changes(
    expected_root: &Path,
    actual_root: &Path,
) -> Result<Vec<SchemaChange>, SchemaError> {
    let expected = load_json_tree(expected_root)?;
    let actual = load_json_tree(actual_root)?;
    let files: BTreeSet<&String> = expected.keys().chain(actual.keys()).collect();
    let mut changes = Vec::new();

    for file in files {
        match (expected.get(file), actual.get(file)) {
            (Some(expected_value), Some(actual_value)) => {
                compare_json(file, "#", expected_value, actual_value, &mut changes);
            }
            (Some(_), None) => changes.push(SchemaChange {
                file: file.clone(),
                pointer: "#".to_owned(),
                kind: ChangeKind::Removed,
            }),
            (None, Some(_)) => changes.push(SchemaChange {
                file: file.clone(),
                pointer: "#".to_owned(),
                kind: ChangeKind::Added,
            }),
            (None, None) => {}
        }
    }

    changes.sort();
    changes.dedup();
    Ok(changes)
}

fn load_snapshot_versions(path: &Path) -> Result<SnapshotVersions, SchemaError> {
    let source = fs::read_to_string(path)?;
    let codex = markdown_package_version(&source, "@openai/codex@").ok_or_else(|| {
        SchemaError::InvalidSnapshot(
            "schemas/VERSIONS.md does not contain an exact @openai/codex package version"
                .to_owned(),
        )
    })?;
    let opencode = markdown_package_version(&source, "opencode-ai@").ok_or_else(|| {
        SchemaError::InvalidSnapshot(
            "schemas/VERSIONS.md does not contain an exact opencode-ai package version".to_owned(),
        )
    })?;
    let acp = markdown_package_version(&source, "agent-client-protocol-json-schema-v1@")
        .ok_or_else(|| {
            SchemaError::InvalidSnapshot(
                "schemas/VERSIONS.md does not contain an exact ACP schema artifact version"
                    .to_owned(),
            )
        })?;
    for (label, version) in [
        ("Codex", codex.as_str()),
        ("OpenCode", opencode.as_str()),
        ("ACP", acp.as_str()),
    ] {
        Version::parse(version).map_err(|error| {
            SchemaError::InvalidSnapshot(format!(
                "{label} snapshot version `{version}` is not valid semver: {error}"
            ))
        })?;
    }
    Ok(SnapshotVersions {
        codex,
        opencode,
        acp,
    })
}

fn markdown_package_version(source: &str, marker: &str) -> Option<String> {
    source.lines().find_map(|line| {
        let (_, suffix) = line.split_once(marker)?;
        let version = suffix.split('`').next()?.trim();
        (!version.is_empty()).then(|| version.to_owned())
    })
}

fn install_command(tool: &str, version: &str) -> String {
    #[cfg(windows)]
    let npm = "npm.cmd";
    #[cfg(not(windows))]
    let npm = "npm";

    match tool {
        "codex" => format!("{npm} install --global @openai/codex@{version}"),
        "opencode" => format!("{npm} install --global opencode-ai@{version}"),
        _ => format!("{npm} install --global {tool}@{version}"),
    }
}

fn parse_tool_observation(
    tool: &'static str,
    _snapshot_version: &str,
    stdout: &[u8],
    stderr: &[u8],
    install: &str,
) -> Result<ToolObservation, SchemaError> {
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    let output = if stdout.is_empty() { stderr } else { stdout };
    let version = output
        .split_ascii_whitespace()
        .rev()
        .find(|candidate| Version::parse(candidate).is_ok())
        .map(str::to_owned)
        .ok_or_else(|| SchemaError::ToolUnavailable {
            tool,
            detail: format!("version probe returned an unrecognized version string `{output}`"),
            install: install.to_owned(),
        })?;
    Ok(ToolObservation { output, version })
}

fn verify_tool_versions(snapshot: &SnapshotVersions) -> Result<ToolVersions, SchemaError> {
    let codex = verify_tool(
        "codex",
        &install_command("codex", &snapshot.codex),
        &snapshot.codex,
    )?;
    let opencode = verify_tool(
        "opencode",
        &install_command("opencode", &snapshot.opencode),
        &snapshot.opencode,
    )?;
    Ok(ToolVersions { codex, opencode })
}

fn verify_tool(
    tool: &'static str,
    install: &str,
    snapshot_version: &str,
) -> Result<ToolObservation, SchemaError> {
    let isolated_state = if tool == "opencode" {
        Some(
            TempDirBuilder::new()
                .prefix("opencode-version-")
                .tempdir()?,
        )
    } else {
        None
    };
    let mut command = child_command(tool_executable(tool)?);
    if let Some(state) = &isolated_state {
        configure_opencode_environment(&mut command, state.path())?;
    }
    let output =
        command
            .arg("--version")
            .output()
            .map_err(|error| SchemaError::ToolUnavailable {
                tool,
                detail: error.to_string(),
                install: install.to_owned(),
            })?;

    if !output.status.success() {
        return Err(SchemaError::ToolUnavailable {
            tool,
            detail: format!("version probe exited with {}", output.status),
            install: install.to_owned(),
        });
    }

    parse_tool_observation(
        tool,
        snapshot_version,
        &output.stdout,
        &output.stderr,
        install,
    )
}

fn fetch_all(
    workspace_root: &Path,
    schemas_root: &Path,
    versions: &ToolVersions,
) -> Result<SnapshotStats, SchemaError> {
    fs::create_dir_all(schemas_root)?;

    let codex_files = fetch_codex(workspace_root, schemas_root, &versions.codex.version)?;
    let opencode_files = fetch_opencode(schemas_root, &versions.opencode.version)?;
    let acp_files = fetch_acp(schemas_root)?;
    let stats = SnapshotStats {
        codex_files,
        opencode_files,
        acp_files,
    };
    let fetched_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    fs::write(
        schemas_root.join("VERSIONS.md"),
        render_versions(&fetched_at, versions, &stats),
    )?;

    Ok(stats)
}

fn fetch_codex(
    workspace_root: &Path,
    schemas_root: &Path,
    observed_version: &str,
) -> Result<usize, SchemaError> {
    let codex_dir = schemas_root.join("codex");
    let status = child_command(tool_executable("codex")?)
        .args(["app-server", "generate-json-schema", "--out"])
        .arg(&codex_dir)
        .current_dir(workspace_root)
        .status()
        .map_err(|error| SchemaError::ToolUnavailable {
            tool: "codex",
            detail: error.to_string(),
            install: install_command("codex", observed_version),
        })?;
    if !status.success() {
        return Err(SchemaError::ProcessFailed {
            tool: "codex app-server generate-json-schema",
            status,
        });
    }

    require_file(&codex_dir.join("codex_app_server_protocol.schemas.json"))?;
    require_file(&codex_dir.join("codex_app_server_protocol.v2.schemas.json"))?;
    require_file(&codex_dir.join("ClientRequest.json"))?;
    require_file(&codex_dir.join("ServerRequest.json"))?;
    require_file(&codex_dir.join("ClientNotification.json"))?;
    require_file(&codex_dir.join("ServerNotification.json"))?;
    validate_json_tree(&codex_dir)
}

fn fetch_opencode(schemas_root: &Path, observed_version: &str) -> Result<usize, SchemaError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let run_dir = TempDirBuilder::new().prefix("opencode-schema-").tempdir()?;
    let port_string = port.to_string();
    let mut server_command = child_command(tool_executable("opencode")?);
    configure_opencode_environment(&mut server_command, run_dir.path())?;
    server_command
        .args([
            "serve",
            "--pure",
            "--hostname",
            "127.0.0.1",
            "--port",
            &port_string,
            "--log-level",
            "ERROR",
        ])
        .current_dir(run_dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        server_command.process_group(0);
    }
    let child = server_command
        .spawn()
        .map_err(|error| SchemaError::ToolUnavailable {
            tool: "opencode",
            detail: error.to_string(),
            install: install_command("opencode", observed_version),
        })?;
    let mut server = ChildGuard::new(child);
    let url = format!("http://127.0.0.1:{port}/doc");
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|source| SchemaError::Http {
            url: url.clone(),
            source,
        })?;
    let deadline = Instant::now() + Duration::from_secs(30);

    let body = loop {
        if let Some(status) = server.try_wait()? {
            return Err(SchemaError::Server(format!(
                "process exited before /doc was ready with status {status}"
            )));
        }

        let last_failure = match client.get(&url).header(ACCEPT, "application/json").send() {
            Ok(response) if response.status().is_success() => {
                let bytes = response
                    .bytes()
                    .map_err(|source| SchemaError::Http {
                        url: url.clone(),
                        source,
                    })?
                    .to_vec();
                break bytes;
            }
            Ok(response) => format!("HTTP {}", response.status()),
            Err(error) => error.to_string(),
        };

        if Instant::now() >= deadline {
            return Err(SchemaError::Server(format!(
                "timed out waiting for {url}: {last_failure}"
            )));
        }
        thread::sleep(Duration::from_millis(100));
    };

    server.stop()?;

    let opencode_dir = schemas_root.join("opencode");
    fs::create_dir_all(&opencode_dir)?;
    let output = opencode_dir.join("openapi.json");
    fs::write(&output, body)?;
    let value = read_json(&output)?;
    let openapi = value
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SchemaError::InvalidSnapshot(
                "OpenCode /doc does not contain a string `openapi` field".to_owned(),
            )
        })?;
    if openapi != "3.1.0" {
        return Err(SchemaError::InvalidSnapshot(format!(
            "OpenCode {} returned OpenAPI {openapi}, expected 3.1.0",
            observed_version
        )));
    }
    let info_version = value
        .get("info")
        .and_then(|info| info.get("version"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SchemaError::InvalidSnapshot(
                "OpenCode /doc does not contain a string `info.version` field".to_owned(),
            )
        })?;
    if info_version != "1.0.0" {
        return Err(SchemaError::InvalidSnapshot(format!(
            "OpenCode {} returned info.version {info_version}, expected 1.0.0",
            observed_version
        )));
    }
    validate_json_tree(&opencode_dir)
}

fn configure_opencode_environment(command: &mut Command, root: &Path) -> io::Result<()> {
    for (variable, directory) in [
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("XDG_STATE_HOME", root.join("state")),
        ("XDG_CACHE_HOME", root.join("cache")),
    ] {
        fs::create_dir_all(&directory)?;
        command.env(variable, directory);
    }
    Ok(())
}

fn fetch_acp(schemas_root: &Path) -> Result<usize, SchemaError> {
    let acp_dir = schemas_root.join("acp");
    fs::create_dir_all(&acp_dir)?;
    if let Some(source) = configured_acp_snapshot()? {
        copy_verified_acp_snapshot(&source, &acp_dir)?;
        println!("schema: used a configured ACP snapshot verified against commit {ACP_COMMIT}");
    } else {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("OneKaleidoscope-schema-snapshot/0.1")
            .build()
            .map_err(|source| SchemaError::Http {
                url: ACP_REPOSITORY.to_owned(),
                source,
            })?;
        download(&client, ACP_SCHEMA_URL, &acp_dir.join("schema.json"))?;
        download(&client, ACP_META_URL, &acp_dir.join("meta.json"))?;
    }

    let meta_path = acp_dir.join("meta.json");
    let meta = read_json(&meta_path)?;
    if meta.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(SchemaError::InvalidSnapshot(
            "ACP meta.json does not declare wire protocol version 1".to_owned(),
        ));
    }
    validate_json_tree(&acp_dir)
}

fn configured_acp_snapshot() -> Result<Option<PathBuf>, SchemaError> {
    configured_acp_snapshot_with_lookup(|name| env::var_os(name))
}

fn configured_acp_snapshot_with_lookup(
    mut lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>, SchemaError> {
    let Some(configured) = lookup(ACP_SNAPSHOT_ENVIRONMENT) else {
        return Ok(None);
    };
    if configured.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(configured);
    if !path.is_absolute() {
        return Err(SchemaError::ToolConfiguration {
            tool: "ACP schema snapshot",
            variable: ACP_SNAPSHOT_ENVIRONMENT,
        });
    }
    Ok(Some(path))
}

fn copy_verified_acp_snapshot(source: &Path, destination: &Path) -> Result<(), SchemaError> {
    for (file, expected) in [
        ("schema.json", ACP_SCHEMA_BLOB),
        ("meta.json", ACP_META_BLOB),
    ] {
        let bytes = fs::read(source.join(file))?;
        let actual = git_blob_digest(&bytes);
        if actual != expected {
            return Err(SchemaError::SnapshotDigest {
                file,
                expected,
                actual,
            });
        }
        fs::write(destination.join(file), bytes)?;
    }
    Ok(())
}

fn git_blob_digest(bytes: &[u8]) -> String {
    let header = format!("blob {}\0", bytes.len());
    let mut digest = Sha1::new();
    digest.update(header.as_bytes());
    digest.update(bytes);
    digest.digest().to_string()
}

fn download(client: &Client, url: &str, output: &Path) -> Result<(), SchemaError> {
    let response = client.get(url).send().map_err(|source| SchemaError::Http {
        url: url.to_owned(),
        source,
    })?;
    if !response.status().is_success() {
        return Err(SchemaError::HttpStatus {
            url: url.to_owned(),
            status: response.status(),
        });
    }
    let bytes = response.bytes().map_err(|source| SchemaError::Http {
        url: url.to_owned(),
        source,
    })?;
    if bytes.is_empty() {
        return Err(SchemaError::InvalidSnapshot(format!(
            "{url} returned an empty file"
        )));
    }
    fs::write(output, &bytes)?;
    Ok(())
}

fn install_snapshot(
    staging: &TempDir,
    staged_schemas: &Path,
    destination: &Path,
) -> Result<(), SchemaError> {
    if !staged_schemas.is_dir() {
        return Err(SchemaError::InvalidSnapshot(
            "staged schemas directory is missing".to_owned(),
        ));
    }

    if destination.exists() {
        let backup = staging.path().join("previous-schemas");
        fs::rename(destination, &backup)?;
        if let Err(error) = fs::rename(staged_schemas, destination) {
            if let Err(restore_error) = fs::rename(&backup, destination) {
                return Err(SchemaError::InvalidSnapshot(format!(
                    "install failed ({error}) and restoring the previous snapshot failed ({restore_error})"
                )));
            }
            return Err(SchemaError::Io(error));
        }
    } else {
        fs::rename(staged_schemas, destination)?;
    }

    Ok(())
}

fn render_versions(fetched_at: &str, versions: &ToolVersions, stats: &SnapshotStats) -> String {
    format!(
        r#"# Upstream schema versions

Fetched at: `{fetched_at}`

All versions and source revisions below are exact. The JSON files under
`schemas/` are unmodified upstream snapshots.

## Codex app-server

- CLI: `{}`
- npm package: `@openai/codex@{}`
- Bundle mode: default/stable output without `--experimental`
- JSON files: `{}`
- Reproduction commands on Windows PowerShell:

```powershell
npm.cmd install --global @openai/codex@{}
codex.cmd app-server generate-json-schema --out schemas/codex
```

- Reproduction commands on macOS/Linux:

```sh
npm install --global @openai/codex@{}
codex app-server generate-json-schema --out schemas/codex
```

## OpenCode

- CLI: `{}`
- npm package: `opencode-ai@{}`
- OpenAPI document: `3.1.0`
- JSON files: `{}`
- Reproduction commands on Windows PowerShell, using two terminals:

```powershell
npm.cmd install --global opencode-ai@{}
opencode.cmd serve --pure --hostname 127.0.0.1 --port 4096 --log-level ERROR
```

```powershell
New-Item -ItemType Directory -Force schemas/opencode | Out-Null
curl.exe --fail --silent --show-error --noproxy "*" --header "Accept: application/json" http://127.0.0.1:4096/doc --output schemas/opencode/openapi.json
```

- Reproduction commands on macOS/Linux, using two terminals:

```sh
npm install --global opencode-ai@{}
opencode serve --pure --hostname 127.0.0.1 --port 4096 --log-level ERROR
```

```sh
mkdir -p schemas/opencode
curl --fail --silent --show-error --noproxy "*" --header "Accept: application/json" http://127.0.0.1:4096/doc --output schemas/opencode/openapi.json
```

## Agent Client Protocol

- Rust crate: `agent-client-protocol@{}`
- Wire protocol: `v1`
- Schema artifact: `agent-client-protocol-json-schema-v1@{}`
- Git tags: `v1.3.0`, `schema-v1.18.0`
- Commit: `{}`
- Repository: `{}`
- Source paths: `schema/v1/schema.json`, `schema/v1/meta.json`
- JSON files: `{}`
- Tag verification command:

```text
git ls-remote --tags {} 'refs/tags/v1.3.0^{{}}' 'refs/tags/schema-v1.18.0^{{}}'
```

Both peeled tags must resolve to `{}`.

- Reproduction commands on Windows PowerShell:

```powershell
New-Item -ItemType Directory -Force schemas/acp | Out-Null
curl.exe --fail --location --silent --show-error {} --output schemas/acp/schema.json
curl.exe --fail --location --silent --show-error {} --output schemas/acp/meta.json
```

- Reproduction commands on macOS/Linux:

```sh
mkdir -p schemas/acp
curl --fail --location --silent --show-error {} --output schemas/acp/schema.json
curl --fail --location --silent --show-error {} --output schemas/acp/meta.json
```
"#,
        versions.codex.output,
        versions.codex.version,
        stats.codex_files,
        versions.codex.version,
        versions.codex.version,
        versions.opencode.output,
        versions.opencode.version,
        stats.opencode_files,
        versions.opencode.version,
        versions.opencode.version,
        ACP_CRATE_VERSION,
        ACP_SCHEMA_VERSION,
        ACP_COMMIT,
        ACP_REPOSITORY,
        stats.acp_files,
        ACP_REPOSITORY,
        ACP_COMMIT,
        ACP_SCHEMA_URL,
        ACP_META_URL,
        ACP_SCHEMA_URL,
        ACP_META_URL,
    )
}

fn validate_json_tree(root: &Path) -> Result<usize, SchemaError> {
    let files = load_json_tree(root)?;
    if files.is_empty() {
        return Err(SchemaError::InvalidSnapshot(format!(
            "{} contains no JSON files",
            display_path(root)
        )));
    }
    Ok(files.len())
}

fn load_json_tree(root: &Path) -> Result<BTreeMap<String, Value>, SchemaError> {
    if !root.is_dir() {
        return Err(SchemaError::InvalidSnapshot(format!(
            "{} is not a directory",
            display_path(root)
        )));
    }

    let mut files = BTreeMap::new();
    visit_json_files(root, root, &mut files)?;
    Ok(files)
}

fn visit_json_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, Value>,
) -> Result<(), SchemaError> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            visit_json_files(root, &path, files)?;
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "json")
        {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| {
                    SchemaError::InvalidSnapshot("JSON path escaped its snapshot root".to_owned())
                })?
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(relative, read_json(&path)?);
        }
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value, SchemaError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|source| SchemaError::Json {
        path: display_path(path),
        source,
    })
}

fn require_file(path: &Path) -> Result<(), SchemaError> {
    let metadata = fs::metadata(path).map_err(|error| {
        SchemaError::InvalidSnapshot(format!(
            "required file {} is unavailable: {error}",
            display_path(path)
        ))
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(SchemaError::InvalidSnapshot(format!(
            "required file {} is empty or not a file",
            display_path(path)
        )));
    }
    Ok(())
}

fn compare_json(
    file: &str,
    pointer: &str,
    expected: &Value,
    actual: &Value,
    changes: &mut Vec<SchemaChange>,
) {
    match (expected, actual) {
        (Value::Object(expected_map), Value::Object(actual_map)) => {
            let keys: BTreeSet<&String> = expected_map.keys().chain(actual_map.keys()).collect();
            for key in keys {
                let child_pointer = json_pointer_child(pointer, key);
                match (expected_map.get(key), actual_map.get(key)) {
                    (Some(expected_child), Some(actual_child)) => {
                        compare_json(file, &child_pointer, expected_child, actual_child, changes)
                    }
                    (Some(_), None) => changes.push(SchemaChange {
                        file: file.to_owned(),
                        pointer: child_pointer,
                        kind: ChangeKind::Removed,
                    }),
                    (None, Some(_)) => changes.push(SchemaChange {
                        file: file.to_owned(),
                        pointer: child_pointer,
                        kind: ChangeKind::Added,
                    }),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(expected_items), Value::Array(actual_items)) => {
            let length = expected_items.len().max(actual_items.len());
            for index in 0..length {
                let child_pointer = json_pointer_child(pointer, &index.to_string());
                match (expected_items.get(index), actual_items.get(index)) {
                    (Some(expected_child), Some(actual_child)) => {
                        compare_json(file, &child_pointer, expected_child, actual_child, changes)
                    }
                    (Some(_), None) => changes.push(SchemaChange {
                        file: file.to_owned(),
                        pointer: child_pointer,
                        kind: ChangeKind::Removed,
                    }),
                    (None, Some(_)) => changes.push(SchemaChange {
                        file: file.to_owned(),
                        pointer: child_pointer,
                        kind: ChangeKind::Added,
                    }),
                    (None, None) => {}
                }
            }
        }
        _ if expected == actual => {}
        _ => changes.push(SchemaChange {
            file: file.to_owned(),
            pointer: pointer.to_owned(),
            kind: ChangeKind::Changed,
        }),
    }
}

fn json_pointer_child(pointer: &str, token: &str) -> String {
    let escaped = token.replace('~', "~0").replace('/', "~1");
    format!("{pointer}/{escaped}")
}

fn display_path(path: &Path) -> String {
    if let Ok(current_dir) = env::current_dir() {
        if let Ok(relative) = path.strip_prefix(current_dir) {
            return relative.to_string_lossy().replace('\\', "/");
        }
    }
    if path.is_absolute() {
        let file_name = path
            .file_name()
            .map_or_else(|| "<path>".into(), |name| name.to_string_lossy());
        format!("<external>/{file_name}")
    } else {
        path.to_string_lossy().replace('\\', "/")
    }
}

fn tool_executable(tool: &'static str) -> Result<OsString, SchemaError> {
    tool_executable_with_lookup(tool, |name| env::var_os(name))
}

fn tool_executable_with_lookup(
    tool: &'static str,
    mut lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<OsString, SchemaError> {
    let variable = match tool {
        "codex" => Some(CODEX_EXECUTABLE_ENVIRONMENT),
        "opencode" => Some(OPENCODE_EXECUTABLE_ENVIRONMENT),
        _ => None,
    };
    if let Some((variable, configured)) =
        variable.and_then(|name| lookup(name).map(|value| (name, value)))
    {
        if configured.is_empty() {
            return Ok(default_tool_executable(tool));
        }
        let path = PathBuf::from(configured);
        if !path.is_absolute() {
            return Err(SchemaError::ToolConfiguration { tool, variable });
        }
        return Ok(path.into_os_string());
    }
    Ok(default_tool_executable(tool))
}

fn default_tool_executable(tool: &str) -> OsString {
    #[cfg(windows)]
    {
        OsString::from(format!("{tool}.cmd"))
    }
    #[cfg(not(windows))]
    {
        OsString::from(tool)
    }
}

fn child_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(test)]
mod executable_tests {
    use super::*;

    fn synthetic_exit_status(code: u32) -> ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;

            ExitStatus::from_raw(code)
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::process::ExitStatusExt;

            ExitStatus::from_raw(i32::try_from(code).unwrap_or(i32::MAX) << 8)
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn failed_tree_signal_cannot_be_hidden_by_root_exit() {
        let tree_signal = Ok(synthetic_exit_status(5));
        let root_exit = synthetic_exit_status(0);

        assert!(!cleanup_is_proven(&tree_signal, Some(&root_exit)));
    }

    #[cfg(not(windows))]
    #[test]
    fn successful_tree_signal_still_requires_bounded_root_exit() {
        let tree_signal = Ok(synthetic_exit_status(0));

        assert!(!cleanup_is_proven(&tree_signal, None));
    }

    #[test]
    fn root_exit_before_cleanup_is_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        let root_exit = synthetic_exit_status(0);

        let Err(error) = require_running_root_for_cleanup(42, Some(&root_exit)) else {
            return Err("an already-exited root unexpectedly proved descendant cleanup".into());
        };

        assert!(error
            .to_string()
            .contains("descendant cleanup cannot be proven"));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn spawned_family_uses_only_numeric_parentage() {
        let family = SpawnedProcessFamily::capture(
            100,
            &[
                ProcessEntry {
                    pid: 100,
                    parent_pid: 10,
                },
                ProcessEntry {
                    pid: 101,
                    parent_pid: 100,
                },
                ProcessEntry {
                    pid: 102,
                    parent_pid: 101,
                },
                ProcessEntry {
                    pid: 219_188,
                    parent_pid: 10,
                },
            ],
        );

        assert_eq!(family.members, BTreeSet::from([100_u32, 101_u32, 102_u32]));
        assert!(!family.members.contains(&219_188));
    }

    #[cfg(windows)]
    #[test]
    fn failed_taskkill_falls_back_to_the_exact_root_handle(
    ) -> Result<(), Box<dyn std::error::Error>> {
        struct Root {
            killed: bool,
        }

        impl RootTerminationTarget for Root {
            fn process_id(&self) -> u32 {
                100
            }

            fn try_wait_now(&mut self) -> io::Result<Option<ExitStatus>> {
                Ok(None)
            }

            fn wait_for_exit(&mut self, _timeout: Duration) -> io::Result<Option<ExitStatus>> {
                Ok(self.killed.then(|| synthetic_exit_status(1)))
            }

            fn kill_direct(&mut self) -> io::Result<()> {
                self.killed = true;
                Ok(())
            }
        }

        let mut root = Root { killed: false };
        let status = terminate_root_with(&mut root, |_| Ok(synthetic_exit_status(1)))?;

        assert!(root.killed);
        assert_eq!(status.code(), Some(1));
        Ok(())
    }

    #[test]
    fn explicit_schema_tool_path_has_priority() {
        #[cfg(windows)]
        let configured = PathBuf::from(r"C:\tools\codex.exe");
        #[cfg(not(windows))]
        let configured = PathBuf::from("/tools/codex");

        let resolved = tool_executable_with_lookup("codex", |name| {
            (name == CODEX_EXECUTABLE_ENVIRONMENT).then(|| configured.clone().into_os_string())
        });

        assert_eq!(resolved.ok(), Some(configured.into_os_string()));
    }

    #[test]
    fn relative_explicit_schema_tool_path_is_rejected() {
        let result = tool_executable_with_lookup("opencode", |name| {
            (name == OPENCODE_EXECUTABLE_ENVIRONMENT).then(|| OsString::from("relative/opencode"))
        });

        assert!(matches!(
            result,
            Err(SchemaError::ToolConfiguration {
                tool: "opencode",
                variable: OPENCODE_EXECUTABLE_ENVIRONMENT,
            })
        ));
    }

    #[test]
    fn relative_explicit_acp_snapshot_path_is_rejected() {
        let result = configured_acp_snapshot_with_lookup(|name| {
            (name == ACP_SNAPSHOT_ENVIRONMENT).then(|| OsString::from("relative/acp"))
        });

        assert!(matches!(
            result,
            Err(SchemaError::ToolConfiguration {
                tool: "ACP schema snapshot",
                variable: ACP_SNAPSHOT_ENVIRONMENT,
            })
        ));
    }

    #[test]
    fn git_blob_digest_includes_the_git_header() {
        assert_eq!(
            git_blob_digest(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn configured_acp_snapshot_rejects_content_from_another_commit(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = tempfile::tempdir()?;
        let destination = tempfile::tempdir()?;
        fs::write(source.path().join("schema.json"), b"{}\n")?;

        let result = copy_verified_acp_snapshot(source.path(), destination.path());

        assert!(matches!(
            result,
            Err(SchemaError::SnapshotDigest {
                file: "schema.json",
                expected: ACP_SCHEMA_BLOB,
                ..
            })
        ));
        assert!(!destination.path().join("schema.json").exists());
        Ok(())
    }

    #[test]
    fn different_observed_version_is_data_not_tool_unavailable(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let observation =
            parse_tool_observation("opencode", "1.18.8", b"1.18.9\r\n", b"", "install command")?;

        assert_eq!(observation.version, "1.18.9");
        assert_eq!(observation.output, "1.18.9");
        Ok(())
    }

    #[test]
    fn executable_tool_failures_use_exit_two() {
        assert_eq!(
            SchemaError::ProcessFailed {
                tool: "codex app-server generate-json-schema",
                status: synthetic_exit_status(1),
            }
            .exit_code(),
            2
        );
        assert_eq!(
            SchemaError::Server("OpenCode exited before /doc was ready".to_owned()).exit_code(),
            2
        );
    }

    #[test]
    fn version_report_shows_snapshot_observed_and_unverified_warning(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let surface = schema_surface::parse_required_surface(
            r#"
schema_version = 1
[[tools]]
name = "codex"
supported_range = "=0.146.0"
[[tools]]
name = "opencode"
supported_range = "=1.18.8"
[[tools]]
name = "acp"
supported_range = "=1.18.0"
[[entries]]
id = "codex.type.Dummy"
tool = "codex"
kind = "type"
name = "Dummy"
reason = "Required by a synthetic UACP test surface."
[[entries]]
id = "opencode.type.Dummy"
tool = "opencode"
kind = "type"
name = "Dummy"
reason = "Required by a synthetic UACP test surface."
[[entries]]
id = "acp.type.Dummy"
tool = "acp"
kind = "type"
name = "Dummy"
reason = "Required by a synthetic UACP test surface."
"#,
        )?;
        let snapshot = SnapshotVersions {
            codex: "0.146.0".to_owned(),
            opencode: "1.18.8".to_owned(),
            acp: "1.18.0".to_owned(),
        };
        let observed = ToolVersions {
            codex: ToolObservation {
                output: "codex-cli 0.146.0".to_owned(),
                version: "0.146.0".to_owned(),
            },
            opencode: ToolObservation {
                output: "1.18.9".to_owned(),
                version: "1.18.9".to_owned(),
            },
        };

        let output = version_report_lines(&surface, &snapshot, &observed).join("\n");

        assert!(output.contains("opencode 1.18.9 (snapshot 1.18.8)"));
        assert!(output.contains("unverified version for opencode"));
        assert!(output.contains("comparison will continue"));
        Ok(())
    }

    #[test]
    fn out_of_surface_drift_is_informational_but_in_surface_drift_exits_one(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let surface = schema_surface::parse_required_surface(
            r#"
schema_version = 1
[[tools]]
name = "opencode"
supported_range = "=1.18.8"
[[entries]]
id = "opencode.type.Session"
tool = "opencode"
kind = "type"
name = "Session"
reason = "Required by the UACP session method family."
"#,
        )?;
        let baseline = tempfile::tempdir()?;
        let observed_outside = tempfile::tempdir()?;
        let observed_inside = tempfile::tempdir()?;
        let snapshot_versions = SnapshotVersions {
            codex: "0.146.0".to_owned(),
            opencode: "1.18.8".to_owned(),
            acp: "1.18.0".to_owned(),
        };
        let observed_versions = ToolVersions {
            codex: ToolObservation {
                output: "codex-cli 0.146.0".to_owned(),
                version: "0.146.0".to_owned(),
            },
            opencode: ToolObservation {
                output: "1.18.9".to_owned(),
                version: "1.18.9".to_owned(),
            },
        };
        write_opencode_schema(baseline.path(), "string", "baseline")?;
        write_opencode_schema(observed_outside.path(), "string", "observed")?;
        write_opencode_schema(observed_inside.path(), "integer", "baseline")?;

        let outside_comparison = schema_surface::compare_required_surface(
            &surface,
            baseline.path(),
            observed_outside.path(),
        )?;
        let outside_raw = semantic_changes(baseline.path(), observed_outside.path())?;
        let outside_counts = out_of_surface_counts(&outside_comparison, &outside_raw);

        assert!(outside_comparison.changes().is_empty());
        assert_eq!(outside_counts.total(), 1);
        assert!(required_surface_result(&outside_comparison).is_ok());

        let inside_comparison = schema_surface::compare_required_surface(
            &surface,
            baseline.path(),
            observed_inside.path(),
        )?;
        let inside_raw = semantic_changes(baseline.path(), observed_inside.path())?;
        let inside_counts = out_of_surface_counts(&inside_comparison, &inside_raw);
        let Err(error) = required_surface_result(&inside_comparison) else {
            return Err("required-surface drift unexpectedly returned success".into());
        };

        assert_eq!(error.exit_code(), 1);
        assert_eq!(inside_counts.total(), 0);
        assert!(
            version_report_lines(&surface, &snapshot_versions, &observed_versions)
                .join("\n")
                .contains("opencode 1.18.9 (snapshot 1.18.8)")
        );
        assert!(inside_comparison
            .changes()
            .iter()
            .map(ToString::to_string)
            .any(|change| change.contains("opencode.type.Session")
                && change.contains("#/components/schemas/Session")));
        Ok(())
    }

    #[test]
    fn refresh_control_files_are_preserved() -> Result<(), Box<dyn std::error::Error>> {
        let source = tempfile::tempdir()?;
        let staged = tempfile::tempdir()?;
        fs::write(source.path().join(REQUIRED_SURFACE_FILE), "surface")?;
        fs::write(source.path().join(SURFACE_HISTORY_FILE), "history")?;

        preserve_control_files(source.path(), staged.path())?;

        assert_eq!(
            fs::read_to_string(staged.path().join(REQUIRED_SURFACE_FILE))?,
            "surface"
        );
        assert_eq!(
            fs::read_to_string(staged.path().join(SURFACE_HISTORY_FILE))?,
            "history"
        );
        Ok(())
    }

    fn write_opencode_schema(
        root: &Path,
        id_type: &str,
        title: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = root.join("opencode");
        fs::create_dir_all(&directory)?;
        let schema = serde_json::json!({
            "openapi": "3.1.0",
            "info": {"title": title, "version": "1.0.0"},
            "paths": {},
            "components": {
                "schemas": {
                    "Session": {
                        "type": "object",
                        "properties": {"id": {"type": id_type}}
                    }
                }
            }
        });
        fs::write(directory.join("openapi.json"), serde_json::to_vec(&schema)?)?;
        Ok(())
    }
}

#[derive(Debug)]
struct ChildGuard {
    child: Child,
    stopped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            stopped: false,
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn stop(&mut self) -> io::Result<()> {
        if !self.stopped {
            terminate_process_tree(&mut self.child)?;
            self.stopped = true;
        }
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = terminate_process_tree(&mut self.child);
            self.stopped = true;
        }
    }
}

#[cfg(windows)]
const PROCESS_FAMILY_EXIT_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(windows)]
const DESCENDANT_FORCE_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessEntry {
    pid: u32,
    parent_pid: u32,
}

#[cfg(windows)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct SpawnedProcessFamily {
    root_pid: u32,
    members: BTreeSet<u32>,
}

#[cfg(windows)]
impl SpawnedProcessFamily {
    fn capture(root_pid: u32, snapshot: &[ProcessEntry]) -> Self {
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

    fn descendant_pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.members
            .iter()
            .copied()
            .filter(|process_id| *process_id != self.root_pid)
    }

    fn remaining<'a>(&self, snapshot: &'a [ProcessEntry]) -> Vec<&'a ProcessEntry> {
        snapshot
            .iter()
            .filter(|process| self.members.contains(&process.pid))
            .collect()
    }
}

#[cfg(windows)]
fn wait_for_spawned_process_family_exit(
    family: &SpawnedProcessFamily,
    timeout: Duration,
    mut snapshot: impl FnMut() -> io::Result<Vec<ProcessEntry>>,
) -> io::Result<()> {
    let started = Instant::now();
    loop {
        let current = snapshot().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "could not verify cleanup of spawned root PID {}: {error}",
                    family.root_pid
                ),
            )
        })?;
        let remaining = family.remaining(&current);
        if remaining.is_empty() {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{} member(s) of spawned root PID {} family remained after cleanup",
                    remaining.len(),
                    family.root_pid
                ),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

// Toolhelp32 enumerates only numeric PID/PPID relationships. It never reads or compares
// executable names, so an unrelated user process cannot become part of the spawned family merely
// because it has the same executable name.
#[cfg(windows)]
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
        // SAFETY: The flags request a system process snapshot and the PID argument is required to
        // be zero for this snapshot kind. No borrowed pointers are passed.
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
        // SAFETY: `snapshot.0` remains live for this call and `raw_entry` points to a writable,
        // correctly sized `ProcessEntry32W`.
        if unsafe { Process32FirstW(snapshot.0, &mut raw_entry) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut processes = Vec::new();
        loop {
            processes.push(ProcessEntry {
                pid: raw_entry.process_id,
                parent_pid: raw_entry.parent_process_id,
            });
            // SAFETY: The same live snapshot and valid writable entry are retained for the
            // complete enumeration.
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

// Holding exact process handles prevents PID reuse from redirecting cleanup to an unrelated
// process while the spawned family is being terminated and verified.
#[cfg(windows)]
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
            // SAFETY: OpenProcess receives a PID captured from Toolhelp32 and no inherited handle.
            // A non-null result is owned by this value and closed exactly once in Drop.
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
            // SAFETY: `self.raw` remains a valid owned process handle until Drop.
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
            // SAFETY: `self.raw` is an owned handle opened with PROCESS_TERMINATE.
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
            // SAFETY: `self.raw` remains valid for the duration of this bounded wait.
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
                // SAFETY: `self.raw` is owned by this value and has not been closed elsewhere.
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

#[cfg(windows)]
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    let root_pid = child.id();
    require_running_root_for_cleanup(root_pid, child.try_wait()?.as_ref())?;
    let snapshot = process_snapshot_ffi::snapshot_processes().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not capture spawned root PID {root_pid} family: {error}"),
        )
    })?;
    let family = SpawnedProcessFamily::capture(root_pid, &snapshot);
    let descendants = process_handle_ffi::CapturedDescendants::capture(family.descendant_pids())
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "could not retain exact handles for spawned root PID {root_pid} descendants: \
                     {error}"
                ),
            )
        });
    let root_termination = terminate_root_with(child, |process_id| {
        let mut taskkill = child_command("taskkill.exe");
        taskkill
            .args(["/PID", &process_id.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run_command_with_timeout(&mut taskkill, Duration::from_secs(10), "taskkill")
    });
    let descendant_termination = match &descendants {
        Ok(descendants) => descendants
            .terminate_and_wait(DESCENDANT_FORCE_EXIT_TIMEOUT)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "could not terminate exact descendants of spawned root PID {root_pid}: \
                         {error}"
                    ),
                )
            }),
        Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
    };
    combine_family_termination(root_pid, root_termination, descendant_termination)?;
    let verification = wait_for_spawned_process_family_exit(
        &family,
        PROCESS_FAMILY_EXIT_TIMEOUT,
        process_snapshot_ffi::snapshot_processes,
    );
    drop(descendants);
    verification
}

#[cfg(windows)]
trait RootTerminationTarget {
    fn process_id(&self) -> u32;
    fn try_wait_now(&mut self) -> io::Result<Option<ExitStatus>>;
    fn wait_for_exit(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>>;
    fn kill_direct(&mut self) -> io::Result<()>;
}

#[cfg(windows)]
impl RootTerminationTarget for Child {
    fn process_id(&self) -> u32 {
        self.id()
    }

    fn try_wait_now(&mut self) -> io::Result<Option<ExitStatus>> {
        self.try_wait()
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        wait_for_child_exit(self, timeout)
    }

    fn kill_direct(&mut self) -> io::Result<()> {
        self.kill()
    }
}

#[cfg(windows)]
fn terminate_root_with(
    child: &mut impl RootTerminationTarget,
    run_taskkill: impl FnOnce(u32) -> io::Result<ExitStatus>,
) -> io::Result<ExitStatus> {
    if let Some(status) = child.try_wait_now()? {
        return Ok(status);
    }
    let process_id = child.process_id();
    let tree_result = run_taskkill(process_id);
    if tree_result.as_ref().is_ok_and(ExitStatus::success) {
        if let Some(status) = child.wait_for_exit(Duration::from_secs(3))? {
            return Ok(status);
        }
    }

    let direct_kill_result = child.kill_direct();
    if let Some(status) = child.wait_for_exit(Duration::from_secs(3))? {
        return Ok(status);
    }

    Err(io::Error::other(format!(
        "spawned root PID {process_id} did not exit after {} and direct kill ({})",
        command_outcome("taskkill", &tree_result),
        direct_kill_result
            .map(|()| "requested successfully".to_owned())
            .unwrap_or_else(|error| error.to_string())
    )))
}

#[cfg(windows)]
fn combine_family_termination(
    root_pid: u32,
    root: io::Result<ExitStatus>,
    descendants: io::Result<()>,
) -> io::Result<()> {
    match (root, descendants) {
        (Ok(_), Ok(())) => Ok(()),
        (Err(root), Ok(())) => Err(root),
        (Ok(_), Err(descendants)) => Err(descendants),
        (Err(root), Err(descendants)) => Err(io::Error::other(format!(
            "spawned root PID {root_pid} cleanup failed ({root}); exact descendant cleanup also \
             failed ({descendants})"
        ))),
    }
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(windows))]
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    let process_id = child.id();
    require_running_root_for_cleanup(process_id, child.try_wait()?.as_ref())?;
    let process_group = format!("-{process_id}");
    let mut terminate_command = child_command("kill");
    terminate_command
        .args(["-TERM", "--", &process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let terminate_result =
        run_command_with_timeout(&mut terminate_command, Duration::from_secs(2), "kill -TERM");
    thread::sleep(Duration::from_millis(250));
    let mut kill_command = child_command("kill");
    kill_command
        .args(["-KILL", "--", &process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let kill_result =
        run_command_with_timeout(&mut kill_command, Duration::from_secs(2), "kill -KILL");

    let mut root_exit = wait_for_child_exit(child, Duration::from_secs(3))?;
    let mut direct_kill_error = None;
    if root_exit.is_none() {
        direct_kill_error = child.kill().err();
        root_exit = wait_for_child_exit(child, Duration::from_secs(3))?;
    }

    if cleanup_is_proven(&terminate_result, root_exit.as_ref())
        || cleanup_is_proven(&kill_result, root_exit.as_ref())
    {
        return Ok(());
    }

    Err(io::Error::other(format!(
        "could not prove process-group cleanup for spawned root PID {}: {}; {}; root exit: {}; \
         direct kill: {}",
        child.id(),
        command_outcome("kill -TERM", &terminate_result),
        command_outcome("kill -KILL", &kill_result),
        root_exit.map_or_else(
            || "not observed within timeout".to_owned(),
            |status| status.to_string()
        ),
        direct_kill_error.map_or_else(
            || "not required or requested successfully".to_owned(),
            |error| error.to_string()
        )
    )))
}

fn require_running_root_for_cleanup(
    process_id: u32,
    initial_status: Option<&ExitStatus>,
) -> io::Result<()> {
    match initial_status {
        None => Ok(()),
        Some(status) => Err(io::Error::other(format!(
            "spawned root PID {process_id} exited with {status} before process-tree cleanup; \
             descendant cleanup cannot be proven"
        ))),
    }
}

#[cfg(not(windows))]
fn cleanup_is_proven(tree_signal: &io::Result<ExitStatus>, root_exit: Option<&ExitStatus>) -> bool {
    tree_signal.as_ref().is_ok_and(ExitStatus::success) && root_exit.is_some()
}

fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    label: &str,
) -> io::Result<ExitStatus> {
    let mut child = command.spawn()?;
    if let Some(status) = wait_for_child_exit(&mut child, timeout)? {
        return Ok(status);
    }

    let kill_error = child.kill().err();
    let reaped = wait_for_child_exit(&mut child, Duration::from_secs(2))?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "{label} exceeded its timeout; helper exit: {}; helper kill: {}",
            reaped.map_or_else(|| "not observed".to_owned(), |status| status.to_string()),
            kill_error.map_or_else(|| "requested".to_owned(), |error| error.to_string())
        ),
    ))
}

fn command_outcome(label: &str, result: &io::Result<ExitStatus>) -> String {
    match result {
        Ok(status) => format!("{label} exited with {status}"),
        Err(error) => format!("{label} failed: {error}"),
    }
}
