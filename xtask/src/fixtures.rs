use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

const FIXTURE_DIRECTORIES: [(&str, AgentKind); 3] = [
    ("codex", AgentKind::Codex),
    ("acp-claude", AgentKind::Acp),
    ("opencode", AgentKind::OpenCode),
];
const OUTER_FIELDS: [&str; 4] = ["dir", "payload", "transport", "ts_ms"];
const REDACTED_TOKEN: &str = "<REDACTED_TOKEN>";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentKind {
    Codex,
    Acp,
    OpenCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    C2s,
    S2c,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transport {
    Stdio,
    Http,
    Sse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyIssue {
    pub file: String,
    pub line: usize,
    pub category: String,
    pub pointer: Option<String>,
}

impl fmt::Display for VerifyIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}: {}", self.file, self.line, self.category)?;
        if let Some(pointer) = &self.pointer {
            write!(formatter, " at {pointer}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerifySummary {
    pub files: usize,
    pub records: usize,
    pub codex_files: usize,
    pub acp_files: usize,
    pub opencode_files: usize,
    pub claude_sidecar_files: usize,
    pub claude_sidecar_records: usize,
    pub claude_sidecar_auth_failure_files: usize,
    pub claude_sidecar_acceptance_files: usize,
}

impl fmt::Display for VerifySummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} file(s), {} record(s) (codex: {}, acp-claude: {}, opencode: {}, claude-sidecar: {} file(s)/{} record(s), acceptance: {}, authentication-failure-only: {})",
            self.files,
            self.records,
            self.codex_files,
            self.acp_files,
            self.opencode_files,
            self.claude_sidecar_files,
            self.claude_sidecar_records,
            self.claude_sidecar_acceptance_files,
            self.claude_sidecar_auth_failure_files
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClaudeSidecarVerifySummary {
    pub files: usize,
    pub records: usize,
    pub acceptance_files: usize,
    pub auth_failure_files: usize,
}

#[derive(Debug)]
pub enum FixtureVerifyError {
    Read {
        label: String,
        source: std::io::Error,
    },
    InvalidSnapshot {
        label: String,
    },
    Failed {
        issues: Vec<VerifyIssue>,
    },
}

impl FixtureVerifyError {
    pub fn issues(&self) -> &[VerifyIssue] {
        match self {
            Self::Failed { issues } => issues,
            _ => &[],
        }
    }
}

impl fmt::Display for FixtureVerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { label, .. } => {
                write!(formatter, "could not read fixture input `{label}`")
            }
            Self::InvalidSnapshot { label } => {
                write!(
                    formatter,
                    "schema snapshot `{label}` is invalid or unsupported"
                )
            }
            Self::Failed { issues } => {
                writeln!(
                    formatter,
                    "fixture verification found {} issue(s):",
                    issues.len()
                )?;
                for issue in issues {
                    writeln!(formatter, "  {issue}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for FixtureVerifyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Identity {
    username: Option<String>,
    home_variants: Vec<String>,
}

impl Identity {
    pub fn from_environment() -> Self {
        let username = env::var("USERNAME")
            .ok()
            .or_else(|| env::var("USER").ok())
            .filter(|value| !value.is_empty());
        let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"));
        let home_variants = home
            .as_deref()
            .map(|value| path_variants(Path::new(value)))
            .unwrap_or_default()
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();
        Self {
            username,
            home_variants,
        }
    }

    pub fn new(username: Option<String>, home: Option<PathBuf>) -> Self {
        let home_variants = home
            .as_deref()
            .map(path_variants)
            .unwrap_or_default()
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();
        Self {
            username,
            home_variants,
        }
    }
}

#[derive(Clone, Debug)]
struct ParsedFile {
    agent: AgentKind,
    label: String,
    records: Vec<ParsedRecord>,
}

#[derive(Clone, Debug)]
struct ParsedRecord {
    line: usize,
    direction: Direction,
    transport: Transport,
    payload: Value,
}

#[derive(Clone, Debug)]
struct DiscoveredFile {
    agent: AgentKind,
    path: PathBuf,
    label: String,
}

pub fn verify_workspace(workspace: &Path) -> Result<VerifySummary, FixtureVerifyError> {
    let fixtures = workspace.join("tests").join("fixtures");
    let schemas = workspace.join("schemas");
    let sandbox = fixtures.join("sandbox");
    let identity = Identity::from_environment();
    let mut summary = verify_paths(&fixtures, &schemas, &sandbox, &identity)?;
    let claude_fixtures = workspace
        .join("crates")
        .join("kaleido-adapter-claude")
        .join("tests")
        .join("fixtures");
    let claude = verify_claude_sidecar_paths(&claude_fixtures, &identity)?;
    summary.files += claude.files;
    summary.records += claude.records;
    summary.claude_sidecar_files = claude.files;
    summary.claude_sidecar_records = claude.records;
    summary.claude_sidecar_auth_failure_files = claude.auth_failure_files;
    summary.claude_sidecar_acceptance_files = claude.acceptance_files;
    Ok(summary)
}

pub fn verify_paths(
    fixtures_root: &Path,
    schemas_root: &Path,
    sandbox_root: &Path,
    identity: &Identity,
) -> Result<VerifySummary, FixtureVerifyError> {
    let mut issues = Vec::new();
    let files = discover_fixture_files(fixtures_root, &mut issues)?;
    if !issues.is_empty() {
        sort_and_deduplicate_issues(&mut issues);
        return Err(FixtureVerifyError::Failed { issues });
    }
    if files.is_empty() {
        return Ok(VerifySummary::default());
    }

    let mut parsed_files = Vec::new();
    let mut summary = VerifySummary::default();

    for file in files {
        let parsed = parse_fixture_file(&file, sandbox_root, identity, &mut issues)?;
        summary.files += 1;
        summary.records += parsed.records.len();
        match file.agent {
            AgentKind::Codex => summary.codex_files += 1,
            AgentKind::Acp => summary.acp_files += 1,
            AgentKind::OpenCode => summary.opencode_files += 1,
        }
        parsed_files.push(parsed);
    }

    if !issues.is_empty() {
        sort_and_deduplicate_issues(&mut issues);
        return Err(FixtureVerifyError::Failed { issues });
    }

    let catalogs = SchemaCatalogs::load(schemas_root)?;
    for file in &parsed_files {
        match file.agent {
            AgentKind::Codex => verify_codex_file(file, &catalogs.codex, &mut issues)?,
            AgentKind::Acp => verify_acp_file(file, &catalogs.acp, &mut issues)?,
            AgentKind::OpenCode => verify_opencode_file(file, &catalogs.opencode, &mut issues)?,
        }
    }

    if issues.is_empty() {
        Ok(summary)
    } else {
        sort_and_deduplicate_issues(&mut issues);
        Err(FixtureVerifyError::Failed { issues })
    }
}

pub fn verify_claude_sidecar_paths(
    fixtures_root: &Path,
    identity: &Identity,
) -> Result<ClaudeSidecarVerifySummary, FixtureVerifyError> {
    let sandbox = fixtures_root.join("sandbox");
    let mut issues = Vec::new();
    let mut files = Vec::new();
    visit_claude_sidecar_fixture_directory(fixtures_root, &sandbox, &mut files, &mut issues)?;
    files.sort();
    if files.is_empty() {
        issues.push(issue(
            &claude_fixture_label(fixtures_root, &sandbox),
            1,
            "Claude sidecar fixture directory contains no JSONL capture",
            None,
        ));
    }

    let mut summary = ClaudeSidecarVerifySummary::default();
    for path in files {
        let label = claude_fixture_label(fixtures_root, &path);
        let metadata_path = claude_fixture_metadata_path(&path).ok_or_else(|| {
            FixtureVerifyError::InvalidSnapshot {
                label: label.clone(),
            }
        })?;
        let metadata = verify_claude_fixture_metadata(
            fixtures_root,
            &metadata_path,
            &sandbox,
            identity,
            &mut issues,
        )?;
        let evidence = parse_claude_sidecar_fixture(
            &path,
            &label,
            &sandbox,
            identity,
            metadata.as_ref(),
            &mut issues,
        )?;
        summary.files += 1;
        summary.records += evidence.records;
        if evidence.auth_failure_complete() {
            summary.auth_failure_files += 1;
        }
        if evidence.acceptance_complete() {
            summary.acceptance_files += 1;
        }
    }

    if issues.is_empty() {
        Ok(summary)
    } else {
        sort_and_deduplicate_issues(&mut issues);
        Err(FixtureVerifyError::Failed { issues })
    }
}

fn visit_claude_sidecar_fixture_directory(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let label = claude_fixture_label(root, directory);
    let mut entries = fs::read_dir(directory)
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let entry_label = claude_fixture_label(root, &path);
        let file_type = entry
            .file_type()
            .map_err(|source| FixtureVerifyError::Read {
                label: entry_label.clone(),
                source,
            })?;
        let indirect_directory = file_type.is_dir()
            && directory_entry_resolves_elsewhere(root, directory, &path, &entry_label)?;
        if file_type.is_symlink() || indirect_directory {
            issues.push(issue(
                &entry_label,
                1,
                "Claude sidecar fixture symlink is not allowed",
                None,
            ));
        } else if file_type.is_dir() {
            visit_claude_sidecar_fixture_directory(root, &path, files, issues)?;
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn claude_fixture_metadata_path(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    Some(path.with_file_name(format!("{stem}.metadata.json")))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaudeExpectedOutcome {
    AuthenticationFailure,
    SimpleTurnSuccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClaudeFixtureMetadata {
    provider_version: String,
    expected_outcome: ClaudeExpectedOutcome,
}

fn verify_claude_fixture_metadata(
    root: &Path,
    path: &Path,
    sandbox: &Path,
    identity: &Identity,
    issues: &mut Vec<VerifyIssue>,
) -> Result<Option<ClaudeFixtureMetadata>, FixtureVerifyError> {
    let label = claude_fixture_label(root, path);
    let contents = fs::read_to_string(path).map_err(|source| FixtureVerifyError::Read {
        label: label.clone(),
        source,
    })?;
    let mut raw_leaks = Vec::new();
    scan_unparsed_line(&contents, sandbox, identity, &label, 1, &mut raw_leaks);
    let value = match serde_json::from_str::<Value>(&contents) {
        Ok(value) => value,
        Err(_) => {
            issues.extend(raw_leaks);
            issues.push(issue(
                &label,
                1,
                "invalid Claude fixture metadata JSON",
                None,
            ));
            return Ok(None);
        }
    };
    let semantic_start = issues.len();
    scan_value_for_leaks(&value, "", false, sandbox, identity, &label, 1, issues);
    merge_raw_leak_findings(raw_leaks, semantic_start, false, issues);

    let Some(object) = value.as_object() else {
        issues.push(issue(
            &label,
            1,
            "Claude fixture metadata must be a JSON object",
            None,
        ));
        return Ok(None);
    };
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = [
        "acceptance_eligible",
        "capture",
        "expected_outcome",
        "provider",
        "provider_version",
    ]
    .into_iter()
    .collect();
    if actual != expected {
        issues.push(issue(
            &label,
            1,
            "Claude fixture metadata fields do not match the closed evidence contract",
            None,
        ));
    }
    for (field, expected_value) in [
        ("capture", "real_provider"),
        ("provider", "@anthropic-ai/claude-agent-sdk"),
    ] {
        if object.get(field).and_then(Value::as_str) != Some(expected_value) {
            issues.push(issue(
                &label,
                1,
                "Claude fixture metadata makes an unsupported evidence claim",
                Some(&pointer_child("", field)),
            ));
        }
    }
    let expected_outcome = match object.get("expected_outcome").and_then(Value::as_str) {
        Some("authentication_failure") => Some(ClaudeExpectedOutcome::AuthenticationFailure),
        Some("simple_turn_success") => Some(ClaudeExpectedOutcome::SimpleTurnSuccess),
        _ => {
            issues.push(issue(
                &label,
                1,
                "Claude fixture metadata makes an unsupported evidence claim",
                Some("/expected_outcome"),
            ));
            None
        }
    };
    let expected_acceptance = match expected_outcome {
        Some(ClaudeExpectedOutcome::AuthenticationFailure) => Some(false),
        Some(ClaudeExpectedOutcome::SimpleTurnSuccess) => Some(true),
        None => None,
    };
    if expected_acceptance.is_some()
        && object.get("acceptance_eligible").and_then(Value::as_bool) != expected_acceptance
    {
        issues.push(issue(
            &label,
            1,
            "Claude fixture acceptance eligibility contradicts its expected outcome",
            Some("/acceptance_eligible"),
        ));
    }
    let provider_version = object
        .get("provider_version")
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty());
    if provider_version.is_none() {
        issues.push(issue(
            &label,
            1,
            "Claude fixture provider_version must be a non-empty string",
            Some("/provider_version"),
        ));
    }
    Ok(provider_version
        .zip(expected_outcome)
        .map(
            |(provider_version, expected_outcome)| ClaudeFixtureMetadata {
                provider_version: provider_version.to_owned(),
                expected_outcome,
            },
        ))
}

#[derive(Debug, Default)]
struct ClaudeFixtureEvidence {
    records: usize,
    saw_ready: bool,
    saw_prompt_accepted: bool,
    saw_session_started: bool,
    saw_sdk_init: bool,
    saw_success_assistant: bool,
    saw_terminal_success: bool,
    saw_auth_failure_assistant: bool,
    saw_terminal_auth_failure: bool,
}

impl ClaudeFixtureEvidence {
    const fn acceptance_complete(&self) -> bool {
        self.saw_ready
            && self.saw_prompt_accepted
            && self.saw_session_started
            && self.saw_sdk_init
            && self.saw_success_assistant
            && self.saw_terminal_success
    }

    const fn auth_failure_complete(&self) -> bool {
        self.saw_ready
            && self.saw_prompt_accepted
            && self.saw_session_started
            && self.saw_sdk_init
            && self.saw_auth_failure_assistant
            && self.saw_terminal_auth_failure
    }
}

fn parse_claude_sidecar_fixture(
    path: &Path,
    label: &str,
    sandbox: &Path,
    identity: &Identity,
    metadata: Option<&ClaudeFixtureMetadata>,
    issues: &mut Vec<VerifyIssue>,
) -> Result<ClaudeFixtureEvidence, FixtureVerifyError> {
    let contents = fs::read_to_string(path).map_err(|source| FixtureVerifyError::Read {
        label: label.to_owned(),
        source,
    })?;
    let mut evidence = ClaudeFixtureEvidence::default();
    let mut session_ids = BTreeSet::new();
    if contents.is_empty() {
        issues.push(issue(label, 1, "empty Claude sidecar fixture", None));
    }

    for (zero_based_line, raw) in contents.lines().enumerate() {
        let line = zero_based_line + 1;
        if raw.trim().is_empty() {
            issues.push(issue(label, line, "blank Claude sidecar JSONL line", None));
            continue;
        }
        let mut raw_leaks = Vec::new();
        scan_unparsed_line(raw, sandbox, identity, label, line, &mut raw_leaks);
        let value = match serde_json::from_str::<Value>(raw) {
            Ok(value) => value,
            Err(_) => {
                issues.extend(raw_leaks);
                issues.push(issue(label, line, "invalid Claude sidecar JSON", None));
                continue;
            }
        };
        let semantic_start = issues.len();
        scan_value_for_leaks(&value, "", false, sandbox, identity, label, line, issues);
        merge_raw_leak_findings(raw_leaks, semantic_start, false, issues);

        let Some(object) = value.as_object() else {
            issues.push(issue(
                label,
                line,
                "Claude sidecar fixture line must be a JSON object",
                None,
            ));
            continue;
        };
        let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
        let expected: BTreeSet<&str> = ["kind", "payload", "protocol", "v"].into_iter().collect();
        if actual != expected {
            issues.push(issue(
                label,
                line,
                "Claude sidecar envelope must contain exactly v, protocol, kind, payload",
                None,
            ));
            continue;
        }
        if object.get("v").and_then(Value::as_u64) != Some(1) {
            issues.push(issue(
                label,
                line,
                "unsupported Claude sidecar envelope version",
                Some("/v"),
            ));
        }
        if object.get("protocol").and_then(Value::as_str) != Some("onekaleidoscope.claude.sidecar")
        {
            issues.push(issue(
                label,
                line,
                "unexpected Claude sidecar protocol identifier",
                Some("/protocol"),
            ));
        }
        let Some(kind) = object.get("kind").and_then(Value::as_str) else {
            issues.push(issue(
                label,
                line,
                "Claude sidecar kind must be a string",
                Some("/kind"),
            ));
            continue;
        };
        let Some(payload) = object.get("payload").and_then(Value::as_object) else {
            issues.push(issue(
                label,
                line,
                "Claude sidecar payload must be a JSON object",
                Some("/payload"),
            ));
            continue;
        };
        evidence.records += 1;
        collect_claude_sidecar_evidence(
            kind,
            payload,
            metadata.map(|value| value.provider_version.as_str()),
            label,
            line,
            &mut session_ids,
            &mut evidence,
            issues,
        );
    }

    if session_ids.len() != 1 {
        issues.push(issue(
            label,
            1,
            "Claude fixture must contain exactly one consistent SDK session id",
            Some("/payload/session_id"),
        ));
    }
    for (present, category) in [
        (
            evidence.saw_ready,
            "Claude fixture is missing sidecar ready",
        ),
        (
            evidence.saw_prompt_accepted,
            "Claude fixture is missing local prompt acceptance",
        ),
        (
            evidence.saw_session_started,
            "Claude fixture is missing a real SDK session start",
        ),
        (
            evidence.saw_sdk_init,
            "Claude fixture is missing the SDK init message",
        ),
    ] {
        if !present {
            issues.push(issue(label, 1, category, None));
        }
    }
    match metadata.map(|value| value.expected_outcome) {
        Some(ClaudeExpectedOutcome::AuthenticationFailure) => {
            for (present, category) in [
                (
                    evidence.saw_auth_failure_assistant,
                    "Claude authentication-failure fixture is missing authentication_failed evidence",
                ),
                (
                    evidence.saw_terminal_auth_failure,
                    "Claude authentication-failure fixture is missing the terminal API-error result",
                ),
            ] {
                if !present {
                    issues.push(issue(label, 1, category, None));
                }
            }
            if evidence.saw_terminal_success {
                issues.push(issue(
                    label,
                    1,
                    "Claude authentication-failure fixture contains a successful result",
                    Some("/payload/event/is_error"),
                ));
            }
        }
        Some(ClaudeExpectedOutcome::SimpleTurnSuccess) => {
            for (present, category) in [
                (
                    evidence.saw_success_assistant,
                    "Claude acceptance fixture is missing a non-error assistant message",
                ),
                (
                    evidence.saw_terminal_success,
                    "Claude acceptance fixture is missing the terminal success result",
                ),
            ] {
                if !present {
                    issues.push(issue(label, 1, category, None));
                }
            }
            if evidence.saw_auth_failure_assistant || evidence.saw_terminal_auth_failure {
                issues.push(issue(
                    label,
                    1,
                    "Claude acceptance fixture contains authentication-failure evidence",
                    Some("/payload/event"),
                ));
            }
        }
        None => {}
    }
    Ok(evidence)
}

#[allow(clippy::too_many_arguments)]
fn collect_claude_sidecar_evidence(
    kind: &str,
    payload: &Map<String, Value>,
    expected_sdk_version: Option<&str>,
    label: &str,
    line: usize,
    session_ids: &mut BTreeSet<String>,
    evidence: &mut ClaudeFixtureEvidence,
    issues: &mut Vec<VerifyIssue>,
) {
    match kind {
        "ready" => {
            evidence.saw_ready = true;
            let actual = payload.get("sdk_version").and_then(Value::as_str);
            if actual.is_none() || actual != expected_sdk_version {
                issues.push(issue(
                    label,
                    line,
                    "Claude fixture SDK version does not match its metadata",
                    Some("/payload/sdk_version"),
                ));
            }
        }
        "prompt_accepted" => evidence.saw_prompt_accepted = true,
        "session_started" => {
            evidence.saw_session_started = true;
            collect_claude_session_id(payload, session_ids);
        }
        "sdk_event" => {
            collect_claude_session_id(payload, session_ids);
            let Some(event) = payload.get("event").and_then(Value::as_object) else {
                issues.push(issue(
                    label,
                    line,
                    "Claude sdk_event payload must contain a closed event object",
                    Some("/payload/event"),
                ));
                return;
            };
            match event.get("event").and_then(Value::as_str) {
                Some("init") => evidence.saw_sdk_init = true,
                Some("assistant")
                    if event.get("error").and_then(Value::as_str)
                        == Some("authentication_failed") =>
                {
                    evidence.saw_auth_failure_assistant = true;
                }
                Some("assistant")
                    if event
                        .get("blocks")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| {
                            blocks.iter().any(|block| {
                                block.get("kind").and_then(Value::as_str) == Some("text")
                                    && block
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .is_some_and(|text| !text.is_empty())
                            })
                        }) =>
                {
                    evidence.saw_success_assistant = true;
                }
                Some("result") => {
                    let is_error = event.get("is_error").and_then(Value::as_bool);
                    match is_error {
                        Some(false)
                            if event.get("subtype").and_then(Value::as_str) == Some("success") =>
                        {
                            evidence.saw_terminal_success = true;
                        }
                        Some(true) => evidence.saw_terminal_auth_failure = true,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn collect_claude_session_id(object: &Map<String, Value>, session_ids: &mut BTreeSet<String>) {
    if let Some(session_id) = object.get("session_id").and_then(Value::as_str) {
        session_ids.insert(session_id.to_owned());
    }
}

fn claude_fixture_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if relative.is_empty() {
                "crates/kaleido-adapter-claude/tests/fixtures".to_owned()
            } else {
                format!("crates/kaleido-adapter-claude/tests/fixtures/{relative}")
            }
        })
        .unwrap_or_else(|_| "crates/kaleido-adapter-claude/tests/fixtures/<external>".to_owned())
}

fn discover_fixture_files(
    root: &Path,
    issues: &mut Vec<VerifyIssue>,
) -> Result<Vec<DiscoveredFile>, FixtureVerifyError> {
    let mut files = Vec::new();
    reject_unclassified_fixture_jsonl(root, issues)?;
    for (directory, agent) in FIXTURE_DIRECTORIES {
        let agent_root = root.join(directory);
        let label = fixture_label(root, &agent_root);
        let file_type = match fs::symlink_metadata(&agent_root) {
            Ok(metadata) => metadata.file_type(),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(FixtureVerifyError::Read {
                    label: label.clone(),
                    source,
                });
            }
        };
        let indirect_directory = file_type.is_dir()
            && directory_entry_resolves_elsewhere(root, root, &agent_root, &label)?;
        if file_type.is_symlink() || indirect_directory {
            issues.push(issue(&label, 1, "fixture symlink is not allowed", None));
            continue;
        }
        visit_fixture_directory(root, &agent_root, agent, &mut files, issues)?;
    }
    files.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(files)
}

fn reject_unclassified_fixture_jsonl(
    root: &Path,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let label = fixture_label(root, root);
    let mut entries = fs::read_dir(root)
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let name = entry.file_name();
        if name == "sandbox"
            || FIXTURE_DIRECTORIES
                .iter()
                .any(|(directory, _)| name == OsStr::new(directory))
        {
            continue;
        }
        visit_unclassified_entry(root, root, &entry.path(), issues)?;
    }
    Ok(())
}

fn visit_unclassified_entry(
    root: &Path,
    parent: &Path,
    entry: &Path,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let label = fixture_label(root, entry);
    let file_type = fs::symlink_metadata(entry)
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?
        .file_type();
    let is_jsonl = entry
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
    if file_type.is_symlink() {
        if is_jsonl {
            issues.push(issue(&label, 1, "fixture symlink is not allowed", None));
        }
        return Ok(());
    }
    if file_type.is_file() {
        if is_jsonl {
            issues.push(issue(
                &label,
                1,
                "fixture JSONL is outside a recognized agent directory",
                None,
            ));
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }
    if directory_entry_resolves_elsewhere(root, parent, entry, &label)? {
        issues.push(issue(&label, 1, "fixture symlink is not allowed", None));
        return Ok(());
    }
    let mut entries = fs::read_dir(entry)
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for child in entries {
        visit_unclassified_entry(root, entry, &child.path(), issues)?;
    }
    Ok(())
}

fn visit_fixture_directory(
    root: &Path,
    directory: &Path,
    agent: AgentKind,
    files: &mut Vec<DiscoveredFile>,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let label = fixture_label(root, directory);
    let mut entries = fs::read_dir(directory)
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let entry_label = fixture_label(root, &entry.path());
        let file_type = entry
            .file_type()
            .map_err(|source| FixtureVerifyError::Read {
                label: entry_label.clone(),
                source,
            })?;
        let indirect_directory = file_type.is_dir()
            && directory_entry_resolves_elsewhere(root, directory, &entry.path(), &entry_label)?;
        if file_type.is_symlink() || indirect_directory {
            issues.push(issue(
                &entry_label,
                1,
                "fixture symlink is not allowed",
                None,
            ));
            continue;
        }
        if file_type.is_dir() {
            visit_fixture_directory(root, &entry.path(), agent, files, issues)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            files.push(DiscoveredFile {
                agent,
                path: entry.path(),
                label: entry_label,
            });
        }
    }
    Ok(())
}

fn directory_entry_resolves_elsewhere(
    root: &Path,
    directory: &Path,
    entry: &Path,
    label: &str,
) -> Result<bool, FixtureVerifyError> {
    let canonical_directory =
        fs::canonicalize(directory).map_err(|source| FixtureVerifyError::Read {
            label: fixture_label(root, directory),
            source,
        })?;
    let canonical_entry = fs::canonicalize(entry).map_err(|source| FixtureVerifyError::Read {
        label: label.to_owned(),
        source,
    })?;
    let Some(file_name) = entry.file_name() else {
        return Ok(true);
    };
    Ok(canonical_entry != canonical_directory.join(file_name))
}

fn parse_fixture_file(
    file: &DiscoveredFile,
    sandbox: &Path,
    identity: &Identity,
    issues: &mut Vec<VerifyIssue>,
) -> Result<ParsedFile, FixtureVerifyError> {
    let contents = fs::read_to_string(&file.path).map_err(|source| FixtureVerifyError::Read {
        label: file.label.clone(),
        source,
    })?;
    let mut records = Vec::new();
    let mut previous_timestamp = None;

    if contents.is_empty() {
        issues.push(issue(&file.label, 1, "empty fixture file", None));
    }

    for (zero_based_line, raw) in contents.lines().enumerate() {
        let line = zero_based_line + 1;
        if raw.trim().is_empty() {
            issues.push(issue(&file.label, line, "blank JSONL line", None));
            continue;
        }

        let mut unparsed_leaks = Vec::new();
        scan_unparsed_line(
            raw,
            sandbox,
            identity,
            &file.label,
            line,
            &mut unparsed_leaks,
        );
        let value = match serde_json::from_str::<Value>(raw) {
            Ok(value) => value,
            Err(_) => {
                issues.extend(unparsed_leaks);
                issues.push(issue(&file.label, line, "invalid JSON", None));
                continue;
            }
        };
        let http_record = file.agent == AgentKind::OpenCode
            && value.get("transport").and_then(Value::as_str) == Some("http");
        let semantic_issue_start = issues.len();
        scan_value_for_leaks(
            &value,
            "",
            http_record,
            sandbox,
            identity,
            &file.label,
            line,
            issues,
        );
        merge_raw_leak_findings(unparsed_leaks, semantic_issue_start, http_record, issues);

        let Some(object) = value.as_object() else {
            issues.push(issue(
                &file.label,
                line,
                "fixture line must be a JSON object",
                None,
            ));
            continue;
        };
        let actual_fields: BTreeSet<&str> = object.keys().map(String::as_str).collect();
        let expected_fields: BTreeSet<&str> = OUTER_FIELDS.into_iter().collect();
        if actual_fields != expected_fields {
            issues.push(issue(
                &file.label,
                line,
                "outer object must contain exactly ts_ms, dir, transport, payload",
                None,
            ));
            continue;
        }

        let Some(timestamp) = object.get("ts_ms").and_then(Value::as_u64) else {
            issues.push(issue(
                &file.label,
                line,
                "ts_ms must be a non-negative integer",
                Some("/ts_ms"),
            ));
            continue;
        };
        if previous_timestamp.is_some_and(|previous| timestamp < previous) {
            issues.push(issue(
                &file.label,
                line,
                "ts_ms must be non-decreasing within a fixture",
                Some("/ts_ms"),
            ));
        }
        previous_timestamp = Some(timestamp);

        let direction = match object.get("dir").and_then(Value::as_str) {
            Some("c2s") => Direction::C2s,
            Some("s2c") => Direction::S2c,
            _ => {
                issues.push(issue(
                    &file.label,
                    line,
                    "dir must be c2s or s2c",
                    Some("/dir"),
                ));
                continue;
            }
        };
        let transport = match object.get("transport").and_then(Value::as_str) {
            Some("stdio") => Transport::Stdio,
            Some("http") => Transport::Http,
            Some("sse") => Transport::Sse,
            _ => {
                issues.push(issue(
                    &file.label,
                    line,
                    "transport must be stdio, http, or sse",
                    Some("/transport"),
                ));
                continue;
            }
        };
        if !transport_is_valid(file.agent, direction, transport) {
            issues.push(issue(
                &file.label,
                line,
                "transport/direction is invalid for this agent",
                Some("/transport"),
            ));
            continue;
        }
        let Some(payload) = object.get("payload") else {
            continue;
        };
        if !payload.is_object() {
            issues.push(issue(
                &file.label,
                line,
                "payload must be a JSON object",
                Some("/payload"),
            ));
            continue;
        }
        records.push(ParsedRecord {
            line,
            direction,
            transport,
            payload: payload.clone(),
        });
    }

    Ok(ParsedFile {
        agent: file.agent,
        label: file.label.clone(),
        records,
    })
}

fn transport_is_valid(agent: AgentKind, direction: Direction, transport: Transport) -> bool {
    match agent {
        AgentKind::Codex | AgentKind::Acp => transport == Transport::Stdio,
        AgentKind::OpenCode => match transport {
            Transport::Http => true,
            Transport::Sse => direction == Direction::S2c,
            Transport::Stdio => false,
        },
    }
}

fn scan_unparsed_line(
    raw: &str,
    sandbox: &Path,
    identity: &Identity,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) {
    if identity
        .home_variants
        .iter()
        .any(|home| contains_ascii_case_insensitive(raw, home))
    {
        issues.push(issue(file, line, "leak: home directory", None));
    }
    for (needle, category) in [
        ("sk-", "leak: secret prefix sk-"),
        ("ghp_", "leak: secret prefix ghp_"),
    ] {
        if contains_sensitive_prefix(raw, needle) {
            issues.push(issue(file, line, category, None));
        }
    }
    if contains_unredacted_bearer(raw) {
        issues.push(issue(file, line, "leak: bearer credential", None));
    }
    if contains_generic_home_path(raw) {
        issues.push(issue(file, line, "leak: home directory", None));
    }
    if contains_outside_absolute_path(raw, sandbox) {
        issues.push(issue(
            file,
            line,
            "leak: absolute path outside fixture sandbox",
            None,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_value_for_leaks(
    value: &Value,
    pointer: &str,
    http_record: bool,
    sandbox: &Path,
    identity: &Identity,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) {
    match value {
        Value::Object(map) => {
            let is_http_envelope = http_record
                && pointer == "/payload"
                && map.contains_key("method")
                && map.contains_key("path")
                && map.contains_key("content_type")
                && map.contains_key("body");
            for (key, child) in map {
                let child_pointer = pointer_child(pointer, key);
                scan_key_for_leaks(key, &child_pointer, sandbox, identity, file, line, issues);
                let lower_key = key.to_ascii_lowercase();
                if matches!(lower_key.as_str(), "api_key" | "authorization")
                    && !is_redacted_sensitive_value(child)
                {
                    issues.push(issue(
                        file,
                        line,
                        "leak: unredacted sensitive field",
                        Some(&child_pointer),
                    ));
                }
                if is_http_envelope && key == "path" {
                    if let Some(path) = child.as_str() {
                        scan_http_path_for_leaks(
                            path,
                            &child_pointer,
                            sandbox,
                            identity,
                            file,
                            line,
                            issues,
                        );
                    }
                } else {
                    scan_value_for_leaks(
                        child,
                        &child_pointer,
                        http_record,
                        sandbox,
                        identity,
                        file,
                        line,
                        issues,
                    );
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                scan_value_for_leaks(
                    child,
                    &pointer_child(pointer, &index.to_string()),
                    http_record,
                    sandbox,
                    identity,
                    file,
                    line,
                    issues,
                );
            }
        }
        Value::String(text) => {
            scan_text_for_leaks(text, pointer, sandbox, identity, file, line, issues);
        }
        _ => {}
    }
}

fn merge_raw_leak_findings(
    raw_findings: Vec<VerifyIssue>,
    semantic_issue_start: usize,
    http_record: bool,
    issues: &mut Vec<VerifyIssue>,
) {
    for raw_finding in raw_findings {
        let represented_semantically =
            issues
                .get(semantic_issue_start..)
                .is_some_and(|semantic_issues| {
                    semantic_issues.iter().any(|semantic| {
                        semantic.file == raw_finding.file
                            && semantic.line == raw_finding.line
                            && semantic.category == raw_finding.category
                    })
                });
        if represented_semantically {
            continue;
        }
        // The raw JSON scanner cannot distinguish an HTTP route such as
        // `/global/health` from a filesystem path. Parsed HTTP records receive
        // the path-aware semantic scan above, so only that one raw category is
        // suppressed for them. All credential and identity findings remain.
        if http_record && raw_finding.category == "leak: absolute path outside fixture sandbox" {
            continue;
        }
        issues.push(raw_finding);
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_text_for_leaks(
    text: &str,
    pointer: &str,
    sandbox: &Path,
    identity: &Identity,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) {
    scan_text_for_sensitive_markers(text, pointer, identity, file, line, issues);
    if contains_outside_absolute_path(text, sandbox) {
        issues.push(issue(
            file,
            line,
            "leak: absolute path outside fixture sandbox",
            Some(pointer),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_key_for_leaks(
    text: &str,
    pointer: &str,
    sandbox: &Path,
    identity: &Identity,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) {
    scan_text_for_sensitive_markers_impl(text, pointer, identity, file, line, issues, false);
    if contains_outside_absolute_path(text, sandbox) {
        issues.push(issue(
            file,
            line,
            "leak: absolute path outside fixture sandbox",
            Some(pointer),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_http_path_for_leaks(
    path: &str,
    pointer: &str,
    sandbox: &Path,
    identity: &Identity,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) {
    // An HTTP route starts with `/`, so applying the generic absolute-path
    // detector to the whole envelope field would reject every valid route.
    // Sensitive markers are still checked across the complete raw path, while
    // decoded route segments, query names, and query values receive the full
    // path-aware scan.
    scan_text_for_sensitive_markers(path, pointer, identity, file, line, issues);
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    for encoded in route.split('/').filter(|segment| !segment.is_empty()) {
        if let Ok(decoded) = percent_decode_component(encoded, false) {
            scan_text_for_leaks(&decoded, pointer, sandbox, identity, file, line, issues);
        }
    }
    if !query.is_empty() {
        for component in query.split('&') {
            let (name, value) = component.split_once('=').unwrap_or((component, ""));
            for encoded in [name, value] {
                if let Ok(decoded) = percent_decode_component(encoded, true) {
                    scan_text_for_leaks(&decoded, pointer, sandbox, identity, file, line, issues);
                }
            }
        }
    }
}

fn scan_text_for_sensitive_markers(
    text: &str,
    pointer: &str,
    identity: &Identity,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) {
    scan_text_for_sensitive_markers_impl(text, pointer, identity, file, line, issues, true);
}

#[allow(clippy::too_many_arguments)]
fn scan_text_for_sensitive_markers_impl(
    text: &str,
    pointer: &str,
    identity: &Identity,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
    include_username: bool,
) {
    if include_username
        && identity
            .username
            .as_ref()
            .is_some_and(|username| contains_ascii_case_insensitive(text, username))
    {
        issues.push(issue(file, line, "leak: current username", Some(pointer)));
    }
    if identity
        .home_variants
        .iter()
        .any(|home| contains_ascii_case_insensitive(text, home))
        || contains_generic_home_path(text)
    {
        issues.push(issue(file, line, "leak: home directory", Some(pointer)));
    }
    if contains_sensitive_prefix(text, "sk-") {
        issues.push(issue(file, line, "leak: secret prefix sk-", Some(pointer)));
    }
    if contains_sensitive_prefix(text, "ghp_") {
        issues.push(issue(file, line, "leak: secret prefix ghp_", Some(pointer)));
    }
    if contains_unredacted_bearer(text) {
        issues.push(issue(file, line, "leak: bearer credential", Some(pointer)));
    }
}

fn is_redacted_sensitive_value(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text == REDACTED_TOKEN || text == "Bearer <REDACTED_TOKEN>")
}

fn contains_unredacted_bearer(text: &str) -> bool {
    contains_sensitive_prefix(
        &text
            .to_ascii_lowercase()
            .replace("bearer <redacted_token>", ""),
        "bearer ",
    )
}

fn contains_sensitive_prefix(text: &str, prefix: &str) -> bool {
    let lower_text = text.to_ascii_lowercase();
    let lower_prefix = prefix.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative) = lower_text[cursor..].find(&lower_prefix) {
        let start = cursor + relative;
        let embedded_in_word = text[..start].chars().next_back().is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
        });
        if !embedded_in_word {
            return true;
        }
        cursor = start + prefix.len();
    }
    false
}

fn contains_generic_home_path(text: &str) -> bool {
    absolute_path_candidates(text).into_iter().any(|candidate| {
        let mut normalized = candidate.replace('\\', "/").to_ascii_lowercase();
        while normalized.contains("//") {
            normalized = normalized.replace("//", "/");
        }
        normalized.starts_with("c:/users/")
            || normalized.starts_with("/home/")
            || normalized.starts_with("/users/")
            || normalized.starts_with("/root/")
    })
}

fn contains_outside_absolute_path(text: &str, sandbox: &Path) -> bool {
    sandbox_placeholder_escapes(text)
        || absolute_path_candidates(text)
            .into_iter()
            .any(|candidate| !path_is_inside_sandbox(&candidate, sandbox))
}

fn absolute_path_candidates(text: &str) -> Vec<String> {
    let mut candidates = BTreeSet::new();
    let trimmed = text.trim();
    if is_windows_drive_absolute(trimmed)
        || is_unc_absolute(trimmed)
        || is_windows_root_absolute(trimmed)
        || (!trimmed.chars().any(char::is_whitespace) && is_unix_absolute_with_segments(trimmed, 1))
    {
        candidates.insert(trimmed.to_owned());
    }

    let mut recognized_path_end = 0_usize;
    for (index, character) in text.char_indices() {
        if !path_start_boundary(text, index) {
            continue;
        }
        let Some(candidate) = text.get(index..).map(absolute_path_token) else {
            continue;
        };
        let windows = character.is_ascii_alphabetic() && is_windows_drive_absolute(candidate);
        let unc = character == '\\' && is_unc_absolute(candidate);
        let windows_root = character == '\\'
            && is_windows_root_absolute(candidate)
            && !is_json_escape_at(text, index)
            && index >= recognized_path_end;
        let unix = character == '/'
            && (is_unix_absolute_with_segments(candidate, 2)
                || (is_unix_absolute_with_segments(candidate, 1)
                    && single_segment_unix_path_context(text, index, candidate)));
        if windows || unc || windows_root || unix {
            candidates.insert(candidate.to_owned());
            recognized_path_end = recognized_path_end.max(index.saturating_add(candidate.len()));
        }
    }
    candidates.into_iter().collect()
}

fn single_segment_unix_path_context(text: &str, start: usize, candidate: &str) -> bool {
    if candidate
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
        != 1
    {
        return false;
    }
    if start == 0 {
        return candidate.len() == text.len();
    }
    let prefix = text[..start].trim_end();
    let command = prefix
        .rsplit(|character: char| {
            character.is_whitespace() || matches!(character, '(' | '[' | '{' | '"' | '\'')
        })
        .next()
        .unwrap_or_default()
        .trim_end_matches([':', '='])
        .to_ascii_lowercase();
    matches!(
        command.as_str(),
        "cat"
            | "type"
            | "read"
            | "open"
            | "head"
            | "tail"
            | "less"
            | "more"
            | "rm"
            | "cp"
            | "mv"
            | "stat"
            | "ls"
            | "dir"
            | "get-content"
    )
}

fn absolute_path_token(tail: &str) -> &str {
    let token = tail
        .split(|character: char| {
            character.is_whitespace()
                || matches!(character, '"' | '\'' | ';' | ',' | ')' | ']' | '}' | '`')
        })
        .next()
        .unwrap_or(tail);
    if token.ends_with("/..") || token.ends_with(r"\..") {
        token
    } else {
        token.trim_end_matches('.')
    }
}

fn is_windows_drive_absolute(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && matches!(bytes.get(2), Some(b'/' | b'\\'))
}

fn is_unc_absolute(candidate: &str) -> bool {
    candidate.starts_with(r"\\")
}

fn is_windows_root_absolute(candidate: &str) -> bool {
    candidate.starts_with('\\') && !is_unc_absolute(candidate)
}

fn is_json_escape_at(text: &str, index: usize) -> bool {
    let Some(next) = text
        .get(index.saturating_add(1)..)
        .and_then(|tail| tail.chars().next())
    else {
        return false;
    };
    if !matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') {
        return false;
    }
    let Some(prefix) = text.get(..index) else {
        return false;
    };
    let mut in_string = false;
    let mut escaped = false;
    for character in prefix.chars() {
        if escaped {
            escaped = false;
        } else if character == '\\' && in_string {
            escaped = true;
        } else if character == '"' {
            in_string = !in_string;
        }
    }
    in_string
}

fn is_unix_absolute_with_segments(candidate: &str, minimum_segments: usize) -> bool {
    candidate.starts_with('/')
        && !candidate.starts_with("//")
        && candidate
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
            >= minimum_segments
}

fn path_start_boundary(text: &str, index: usize) -> bool {
    let Some(prefix) = text.get(..index) else {
        return false;
    };
    if ["<sandbox>", "<home>", "<outside_path>"]
        .iter()
        .any(|placeholder| {
            prefix
                .get(prefix.len().saturating_sub(placeholder.len())..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(placeholder))
        })
    {
        return false;
    }
    let prefix_bytes = prefix.as_bytes();
    if prefix_bytes.last() == Some(&b':')
        && prefix_bytes
            .get(prefix_bytes.len().saturating_sub(2))
            .is_some_and(u8::is_ascii_alphabetic)
    {
        let drive_start = prefix_bytes.len().saturating_sub(2);
        let drive_has_boundary = prefix
            .get(..drive_start)
            .and_then(|before_drive| before_drive.chars().next_back())
            .is_none_or(|previous| {
                !previous.is_alphanumeric() && !matches!(previous, '_' | '/' | '\\')
            });
        if drive_has_boundary {
            return false;
        }
    }
    prefix
        .chars()
        .next_back()
        .is_none_or(|previous| !previous.is_alphanumeric() && !matches!(previous, '_' | '/' | '\\'))
}

fn path_is_inside_sandbox(candidate: &str, sandbox: &Path) -> bool {
    let Some(candidate) = normalize_absolute_path(candidate) else {
        return false;
    };
    let Some(sandbox) = normalize_absolute_path(&sandbox.to_string_lossy()) else {
        return false;
    };
    candidate == sandbox
        || candidate
            .strip_prefix(&sandbox)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn normalize_absolute_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let windows_semantics = normalized.starts_with("//")
        || (normalized.as_bytes().get(1) == Some(&b':')
            && normalized.as_bytes().get(2) == Some(&b'/'));
    let normalized = if windows_semantics {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    };
    let (root, tail, protected_components) = if normalized.starts_with("//") {
        ("//", normalized.trim_start_matches('/'), 2_usize)
    } else if normalized.starts_with('/') {
        ("/", normalized.trim_start_matches('/'), 0_usize)
    } else if normalized.as_bytes().get(1) == Some(&b':')
        && normalized.as_bytes().get(2) == Some(&b'/')
    {
        (normalized.get(..2)?, normalized.get(3..)?, 0_usize)
    } else {
        return None;
    };

    let mut components: Vec<&str> = Vec::new();
    for component in tail.split('/') {
        match component {
            "" | "." => {}
            ".." if components.len() > protected_components => {
                components.pop();
            }
            ".." => return None,
            _ => components.push(component),
        }
    }

    let joined = components.join("/");
    match root {
        "/" => Some(format!("/{joined}")),
        "//" => Some(format!("//{joined}")),
        drive => {
            if joined.is_empty() {
                Some(format!("{drive}/"))
            } else {
                Some(format!("{drive}/{joined}"))
            }
        }
    }
}

fn sandbox_placeholder_escapes(text: &str) -> bool {
    let normalized = text.replace('\\', "/").to_ascii_lowercase();
    let marker = "<sandbox>";
    let mut cursor = 0_usize;
    while let Some(relative) = normalized[cursor..].find(marker) {
        let start = cursor + relative;
        let after_marker = start + marker.len();
        if normalized
            .as_bytes()
            .get(after_marker)
            .is_some_and(|byte| *byte == b'/')
            && normalized
                .get(after_marker..)
                .is_some_and(relative_path_escapes_root)
        {
            return true;
        }
        cursor = after_marker;
    }
    false
}

fn relative_path_escapes_root(suffix: &str) -> bool {
    let candidate = absolute_path_token(suffix);
    let mut depth = 0_usize;
    for component in candidate.split('/') {
        match component {
            "" | "." => {}
            ".." if depth == 0 => return true,
            ".." => depth -= 1,
            _ => depth = depth.saturating_add(1),
        }
    }
    false
}

fn path_variants(path: &Path) -> Vec<String> {
    let native = path.to_string_lossy().into_owned();
    let backward = native.replace('/', "\\");
    vec![
        native.clone(),
        native.replace('\\', "/"),
        backward.clone(),
        backward.replace('\\', "\\\\"),
    ]
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        false
    } else {
        haystack
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    }
}

#[derive(Debug)]
struct SchemaCatalogs {
    codex: CodexCatalog,
    acp: AcpCatalog,
    opencode: OpenCodeCatalog,
}

impl SchemaCatalogs {
    fn load(root: &Path) -> Result<Self, FixtureVerifyError> {
        Ok(Self {
            codex: CodexCatalog::load(&root.join("codex"))?,
            acp: AcpCatalog::load(&root.join("acp").join("schema.json"))?,
            opencode: OpenCodeCatalog::load(&root.join("opencode").join("openapi.json"))?,
        })
    }
}

#[derive(Debug)]
struct CodexCatalog {
    client_requests: BTreeMap<String, Value>,
    server_requests: BTreeMap<String, Value>,
    client_notifications: BTreeMap<String, Value>,
    server_notifications: BTreeMap<String, Value>,
    responses: BTreeMap<(DirectionKey, String), Value>,
    success_wrapper: Value,
    error_wrapper: Value,
}

impl CodexCatalog {
    fn load(root: &Path) -> Result<Self, FixtureVerifyError> {
        let client_requests = load_codex_branches(
            &root.join("ClientRequest.json"),
            "schemas/codex/ClientRequest.json",
        )?;
        let server_requests = load_codex_branches(
            &root.join("ServerRequest.json"),
            "schemas/codex/ServerRequest.json",
        )?;
        let client_notifications = load_codex_branches(
            &root.join("ClientNotification.json"),
            "schemas/codex/ClientNotification.json",
        )?;
        let server_notifications = load_codex_branches(
            &root.join("ServerNotification.json"),
            "schemas/codex/ServerNotification.json",
        )?;
        let response_files = load_codex_response_files(root)?;
        let mut responses = BTreeMap::new();
        for (direction, methods) in [
            (DirectionKey::C2s, &client_requests),
            (DirectionKey::S2c, &server_requests),
        ] {
            for (method, schema) in methods {
                let response_name = codex_response_name(method, schema).ok_or_else(|| {
                    FixtureVerifyError::InvalidSnapshot {
                        label: "schemas/codex response mapping".to_owned(),
                    }
                })?;
                let response = response_files.get(&response_name).ok_or_else(|| {
                    FixtureVerifyError::InvalidSnapshot {
                        label: format!("schemas/codex/{response_name}.json"),
                    }
                })?;
                responses.insert((direction, method.clone()), response.clone());
            }
        }
        Ok(Self {
            client_requests,
            server_requests,
            client_notifications,
            server_notifications,
            responses,
            success_wrapper: read_json(
                &root.join("JSONRPCResponse.json"),
                "schemas/codex/JSONRPCResponse.json",
            )?,
            error_wrapper: read_json(
                &root.join("JSONRPCError.json"),
                "schemas/codex/JSONRPCError.json",
            )?,
        })
    }
}

fn load_codex_branches(
    path: &Path,
    label: &str,
) -> Result<BTreeMap<String, Value>, FixtureVerifyError> {
    let aggregate = read_json(path, label)?;
    let branches = aggregate
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| FixtureVerifyError::InvalidSnapshot {
            label: label.to_owned(),
        })?;
    let mut methods = BTreeMap::new();
    for branch in branches {
        let method = branch
            .pointer("/properties/method/enum/0")
            .and_then(Value::as_str)
            .ok_or_else(|| FixtureVerifyError::InvalidSnapshot {
                label: label.to_owned(),
            })?;
        let mut schema = Map::new();
        if let Some(draft) = aggregate.get("$schema") {
            schema.insert("$schema".to_owned(), draft.clone());
        }
        schema.insert("allOf".to_owned(), Value::Array(vec![branch.clone()]));
        if let Some(definitions) = aggregate.get("definitions") {
            schema.insert("definitions".to_owned(), definitions.clone());
        }
        if methods
            .insert(method.to_owned(), Value::Object(schema))
            .is_some()
        {
            return Err(FixtureVerifyError::InvalidSnapshot {
                label: label.to_owned(),
            });
        }
    }
    Ok(methods)
}

fn load_codex_response_files(root: &Path) -> Result<BTreeMap<String, Value>, FixtureVerifyError> {
    let mut files = BTreeMap::new();
    visit_codex_response_files(root, root, &mut files)?;
    Ok(files)
}

fn visit_codex_response_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, Value>,
) -> Result<(), FixtureVerifyError> {
    let label = schema_label(root, directory, "schemas/codex");
    let mut entries = fs::read_dir(directory)
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| FixtureVerifyError::Read {
            label: label.clone(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry
            .file_type()
            .map_err(|source| FixtureVerifyError::Read {
                label: schema_label(root, &entry.path(), "schemas/codex"),
                source,
            })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            visit_codex_response_files(root, &entry.path(), files)?;
        } else if file_type.is_file()
            && entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("Response.json"))
        {
            let stem = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| FixtureVerifyError::InvalidSnapshot {
                    label: schema_label(root, &entry.path(), "schemas/codex"),
                })?
                .to_owned();
            let label = schema_label(root, &entry.path(), "schemas/codex");
            let schema = read_json(&entry.path(), &label)?;
            if files.insert(stem, schema).is_some() {
                return Err(FixtureVerifyError::InvalidSnapshot { label });
            }
        }
    }
    Ok(())
}

fn codex_response_name(method: &str, request_schema: &Value) -> Option<String> {
    if let Some(exception) = codex_response_exception(method) {
        return Some(exception.to_owned());
    }
    let reference = request_schema
        .pointer("/allOf/0/properties/params/$ref")
        .and_then(Value::as_str)
        .or_else(|| {
            request_schema
                .pointer("/allOf/0/properties/params/allOf/0/$ref")
                .and_then(Value::as_str)
        })?;
    let params = reference.rsplit('/').next()?;
    params
        .strip_suffix("Params")
        .map(|prefix| format!("{prefix}Response"))
}

fn codex_response_exception(method: &str) -> Option<&'static str> {
    // These are the only response names in the pinned snapshot that cannot be
    // derived mechanically from the request parameter type.
    match method {
        "config/mcpServer/reload" => Some("McpServerRefreshResponse"),
        "windowsSandbox/readiness" => Some("WindowsSandboxReadinessResponse"),
        "account/logout" => Some("LogoutAccountResponse"),
        "account/rateLimits/read" => Some("GetAccountRateLimitsResponse"),
        "account/usage/read" => Some("GetAccountTokenUsageResponse"),
        "account/workspaceMessages/read" => Some("GetWorkspaceMessagesResponse"),
        "externalAgentConfig/import/readHistories" => {
            Some("ExternalAgentConfigImportHistoriesReadResponse")
        }
        "config/value/write" | "config/batchWrite" => Some("ConfigWriteResponse"),
        "configRequirements/read" => Some("ConfigRequirementsReadResponse"),
        _ => None,
    }
}

#[derive(Debug)]
struct AcpCatalog {
    root: Value,
    requests: BTreeMap<(DirectionKey, String), Value>,
    responses: BTreeMap<(DirectionKey, String), Value>,
    notifications: BTreeMap<(DirectionKey, String), Value>,
    agent_response: Value,
    client_response: Value,
    error: Value,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum DirectionKey {
    C2s,
    S2c,
}

impl From<Direction> for DirectionKey {
    fn from(value: Direction) -> Self {
        match value {
            Direction::C2s => Self::C2s,
            Direction::S2c => Self::S2c,
        }
    }
}

impl AcpCatalog {
    fn load(path: &Path) -> Result<Self, FixtureVerifyError> {
        let root = read_json(path, "schemas/acp/schema.json")?;
        let definitions = root
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| FixtureVerifyError::InvalidSnapshot {
                label: "schemas/acp/schema.json".to_owned(),
            })?;
        let mut requests = BTreeMap::new();
        let mut responses = BTreeMap::new();
        let mut notifications = BTreeMap::new();

        for (name, schema) in &definitions {
            let Some(method) = schema.get("x-method").and_then(Value::as_str) else {
                continue;
            };
            let Some(side) = schema.get("x-side").and_then(Value::as_str) else {
                continue;
            };
            let directions: &[DirectionKey] = match side {
                "agent" => &[DirectionKey::C2s],
                "client" => &[DirectionKey::S2c],
                "protocol" => &[DirectionKey::C2s, DirectionKey::S2c],
                _ => {
                    return Err(FixtureVerifyError::InvalidSnapshot {
                        label: "schemas/acp/schema.json".to_owned(),
                    });
                }
            };
            let target = if name.ends_with("Request") {
                &mut requests
            } else if name.ends_with("Response") {
                &mut responses
            } else if name.ends_with("Notification") {
                &mut notifications
            } else {
                continue;
            };
            for direction in directions {
                let exact = acp_definition_schema(&root, name);
                if target
                    .insert((*direction, method.to_owned()), exact)
                    .is_some()
                {
                    return Err(FixtureVerifyError::InvalidSnapshot {
                        label: "schemas/acp/schema.json".to_owned(),
                    });
                }
            }
        }

        Ok(Self {
            agent_response: acp_definition_schema(&root, "AgentResponse"),
            client_response: acp_definition_schema(&root, "ClientResponse"),
            error: acp_definition_schema(&root, "Error"),
            root,
            requests,
            responses,
            notifications,
        })
    }
}

fn acp_definition_schema(root: &Value, name: &str) -> Value {
    let mut schema = Map::new();
    if let Some(draft) = root.get("$schema") {
        schema.insert("$schema".to_owned(), draft.clone());
    }
    schema.insert("$ref".to_owned(), Value::String(format!("#/$defs/{name}")));
    if let Some(definitions) = root.get("$defs") {
        schema.insert("$defs".to_owned(), definitions.clone());
    }
    Value::Object(schema)
}

#[derive(Debug)]
struct OpenCodeCatalog {
    document: Value,
}

impl OpenCodeCatalog {
    fn load(path: &Path) -> Result<Self, FixtureVerifyError> {
        let document = read_json(path, "schemas/opencode/openapi.json")?;
        if document.get("openapi").and_then(Value::as_str) != Some("3.1.0") {
            return Err(FixtureVerifyError::InvalidSnapshot {
                label: "schemas/opencode/openapi.json".to_owned(),
            });
        }
        if document.pointer("/components/schemas/Event").is_none()
            || document
                .pointer("/components/schemas/GlobalEvent")
                .is_none()
            || document.get("paths").and_then(Value::as_object).is_none()
        {
            return Err(FixtureVerifyError::InvalidSnapshot {
                label: "schemas/opencode/openapi.json".to_owned(),
            });
        }
        Ok(Self { document })
    }
}

fn openapi_schema(document: &Value, reference_or_schema: &Value) -> Value {
    let target = if let Some(reference) = reference_or_schema.as_str() {
        let mut map = Map::new();
        map.insert("$ref".to_owned(), Value::String(reference.to_owned()));
        Value::Object(map)
    } else {
        reference_or_schema.clone()
    };
    let mut schema = Map::new();
    schema.insert(
        "$schema".to_owned(),
        Value::String("https://json-schema.org/draft/2020-12/schema".to_owned()),
    );
    schema.insert("allOf".to_owned(), Value::Array(vec![target]));
    if let Some(components) = document.get("components") {
        schema.insert("components".to_owned(), components.clone());
    }
    Value::Object(schema)
}

#[derive(Clone, Debug)]
struct PendingRequest {
    method: String,
    origin: Direction,
    line: usize,
}

type PendingRequests = HashMap<(DirectionKey, String), PendingRequest>;

fn verify_codex_file(
    file: &ParsedFile,
    catalog: &CodexCatalog,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let mut pending = PendingRequests::new();
    for record in &file.records {
        let object =
            record
                .payload
                .as_object()
                .ok_or_else(|| FixtureVerifyError::InvalidSnapshot {
                    label: "internal fixture payload".to_owned(),
                })?;
        if object.contains_key("method") {
            let is_request = object.contains_key("id");
            let method = match object.get("method").and_then(Value::as_str) {
                Some(method) => method,
                None => {
                    issues.push(issue(
                        &file.label,
                        record.line,
                        "method must be a string",
                        Some("/payload/method"),
                    ));
                    continue;
                }
            };
            let schemas = match (record.direction, is_request) {
                (Direction::C2s, true) => &catalog.client_requests,
                (Direction::S2c, true) => &catalog.server_requests,
                (Direction::C2s, false) => &catalog.client_notifications,
                (Direction::S2c, false) => &catalog.server_notifications,
            };
            let Some(schema) = schemas.get(method) else {
                issues.push(issue(
                    &file.label,
                    record.line,
                    "unknown Codex method",
                    Some("/payload/method"),
                ));
                continue;
            };
            validate_instance(
                schema,
                &record.payload,
                "/payload",
                "Codex method schema validation failed",
                &file.label,
                record.line,
                issues,
            )?;
            if is_request {
                track_request(
                    object,
                    method,
                    record.direction,
                    &file.label,
                    record.line,
                    &mut pending,
                    issues,
                );
            }
        } else {
            verify_codex_response(record, file, catalog, &mut pending, issues)?;
        }
    }
    report_unmatched_requests(
        file,
        &pending,
        "Codex request has no matching response",
        issues,
    );
    Ok(())
}

fn verify_codex_response(
    record: &ParsedRecord,
    file: &ParsedFile,
    catalog: &CodexCatalog,
    pending: &mut PendingRequests,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let Some(object) = record.payload.as_object() else {
        return Ok(());
    };
    let Some(id) = object.get("id").and_then(request_id_key) else {
        issues.push(issue(
            &file.label,
            record.line,
            "response id must be a string or integer",
            Some("/payload/id"),
        ));
        return Ok(());
    };
    let request_origin = opposite_direction(record.direction);
    let Some(request) = pending.remove(&(DirectionKey::from(request_origin), id)) else {
        issues.push(issue(
            &file.label,
            record.line,
            "unknown Codex response id",
            Some("/payload/id"),
        ));
        return Ok(());
    };
    if let Some(result) = object.get("result") {
        validate_instance(
            &catalog.success_wrapper,
            &record.payload,
            "/payload",
            "Codex response envelope validation failed",
            &file.label,
            record.line,
            issues,
        )?;
        let response_key = (DirectionKey::from(request.origin), request.method);
        let Some(schema) = catalog.responses.get(&response_key) else {
            return Err(FixtureVerifyError::InvalidSnapshot {
                label: "schemas/codex response mapping".to_owned(),
            });
        };
        validate_instance(
            schema,
            result,
            "/payload/result",
            "Codex response schema validation failed",
            &file.label,
            record.line,
            issues,
        )?;
    } else if object.contains_key("error") {
        validate_instance(
            &catalog.error_wrapper,
            &record.payload,
            "/payload",
            "Codex error response schema validation failed",
            &file.label,
            record.line,
            issues,
        )?;
    } else {
        issues.push(issue(
            &file.label,
            record.line,
            "response must contain result or error",
            Some("/payload"),
        ));
    }
    Ok(())
}

fn verify_acp_file(
    file: &ParsedFile,
    catalog: &AcpCatalog,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let mut pending = PendingRequests::new();
    for record in &file.records {
        validate_instance(
            &catalog.root,
            &record.payload,
            "/payload",
            "ACP root schema validation failed",
            &file.label,
            record.line,
            issues,
        )?;
        let Some(object) = record.payload.as_object() else {
            continue;
        };
        if object.contains_key("method") {
            let method = match object.get("method").and_then(Value::as_str) {
                Some(method) => method,
                None => {
                    issues.push(issue(
                        &file.label,
                        record.line,
                        "method must be a string",
                        Some("/payload/method"),
                    ));
                    continue;
                }
            };
            let direction = DirectionKey::from(record.direction);
            let is_request = object.contains_key("id");
            let schemas = if is_request {
                &catalog.requests
            } else {
                &catalog.notifications
            };
            let Some(schema) = schemas.get(&(direction, method.to_owned())) else {
                issues.push(issue(
                    &file.label,
                    record.line,
                    "unknown ACP method",
                    Some("/payload/method"),
                ));
                continue;
            };
            match object.get("params") {
                Some(params) if !params.is_null() => {
                    validate_instance(
                        schema,
                        params,
                        "/payload/params",
                        "ACP method schema validation failed",
                        &file.label,
                        record.line,
                        issues,
                    )?;
                }
                _ => issues.push(issue(
                    &file.label,
                    record.line,
                    "ACP method params must be present and non-null",
                    Some("/payload/params"),
                )),
            }
            if is_request {
                track_request(
                    object,
                    method,
                    record.direction,
                    &file.label,
                    record.line,
                    &mut pending,
                    issues,
                );
            }
        } else {
            verify_acp_response(record, file, catalog, &mut pending, issues)?;
        }
    }
    report_unmatched_requests(
        file,
        &pending,
        "ACP request has no matching response",
        issues,
    );
    Ok(())
}

fn verify_acp_response(
    record: &ParsedRecord,
    file: &ParsedFile,
    catalog: &AcpCatalog,
    pending: &mut PendingRequests,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let Some(object) = record.payload.as_object() else {
        return Ok(());
    };
    let Some(id) = object.get("id").and_then(request_id_key) else {
        issues.push(issue(
            &file.label,
            record.line,
            "response id must be a string or integer",
            Some("/payload/id"),
        ));
        return Ok(());
    };
    let request_origin = opposite_direction(record.direction);
    let Some(request) = pending.remove(&(DirectionKey::from(request_origin), id)) else {
        issues.push(issue(
            &file.label,
            record.line,
            "unknown ACP response id",
            Some("/payload/id"),
        ));
        return Ok(());
    };
    let response_wrapper = match request.origin {
        Direction::C2s => &catalog.agent_response,
        Direction::S2c => &catalog.client_response,
    };
    validate_instance(
        response_wrapper,
        &record.payload,
        "/payload",
        "ACP response envelope validation failed",
        &file.label,
        record.line,
        issues,
    )?;

    if let Some(result) = object.get("result") {
        let key = (DirectionKey::from(request.origin), request.method);
        let Some(schema) = catalog.responses.get(&key) else {
            return Err(FixtureVerifyError::InvalidSnapshot {
                label: "schemas/acp method response mapping".to_owned(),
            });
        };
        validate_instance(
            schema,
            result,
            "/payload/result",
            "ACP response schema validation failed",
            &file.label,
            record.line,
            issues,
        )?;
    } else if let Some(error) = object.get("error") {
        validate_instance(
            &catalog.error,
            error,
            "/payload/error",
            "ACP error response schema validation failed",
            &file.label,
            record.line,
            issues,
        )?;
    } else {
        issues.push(issue(
            &file.label,
            record.line,
            "response must contain result or error",
            Some("/payload"),
        ));
    }
    Ok(())
}

fn track_request(
    object: &Map<String, Value>,
    method: &str,
    direction: Direction,
    file: &str,
    line: usize,
    pending: &mut PendingRequests,
    issues: &mut Vec<VerifyIssue>,
) {
    let Some(id) = object.get("id").and_then(request_id_key) else {
        issues.push(issue(
            file,
            line,
            "request id must be a string or integer",
            Some("/payload/id"),
        ));
        return;
    };
    if pending
        .insert(
            (DirectionKey::from(direction), id),
            PendingRequest {
                method: method.to_owned(),
                origin: direction,
                line,
            },
        )
        .is_some()
    {
        issues.push(issue(
            file,
            line,
            "duplicate outstanding request id",
            Some("/payload/id"),
        ));
    }
}

fn report_unmatched_requests(
    file: &ParsedFile,
    pending: &PendingRequests,
    category: &str,
    issues: &mut Vec<VerifyIssue>,
) {
    for request in pending.values() {
        issues.push(issue(
            &file.label,
            request.line,
            category,
            Some("/payload/id"),
        ));
    }
}

fn opposite_direction(direction: Direction) -> Direction {
    match direction {
        Direction::C2s => Direction::S2c,
        Direction::S2c => Direction::C2s,
    }
}

fn request_id_key(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(format!("s:{text}")),
        Value::Number(number) if number.is_i64() || number.is_u64() => Some(format!("n:{number}")),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HttpRequestKey {
    method: String,
    path: String,
}

#[derive(Clone, Debug)]
struct PendingHttpRequest {
    key: HttpRequestKey,
    line: usize,
    event_schema: Option<Value>,
}

fn verify_opencode_file(
    file: &ParsedFile,
    catalog: &OpenCodeCatalog,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let mut pending: VecDeque<PendingHttpRequest> = VecDeque::new();
    let mut active_event_schema: Option<Value> = None;
    for record in &file.records {
        match record.transport {
            Transport::Sse => {
                if let Some(position) = pending
                    .iter()
                    .position(|request| request.event_schema.is_some())
                {
                    active_event_schema = pending
                        .remove(position)
                        .and_then(|request| request.event_schema);
                }
                if let Some(schema) = &active_event_schema {
                    validate_instance(
                        schema,
                        &record.payload,
                        "/payload",
                        "OpenCode SSE Event schema validation failed",
                        &file.label,
                        record.line,
                        issues,
                    )?;
                } else {
                    issues.push(issue(
                        &file.label,
                        record.line,
                        "OpenCode SSE event has no matching stream request",
                        Some("/payload"),
                    ));
                }
            }
            Transport::Http => {
                verify_opencode_http(record, file, catalog, &mut pending, issues)?;
            }
            Transport::Stdio => {}
        }
    }
    for request in pending {
        issues.push(issue(
            &file.label,
            request.line,
            "OpenCode HTTP request has no matching response",
            Some("/payload/path"),
        ));
    }
    Ok(())
}

fn verify_opencode_http(
    record: &ParsedRecord,
    file: &ParsedFile,
    catalog: &OpenCodeCatalog,
    pending: &mut VecDeque<PendingHttpRequest>,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let Some(envelope) = record.payload.as_object() else {
        return Ok(());
    };
    let expected: BTreeSet<&str> = ["body", "content_type", "method", "path", "status"]
        .into_iter()
        .collect();
    let actual: BTreeSet<&str> = envelope.keys().map(String::as_str).collect();
    if actual != expected {
        issues.push(issue(
            &file.label,
            record.line,
            "HTTP envelope must contain exactly method, path, status, content_type, body",
            Some("/payload"),
        ));
        return Ok(());
    }
    let Some(method) = envelope.get("method").and_then(Value::as_str) else {
        issues.push(issue(
            &file.label,
            record.line,
            "HTTP method must be a string",
            Some("/payload/method"),
        ));
        return Ok(());
    };
    let Some(path) = envelope.get("path").and_then(Value::as_str) else {
        issues.push(issue(
            &file.label,
            record.line,
            "HTTP path must be a string",
            Some("/payload/path"),
        ));
        return Ok(());
    };
    let Some(content_type) = envelope.get("content_type").and_then(Value::as_str) else {
        issues.push(issue(
            &file.label,
            record.line,
            "HTTP content_type must be a string",
            Some("/payload/content_type"),
        ));
        return Ok(());
    };
    let Some(body) = envelope.get("body") else {
        return Ok(());
    };
    let Some(operation) = find_openapi_operation(&catalog.document, method, path) else {
        issues.push(issue(
            &file.label,
            record.line,
            "unknown or ambiguous OpenCode HTTP operation",
            Some("/payload/path"),
        ));
        return Ok(());
    };
    validate_openapi_parameters(
        &catalog.document,
        &operation,
        path,
        &file.label,
        record.line,
        issues,
    )?;
    let key = HttpRequestKey {
        method: method.to_ascii_uppercase(),
        path: path.to_owned(),
    };

    match record.direction {
        Direction::C2s => {
            if !envelope.get("status").is_some_and(Value::is_null) {
                issues.push(issue(
                    &file.label,
                    record.line,
                    "HTTP request status must be null",
                    Some("/payload/status"),
                ));
            }
            validate_openapi_content(
                &catalog.document,
                operation.operation.get("requestBody"),
                content_type,
                body,
                "/payload/body",
                "OpenCode request body schema validation failed",
                &file.label,
                record.line,
                issues,
            )?;
            pending.push_back(PendingHttpRequest {
                key,
                line: record.line,
                event_schema: operation_event_stream_schema(
                    &catalog.document,
                    operation.operation,
                )?,
            });
        }
        Direction::S2c => {
            if let Some(position) = pending.iter().position(|request| request.key == key) {
                pending.remove(position);
            } else {
                issues.push(issue(
                    &file.label,
                    record.line,
                    "OpenCode HTTP response has no matching request",
                    Some("/payload/path"),
                ));
            }
            let Some(status) = envelope.get("status").and_then(Value::as_u64) else {
                issues.push(issue(
                    &file.label,
                    record.line,
                    "HTTP response status must be an integer",
                    Some("/payload/status"),
                ));
                return Ok(());
            };
            if !(100..=599).contains(&status) {
                issues.push(issue(
                    &file.label,
                    record.line,
                    "HTTP response status is out of range",
                    Some("/payload/status"),
                ));
                return Ok(());
            }
            let response = operation
                .operation
                .get("responses")
                .and_then(Value::as_object)
                .and_then(|responses| {
                    responses
                        .get(&status.to_string())
                        .or_else(|| responses.get("default"))
                });
            let Some(response) = response else {
                issues.push(issue(
                    &file.label,
                    record.line,
                    "HTTP status is not declared by the OpenAPI operation",
                    Some("/payload/status"),
                ));
                return Ok(());
            };
            validate_openapi_content(
                &catalog.document,
                Some(response),
                content_type,
                body,
                "/payload/body",
                "OpenCode response body schema validation failed",
                &file.label,
                record.line,
                issues,
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct OpenApiOperation<'a> {
    operation: &'a Value,
    path_item: &'a Value,
    template: &'a str,
}

fn find_openapi_operation<'a>(
    document: &'a Value,
    method: &str,
    requested_path: &str,
) -> Option<OpenApiOperation<'a>> {
    let method = method.to_ascii_lowercase();
    let path = requested_path.split('?').next()?;
    if !path.starts_with('/') {
        return None;
    }
    let paths = document.get("paths")?.as_object()?;
    let mut matches: Vec<(usize, OpenApiOperation<'a>)> = paths
        .iter()
        .filter_map(|(template, item)| {
            if openapi_path_matches(template, path) {
                let specificity = template
                    .split('/')
                    .filter(|segment| !segment.starts_with('{'))
                    .count();
                item.get(&method).map(|operation| {
                    (
                        specificity,
                        OpenApiOperation {
                            operation,
                            path_item: item,
                            template,
                        },
                    )
                })
            } else {
                None
            }
        })
        .collect();
    matches.sort_by(|left, right| right.0.cmp(&left.0));
    let best = matches.first()?;
    if matches.get(1).is_some_and(|second| second.0 == best.0) {
        None
    } else {
        Some(best.1)
    }
}

fn openapi_path_matches(template: &str, actual: &str) -> bool {
    let template_segments: Vec<_> = template.trim_matches('/').split('/').collect();
    let actual_segments: Vec<_> = actual.trim_matches('/').split('/').collect();
    template_segments.len() == actual_segments.len()
        && template_segments
            .iter()
            .zip(actual_segments.iter())
            .all(|(expected, actual)| {
                (expected.starts_with('{') && expected.ends_with('}') && !actual.is_empty())
                    || expected == actual
            })
}

fn operation_event_stream_schema(
    document: &Value,
    operation: &Value,
) -> Result<Option<Value>, FixtureVerifyError> {
    let mut schemas = Vec::new();
    if let Some(responses) = operation.get("responses").and_then(Value::as_object) {
        for response in responses.values() {
            let response = resolve_openapi_reference(document, response)?;
            let Some(content) = response.get("content").and_then(Value::as_object) else {
                continue;
            };
            for (media_type, media) in content {
                if !media_type.eq_ignore_ascii_case("text/event-stream") {
                    continue;
                }
                let schema =
                    media
                        .get("schema")
                        .ok_or_else(|| FixtureVerifyError::InvalidSnapshot {
                            label: "schemas/opencode/openapi.json event stream schema".to_owned(),
                        })?;
                let schema = openapi_schema(document, schema);
                if !schemas.contains(&schema) {
                    schemas.push(schema);
                }
            }
        }
    }
    if schemas.len() > 1 {
        return Err(FixtureVerifyError::InvalidSnapshot {
            label: "schemas/opencode/openapi.json ambiguous event stream schema".to_owned(),
        });
    }
    Ok(schemas.pop())
}

fn validate_openapi_parameters(
    document: &Value,
    matched: &OpenApiOperation<'_>,
    requested_path: &str,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let path_parameters =
        matched_path_parameters(matched.template, requested_path).map_err(|()| {
            FixtureVerifyError::InvalidSnapshot {
                label: "schemas/opencode/openapi.json path template".to_owned(),
            }
        })?;
    let query_parameters = match query_parameters(requested_path) {
        Ok(parameters) => parameters,
        Err(()) => {
            issues.push(issue(
                file,
                line,
                "OpenCode HTTP parameter encoding is invalid",
                Some("/payload/path"),
            ));
            return Ok(());
        }
    };
    let mut parameters = BTreeMap::new();
    for container in [matched.path_item, matched.operation] {
        if let Some(items) = container.get("parameters").and_then(Value::as_array) {
            for parameter in items {
                let parameter = resolve_openapi_reference(document, parameter)?;
                let Some(name) = parameter.get("name").and_then(Value::as_str) else {
                    return Err(FixtureVerifyError::InvalidSnapshot {
                        label: "schemas/opencode/openapi.json parameter name".to_owned(),
                    });
                };
                let Some(location) = parameter.get("in").and_then(Value::as_str) else {
                    return Err(FixtureVerifyError::InvalidSnapshot {
                        label: "schemas/opencode/openapi.json parameter location".to_owned(),
                    });
                };
                if matches!(location, "path" | "query") {
                    parameters.insert((location.to_owned(), name.to_owned()), parameter);
                }
            }
        }
    }

    for ((location, name), parameter) in parameters {
        let required = parameter
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let value = match location.as_str() {
            "path" => path_parameters.get(&name),
            "query" => query_parameters
                .get(&name)
                .and_then(|values| values.first()),
            _ => None,
        };
        let Some(value) = value else {
            if required {
                issues.push(issue(
                    file,
                    line,
                    match location.as_str() {
                        "path" => "OpenCode required path parameter is missing",
                        "query" => "OpenCode required query parameter is missing",
                        _ => "OpenCode required parameter is missing",
                    },
                    Some("/payload/path"),
                ));
            }
            continue;
        };
        let Some(schema) = parameter.get("schema") else {
            return Err(FixtureVerifyError::InvalidSnapshot {
                label: "schemas/opencode/openapi.json parameter schema".to_owned(),
            });
        };
        let instance = openapi_parameter_instance(schema, value);
        let schema = openapi_schema(document, schema);
        validate_instance(
            &schema,
            &instance,
            "/payload/path",
            match location.as_str() {
                "path" => "OpenCode path parameter schema validation failed",
                "query" => "OpenCode query parameter schema validation failed",
                _ => "OpenCode parameter schema validation failed",
            },
            file,
            line,
            issues,
        )?;
    }
    Ok(())
}

fn resolve_openapi_reference<'a>(
    document: &'a Value,
    mut value: &'a Value,
) -> Result<&'a Value, FixtureVerifyError> {
    for _ in 0..32 {
        let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
            return Ok(value);
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            return Err(FixtureVerifyError::InvalidSnapshot {
                label: "schemas/opencode/openapi.json external parameter reference".to_owned(),
            });
        };
        value = document
            .pointer(pointer)
            .ok_or_else(|| FixtureVerifyError::InvalidSnapshot {
                label: "schemas/opencode/openapi.json parameter reference".to_owned(),
            })?;
    }
    Err(FixtureVerifyError::InvalidSnapshot {
        label: "schemas/opencode/openapi.json parameter reference cycle".to_owned(),
    })
}

fn matched_path_parameters(
    template: &str,
    requested_path: &str,
) -> Result<BTreeMap<String, String>, ()> {
    let actual = requested_path.split('?').next().ok_or(())?;
    let mut values = BTreeMap::new();
    for (expected, actual) in template
        .trim_matches('/')
        .split('/')
        .zip(actual.trim_matches('/').split('/'))
    {
        if let Some(name) = expected
            .strip_prefix('{')
            .and_then(|name| name.strip_suffix('}'))
        {
            values.insert(name.to_owned(), percent_decode_component(actual, false)?);
        }
    }
    Ok(values)
}

fn query_parameters(path: &str) -> Result<BTreeMap<String, Vec<String>>, ()> {
    let Some((_, query)) = path.split_once('?') else {
        return Ok(BTreeMap::new());
    };
    let mut values = BTreeMap::<String, Vec<String>>::new();
    for component in query.split('&').filter(|component| !component.is_empty()) {
        let (name, value) = component.split_once('=').unwrap_or((component, ""));
        let name = percent_decode_component(name, true)?;
        let value = percent_decode_component(value, true)?;
        values.entry(name).or_default().push(value);
    }
    Ok(values)
}

fn percent_decode_component(encoded: &str, plus_as_space: bool) -> Result<String, ()> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(byte) = bytes.get(index).copied() {
        match byte {
            b'%' => {
                let high = bytes
                    .get(index + 1)
                    .and_then(|byte| hex_value(*byte))
                    .ok_or(())?;
                let low = bytes
                    .get(index + 2)
                    .and_then(|byte| hex_value(*byte))
                    .ok_or(())?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            b'+' if plus_as_space => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn openapi_parameter_instance(schema: &Value, value: &str) -> Value {
    match schema.get("type").and_then(Value::as_str) {
        Some("integer") => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_owned())),
        Some("number") => value
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(value.to_owned())),
        Some("boolean") => match value {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(value.to_owned()),
        },
        _ => Value::String(value.to_owned()),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_openapi_content(
    document: &Value,
    container: Option<&Value>,
    content_type: &str,
    body: &Value,
    pointer: &str,
    category: &str,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let Some(container) = container else {
        if !body.is_null() {
            issues.push(issue(
                file,
                line,
                "HTTP operation does not declare a body",
                Some(pointer),
            ));
        }
        return Ok(());
    };
    let content = container.get("content").and_then(Value::as_object);
    let Some(content) = content else {
        if !body.is_null() {
            issues.push(issue(
                file,
                line,
                "HTTP operation does not declare response content",
                Some(pointer),
            ));
        }
        return Ok(());
    };
    let media_type = content_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or(content_type);
    let media = content
        .iter()
        .find(|(declared, _)| declared.eq_ignore_ascii_case(media_type))
        .map(|(_, value)| value);
    let Some(media) = media else {
        issues.push(issue(
            file,
            line,
            "HTTP content_type is not declared by the OpenAPI operation",
            Some("/payload/content_type"),
        ));
        return Ok(());
    };
    let Some(schema) = media.get("schema") else {
        return Ok(());
    };
    let schema = openapi_schema(document, schema);
    validate_instance(&schema, body, pointer, category, file, line, issues)
}

#[allow(clippy::too_many_arguments)]
fn validate_instance(
    schema: &Value,
    instance: &Value,
    pointer_prefix: &str,
    category: &str,
    file: &str,
    line: usize,
    issues: &mut Vec<VerifyIssue>,
) -> Result<(), FixtureVerifyError> {
    let mut validation_errors = Vec::new();
    validate_json_schema(schema, schema, instance, "", &mut validation_errors, 0)?;
    for instance_pointer in validation_errors {
        let pointer = if instance_pointer.is_empty() {
            pointer_prefix.to_owned()
        } else {
            format!("{pointer_prefix}{instance_pointer}")
        };
        issues.push(issue(file, line, category, Some(&pointer)));
    }
    Ok(())
}

fn validate_json_schema(
    root: &Value,
    schema: &Value,
    instance: &Value,
    instance_pointer: &str,
    errors: &mut Vec<String>,
    depth: usize,
) -> Result<(), FixtureVerifyError> {
    // Verification must stay offline. This evaluator implements the complete
    // constraint-keyword set present in the three pinned T-003 snapshots and
    // rejects unsupported keywords, formats, and patterns instead of ignoring
    // a constraint.
    if depth > 256 {
        return Err(invalid_validator_snapshot());
    }
    match schema {
        Value::Bool(true) => Ok(()),
        Value::Bool(false) => {
            errors.push(instance_pointer.to_owned());
            Ok(())
        }
        Value::Object(object) => {
            audit_schema_object(object)?;
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                let Some(pointer) = reference.strip_prefix('#') else {
                    return Err(invalid_validator_snapshot());
                };
                let Some(referenced) = root.pointer(pointer) else {
                    return Err(invalid_validator_snapshot());
                };
                validate_json_schema(
                    root,
                    referenced,
                    instance,
                    instance_pointer,
                    errors,
                    depth + 1,
                )?;
            }

            validate_combinators(root, object, instance, instance_pointer, errors, depth)?;
            validate_type(object, instance, instance_pointer, errors)?;
            validate_enum_and_const(object, instance, instance_pointer, errors);
            validate_object(root, object, instance, instance_pointer, errors, depth)?;
            validate_array(root, object, instance, instance_pointer, errors, depth)?;
            validate_string(object, instance, instance_pointer, errors)?;
            validate_number(object, instance, instance_pointer, errors)?;
            Ok(())
        }
        _ => Err(invalid_validator_snapshot()),
    }
}

fn audit_schema_object(schema: &Map<String, Value>) -> Result<(), FixtureVerifyError> {
    for key in schema.keys() {
        let supported = matches!(
            key.as_str(),
            "$schema"
                | "$id"
                | "$comment"
                | "$ref"
                | "$defs"
                | "definitions"
                | "components"
                | "title"
                | "description"
                | "discriminator"
                | "default"
                | "examples"
                | "deprecated"
                | "readOnly"
                | "writeOnly"
                | "type"
                | "properties"
                | "required"
                | "additionalProperties"
                | "patternProperties"
                | "items"
                | "prefixItems"
                | "anyOf"
                | "oneOf"
                | "allOf"
                | "not"
                | "enum"
                | "const"
                | "minimum"
                | "maximum"
                | "exclusiveMinimum"
                | "minLength"
                | "maxLength"
                | "minItems"
                | "maxItems"
                | "pattern"
                | "format"
                | "contentMediaType"
                | "contentSchema"
        ) || key.starts_with("x-");
        if !supported {
            return Err(invalid_validator_snapshot());
        }
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        if !pattern_is_supported(pattern) {
            return Err(invalid_validator_snapshot());
        }
    }
    if let Some(patterns) = schema.get("patternProperties") {
        let Some(patterns) = patterns.as_object() else {
            return Err(invalid_validator_snapshot());
        };
        if patterns
            .keys()
            .any(|pattern| !pattern_is_supported(pattern))
        {
            return Err(invalid_validator_snapshot());
        }
    }
    if let Some(format) = schema.get("format").and_then(Value::as_str) {
        if !matches!(
            format,
            "int64" | "int32" | "uint64" | "uint32" | "uint16" | "uint" | "double" | "binary"
        ) {
            return Err(invalid_validator_snapshot());
        }
    }
    Ok(())
}

fn validate_combinators(
    root: &Value,
    schema: &Map<String, Value>,
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
    depth: usize,
) -> Result<(), FixtureVerifyError> {
    if let Some(all_of) = schema.get("allOf") {
        for child in schema_array(all_of)? {
            validate_json_schema(root, child, instance, pointer, errors, depth + 1)?;
        }
    }
    if let Some(any_of) = schema.get("anyOf") {
        validate_schema_alternatives(
            root,
            schema_array(any_of)?,
            instance,
            pointer,
            errors,
            depth,
            false,
        )?;
    }
    if let Some(one_of) = schema.get("oneOf") {
        validate_schema_alternatives(
            root,
            schema_array(one_of)?,
            instance,
            pointer,
            errors,
            depth,
            true,
        )?;
    }
    if let Some(not_schema) = schema.get("not") {
        let mut nested = Vec::new();
        validate_json_schema(root, not_schema, instance, pointer, &mut nested, depth + 1)?;
        if nested.is_empty() {
            errors.push(pointer.to_owned());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_schema_alternatives(
    root: &Value,
    alternatives: &[Value],
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
    depth: usize,
    exactly_one: bool,
) -> Result<(), FixtureVerifyError> {
    let mut successful = 0;
    let mut best_errors: Option<Vec<String>> = None;
    for alternative in alternatives {
        let mut candidate_errors = Vec::new();
        validate_json_schema(
            root,
            alternative,
            instance,
            pointer,
            &mut candidate_errors,
            depth + 1,
        )?;
        if candidate_errors.is_empty() {
            successful += 1;
        } else if best_errors
            .as_ref()
            .is_none_or(|best| candidate_errors.len() < best.len())
        {
            best_errors = Some(candidate_errors);
        }
    }
    let valid = if exactly_one {
        successful == 1
    } else {
        successful >= 1
    };
    if !valid {
        if successful > 1 {
            errors.push(pointer.to_owned());
        } else if let Some(best) = best_errors {
            errors.extend(best);
        } else {
            errors.push(pointer.to_owned());
        }
    }
    Ok(())
}

fn validate_type(
    schema: &Map<String, Value>,
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
) -> Result<(), FixtureVerifyError> {
    let Some(expected) = schema.get("type") else {
        return Ok(());
    };
    let matches = match expected {
        Value::String(name) => instance_matches_type(instance, name)?,
        Value::Array(names) => {
            let mut matched = false;
            for name in names {
                let Some(name) = name.as_str() else {
                    return Err(invalid_validator_snapshot());
                };
                matched |= instance_matches_type(instance, name)?;
            }
            matched
        }
        _ => return Err(invalid_validator_snapshot()),
    };
    if !matches {
        errors.push(pointer.to_owned());
    }
    Ok(())
}

fn instance_matches_type(instance: &Value, expected: &str) -> Result<bool, FixtureVerifyError> {
    let matches = match expected {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "number" => instance.is_number(),
        "integer" => instance
            .as_number()
            .is_some_and(number_is_json_schema_integer),
        _ => return Err(invalid_validator_snapshot()),
    };
    Ok(matches)
}

fn number_is_json_schema_integer(number: &serde_json::Number) -> bool {
    number.is_i64()
        || number.is_u64()
        || number
            .as_f64()
            .is_some_and(|value| value.is_finite() && value.fract() == 0.0)
}

fn validate_enum_and_const(
    schema: &Map<String, Value>,
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
) {
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|allowed| !allowed.contains(instance))
    {
        errors.push(pointer.to_owned());
    }
    if schema
        .get("const")
        .is_some_and(|expected| expected != instance)
    {
        errors.push(pointer.to_owned());
    }
}

fn validate_object(
    root: &Value,
    schema: &Map<String, Value>,
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
    depth: usize,
) -> Result<(), FixtureVerifyError> {
    let Some(instance) = instance.as_object() else {
        return Ok(());
    };
    let properties = match schema.get("properties") {
        Some(value) => Some(value.as_object().ok_or_else(invalid_validator_snapshot)?),
        None => None,
    };
    if let Some(required) = schema.get("required") {
        for required_property in schema_array(required)? {
            let Some(required_property) = required_property.as_str() else {
                return Err(invalid_validator_snapshot());
            };
            if !instance.contains_key(required_property) {
                errors.push(pointer_child(pointer, required_property));
            }
        }
    }
    if let Some(properties) = properties {
        for (property, child_schema) in properties {
            if let Some(child_instance) = instance.get(property) {
                validate_json_schema(
                    root,
                    child_schema,
                    child_instance,
                    &pointer_child(pointer, property),
                    errors,
                    depth + 1,
                )?;
            }
        }
    }

    let pattern_properties = match schema.get("patternProperties") {
        Some(value) => Some(value.as_object().ok_or_else(invalid_validator_snapshot)?),
        None => None,
    };
    if let Some(pattern_properties) = pattern_properties {
        for (pattern, child_schema) in pattern_properties {
            for (property, child_instance) in instance {
                if pattern_matches(pattern, property)? {
                    validate_json_schema(
                        root,
                        child_schema,
                        child_instance,
                        &pointer_child(pointer, property),
                        errors,
                        depth + 1,
                    )?;
                }
            }
        }
    }

    if let Some(additional) = schema.get("additionalProperties") {
        for (property, child_instance) in instance {
            let declared = properties.is_some_and(|properties| properties.contains_key(property));
            let patterned = pattern_properties.is_some_and(|patterns| {
                patterns
                    .keys()
                    .any(|pattern| pattern_matches(pattern, property).unwrap_or(false))
            });
            if declared || patterned {
                continue;
            }
            match additional {
                Value::Bool(true) => {}
                Value::Bool(false) => errors.push(pointer_child(pointer, property)),
                Value::Object(_) => validate_json_schema(
                    root,
                    additional,
                    child_instance,
                    &pointer_child(pointer, property),
                    errors,
                    depth + 1,
                )?,
                _ => return Err(invalid_validator_snapshot()),
            }
        }
    }
    Ok(())
}

fn validate_array(
    root: &Value,
    schema: &Map<String, Value>,
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
    depth: usize,
) -> Result<(), FixtureVerifyError> {
    let Some(items) = instance.as_array() else {
        return Ok(());
    };
    validate_usize_bound(schema, "minItems", items.len(), false, pointer, errors)?;
    validate_usize_bound(schema, "maxItems", items.len(), true, pointer, errors)?;

    let prefix_len = if let Some(prefix_items) = schema.get("prefixItems") {
        let prefix_items = schema_array(prefix_items)?;
        for (index, child_schema) in prefix_items.iter().enumerate() {
            if let Some(child_instance) = items.get(index) {
                validate_json_schema(
                    root,
                    child_schema,
                    child_instance,
                    &pointer_child(pointer, &index.to_string()),
                    errors,
                    depth + 1,
                )?;
            }
        }
        prefix_items.len()
    } else {
        0
    };
    if let Some(item_schema) = schema.get("items") {
        for (index, child_instance) in items.iter().enumerate().skip(prefix_len) {
            validate_json_schema(
                root,
                item_schema,
                child_instance,
                &pointer_child(pointer, &index.to_string()),
                errors,
                depth + 1,
            )?;
        }
    }
    Ok(())
}

fn validate_string(
    schema: &Map<String, Value>,
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
) -> Result<(), FixtureVerifyError> {
    let Some(text) = instance.as_str() else {
        return Ok(());
    };
    let length = text.chars().count();
    validate_usize_bound(schema, "minLength", length, false, pointer, errors)?;
    validate_usize_bound(schema, "maxLength", length, true, pointer, errors)?;
    if let Some(pattern) = schema.get("pattern") {
        let Some(pattern) = pattern.as_str() else {
            return Err(invalid_validator_snapshot());
        };
        if !pattern_matches(pattern, text)? {
            errors.push(pointer.to_owned());
        }
    }
    Ok(())
}

fn pattern_matches(pattern: &str, text: &str) -> Result<bool, FixtureVerifyError> {
    let matches = match pattern {
        "^evt_" => text.starts_with("evt_"),
        "^ses" => text.starts_with("ses"),
        "^msg_" => text.starts_with("msg_"),
        "^msg" => text.starts_with("msg"),
        "^prt" => text.starts_with("prt"),
        "^que" => text.starts_with("que"),
        "^per" => text.starts_with("per"),
        "^pty" => text.starts_with("pty"),
        "^wrk" => text.starts_with("wrk"),
        "^#[0-9a-fA-F]{6}$" => {
            text.len() == 7
                && text.starts_with('#')
                && text
                    .get(1..)
                    .is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        }
        _ => return Err(invalid_validator_snapshot()),
    };
    Ok(matches)
}

fn pattern_is_supported(pattern: &str) -> bool {
    matches!(
        pattern,
        "^evt_"
            | "^ses"
            | "^msg_"
            | "^msg"
            | "^prt"
            | "^que"
            | "^per"
            | "^pty"
            | "^wrk"
            | "^#[0-9a-fA-F]{6}$"
    )
}

fn validate_number(
    schema: &Map<String, Value>,
    instance: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
) -> Result<(), FixtureVerifyError> {
    let Some(number) = instance.as_number() else {
        return Ok(());
    };
    let Some(value) = number.as_f64() else {
        return Err(invalid_validator_snapshot());
    };
    for (keyword, comparison) in [
        ("minimum", NumericComparison::GreaterOrEqual),
        ("maximum", NumericComparison::LessOrEqual),
        ("exclusiveMinimum", NumericComparison::Greater),
    ] {
        if let Some(bound) = schema.get(keyword) {
            let Some(bound) = bound.as_f64() else {
                return Err(invalid_validator_snapshot());
            };
            if !comparison.matches(value, bound) {
                errors.push(pointer.to_owned());
            }
        }
    }
    if let Some(format) = schema.get("format") {
        let Some(format) = format.as_str() else {
            return Err(invalid_validator_snapshot());
        };
        if !number_matches_format(number, format)? {
            errors.push(pointer.to_owned());
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum NumericComparison {
    Greater,
    GreaterOrEqual,
    LessOrEqual,
}

impl NumericComparison {
    fn matches(self, value: f64, bound: f64) -> bool {
        match self {
            Self::Greater => value > bound,
            Self::GreaterOrEqual => value >= bound,
            Self::LessOrEqual => value <= bound,
        }
    }
}

fn number_matches_format(
    number: &serde_json::Number,
    format: &str,
) -> Result<bool, FixtureVerifyError> {
    let integer_text = number.to_string();
    let matches = match format {
        "int64" => integer_text.parse::<i64>().is_ok(),
        "int32" => integer_text.parse::<i32>().is_ok(),
        "uint64" | "uint" => integer_text.parse::<u64>().is_ok(),
        "uint32" => integer_text.parse::<u32>().is_ok(),
        "uint16" => integer_text.parse::<u16>().is_ok(),
        "double" => number.as_f64().is_some(),
        "binary" => return Ok(false),
        _ => return Err(invalid_validator_snapshot()),
    };
    Ok(matches)
}

fn validate_usize_bound(
    schema: &Map<String, Value>,
    keyword: &str,
    actual: usize,
    maximum: bool,
    pointer: &str,
    errors: &mut Vec<String>,
) -> Result<(), FixtureVerifyError> {
    let Some(bound) = schema.get(keyword) else {
        return Ok(());
    };
    let Some(bound) = bound.as_u64() else {
        return Err(invalid_validator_snapshot());
    };
    let valid = if maximum {
        u64::try_from(actual).is_ok_and(|actual| actual <= bound)
    } else {
        u64::try_from(actual).is_ok_and(|actual| actual >= bound)
    };
    if !valid {
        errors.push(pointer.to_owned());
    }
    Ok(())
}

fn schema_array(value: &Value) -> Result<&[Value], FixtureVerifyError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(invalid_validator_snapshot)
}

fn invalid_validator_snapshot() -> FixtureVerifyError {
    FixtureVerifyError::InvalidSnapshot {
        label: "schema validator compilation".to_owned(),
    }
}

fn read_json(path: &Path, label: &str) -> Result<Value, FixtureVerifyError> {
    let bytes = fs::read(path).map_err(|source| FixtureVerifyError::Read {
        label: label.to_owned(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|_| FixtureVerifyError::InvalidSnapshot {
        label: label.to_owned(),
    })
}

fn fixture_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if relative.is_empty() {
                "tests/fixtures".to_owned()
            } else {
                format!("tests/fixtures/{relative}")
            }
        })
        .unwrap_or_else(|_| "tests/fixtures/<external>".to_owned())
}

fn schema_label(root: &Path, path: &Path, prefix: &str) -> String {
    path.strip_prefix(root)
        .map(|relative| {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if relative.is_empty() {
                prefix.to_owned()
            } else {
                format!("{prefix}/{relative}")
            }
        })
        .unwrap_or_else(|_| format!("{prefix}/<external>"))
}

fn pointer_child(pointer: &str, token: &str) -> String {
    format!("{pointer}/{}", token.replace('~', "~0").replace('/', "~1"))
}

fn issue(file: &str, line: usize, category: &str, pointer: Option<&str>) -> VerifyIssue {
    VerifyIssue {
        file: file.to_owned(),
        line,
        category: category.to_owned(),
        pointer: pointer.map(str::to_owned),
    }
}

fn sort_and_deduplicate_issues(issues: &mut Vec<VerifyIssue>) {
    issues.sort_by(|left, right| {
        (
            &left.file,
            left.line,
            issue_priority(&left.category),
            &left.category,
            left.pointer.as_deref(),
        )
            .cmp(&(
                &right.file,
                right.line,
                issue_priority(&right.category),
                &right.category,
                right.pointer.as_deref(),
            ))
    });
    issues.dedup();
}

fn issue_priority(category: &str) -> u8 {
    if category.starts_with("leak:") {
        0
    } else {
        1
    }
}
