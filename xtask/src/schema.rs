use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use reqwest::blocking::Client;
use reqwest::header::ACCEPT;
use serde_json::Value;
use tempfile::{Builder as TempDirBuilder, TempDir};
use thiserror::Error;

pub const CODEX_VERSION: &str = "0.144.6";
pub const OPENCODE_VERSION: &str = "1.18.8";
pub const ACP_CRATE_VERSION: &str = "1.3.0";
pub const ACP_SCHEMA_VERSION: &str = "1.18.0";
pub const ACP_COMMIT: &str = "48b2abf1ac750fece26e03e92e773ccbd4754f5d";

const CODEX_VERSION_OUTPUT: &str = "codex-cli 0.144.6";
const OPENCODE_VERSION_OUTPUT: &str = "1.18.8";
const ACP_REPOSITORY: &str = "https://github.com/agentclientprotocol/agent-client-protocol.git";
const ACP_SCHEMA_URL: &str = "https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/schema.json";
const ACP_META_URL: &str = "https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/meta.json";

#[cfg(windows)]
const CODEX_INSTALL_COMMAND: &str = "npm.cmd install --global @openai/codex@0.144.6";
#[cfg(not(windows))]
const CODEX_INSTALL_COMMAND: &str = "npm install --global @openai/codex@0.144.6";
#[cfg(windows)]
const OPENCODE_INSTALL_COMMAND: &str = "npm.cmd install --global opencode-ai@1.18.8";
#[cfg(not(windows))]
const OPENCODE_INSTALL_COMMAND: &str = "npm install --global opencode-ai@1.18.8";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaCommand {
    Refresh,
    Diff,
}

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("required tool `{tool}` is unavailable ({detail}); install exactly with: {install}")]
    ToolUnavailable {
        tool: &'static str,
        detail: String,
        install: &'static str,
    },
    #[error(
        "tool `{tool}` has version `{actual}`, expected `{expected}`; install exactly with: {install}"
    )]
    ToolVersion {
        tool: &'static str,
        expected: &'static str,
        actual: String,
        install: &'static str,
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
    #[error("OpenCode schema server failure: {0}")]
    Server(String),
    #[error("schema drift detected at {} path(s)", .0.len())]
    Drift(Vec<SchemaChange>),
}

impl SchemaError {
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::ToolUnavailable { .. } | Self::ToolVersion { .. } => 2,
            Self::Drift(_) => 1,
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
struct ToolVersions {
    codex: String,
    opencode: String,
}

#[derive(Debug)]
struct SnapshotStats {
    codex_files: usize,
    opencode_files: usize,
    acp_files: usize,
}

pub fn run(command: SchemaCommand, workspace_root: &Path) -> Result<(), SchemaError> {
    let versions = verify_tool_versions()?;
    let target = workspace_root.join("target");
    fs::create_dir_all(&target)?;
    let staging = TempDirBuilder::new()
        .prefix("schema-snapshot-")
        .tempdir_in(&target)?;
    let staged_schemas = staging.path().join("schemas");

    println!(
        "schema: fetching Codex {}, OpenCode {}, ACP crate {} / schema {}",
        CODEX_VERSION, OPENCODE_VERSION, ACP_CRATE_VERSION, ACP_SCHEMA_VERSION
    );
    let stats = fetch_all(workspace_root, &staged_schemas, &versions)?;

    match command {
        SchemaCommand::Refresh => {
            install_snapshot(&staging, &staged_schemas, &workspace_root.join("schemas"))?;
            println!(
                "schema refresh: wrote {} Codex, {} OpenCode, and {} ACP JSON file(s)",
                stats.codex_files, stats.opencode_files, stats.acp_files
            );
            Ok(())
        }
        SchemaCommand::Diff => {
            let snapshot = workspace_root.join("schemas");
            match verify_semantic_match(&snapshot, &staged_schemas) {
                Ok(()) => {
                    println!(
                        "schema diff: no semantic drift ({} JSON files compared)",
                        stats.codex_files + stats.opencode_files + stats.acp_files
                    );
                    Ok(())
                }
                Err(SchemaError::Drift(changes)) => {
                    for change in &changes {
                        println!("{change}");
                    }
                    Err(SchemaError::Drift(changes))
                }
                Err(error) => Err(error),
            }
        }
    }
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

fn verify_tool_versions() -> Result<ToolVersions, SchemaError> {
    let codex = verify_tool("codex", CODEX_VERSION_OUTPUT, CODEX_INSTALL_COMMAND)?;
    let opencode = verify_tool(
        "opencode",
        OPENCODE_VERSION_OUTPUT,
        OPENCODE_INSTALL_COMMAND,
    )?;
    Ok(ToolVersions { codex, opencode })
}

fn verify_tool(
    tool: &'static str,
    expected: &'static str,
    install: &'static str,
) -> Result<String, SchemaError> {
    let output = child_command(tool_executable(tool))
        .arg("--version")
        .output()
        .map_err(|error| SchemaError::ToolUnavailable {
            tool,
            detail: error.to_string(),
            install,
        })?;

    if !output.status.success() {
        return Err(SchemaError::ToolUnavailable {
            tool,
            detail: format!("version probe exited with {}", output.status),
            install,
        });
    }

    let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if actual != expected {
        return Err(SchemaError::ToolVersion {
            tool,
            expected,
            actual,
            install,
        });
    }

    Ok(actual)
}

fn fetch_all(
    workspace_root: &Path,
    schemas_root: &Path,
    versions: &ToolVersions,
) -> Result<SnapshotStats, SchemaError> {
    fs::create_dir_all(schemas_root)?;

    let codex_files = fetch_codex(workspace_root, schemas_root)?;
    let opencode_files = fetch_opencode(schemas_root)?;
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

fn fetch_codex(workspace_root: &Path, schemas_root: &Path) -> Result<usize, SchemaError> {
    let codex_dir = schemas_root.join("codex");
    let status = child_command(tool_executable("codex"))
        .args(["app-server", "generate-json-schema", "--out"])
        .arg(&codex_dir)
        .current_dir(workspace_root)
        .status()?;
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

fn fetch_opencode(schemas_root: &Path) -> Result<usize, SchemaError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let run_dir = TempDirBuilder::new().prefix("opencode-schema-").tempdir()?;
    let port_string = port.to_string();
    let mut server_command = child_command(tool_executable("opencode"));
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
            install: OPENCODE_INSTALL_COMMAND,
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
            OPENCODE_VERSION
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
            OPENCODE_VERSION
        )));
    }
    validate_json_tree(&opencode_dir)
}

fn fetch_acp(schemas_root: &Path) -> Result<usize, SchemaError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("OneKaleidoscope-schema-snapshot/0.1")
        .build()
        .map_err(|source| SchemaError::Http {
            url: ACP_REPOSITORY.to_owned(),
            source,
        })?;
    let acp_dir = schemas_root.join("acp");
    fs::create_dir_all(&acp_dir)?;
    download(&client, ACP_SCHEMA_URL, &acp_dir.join("schema.json"))?;
    download(&client, ACP_META_URL, &acp_dir.join("meta.json"))?;

    let meta_path = acp_dir.join("meta.json");
    let meta = read_json(&meta_path)?;
    if meta.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(SchemaError::InvalidSnapshot(
            "ACP meta.json does not declare wire protocol version 1".to_owned(),
        ));
    }
    validate_json_tree(&acp_dir)
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
        versions.codex,
        CODEX_VERSION,
        stats.codex_files,
        CODEX_VERSION,
        CODEX_VERSION,
        versions.opencode,
        OPENCODE_VERSION,
        stats.opencode_files,
        OPENCODE_VERSION,
        OPENCODE_VERSION,
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

fn tool_executable(tool: &str) -> OsString {
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
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    let status = child_command("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    let waited = child.wait()?;
    if status.success() || waited.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "taskkill failed with {status}; child exited with {waited}"
        )))
    }
}

#[cfg(not(windows))]
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    let process_group = format!("-{}", child.id());
    let terminate_status = child_command("kill")
        .args(["-TERM", "--", &process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    thread::sleep(Duration::from_millis(250));
    let kill_status = child_command("kill")
        .args(["-KILL", "--", &process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    let waited = child.wait()?;
    if terminate_status.success() || kill_status.success() || waited.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "process-group termination failed with {terminate_status} and {kill_status}; child exited with {waited}"
        )))
    }
}
