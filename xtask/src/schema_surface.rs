use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use semver::{Version, VersionReq};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::deps::{
    parse_restricted_toml, RestrictedTable, RestrictedToml, RestrictedTomlError, RestrictedValue,
};

pub const REQUIRED_SURFACE_PATH: &str = "schemas/required-surface.toml";

const SUPPORTED_SCHEMA_VERSION: u64 = 1;
const CODEX_DOCUMENT: &str = "codex/codex_app_server_protocol.schemas.json";
const ACP_DOCUMENT: &str = "acp/schema.json";
const OPENCODE_DOCUMENT: &str = "opencode/openapi.json";
const CODEX_METHOD_UNIONS: [&str; 4] = [
    "ClientRequest",
    "ServerRequest",
    "ClientNotification",
    "ServerNotification",
];
const DIGEST_DOMAIN: &[u8] = b"OneKaleidoscope required surface v1\0";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SurfaceTool {
    Codex,
    Acp,
    OpenCode,
}

impl SurfaceTool {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Acp => "acp",
            Self::OpenCode => "opencode",
        }
    }

    const fn document(self) -> &'static str {
        match self {
            Self::Codex => CODEX_DOCUMENT,
            Self::Acp => ACP_DOCUMENT,
            Self::OpenCode => OPENCODE_DOCUMENT,
        }
    }

    fn parse(value: &str) -> Result<Self, RequiredSurfaceError> {
        match value {
            "codex" => Ok(Self::Codex),
            "acp" => Ok(Self::Acp),
            "opencode" => Ok(Self::OpenCode),
            _ => Err(RequiredSurfaceError::InvalidConfig(format!(
                "unsupported upstream tool `{value}`"
            ))),
        }
    }
}

impl fmt::Display for SurfaceTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SurfaceEntryKind {
    Method,
    Type,
}

impl SurfaceEntryKind {
    fn parse(value: &str) -> Result<Self, RequiredSurfaceError> {
        match value {
            "method" => Ok(Self::Method),
            "type" => Ok(Self::Type),
            _ => Err(RequiredSurfaceError::InvalidConfig(format!(
                "unsupported required-surface entry kind `{value}`"
            ))),
        }
    }
}

impl fmt::Display for SurfaceEntryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Method => formatter.write_str("method"),
            Self::Type => formatter.write_str("type"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceToolRule {
    pub name: SurfaceTool,
    pub supported_range: String,
    parsed_supported_range: VersionReq,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceEntry {
    pub id: String,
    pub tool: SurfaceTool,
    pub kind: SurfaceEntryKind,
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredSurface {
    tools: BTreeMap<SurfaceTool, SurfaceToolRule>,
    entries: Vec<SurfaceEntry>,
}

impl RequiredSurface {
    pub fn tools(&self) -> impl Iterator<Item = &SurfaceToolRule> {
        self.tools.values()
    }

    pub fn entries(&self) -> &[SurfaceEntry] {
        &self.entries
    }

    pub fn supported_range(&self, tool: SurfaceTool) -> Option<&str> {
        self.tools
            .get(&tool)
            .map(|rule| rule.supported_range.as_str())
    }

    pub fn version_support(
        &self,
        tool: SurfaceTool,
        observed_version: &str,
    ) -> Option<VersionSupport> {
        let rule = self.tools.get(&tool)?;
        let Ok(version) = Version::parse(observed_version) else {
            return Some(VersionSupport::Unparseable {
                supported_range: rule.supported_range.clone(),
            });
        };
        if rule.parsed_supported_range.matches(&version) {
            Some(VersionSupport::Supported)
        } else {
            Some(VersionSupport::Unverified {
                supported_range: rule.supported_range.clone(),
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionSupport {
    Supported,
    Unverified { supported_range: String },
    Unparseable { supported_range: String },
}

#[derive(Debug, Error)]
pub enum RequiredSurfaceError {
    #[error("required-surface I/O failure: {0}")]
    Io(#[from] io::Error),
    #[error("required-surface TOML is invalid: {0}")]
    Toml(#[from] RestrictedTomlError),
    #[error("required-surface configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("invalid JSON in `{file}`: {source}")]
    Json {
        file: &'static str,
        source: serde_json::Error,
    },
    #[error(
        "required-surface baseline entry `{entry_id}` ({tool} {kind} `{name}`) does not exist: {detail}"
    )]
    BaselineEntryMissing {
        entry_id: String,
        tool: SurfaceTool,
        kind: SurfaceEntryKind,
        name: String,
        detail: String,
    },
    #[error("upstream schema `{file}` is structurally invalid: {detail}")]
    InvalidSchema { file: &'static str, detail: String },
    #[error("could not serialize required-surface digest input: {0}")]
    DigestSerialization(serde_json::Error),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SurfaceChangeKind {
    Added,
    Changed,
    Removed,
}

impl fmt::Display for SurfaceChangeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Added => formatter.write_str("added"),
            Self::Changed => formatter.write_str("changed"),
            Self::Removed => formatter.write_str("removed"),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SurfaceChange {
    pub entry_id: String,
    pub file: String,
    pub pointer: String,
    pub kind: SurfaceChangeKind,
}

impl fmt::Display for SurfaceChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}{} ({}, required by {})",
            self.file, self.pointer, self.kind, self.entry_id
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceSide {
    Baseline,
    Observed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RawChangeDisposition {
    InSurface { entry_ids: Vec<String> },
    OutOfSurface,
}

pub type ToolSurfaceDigests = BTreeMap<String, BTreeMap<String, String>>;

#[derive(Clone, Debug)]
pub struct SurfaceComparison {
    changes: Vec<SurfaceChange>,
    baseline_locations: Vec<SurfaceLocation>,
    observed_locations: Vec<SurfaceLocation>,
    baseline_digests: ToolSurfaceDigests,
    observed_digests: ToolSurfaceDigests,
}

impl SurfaceComparison {
    pub fn changes(&self) -> &[SurfaceChange] {
        &self.changes
    }

    pub fn baseline_digests(&self) -> &ToolSurfaceDigests {
        &self.baseline_digests
    }

    pub fn observed_digests(&self) -> &ToolSurfaceDigests {
        &self.observed_digests
    }

    pub fn owners_at(&self, side: SurfaceSide, file: &str, pointer: &str) -> Vec<String> {
        let locations = match side {
            SurfaceSide::Baseline => &self.baseline_locations,
            SurfaceSide::Observed => &self.observed_locations,
        };
        let mut owners = locations
            .iter()
            .filter(|location| location.owns(file, pointer))
            .map(|location| location.entry_id.clone())
            .collect::<Vec<_>>();
        owners.sort();
        owners.dedup();
        owners
    }

    pub fn classify_full_change(
        &self,
        file: &str,
        pointer: &str,
        kind: SurfaceChangeKind,
    ) -> RawChangeDisposition {
        let mut owners = match kind {
            SurfaceChangeKind::Added => self.owners_at(SurfaceSide::Observed, file, pointer),
            SurfaceChangeKind::Removed => self.owners_at(SurfaceSide::Baseline, file, pointer),
            SurfaceChangeKind::Changed => {
                let mut owners = self.owners_at(SurfaceSide::Baseline, file, pointer);
                owners.extend(self.owners_at(SurfaceSide::Observed, file, pointer));
                owners
            }
        };
        owners.sort();
        owners.dedup();
        if owners.is_empty() {
            RawChangeDisposition::OutOfSurface
        } else {
            RawChangeDisposition::InSurface { entry_ids: owners }
        }
    }
}

#[derive(Clone, Debug)]
struct SurfaceLocation {
    entry_id: String,
    file: String,
    pointer: String,
}

impl SurfaceLocation {
    fn owns(&self, file: &str, pointer: &str) -> bool {
        self.file == file
            && (self.pointer == "#"
                || pointer == self.pointer
                || pointer
                    .strip_prefix(&self.pointer)
                    .is_some_and(|suffix| suffix.starts_with('/')))
    }
}

#[derive(Debug)]
struct SchemaDocuments {
    documents: BTreeMap<SurfaceTool, Option<Value>>,
}

#[derive(Clone, Debug)]
struct ResolvedFragment {
    file: String,
    pointer: String,
    value: Value,
}

#[derive(Clone, Debug, Default)]
struct ResolvedEntry {
    missing_root: bool,
    missing_references: BTreeSet<String>,
    fragments: BTreeMap<String, ResolvedFragment>,
}

#[derive(Clone, Debug)]
struct RootSelection {
    semantic_key: String,
    pointer: String,
    value: Value,
}

pub fn load_required_surface(
    workspace_root: &Path,
) -> Result<RequiredSurface, RequiredSurfaceError> {
    let source = fs::read_to_string(workspace_root.join(REQUIRED_SURFACE_PATH))?;
    parse_required_surface(&source)
}

pub fn parse_required_surface(source: &str) -> Result<RequiredSurface, RequiredSurfaceError> {
    let document = parse_restricted_toml(source)?;
    reject_document_shape(&document)?;
    let schema_version = document
        .root()
        .get("schema_version")
        .and_then(RestrictedValue::as_integer)
        .ok_or_else(|| {
            RequiredSurfaceError::InvalidConfig(
                "root `schema_version` must be a non-negative integer".to_owned(),
            )
        })?;
    if schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "unsupported schema_version `{schema_version}`; expected `{SUPPORTED_SCHEMA_VERSION}`"
        )));
    }

    let tool_tables = document.array_tables(&["tools"]).ok_or_else(|| {
        RequiredSurfaceError::InvalidConfig("missing `[[tools]]` declarations".to_owned())
    })?;
    if tool_tables.is_empty() {
        return Err(RequiredSurfaceError::InvalidConfig(
            "`[[tools]]` must not be empty".to_owned(),
        ));
    }
    let mut tools = BTreeMap::new();
    for table in tool_tables {
        reject_unknown_keys(table, &["name", "supported_range"], "[[tools]]")?;
        let name_text = required_nonempty_string(table, "name", "[[tools]]")?;
        let name = SurfaceTool::parse(&name_text)?;
        let supported_range = required_nonempty_string(table, "supported_range", "[[tools]]")?;
        let parsed_supported_range = VersionReq::parse(&supported_range).map_err(|error| {
            RequiredSurfaceError::InvalidConfig(format!(
                "[[tools]] `{name}` has invalid supported_range `{supported_range}`: {error}"
            ))
        })?;
        let rule = SurfaceToolRule {
            name,
            supported_range,
            parsed_supported_range,
        };
        if tools.insert(name, rule).is_some() {
            return Err(RequiredSurfaceError::InvalidConfig(format!(
                "duplicate `[[tools]]` declaration for `{name}`"
            )));
        }
    }

    let entry_tables = document.array_tables(&["entries"]).ok_or_else(|| {
        RequiredSurfaceError::InvalidConfig("missing `[[entries]]` declarations".to_owned())
    })?;
    if entry_tables.is_empty() {
        return Err(RequiredSurfaceError::InvalidConfig(
            "`[[entries]]` must not be empty".to_owned(),
        ));
    }
    let mut entry_ids = BTreeSet::new();
    let mut used_tools = BTreeSet::new();
    let mut entries = Vec::with_capacity(entry_tables.len());
    for table in entry_tables {
        reject_unknown_keys(
            table,
            &["id", "tool", "kind", "name", "reason"],
            "[[entries]]",
        )?;
        let id = required_nonempty_string(table, "id", "[[entries]]")?;
        if !entry_ids.insert(id.clone()) {
            return Err(RequiredSurfaceError::InvalidConfig(format!(
                "duplicate required-surface entry id `{id}`"
            )));
        }
        let tool_text = required_nonempty_string(table, "tool", "[[entries]]")?;
        let tool = SurfaceTool::parse(&tool_text)?;
        if !tools.contains_key(&tool) {
            return Err(RequiredSurfaceError::InvalidConfig(format!(
                "entry `{id}` references undeclared tool `{tool}`"
            )));
        }
        let kind_text = required_nonempty_string(table, "kind", "[[entries]]")?;
        let kind = SurfaceEntryKind::parse(&kind_text)?;
        let name = required_nonempty_string(table, "name", "[[entries]]")?;
        validate_entry_name(tool, kind, &name, &id)?;
        let reason = required_nonempty_string(table, "reason", "[[entries]]")?;
        used_tools.insert(tool);
        entries.push(SurfaceEntry {
            id,
            tool,
            kind,
            name,
            reason,
        });
    }

    if let Some(unused) = tools.keys().find(|tool| !used_tools.contains(tool)) {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "tool `{unused}` has no required-surface entries"
        )));
    }

    entries.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(RequiredSurface { tools, entries })
}

pub fn validate_baseline(
    surface: &RequiredSurface,
    schemas_root: &Path,
) -> Result<(), RequiredSurfaceError> {
    let documents = SchemaDocuments::load(surface, schemas_root)?;
    let resolved = resolve_all(surface, &documents)?;
    validate_resolved_baseline(surface, &resolved)
}

pub fn compare_required_surface(
    surface: &RequiredSurface,
    baseline_root: &Path,
    observed_root: &Path,
) -> Result<SurfaceComparison, RequiredSurfaceError> {
    let baseline_documents = SchemaDocuments::load(surface, baseline_root)?;
    let observed_documents = SchemaDocuments::load(surface, observed_root)?;
    let baseline = resolve_all(surface, &baseline_documents)?;
    validate_resolved_baseline(surface, &baseline)?;
    let observed = resolve_all(surface, &observed_documents)?;

    let mut changes = Vec::new();
    for entry in surface.entries() {
        let baseline_entry = baseline.get(&entry.id).ok_or_else(|| {
            RequiredSurfaceError::InvalidConfig(format!(
                "internal resolution omitted baseline entry `{}`",
                entry.id
            ))
        })?;
        let observed_entry = observed.get(&entry.id).ok_or_else(|| {
            RequiredSurfaceError::InvalidConfig(format!(
                "internal resolution omitted observed entry `{}`",
                entry.id
            ))
        })?;
        compare_entry(entry, baseline_entry, observed_entry, &mut changes);
    }
    changes.sort();
    changes.dedup();

    Ok(SurfaceComparison {
        changes,
        baseline_locations: collect_locations(&baseline),
        observed_locations: collect_locations(&observed),
        baseline_digests: digest_all(surface, &baseline)?,
        observed_digests: digest_all(surface, &observed)?,
    })
}

fn reject_document_shape(document: &RestrictedToml) -> Result<(), RequiredSurfaceError> {
    if let Some(key) = document
        .root()
        .keys()
        .find(|key| key.as_str() != "schema_version")
    {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "root contains unsupported key `{key}`"
        )));
    }
    if let Some((path, _)) = document.tables().next() {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "unsupported regular table `[{}]`",
            path.segments().join(".")
        )));
    }
    if let Some((path, _)) = document
        .all_array_tables()
        .find(|(path, _)| path.segments() != ["tools"] && path.segments() != ["entries"])
    {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "unsupported array table `[[{}]]`",
            path.segments().join(".")
        )));
    }
    Ok(())
}

fn reject_unknown_keys(
    table: &RestrictedTable,
    allowed: &[&str],
    context: &str,
) -> Result<(), RequiredSurfaceError> {
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "{context} contains unsupported key `{key}`"
        )));
    }
    Ok(())
}

fn required_nonempty_string(
    table: &RestrictedTable,
    key: &str,
    context: &str,
) -> Result<String, RequiredSurfaceError> {
    let value = match table.get(key) {
        Some(RestrictedValue::String(value)) => value,
        Some(_) => {
            return Err(RequiredSurfaceError::InvalidConfig(format!(
                "{context}.{key} must be a string"
            )));
        }
        None => {
            return Err(RequiredSurfaceError::InvalidConfig(format!(
                "{context} is missing `{key}`"
            )));
        }
    };
    if value.trim().is_empty() {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "{context}.{key} must not be empty"
        )));
    }
    Ok(value.clone())
}

fn validate_entry_name(
    tool: SurfaceTool,
    kind: SurfaceEntryKind,
    name: &str,
    id: &str,
) -> Result<(), RequiredSurfaceError> {
    if tool == SurfaceTool::OpenCode && kind == SurfaceEntryKind::Method {
        let mut parts = name.split_ascii_whitespace();
        let method = parts.next();
        let path = parts.next();
        if method.is_none()
            || path.is_none()
            || parts.next().is_some()
            || !path.is_some_and(|value| value.starts_with('/'))
        {
            return Err(RequiredSurfaceError::InvalidConfig(format!(
                "OpenCode method entry `{id}` must use `HTTP_METHOD /path`"
            )));
        }
    }
    if tool == SurfaceTool::Codex
        && kind == SurfaceEntryKind::Type
        && name.starts_with("v2/")
        && name.get(3..).is_none_or(|value| value.is_empty())
    {
        return Err(RequiredSurfaceError::InvalidConfig(format!(
            "Codex v2 type entry `{id}` must name a type after `v2/`"
        )));
    }
    Ok(())
}

impl SchemaDocuments {
    fn load(surface: &RequiredSurface, schemas_root: &Path) -> Result<Self, RequiredSurfaceError> {
        let tools = surface
            .entries()
            .iter()
            .map(|entry| entry.tool)
            .collect::<BTreeSet<_>>();
        let mut documents = BTreeMap::new();
        for tool in tools {
            let file = tool.document();
            let path = schemas_root.join(file.replace('/', std::path::MAIN_SEPARATOR_STR));
            let document = match fs::read(&path) {
                Ok(bytes) => Some(
                    serde_json::from_slice(&bytes)
                        .map_err(|source| RequiredSurfaceError::Json { file, source })?,
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            documents.insert(tool, document);
        }
        Ok(Self { documents })
    }

    fn get(&self, tool: SurfaceTool) -> Option<&Value> {
        self.documents.get(&tool).and_then(Option::as_ref)
    }
}

fn resolve_all(
    surface: &RequiredSurface,
    documents: &SchemaDocuments,
) -> Result<BTreeMap<String, ResolvedEntry>, RequiredSurfaceError> {
    surface
        .entries()
        .iter()
        .map(|entry| {
            resolve_entry(entry, documents.get(entry.tool))
                .map(|resolved| (entry.id.clone(), resolved))
        })
        .collect()
}

fn resolve_entry(
    entry: &SurfaceEntry,
    document: Option<&Value>,
) -> Result<ResolvedEntry, RequiredSurfaceError> {
    let Some(document) = document else {
        return Ok(ResolvedEntry {
            missing_root: true,
            ..ResolvedEntry::default()
        });
    };
    let roots = match (entry.tool, entry.kind) {
        (SurfaceTool::Codex, SurfaceEntryKind::Method) => {
            resolve_codex_method(document, &entry.name)?
        }
        (SurfaceTool::Codex, SurfaceEntryKind::Type) => resolve_codex_type(document, &entry.name)?,
        (SurfaceTool::Acp, SurfaceEntryKind::Method) => resolve_acp_method(document, &entry.name)?,
        (SurfaceTool::Acp, SurfaceEntryKind::Type) => {
            resolve_named_value(document, &["$defs"], &entry.name, "root")?
        }
        (SurfaceTool::OpenCode, SurfaceEntryKind::Method) => {
            resolve_opencode_method(document, &entry.name)?
        }
        (SurfaceTool::OpenCode, SurfaceEntryKind::Type) => {
            resolve_named_value(document, &["components", "schemas"], &entry.name, "root")?
        }
    };
    if roots.is_empty() {
        return Ok(ResolvedEntry {
            missing_root: true,
            ..ResolvedEntry::default()
        });
    }

    let mut resolved = ResolvedEntry::default();
    for root in roots {
        insert_fragment(
            &mut resolved.fragments,
            root.semantic_key.clone(),
            entry.tool.document(),
            root.pointer,
            root.value.clone(),
        )?;
        // Method entries own only the wire envelope. Following an aggregate operation such as
        // OpenCode GET /event would silently promote every unrelated event variant into the
        // required surface. Payload types are explicit config entries and own their full closure.
        if entry.kind == SurfaceEntryKind::Type {
            collect_reference_closure(document, &root.value, entry.tool.document(), &mut resolved)?;
        }
    }
    Ok(resolved)
}

fn resolve_codex_method(
    document: &Value,
    method: &str,
) -> Result<Vec<RootSelection>, RequiredSurfaceError> {
    let Some(definitions) = document.get("definitions").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut roots = Vec::new();
    for union_name in CODEX_METHOD_UNIONS {
        let Some(branches) = definitions
            .get(union_name)
            .and_then(|union| union.get("oneOf"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for (index, branch) in branches.iter().enumerate() {
            if schema_declares_method(document, branch, method, &mut BTreeSet::new()) {
                let title = branch
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(method);
                roots.push(RootSelection {
                    semantic_key: format!("method:{union_name}:{title}"),
                    pointer: format!(
                        "#/definitions/{}/oneOf/{index}",
                        escape_pointer_token(union_name)
                    ),
                    value: branch.clone(),
                });
            }
        }
    }
    ensure_unique_semantic_keys(CODEX_DOCUMENT, &roots)?;
    Ok(roots)
}

fn schema_declares_method(
    document: &Value,
    schema: &Value,
    method: &str,
    visited: &mut BTreeSet<String>,
) -> bool {
    if schema
        .pointer("/properties/method/enum")
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(method)))
    {
        return true;
    }
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return false;
    };
    if !reference.starts_with('#') || !visited.insert(reference.to_owned()) {
        return false;
    }
    resolve_local_reference(document, reference)
        .is_some_and(|target| schema_declares_method(document, target, method, visited))
}

fn resolve_codex_type(
    document: &Value,
    name: &str,
) -> Result<Vec<RootSelection>, RequiredSurfaceError> {
    if let Some(v2_name) = name.strip_prefix("v2/") {
        return resolve_named_value(document, &["definitions", "v2"], v2_name, "root");
    }
    let top = resolve_named_value(document, &["definitions"], name, "root")?;
    if !top.is_empty() {
        return Ok(top);
    }
    resolve_named_value(document, &["definitions", "v2"], name, "root")
}

fn resolve_acp_method(
    document: &Value,
    method: &str,
) -> Result<Vec<RootSelection>, RequiredSurfaceError> {
    let Some(definitions) = document.get("$defs").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut roots = Vec::new();
    for (name, schema) in definitions {
        if schema.get("x-method").and_then(Value::as_str) == Some(method) {
            roots.push(RootSelection {
                semantic_key: format!("method:{name}"),
                pointer: format!("#/$defs/{}", escape_pointer_token(name)),
                value: schema.clone(),
            });
        }
    }
    ensure_unique_semantic_keys(ACP_DOCUMENT, &roots)?;
    Ok(roots)
}

fn resolve_opencode_method(
    document: &Value,
    name: &str,
) -> Result<Vec<RootSelection>, RequiredSurfaceError> {
    let mut parts = name.split_ascii_whitespace();
    let Some(method) = parts.next() else {
        return Ok(Vec::new());
    };
    let Some(path) = parts.next() else {
        return Ok(Vec::new());
    };
    if parts.next().is_some() {
        return Ok(Vec::new());
    }
    let operation_name = method.to_ascii_lowercase();
    let Some(operation) = document
        .get("paths")
        .and_then(|paths| paths.get(path))
        .and_then(|path_item| path_item.get(&operation_name))
    else {
        return Ok(Vec::new());
    };
    Ok(vec![RootSelection {
        semantic_key: format!("method:{name}"),
        pointer: format!(
            "#/paths/{}/{}",
            escape_pointer_token(path),
            escape_pointer_token(&operation_name)
        ),
        value: operation.clone(),
    }])
}

fn resolve_named_value(
    document: &Value,
    parents: &[&str],
    name: &str,
    semantic_key: &str,
) -> Result<Vec<RootSelection>, RequiredSurfaceError> {
    let mut current = document;
    let mut pointer = String::from("#");
    for parent in parents {
        let Some(next) = current.get(*parent) else {
            return Ok(Vec::new());
        };
        current = next;
        pointer.push('/');
        pointer.push_str(&escape_pointer_token(parent));
    }
    let Some(value) = current.get(name) else {
        return Ok(Vec::new());
    };
    pointer.push('/');
    pointer.push_str(&escape_pointer_token(name));
    Ok(vec![RootSelection {
        semantic_key: semantic_key.to_owned(),
        pointer,
        value: value.clone(),
    }])
}

fn ensure_unique_semantic_keys(
    file: &'static str,
    roots: &[RootSelection],
) -> Result<(), RequiredSurfaceError> {
    let mut keys = BTreeSet::new();
    if let Some(duplicate) = roots
        .iter()
        .map(|root| root.semantic_key.as_str())
        .find(|key| !keys.insert((*key).to_owned()))
    {
        return Err(RequiredSurfaceError::InvalidSchema {
            file,
            detail: format!("semantic selector produced duplicate key `{duplicate}`"),
        });
    }
    Ok(())
}

fn collect_reference_closure(
    document: &Value,
    root: &Value,
    file: &'static str,
    resolved: &mut ResolvedEntry,
) -> Result<(), RequiredSurfaceError> {
    let mut pending = BTreeSet::new();
    collect_local_references(root, &mut pending);
    let mut visited = BTreeSet::new();
    while let Some(reference) = pending.pop_first() {
        if !visited.insert(reference.clone()) {
            continue;
        }
        let Some(value) = resolve_local_reference(document, &reference) else {
            resolved.missing_references.insert(reference);
            continue;
        };
        insert_fragment(
            &mut resolved.fragments,
            format!("ref:{reference}"),
            file,
            reference.clone(),
            value.clone(),
        )?;
        collect_local_references(value, &mut pending);
    }
    Ok(())
}

fn collect_local_references(value: &Value, references: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                if reference.starts_with('#') {
                    references.insert(reference.to_owned());
                }
            }
            for child in object.values() {
                collect_local_references(child, references);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_local_references(child, references);
            }
        }
        _ => {}
    }
}

fn resolve_local_reference<'a>(document: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        Some(document)
    } else if pointer.starts_with('/') {
        document.pointer(pointer)
    } else {
        None
    }
}

fn insert_fragment(
    fragments: &mut BTreeMap<String, ResolvedFragment>,
    semantic_key: String,
    file: &str,
    pointer: String,
    value: Value,
) -> Result<(), RequiredSurfaceError> {
    if fragments
        .insert(
            semantic_key.clone(),
            ResolvedFragment {
                file: file.to_owned(),
                pointer,
                value,
            },
        )
        .is_some()
    {
        return Err(RequiredSurfaceError::InvalidSchema {
            file: match file {
                CODEX_DOCUMENT => CODEX_DOCUMENT,
                ACP_DOCUMENT => ACP_DOCUMENT,
                OPENCODE_DOCUMENT => OPENCODE_DOCUMENT,
                _ => "<schema>",
            },
            detail: format!("duplicate resolved fragment key `{semantic_key}`"),
        });
    }
    Ok(())
}

fn validate_resolved_baseline(
    surface: &RequiredSurface,
    resolved: &BTreeMap<String, ResolvedEntry>,
) -> Result<(), RequiredSurfaceError> {
    for entry in surface.entries() {
        let Some(value) = resolved.get(&entry.id) else {
            return Err(RequiredSurfaceError::InvalidConfig(format!(
                "internal resolution omitted baseline entry `{}`",
                entry.id
            )));
        };
        let detail = if value.missing_root {
            Some("stable semantic selector found no matching entry".to_owned())
        } else if !value.missing_references.is_empty() {
            Some(format!(
                "local reference(s) do not resolve: {}",
                value
                    .missing_references
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        } else {
            None
        };
        if let Some(detail) = detail {
            return Err(RequiredSurfaceError::BaselineEntryMissing {
                entry_id: entry.id.clone(),
                tool: entry.tool,
                kind: entry.kind,
                name: entry.name.clone(),
                detail,
            });
        }
    }
    Ok(())
}

fn compare_entry(
    entry: &SurfaceEntry,
    baseline: &ResolvedEntry,
    observed: &ResolvedEntry,
    changes: &mut Vec<SurfaceChange>,
) {
    if observed.missing_root {
        if let Some(root) = baseline.fragments.values().next() {
            changes.push(SurfaceChange {
                entry_id: entry.id.clone(),
                file: root.file.clone(),
                pointer: root.pointer.clone(),
                kind: SurfaceChangeKind::Removed,
            });
        }
        return;
    }

    let keys = baseline
        .fragments
        .keys()
        .chain(observed.fragments.keys())
        .collect::<BTreeSet<_>>();
    for key in keys {
        match (baseline.fragments.get(key), observed.fragments.get(key)) {
            (Some(expected), Some(actual)) => compare_json(
                &entry.id,
                expected,
                actual,
                &expected.value,
                &actual.value,
                changes,
            ),
            (Some(expected), None) => changes.push(SurfaceChange {
                entry_id: entry.id.clone(),
                file: expected.file.clone(),
                pointer: expected.pointer.clone(),
                kind: SurfaceChangeKind::Removed,
            }),
            (None, Some(actual)) => changes.push(SurfaceChange {
                entry_id: entry.id.clone(),
                file: actual.file.clone(),
                pointer: actual.pointer.clone(),
                kind: SurfaceChangeKind::Added,
            }),
            (None, None) => {}
        }
    }
}

fn compare_json(
    entry_id: &str,
    expected_fragment: &ResolvedFragment,
    actual_fragment: &ResolvedFragment,
    expected: &Value,
    actual: &Value,
    changes: &mut Vec<SurfaceChange>,
) {
    match (expected, actual) {
        (Value::Object(expected_map), Value::Object(actual_map)) => {
            let keys = expected_map
                .keys()
                .chain(actual_map.keys())
                .collect::<BTreeSet<_>>();
            for key in keys {
                match (expected_map.get(key), actual_map.get(key)) {
                    (Some(expected_child), Some(actual_child)) => compare_json(
                        entry_id,
                        &expected_fragment.child(key),
                        &actual_fragment.child(key),
                        expected_child,
                        actual_child,
                        changes,
                    ),
                    (Some(_), None) => changes.push(SurfaceChange {
                        entry_id: entry_id.to_owned(),
                        file: expected_fragment.file.clone(),
                        pointer: expected_fragment.child_pointer(key),
                        kind: SurfaceChangeKind::Removed,
                    }),
                    (None, Some(_)) => changes.push(SurfaceChange {
                        entry_id: entry_id.to_owned(),
                        file: actual_fragment.file.clone(),
                        pointer: actual_fragment.child_pointer(key),
                        kind: SurfaceChangeKind::Added,
                    }),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(expected_items), Value::Array(actual_items)) => {
            let length = expected_items.len().max(actual_items.len());
            for index in 0..length {
                let token = index.to_string();
                match (expected_items.get(index), actual_items.get(index)) {
                    (Some(expected_child), Some(actual_child)) => compare_json(
                        entry_id,
                        &expected_fragment.child(&token),
                        &actual_fragment.child(&token),
                        expected_child,
                        actual_child,
                        changes,
                    ),
                    (Some(_), None) => changes.push(SurfaceChange {
                        entry_id: entry_id.to_owned(),
                        file: expected_fragment.file.clone(),
                        pointer: expected_fragment.child_pointer(&token),
                        kind: SurfaceChangeKind::Removed,
                    }),
                    (None, Some(_)) => changes.push(SurfaceChange {
                        entry_id: entry_id.to_owned(),
                        file: actual_fragment.file.clone(),
                        pointer: actual_fragment.child_pointer(&token),
                        kind: SurfaceChangeKind::Added,
                    }),
                    (None, None) => {}
                }
            }
        }
        _ if expected == actual => {}
        _ => changes.push(SurfaceChange {
            entry_id: entry_id.to_owned(),
            file: actual_fragment.file.clone(),
            pointer: actual_fragment.pointer.clone(),
            kind: SurfaceChangeKind::Changed,
        }),
    }
}

impl ResolvedFragment {
    fn child(&self, token: &str) -> Self {
        Self {
            file: self.file.clone(),
            pointer: self.child_pointer(token),
            value: Value::Null,
        }
    }

    fn child_pointer(&self, token: &str) -> String {
        format!("{}/{}", self.pointer, escape_pointer_token(token))
    }
}

fn collect_locations(resolved: &BTreeMap<String, ResolvedEntry>) -> Vec<SurfaceLocation> {
    let mut locations = Vec::new();
    for (entry_id, entry) in resolved {
        for fragment in entry.fragments.values() {
            locations.push(SurfaceLocation {
                entry_id: entry_id.clone(),
                file: fragment.file.clone(),
                pointer: fragment.pointer.clone(),
            });
        }
    }
    locations.sort_by(|left, right| {
        (&left.file, &left.pointer, &left.entry_id).cmp(&(
            &right.file,
            &right.pointer,
            &right.entry_id,
        ))
    });
    locations
}

fn digest_all(
    surface: &RequiredSurface,
    resolved: &BTreeMap<String, ResolvedEntry>,
) -> Result<ToolSurfaceDigests, RequiredSurfaceError> {
    let mut by_tool = BTreeMap::<String, BTreeMap<String, String>>::new();
    for entry in surface.entries() {
        let value = resolved.get(&entry.id).ok_or_else(|| {
            RequiredSurfaceError::InvalidConfig(format!(
                "internal resolution omitted digest entry `{}`",
                entry.id
            ))
        })?;
        by_tool
            .entry(entry.tool.as_str().to_owned())
            .or_default()
            .insert(entry.id.clone(), digest_entry(&entry.id, value)?);
    }
    Ok(by_tool)
}

fn digest_entry(id: &str, resolved: &ResolvedEntry) -> Result<String, RequiredSurfaceError> {
    let mut fragments = Map::new();
    for (key, fragment) in &resolved.fragments {
        fragments.insert(key.clone(), fragment.value.clone());
    }
    let mut missing_references = resolved
        .missing_references
        .iter()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    missing_references.sort_by(|left, right| left.as_str().cmp(&right.as_str()));

    let mut digest_input = Map::new();
    digest_input.insert("entry_id".to_owned(), Value::String(id.to_owned()));
    digest_input.insert(
        "missing_root".to_owned(),
        Value::Bool(resolved.missing_root),
    );
    digest_input.insert(
        "missing_references".to_owned(),
        Value::Array(missing_references),
    );
    digest_input.insert("fragments".to_owned(), Value::Object(fragments));

    let mut canonical = Vec::new();
    write_canonical_json(&Value::Object(digest_input), &mut canonical)?;
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(&canonical);
    let digest = hasher.finalize();
    let mut hexadecimal = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hexadecimal.push(hexadecimal_digit(byte >> 4));
        hexadecimal.push(hexadecimal_digit(byte & 0x0f));
    }
    Ok(format!("sha256:{hexadecimal}"))
}

fn hexadecimal_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'a' + (nibble - 10)),
        _ => '?',
    }
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<(), RequiredSurfaceError> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(string) => {
            let encoded =
                serde_json::to_string(string).map_err(RequiredSurfaceError::DigestSerialization)?;
            output.extend_from_slice(encoded.as_bytes());
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, child) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(child, output)?;
            }
            output.push(b']');
        }
        Value::Object(object) => {
            output.push(b'{');
            let sorted = object.iter().collect::<BTreeMap<_, _>>();
            for (index, (key, child)) in sorted.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                let encoded = serde_json::to_string(key)
                    .map_err(RequiredSurfaceError::DigestSerialization)?;
                output.extend_from_slice(encoded.as_bytes());
                output.push(b':');
                write_canonical_json(child, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use tempfile::tempdir;

    const CODEX_TOOL: &str = r#"
schema_version = 1

[[tools]]
name = "codex"
supported_range = ">=0.146.0, <0.147.0"
"#;

    fn config_with_entry(kind: &str, name: &str, reason: Option<&str>) -> String {
        let mut source = format!(
            r#"{CODEX_TOOL}
[[entries]]
id = "codex-entry"
tool = "codex"
kind = "{kind}"
name = "{name}"
"#
        );
        if let Some(reason) = reason {
            source.push_str(&format!("reason = \"{reason}\"\n"));
        }
        source
    }

    fn write_codex_document(root: &Path, value: &Value) {
        let path = root.join(CODEX_DOCUMENT);
        fs::create_dir_all(path.parent().expect("schema path must have a parent"))
            .expect("schema directory must be created");
        fs::write(
            path,
            serde_json::to_vec(value).expect("schema JSON must serialize"),
        )
        .expect("schema JSON must be written");
    }

    fn write_opencode_document(root: &Path, value: &Value) {
        let path = root.join(OPENCODE_DOCUMENT);
        fs::create_dir_all(path.parent().expect("schema path must have a parent"))
            .expect("schema directory must be created");
        fs::write(
            path,
            serde_json::to_vec(value).expect("schema JSON must serialize"),
        )
        .expect("schema JSON must be written");
    }

    fn method_branch(method: &str, title: &str) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "method": {"type": "string", "enum": [method]},
                "params": {"type": "object"}
            },
            "title": title
        })
    }

    #[test]
    fn missing_reason_is_rejected() {
        let error = parse_required_surface(&config_with_entry("type", "Foo", None))
            .expect_err("missing reason must fail closed");
        assert!(error.to_string().contains("missing `reason`"));
    }

    #[test]
    fn nonexistent_baseline_entry_is_rejected_with_its_id() {
        let surface = parse_required_surface(&config_with_entry(
            "type",
            "MissingType",
            Some("UACP lifecycle"),
        ))
        .expect("config must parse");
        let schemas = tempdir().expect("schema directory must be created");
        write_codex_document(
            schemas.path(),
            &serde_json::json!({"definitions":{"v2":{}}}),
        );

        let error =
            validate_baseline(&surface, schemas.path()).expect_err("missing entry must fail");
        let rendered = error.to_string();
        assert!(rendered.contains("codex-entry"));
        assert!(rendered.contains("MissingType"));
    }

    #[test]
    fn missing_observed_entry_is_reported_as_removed_surface_drift() {
        let surface = parse_required_surface(&config_with_entry(
            "type",
            "Lifecycle",
            Some("UACP lifecycle"),
        ))
        .expect("config must parse");
        let baseline = tempdir().expect("baseline must be created");
        let observed = tempdir().expect("observed must be created");
        write_codex_document(
            baseline.path(),
            &serde_json::json!({
                "definitions":{"Lifecycle":{"type":"object"},"v2":{}}
            }),
        );
        write_codex_document(
            observed.path(),
            &serde_json::json!({"definitions":{"v2":{}}}),
        );

        let comparison = compare_required_surface(&surface, baseline.path(), observed.path())
            .expect("observed removal is drift, not a config error");
        assert_eq!(
            comparison.changes(),
            &[SurfaceChange {
                entry_id: "codex-entry".to_owned(),
                file: CODEX_DOCUMENT.to_owned(),
                pointer: "#/definitions/Lifecycle".to_owned(),
                kind: SurfaceChangeKind::Removed,
            }]
        );
    }

    #[test]
    fn codex_method_union_reordering_is_not_surface_drift() {
        let surface = parse_required_surface(&config_with_entry(
            "method",
            "thread/start",
            Some("UACP session start"),
        ))
        .expect("config must parse");
        let baseline = tempdir().expect("baseline must be created");
        let observed = tempdir().expect("observed must be created");
        let target = method_branch("thread/start", "Thread/startRequest");
        let unrelated = method_branch("thread/archive", "Thread/archiveRequest");
        write_codex_document(
            baseline.path(),
            &serde_json::json!({
                "definitions":{
                    "ClientRequest":{"oneOf":[unrelated.clone(),target.clone()]},
                    "v2":{}
                }
            }),
        );
        write_codex_document(
            observed.path(),
            &serde_json::json!({
                "definitions":{
                    "ClientRequest":{"oneOf":[target,unrelated]},
                    "v2":{}
                }
            }),
        );

        let comparison = compare_required_surface(&surface, baseline.path(), observed.path())
            .expect("semantic method lookup must survive branch reordering");
        assert!(comparison.changes().is_empty());
        assert_eq!(comparison.baseline_digests(), comparison.observed_digests());
    }

    #[test]
    fn referenced_type_change_is_owned_by_the_root_entry() {
        let surface = parse_required_surface(&config_with_entry(
            "type",
            "Lifecycle",
            Some("UACP lifecycle"),
        ))
        .expect("config must parse");
        let baseline = tempdir().expect("baseline must be created");
        let observed = tempdir().expect("observed must be created");
        write_codex_document(
            baseline.path(),
            &serde_json::json!({
                "definitions":{
                    "Lifecycle":{"$ref":"#/definitions/v2/LifecycleState"},
                    "v2":{"LifecycleState":{"type":"string","const":"running"}}
                }
            }),
        );
        write_codex_document(
            observed.path(),
            &serde_json::json!({
                "definitions":{
                    "Lifecycle":{"$ref":"#/definitions/v2/LifecycleState"},
                    "v2":{"LifecycleState":{"type":"string","const":"completed"}}
                }
            }),
        );

        let comparison = compare_required_surface(&surface, baseline.path(), observed.path())
            .expect("referenced type must be compared");
        assert_eq!(
            comparison.changes(),
            &[SurfaceChange {
                entry_id: "codex-entry".to_owned(),
                file: CODEX_DOCUMENT.to_owned(),
                pointer: "#/definitions/v2/LifecycleState/const".to_owned(),
                kind: SurfaceChangeKind::Changed,
            }]
        );
        assert_ne!(comparison.baseline_digests(), comparison.observed_digests());
        assert_eq!(
            comparison.classify_full_change(
                CODEX_DOCUMENT,
                "#/definitions/v2/LifecycleState/const",
                SurfaceChangeKind::Changed,
            ),
            RawChangeDisposition::InSurface {
                entry_ids: vec!["codex-entry".to_owned()]
            }
        );
    }

    #[test]
    fn aggregate_method_refs_do_not_import_undeclared_payload_types() {
        let surface = parse_required_surface(
            r#"
schema_version = 1
[[tools]]
name = "opencode"
supported_range = "=1.18.8"
[[entries]]
id = "opencode.method.event"
tool = "opencode"
kind = "method"
name = "GET /event"
reason = "Required as the UACP structured event transport."
[[entries]]
id = "opencode.type.RequiredEvent"
tool = "opencode"
kind = "type"
name = "RequiredEvent"
reason = "Required by a UACP event variant."
"#,
        )
        .expect("config must parse");
        let baseline = tempdir().expect("baseline must be created");
        let outside = tempdir().expect("outside observation must be created");
        let inside = tempdir().expect("inside observation must be created");
        let document = |required: &str, unrelated: &str| {
            serde_json::json!({
                "paths": {
                    "/event": {
                        "get": {
                            "responses": {
                                "200": {
                                    "content": {
                                        "text/event-stream": {
                                            "schema": {"$ref": "#/components/schemas/Event"}
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                "components": {
                    "schemas": {
                        "Event": {
                            "anyOf": [
                                {"$ref": "#/components/schemas/RequiredEvent"},
                                {"$ref": "#/components/schemas/UnrelatedEvent"}
                            ]
                        },
                        "RequiredEvent": {"type": "string", "const": required},
                        "UnrelatedEvent": {"type": "string", "const": unrelated}
                    }
                }
            })
        };
        write_opencode_document(baseline.path(), &document("required", "before"));
        write_opencode_document(outside.path(), &document("required", "after"));
        write_opencode_document(inside.path(), &document("changed", "before"));

        let outside_comparison =
            compare_required_surface(&surface, baseline.path(), outside.path())
                .expect("undeclared payload change must remain outside the surface");
        assert!(outside_comparison.changes().is_empty());

        let inside_comparison = compare_required_surface(&surface, baseline.path(), inside.path())
            .expect("declared payload change must be reported");
        assert!(inside_comparison
            .changes()
            .iter()
            .any(|change| change.entry_id == "opencode.type.RequiredEvent"));
    }

    #[test]
    fn supported_range_is_queryable_without_becoming_a_gate() {
        let surface = parse_required_surface(&config_with_entry(
            "type",
            "Lifecycle",
            Some("UACP lifecycle"),
        ))
        .expect("config must parse");
        assert_eq!(
            surface.version_support(SurfaceTool::Codex, "0.146.2"),
            Some(VersionSupport::Supported)
        );
        assert_eq!(
            surface.version_support(SurfaceTool::Codex, "0.147.0"),
            Some(VersionSupport::Unverified {
                supported_range: ">=0.146.0, <0.147.0".to_owned()
            })
        );
    }

    #[test]
    fn repository_required_surface_resolves_against_the_committed_snapshot() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask must be inside the workspace");
        let surface =
            load_required_surface(workspace).expect("repository required surface must parse");

        validate_baseline(&surface, &workspace.join("schemas"))
            .expect("every provisional entry must resolve in the committed snapshot");
        let comparison = compare_required_surface(
            &surface,
            &workspace.join("schemas"),
            &workspace.join("schemas"),
        )
        .expect("the committed snapshot must compare with itself");
        assert!(comparison.changes().is_empty());
        assert_eq!(
            comparison
                .baseline_digests()
                .values()
                .map(BTreeMap::len)
                .sum::<usize>(),
            surface.entries().len()
        );
        assert_eq!(comparison.baseline_digests(), comparison.observed_digests());
    }
}
