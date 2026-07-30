use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};

use serde::Deserialize;

const RULES_RELATIVE_PATH: &str = "docs/dependency-rules.toml";
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyRules {
    pub schema_version: u32,
    pub coverage_exempt_paths: Vec<String>,
    pub crates: BTreeMap<String, CrateRule>,
    pub deny_edges: Vec<DenyEdgeRule>,
    pub exclusive_targets: Vec<ExclusiveTargetRule>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CrateRule {
    pub may_depend_on: Vec<String>,
    pub deny_dependencies: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DenyEdgeRule {
    pub rule: String,
    pub from: String,
    pub to: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExclusiveTargetRule {
    pub rule: String,
    pub to: String,
    pub allowed_from: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestrictedValue {
    String(String),
    Integer(u64),
    StringArray(Vec<String>),
}

impl RestrictedValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub const fn as_integer(&self) -> Option<u64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_string_array(&self) -> Option<&[String]> {
        match self {
            Self::StringArray(values) => Some(values),
            _ => None,
        }
    }
}

pub type RestrictedTable = BTreeMap<String, RestrictedValue>;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RestrictedPath(Vec<String>);

impl RestrictedPath {
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    fn from_segments(segments: impl IntoIterator<Item = String>) -> Self {
        Self(segments.into_iter().collect())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RestrictedToml {
    root: RestrictedTable,
    tables: BTreeMap<RestrictedPath, RestrictedTable>,
    array_tables: BTreeMap<RestrictedPath, Vec<RestrictedTable>>,
}

impl RestrictedToml {
    pub fn root(&self) -> &RestrictedTable {
        &self.root
    }

    pub fn table(&self, path: &[&str]) -> Option<&RestrictedTable> {
        let path = RestrictedPath::from_segments(path.iter().map(|segment| (*segment).to_owned()));
        self.tables.get(&path)
    }

    pub fn array_tables(&self, path: &[&str]) -> Option<&[RestrictedTable]> {
        let path = RestrictedPath::from_segments(path.iter().map(|segment| (*segment).to_owned()));
        self.array_tables.get(&path).map(Vec::as_slice)
    }

    pub fn tables(&self) -> impl Iterator<Item = (&RestrictedPath, &RestrictedTable)> {
        self.tables.iter()
    }

    pub fn all_array_tables(&self) -> impl Iterator<Item = (&RestrictedPath, &[RestrictedTable])> {
        self.array_tables
            .iter()
            .map(|(path, tables)| (path, tables.as_slice()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestrictedTomlError {
    pub line: usize,
    pub detail: String,
}

impl fmt::Display for RestrictedTomlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.detail)
    }
}

impl std::error::Error for RestrictedTomlError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct CargoMetadata {
    pub workspace_root: PathBuf,
    pub workspace_members: Vec<String>,
    pub packages: Vec<MetadataPackage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct MetadataPackage {
    pub id: String,
    pub name: String,
    pub manifest_path: PathBuf,
    #[serde(default)]
    pub dependencies: Vec<MetadataDependency>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct MetadataDependency {
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestInput {
    pub relative_path: PathBuf,
    pub source: String,
}

impl ManifestInput {
    pub fn new(relative_path: impl Into<PathBuf>, source: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            source: source.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ViolationRule {
    Matrix,
    AdapterIsolation,
    UiIsolation,
    DeclaredDeny,
    ForbiddenDependency,
    Coverage,
    WorkspaceMembership,
}

impl fmt::Display for ViolationRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Matrix => formatter.write_str("dependency matrix"),
            Self::AdapterIsolation => formatter.write_str("A-10 adapter isolation"),
            Self::UiIsolation => formatter.write_str("UI adapter isolation"),
            Self::DeclaredDeny => formatter.write_str("declared deny edge"),
            Self::ForbiddenDependency => formatter.write_str("forbidden dependency"),
            Self::Coverage => formatter.write_str("rule coverage"),
            Self::WorkspaceMembership => formatter.write_str("workspace membership"),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Violation {
    pub from: String,
    pub to: String,
    pub rule: ViolationRule,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} -> {}: {}: {}",
            self.from, self.to, self.rule, self.detail
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckReport {
    pub workspace_members: usize,
    pub internal_edges: usize,
    pub crate_manifests: usize,
}

impl fmt::Display for CheckReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} workspace member(s), {} internal edge(s), {} crates/* manifest(s)",
            self.workspace_members, self.internal_edges, self.crate_manifests
        )
    }
}

#[derive(Debug)]
pub enum DependencyCheckError {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    RulesToml(RestrictedTomlError),
    MetadataJson(serde_json::Error),
    MetadataCommand(ExitStatus),
    InvalidRules(String),
    InvalidMetadata(String),
    InvalidManifest {
        path: String,
        detail: String,
    },
    Violations(Vec<Violation>),
}

impl DependencyCheckError {
    pub fn violations(&self) -> Option<&[Violation]> {
        match self {
            Self::Violations(violations) => Some(violations),
            _ => None,
        }
    }
}

impl fmt::Display for DependencyCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::RulesToml(error) => write!(formatter, "invalid dependency rules TOML: {error}"),
            Self::MetadataJson(error) => write!(formatter, "invalid cargo metadata JSON: {error}"),
            Self::MetadataCommand(status) => {
                write!(formatter, "`cargo metadata` failed with {status}")
            }
            Self::InvalidRules(detail) => write!(formatter, "invalid dependency rules: {detail}"),
            Self::InvalidMetadata(detail) => write!(formatter, "invalid cargo metadata: {detail}"),
            Self::InvalidManifest { path, detail } => {
                write!(formatter, "invalid crate manifest `{path}`: {detail}")
            }
            Self::Violations(violations) => {
                writeln!(
                    formatter,
                    "dependency check found {} violation(s):",
                    violations.len()
                )?;
                for violation in violations {
                    writeln!(formatter, "{violation}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for DependencyCheckError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::RulesToml(error) => Some(error),
            Self::MetadataJson(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CurrentTomlSection {
    Root,
    Table(RestrictedPath),
    ArrayTable(RestrictedPath, usize),
}

pub fn parse_restricted_toml(source: &str) -> Result<RestrictedToml, RestrictedTomlError> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut document = RestrictedToml::default();
    let mut current = CurrentTomlSection::Root;
    let mut line_index = 0_usize;

    while let Some(raw_line) = lines.get(line_index) {
        let line_number = line_index + 1;
        let without_comment = strip_toml_comment(raw_line, line_number)?;
        let statement = without_comment.trim();
        if statement.is_empty() {
            line_index += 1;
            continue;
        }

        if statement.starts_with('[') {
            let (path, is_array) = parse_table_header(statement, line_number)?;
            if is_array {
                if document.tables.contains_key(&path) {
                    return Err(toml_error(
                        line_number,
                        "table path is already used as a regular table",
                    ));
                }
                let tables = document.array_tables.entry(path.clone()).or_default();
                tables.push(RestrictedTable::new());
                current = CurrentTomlSection::ArrayTable(path, tables.len() - 1);
            } else {
                if document.tables.contains_key(&path) || document.array_tables.contains_key(&path)
                {
                    return Err(toml_error(line_number, "duplicate table declaration"));
                }
                document.tables.insert(path.clone(), RestrictedTable::new());
                current = CurrentTomlSection::Table(path);
            }
            line_index += 1;
            continue;
        }

        let (key, initial_value) = split_assignment(statement, line_number)?;
        let mut value_source = initial_value.to_owned();
        if initial_value.trim_start().starts_with('[') {
            while !array_value_is_complete(&value_source, line_number)? {
                line_index += 1;
                let Some(next_line) = lines.get(line_index) else {
                    return Err(toml_error(line_number, "unterminated string array"));
                };
                let next_line = strip_toml_comment(next_line, line_index + 1)?;
                value_source.push('\n');
                value_source.push_str(next_line.trim());
            }
        }
        let value = parse_restricted_value(&value_source, line_number)?;
        let table = current_table_mut(&mut document, &current).ok_or_else(|| {
            toml_error(
                line_number,
                "internal parser state did not identify the current table",
            )
        })?;
        if table.insert(key.to_owned(), value).is_some() {
            return Err(toml_error(
                line_number,
                &format!("duplicate key `{key}` in the same table"),
            ));
        }
        line_index += 1;
    }
    Ok(document)
}

fn current_table_mut<'a>(
    document: &'a mut RestrictedToml,
    current: &CurrentTomlSection,
) -> Option<&'a mut RestrictedTable> {
    match current {
        CurrentTomlSection::Root => Some(&mut document.root),
        CurrentTomlSection::Table(path) => document.tables.get_mut(path),
        CurrentTomlSection::ArrayTable(path, index) => document
            .array_tables
            .get_mut(path)
            .and_then(|tables| tables.get_mut(*index)),
    }
}

fn strip_toml_comment(line: &str, line_number: usize) -> Result<&str, RestrictedTomlError> {
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
        } else if character == '#' {
            return line
                .get(..offset)
                .ok_or_else(|| toml_error(line_number, "comment boundary was not valid UTF-8"));
        }
    }
    if in_string {
        return Err(toml_error(line_number, "unterminated basic string"));
    }
    Ok(line)
}

fn parse_table_header(
    statement: &str,
    line_number: usize,
) -> Result<(RestrictedPath, bool), RestrictedTomlError> {
    let is_array = statement.starts_with("[[");
    let (opening, closing) = if is_array { ("[[", "]]") } else { ("[", "]") };
    if !statement.ends_with(closing) {
        return Err(toml_error(line_number, "malformed table header"));
    }
    let inner = statement
        .strip_prefix(opening)
        .and_then(|value| value.strip_suffix(closing))
        .ok_or_else(|| toml_error(line_number, "malformed table header"))?
        .trim();
    if inner.is_empty() {
        return Err(toml_error(line_number, "table path must not be empty"));
    }

    let mut raw_segments = Vec::new();
    let mut segment_start = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in inner.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
        } else if character == '.' {
            let segment = inner
                .get(segment_start..offset)
                .ok_or_else(|| toml_error(line_number, "table segment was not valid UTF-8"))?;
            raw_segments.push(segment.trim());
            segment_start = offset + character.len_utf8();
        }
    }
    if in_string {
        return Err(toml_error(
            line_number,
            "unterminated quoted table path segment",
        ));
    }
    let final_segment = inner
        .get(segment_start..)
        .ok_or_else(|| toml_error(line_number, "table segment was not valid UTF-8"))?;
    raw_segments.push(final_segment.trim());

    let mut segments = Vec::with_capacity(raw_segments.len());
    for raw_segment in raw_segments {
        if raw_segment.is_empty() {
            return Err(toml_error(
                line_number,
                "table path contains an empty segment",
            ));
        }
        let segment = if raw_segment.starts_with('"') {
            parse_basic_string(raw_segment, line_number)?
        } else {
            if !raw_segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(toml_error(
                    line_number,
                    "bare table path segments may contain only ASCII letters, digits, `_`, or `-`",
                ));
            }
            raw_segment.to_owned()
        };
        if segment.is_empty() {
            return Err(toml_error(
                line_number,
                "table path contains an empty segment",
            ));
        }
        segments.push(segment);
    }
    Ok((RestrictedPath::from_segments(segments), is_array))
}

fn split_assignment(
    statement: &str,
    line_number: usize,
) -> Result<(&str, &str), RestrictedTomlError> {
    let mut in_string = false;
    let mut escaped = false;
    let mut delimiter = None;
    for (offset, character) in statement.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
        } else if character == '=' {
            delimiter = Some(offset);
            break;
        }
    }
    let delimiter =
        delimiter.ok_or_else(|| toml_error(line_number, "expected `key = value` assignment"))?;
    let key = statement
        .get(..delimiter)
        .ok_or_else(|| toml_error(line_number, "assignment key was not valid UTF-8"))?
        .trim();
    let value = statement
        .get(delimiter + 1..)
        .ok_or_else(|| toml_error(line_number, "assignment value was not valid UTF-8"))?
        .trim();
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(toml_error(
            line_number,
            "assignment keys may contain only ASCII letters, digits, `_`, or `-`",
        ));
    }
    if value.is_empty() {
        return Err(toml_error(
            line_number,
            "assignment value must not be empty",
        ));
    }
    Ok((key, value))
}

fn array_value_is_complete(source: &str, line_number: usize) -> Result<bool, RestrictedTomlError> {
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0_u32;
    let mut saw_open = false;
    for character in source.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
        } else if character == '[' {
            saw_open = true;
            depth = depth.saturating_add(1);
        } else if character == ']' {
            let Some(next_depth) = depth.checked_sub(1) else {
                return Err(toml_error(line_number, "unexpected `]` in array"));
            };
            depth = next_depth;
        }
    }
    if in_string {
        return Err(toml_error(line_number, "unterminated basic string"));
    }
    Ok(saw_open && depth == 0)
}

fn parse_restricted_value(
    source: &str,
    line_number: usize,
) -> Result<RestrictedValue, RestrictedTomlError> {
    let source = source.trim();
    if source.starts_with('"') {
        return parse_basic_string(source, line_number).map(RestrictedValue::String);
    }
    if source.starts_with('[') {
        return parse_string_array(source, line_number).map(RestrictedValue::StringArray);
    }
    if source.bytes().all(|byte| byte.is_ascii_digit()) {
        let value = source
            .parse::<u64>()
            .map_err(|error| toml_error(line_number, &format!("invalid integer: {error}")))?;
        return Ok(RestrictedValue::Integer(value));
    }
    Err(toml_error(
        line_number,
        "only basic strings, non-negative integers, and string arrays are supported",
    ))
}

fn parse_basic_string(source: &str, line_number: usize) -> Result<String, RestrictedTomlError> {
    if !source.starts_with('"') || !source.ends_with('"') || source.len() < 2 {
        return Err(toml_error(line_number, "malformed basic string"));
    }
    let inner = source
        .get(1..source.len() - 1)
        .ok_or_else(|| toml_error(line_number, "basic string boundary was not valid UTF-8"))?;
    let mut output = String::new();
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let escaped = characters
            .next()
            .ok_or_else(|| toml_error(line_number, "basic string ends with an escape"))?;
        match escaped {
            '"' => output.push('"'),
            '\\' => output.push('\\'),
            'b' => output.push('\u{0008}'),
            't' => output.push('\t'),
            'n' => output.push('\n'),
            'f' => output.push('\u{000c}'),
            'r' => output.push('\r'),
            'u' => output.push(parse_unicode_escape(&mut characters, 4, line_number)?),
            'U' => output.push(parse_unicode_escape(&mut characters, 8, line_number)?),
            other => {
                return Err(toml_error(
                    line_number,
                    &format!("unsupported basic string escape `\\{other}`"),
                ));
            }
        }
    }
    Ok(output)
}

fn parse_unicode_escape(
    characters: &mut impl Iterator<Item = char>,
    digits: usize,
    line_number: usize,
) -> Result<char, RestrictedTomlError> {
    let mut value = 0_u32;
    for _ in 0..digits {
        let digit = characters
            .next()
            .and_then(|character| character.to_digit(16))
            .ok_or_else(|| toml_error(line_number, "invalid Unicode escape"))?;
        value = value
            .checked_mul(16)
            .and_then(|current| current.checked_add(digit))
            .ok_or_else(|| toml_error(line_number, "Unicode escape overflowed"))?;
    }
    char::from_u32(value).ok_or_else(|| toml_error(line_number, "invalid Unicode scalar value"))
}

fn parse_string_array(
    source: &str,
    line_number: usize,
) -> Result<Vec<String>, RestrictedTomlError> {
    let inner = source
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| toml_error(line_number, "malformed string array"))?;
    let mut values = Vec::new();
    let mut offset = 0_usize;
    while let Some(remaining) = inner.get(offset..) {
        let trimmed = remaining.trim_start();
        offset += remaining.len().saturating_sub(trimmed.len());
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with(',') {
            return Err(toml_error(
                line_number,
                "string array contains an empty item",
            ));
        }
        if !trimmed.starts_with('"') {
            return Err(toml_error(
                line_number,
                "string arrays may contain only basic strings",
            ));
        }
        let string_end = find_basic_string_end(trimmed, line_number)?;
        let raw_string = trimmed
            .get(..=string_end)
            .ok_or_else(|| toml_error(line_number, "array string boundary was not valid UTF-8"))?;
        values.push(parse_basic_string(raw_string, line_number)?);
        offset += string_end + 1;

        let remaining = inner
            .get(offset..)
            .ok_or_else(|| toml_error(line_number, "array boundary was not valid UTF-8"))?;
        let trimmed = remaining.trim_start();
        offset += remaining.len().saturating_sub(trimmed.len());
        if trimmed.is_empty() {
            break;
        }
        let Some(after_comma) = trimmed.strip_prefix(',') else {
            return Err(toml_error(
                line_number,
                "string array items must be separated by commas",
            ));
        };
        offset += trimmed.len().saturating_sub(after_comma.len());
        if after_comma.trim().is_empty() {
            break;
        }
    }
    Ok(values)
}

fn find_basic_string_end(source: &str, line_number: usize) -> Result<usize, RestrictedTomlError> {
    let mut escaped = false;
    for (offset, character) in source.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Ok(offset);
        }
    }
    Err(toml_error(line_number, "unterminated array string"))
}

fn toml_error(line: usize, detail: &str) -> RestrictedTomlError {
    RestrictedTomlError {
        line,
        detail: detail.to_owned(),
    }
}

pub fn parse_rules(source: &str) -> Result<DependencyRules, DependencyCheckError> {
    let document = parse_restricted_toml(source).map_err(DependencyCheckError::RulesToml)?;
    let rules = dependency_rules_from_document(&document)?;
    validate_rules(&rules)?;
    Ok(rules)
}

pub fn parse_metadata(source: &str) -> Result<CargoMetadata, DependencyCheckError> {
    let metadata = serde_json::from_str(source).map_err(DependencyCheckError::MetadataJson)?;
    validate_metadata(&metadata)?;
    Ok(metadata)
}

pub fn load_rules(path: &Path) -> Result<DependencyRules, DependencyCheckError> {
    let source = fs::read_to_string(path).map_err(|source| DependencyCheckError::Io {
        operation: "could not read dependency rules",
        source,
    })?;
    parse_rules(&source)
}

pub fn load_metadata(workspace_root: &Path) -> Result<CargoMetadata, DependencyCheckError> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root)
        .output()
        .map_err(|source| DependencyCheckError::Io {
            operation: "could not start cargo metadata",
            source,
        })?;
    if !output.status.success() {
        return Err(DependencyCheckError::MetadataCommand(output.status));
    }
    let source = String::from_utf8(output.stdout).map_err(|error| {
        DependencyCheckError::InvalidMetadata(format!(
            "cargo metadata stdout was not UTF-8: {error}"
        ))
    })?;
    parse_metadata(&source)
}

pub fn discover_crate_manifests(
    workspace_root: &Path,
) -> Result<Vec<ManifestInput>, DependencyCheckError> {
    let crates_root = workspace_root.join("crates");
    let entries = match fs::read_dir(&crates_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DependencyCheckError::Io {
                operation: "could not enumerate crates directory",
                source,
            });
        }
    };
    let mut entries =
        entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| DependencyCheckError::Io {
                operation: "could not enumerate crates directory",
                source,
            })?;
    entries.sort_by_key(fs::DirEntry::file_name);

    let mut manifests = Vec::new();
    for entry in entries {
        let file_type = entry
            .file_type()
            .map_err(|source| DependencyCheckError::Io {
                operation: "could not inspect crates directory entry",
                source,
            })?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let manifest_path = entry.path().join("Cargo.toml");
        match fs::metadata(&manifest_path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(DependencyCheckError::Io {
                    operation: "could not inspect crate manifest",
                    source,
                });
            }
        }
        let source =
            fs::read_to_string(&manifest_path).map_err(|source| DependencyCheckError::Io {
                operation: "could not read crate manifest",
                source,
            })?;
        let relative_path = manifest_path
            .strip_prefix(workspace_root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                DependencyCheckError::InvalidMetadata(
                    "discovered crate manifest escaped the workspace root".to_owned(),
                )
            })?;
        manifests.push(ManifestInput::new(relative_path, source));
    }
    Ok(manifests)
}

pub fn check_workspace(workspace_root: &Path) -> Result<CheckReport, DependencyCheckError> {
    let rules = load_rules(&workspace_root.join(RULES_RELATIVE_PATH))?;
    let metadata = load_metadata(workspace_root)?;
    let manifests = discover_crate_manifests(workspace_root)?;
    check_parsed_inputs(&rules, &metadata, &manifests)
}

pub fn check_with_inputs(
    rules_toml: &str,
    metadata_json: &str,
    crate_manifests: &[ManifestInput],
) -> Result<CheckReport, DependencyCheckError> {
    let rules = parse_rules(rules_toml)?;
    let metadata = parse_metadata(metadata_json)?;
    check_parsed_inputs(&rules, &metadata, crate_manifests)
}

pub fn check_parsed_inputs(
    rules: &DependencyRules,
    metadata: &CargoMetadata,
    crate_manifests: &[ManifestInput],
) -> Result<CheckReport, DependencyCheckError> {
    validate_rules(rules)?;
    validate_metadata(metadata)?;

    let workspace_packages = workspace_packages(metadata)?;
    let workspace_names = workspace_packages
        .values()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut packages_by_name = BTreeMap::new();
    for package in workspace_packages.values() {
        if packages_by_name
            .insert(package.name.as_str(), *package)
            .is_some()
        {
            return Err(DependencyCheckError::InvalidMetadata(format!(
                "workspace package name `{}` is not unique",
                package.name
            )));
        }
    }

    let mut violations = Vec::new();
    let mut internal_edges = 0_usize;
    for package in workspace_packages.values() {
        let relative_manifest = relative_manifest_path(metadata, &package.manifest_path)?;
        let crate_directory = relative_manifest
            .parent()
            .map(normalize_path)
            .unwrap_or_default();
        let rule = rules.crates.get(&package.name);
        let coverage_exempt = rules
            .coverage_exempt_paths
            .iter()
            .any(|pattern| wildcard_matches(pattern, &crate_directory));
        if rule.is_none() && !coverage_exempt {
            violations.push(Violation {
                from: package.name.clone(),
                to: "<dependency-rules>".to_owned(),
                rule: ViolationRule::Coverage,
                detail: format!(
                    "`{}` exists in the workspace but has no crate declaration",
                    normalize_path(&relative_manifest)
                ),
            });
        }

        for dependency in &package.dependencies {
            if let Some(rule) = rule {
                for pattern in &rule.deny_dependencies {
                    if wildcard_matches(pattern, &dependency.name) {
                        violations.push(Violation {
                            from: package.name.clone(),
                            to: dependency.name.clone(),
                            rule: ViolationRule::ForbiddenDependency,
                            detail: format!("dependency name matches deny pattern `{pattern}`"),
                        });
                    }
                }
            }
            for deny_edge in &rules.deny_edges {
                if wildcard_matches(&deny_edge.from, &package.name)
                    && wildcard_matches(&deny_edge.to, &dependency.name)
                {
                    let rule = match deny_edge.rule.as_str() {
                        "A-10" => ViolationRule::AdapterIsolation,
                        "UI-ADAPTER" => ViolationRule::UiIsolation,
                        _ => ViolationRule::DeclaredDeny,
                    };
                    violations.push(Violation {
                        from: package.name.clone(),
                        to: dependency.name.clone(),
                        rule,
                        detail: format!("{}: {}", deny_edge.rule, deny_edge.reason),
                    });
                }
            }
            for exclusive_target in &rules.exclusive_targets {
                let source_is_allowed = exclusive_target
                    .allowed_from
                    .iter()
                    .any(|pattern| wildcard_matches(pattern, &package.name));
                if wildcard_matches(&exclusive_target.to, &dependency.name) && !source_is_allowed {
                    violations.push(Violation {
                        from: package.name.clone(),
                        to: dependency.name.clone(),
                        rule: ViolationRule::UiIsolation,
                        detail: format!("{}: {}", exclusive_target.rule, exclusive_target.reason),
                    });
                }
            }

            if !workspace_names.contains(dependency.name.as_str()) {
                continue;
            }
            internal_edges += 1;
            let is_allowed = rule.is_some_and(|rule| {
                rule.may_depend_on
                    .iter()
                    .any(|pattern| wildcard_matches(pattern, &dependency.name))
            });
            if !is_allowed {
                violations.push(Violation {
                    from: package.name.clone(),
                    to: dependency.name.clone(),
                    rule: ViolationRule::Matrix,
                    detail: "workspace-internal edge is not listed in `may_depend_on`".to_owned(),
                });
            }
        }
    }

    let workspace_manifest_keys = workspace_packages
        .values()
        .map(|package| {
            relative_manifest_path(metadata, &package.manifest_path)
                .map(|path| (package.name.as_str(), normalize_path(&path)))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for manifest in crate_manifests {
        let manifest_path = normalize_relative_input_path(&manifest.relative_path)?;
        let crate_name = manifest_package_name(&manifest.source, &manifest_path)?;
        if !rules.crates.contains_key(&crate_name) {
            violations.push(Violation {
                from: crate_name.clone(),
                to: "<dependency-rules>".to_owned(),
                rule: ViolationRule::Coverage,
                detail: format!(
                    "`{manifest_path}` exists under crates/ but has no crate declaration"
                ),
            });
        }
        if !workspace_manifest_keys.contains(&(crate_name.as_str(), manifest_path.clone())) {
            violations.push(Violation {
                from: crate_name,
                to: "<cargo-workspace>".to_owned(),
                rule: ViolationRule::WorkspaceMembership,
                detail: format!(
                    "`{manifest_path}` exists under crates/ but is absent from cargo metadata"
                ),
            });
        }
    }

    violations.sort();
    violations.dedup();
    if !violations.is_empty() {
        return Err(DependencyCheckError::Violations(violations));
    }
    Ok(CheckReport {
        workspace_members: workspace_packages.len(),
        internal_edges,
        crate_manifests: crate_manifests.len(),
    })
}

fn dependency_rules_from_document(
    document: &RestrictedToml,
) -> Result<DependencyRules, DependencyCheckError> {
    reject_unknown_keys(
        document.root(),
        &["schema_version", "coverage_exempt_paths"],
        "root table",
    )?;
    let schema_version = required_integer(document.root(), "schema_version", "root table")?;
    let schema_version = u32::try_from(schema_version).map_err(|_| {
        DependencyCheckError::InvalidRules("schema_version does not fit in u32".to_owned())
    })?;
    let coverage_exempt_paths =
        required_string_array(document.root(), "coverage_exempt_paths", "root table")?;

    let mut crates = BTreeMap::new();
    for (path, table) in document.tables() {
        match path.segments() {
            [scope, crate_name] if scope == "crates" => {
                reject_unknown_keys(
                    table,
                    &["may_depend_on", "deny_dependencies"],
                    &format!("crate `{crate_name}`"),
                )?;
                let may_depend_on = required_string_array(
                    table,
                    "may_depend_on",
                    &format!("crate `{crate_name}`"),
                )?;
                let deny_dependencies = optional_string_array(
                    table,
                    "deny_dependencies",
                    &format!("crate `{crate_name}`"),
                )?;
                if crates
                    .insert(
                        crate_name.clone(),
                        CrateRule {
                            may_depend_on,
                            deny_dependencies,
                        },
                    )
                    .is_some()
                {
                    return Err(DependencyCheckError::InvalidRules(format!(
                        "duplicate crate declaration `{crate_name}`"
                    )));
                }
            }
            [scope, rule]
                if scope == "antipatterns"
                    && matches!(rule.as_str(), "a1" | "a2" | "a4" | "a6" | "a11") => {}
            _ => {
                return Err(DependencyCheckError::InvalidRules(format!(
                    "unsupported table `[{}]`",
                    path.segments().join(".")
                )));
            }
        }
    }

    let mut deny_edges = Vec::new();
    let mut exclusive_targets = Vec::new();
    for (path, tables) in document.all_array_tables() {
        match path.segments() {
            [name] if name == "deny_edges" => {
                for table in tables {
                    reject_unknown_keys(
                        table,
                        &["rule", "from", "to", "reason"],
                        "[[deny_edges]]",
                    )?;
                    deny_edges.push(DenyEdgeRule {
                        rule: required_string(table, "rule", "[[deny_edges]]")?,
                        from: required_string(table, "from", "[[deny_edges]]")?,
                        to: required_string(table, "to", "[[deny_edges]]")?,
                        reason: required_string(table, "reason", "[[deny_edges]]")?,
                    });
                }
            }
            [name] if name == "exclusive_targets" => {
                for table in tables {
                    reject_unknown_keys(
                        table,
                        &["rule", "to", "allowed_from", "reason"],
                        "[[exclusive_targets]]",
                    )?;
                    exclusive_targets.push(ExclusiveTargetRule {
                        rule: required_string(table, "rule", "[[exclusive_targets]]")?,
                        to: required_string(table, "to", "[[exclusive_targets]]")?,
                        allowed_from: required_string_array(
                            table,
                            "allowed_from",
                            "[[exclusive_targets]]",
                        )?,
                        reason: required_string(table, "reason", "[[exclusive_targets]]")?,
                    });
                }
            }
            [scope, rule, sources]
                if scope == "antipatterns" && rule == "a6" && sources == "sources" => {}
            _ => {
                return Err(DependencyCheckError::InvalidRules(format!(
                    "unsupported array table `[[{}]]`",
                    path.segments().join(".")
                )));
            }
        }
    }

    Ok(DependencyRules {
        schema_version,
        coverage_exempt_paths,
        crates,
        deny_edges,
        exclusive_targets,
    })
}

fn reject_unknown_keys(
    table: &RestrictedTable,
    allowed: &[&str],
    context: &str,
) -> Result<(), DependencyCheckError> {
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(DependencyCheckError::InvalidRules(format!(
            "{context} contains unsupported key `{key}`"
        )));
    }
    Ok(())
}

fn required_integer(
    table: &RestrictedTable,
    key: &str,
    context: &str,
) -> Result<u64, DependencyCheckError> {
    table
        .get(key)
        .and_then(RestrictedValue::as_integer)
        .ok_or_else(|| {
            DependencyCheckError::InvalidRules(format!("{context} requires integer key `{key}`"))
        })
}

fn required_string(
    table: &RestrictedTable,
    key: &str,
    context: &str,
) -> Result<String, DependencyCheckError> {
    table
        .get(key)
        .and_then(RestrictedValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            DependencyCheckError::InvalidRules(format!(
                "{context} requires non-empty string key `{key}`"
            ))
        })
}

fn required_string_array(
    table: &RestrictedTable,
    key: &str,
    context: &str,
) -> Result<Vec<String>, DependencyCheckError> {
    table
        .get(key)
        .and_then(RestrictedValue::as_string_array)
        .map(<[String]>::to_vec)
        .ok_or_else(|| {
            DependencyCheckError::InvalidRules(format!(
                "{context} requires string-array key `{key}`"
            ))
        })
}

fn optional_string_array(
    table: &RestrictedTable,
    key: &str,
    context: &str,
) -> Result<Vec<String>, DependencyCheckError> {
    match table.get(key) {
        None => Ok(Vec::new()),
        Some(value) => value
            .as_string_array()
            .map(<[String]>::to_vec)
            .ok_or_else(|| {
                DependencyCheckError::InvalidRules(format!(
                    "{context} key `{key}` must be a string array"
                ))
            }),
    }
}

fn validate_rules(rules: &DependencyRules) -> Result<(), DependencyCheckError> {
    if rules.schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(DependencyCheckError::InvalidRules(format!(
            "schema_version {} is unsupported; expected {SUPPORTED_SCHEMA_VERSION}",
            rules.schema_version
        )));
    }
    if rules.crates.is_empty() {
        return Err(DependencyCheckError::InvalidRules(
            "at least one crate declaration is required".to_owned(),
        ));
    }
    for path_pattern in &rules.coverage_exempt_paths {
        validate_relative_pattern(path_pattern, "coverage exemption")?;
    }
    for (crate_name, rule) in &rules.crates {
        if crate_name.is_empty() || crate_name.contains('*') {
            return Err(DependencyCheckError::InvalidRules(format!(
                "crate declaration `{crate_name}` must be a non-empty exact package name"
            )));
        }
        for pattern in rule
            .may_depend_on
            .iter()
            .chain(rule.deny_dependencies.iter())
        {
            if pattern.is_empty() {
                return Err(DependencyCheckError::InvalidRules(format!(
                    "crate `{crate_name}` contains an empty dependency pattern"
                )));
            }
        }
        for allowed in &rule.may_depend_on {
            if !rules
                .crates
                .keys()
                .any(|candidate| wildcard_matches(allowed, candidate))
            {
                return Err(DependencyCheckError::InvalidRules(format!(
                    "crate `{crate_name}` allows `{allowed}`, which matches no declared crate"
                )));
            }
        }
    }
    for deny_edge in &rules.deny_edges {
        if deny_edge.rule.is_empty()
            || deny_edge.from.is_empty()
            || deny_edge.to.is_empty()
            || deny_edge.reason.is_empty()
        {
            return Err(DependencyCheckError::InvalidRules(
                "deny edge rule/from/to/reason must all be non-empty".to_owned(),
            ));
        }
        if !rules
            .crates
            .keys()
            .any(|crate_name| wildcard_matches(&deny_edge.from, crate_name))
        {
            return Err(DependencyCheckError::InvalidRules(format!(
                "deny edge source `{}` matches no declared crate",
                deny_edge.from
            )));
        }
    }
    for exclusive_target in &rules.exclusive_targets {
        if exclusive_target.rule.is_empty()
            || exclusive_target.to.is_empty()
            || exclusive_target.allowed_from.is_empty()
            || exclusive_target.reason.is_empty()
            || exclusive_target.allowed_from.iter().any(String::is_empty)
        {
            return Err(DependencyCheckError::InvalidRules(
                "exclusive target rule/to/allowed_from/reason must all be non-empty".to_owned(),
            ));
        }
        if !rules
            .crates
            .keys()
            .any(|crate_name| wildcard_matches(&exclusive_target.to, crate_name))
        {
            return Err(DependencyCheckError::InvalidRules(format!(
                "exclusive target `{}` matches no declared crate",
                exclusive_target.to
            )));
        }
        for allowed_source in &exclusive_target.allowed_from {
            if !rules
                .crates
                .keys()
                .any(|crate_name| wildcard_matches(allowed_source, crate_name))
            {
                return Err(DependencyCheckError::InvalidRules(format!(
                    "exclusive target allowed source `{allowed_source}` matches no declared crate"
                )));
            }
        }
    }
    Ok(())
}

fn validate_relative_pattern(
    pattern: &str,
    label: &'static str,
) -> Result<(), DependencyCheckError> {
    if pattern.is_empty() {
        return Err(DependencyCheckError::InvalidRules(format!(
            "{label} must not be empty"
        )));
    }
    let path = Path::new(pattern);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(DependencyCheckError::InvalidRules(format!(
            "{label} `{pattern}` must be a relative in-workspace pattern"
        )));
    }
    Ok(())
}

fn validate_metadata(metadata: &CargoMetadata) -> Result<(), DependencyCheckError> {
    if metadata.workspace_root.as_os_str().is_empty() {
        return Err(DependencyCheckError::InvalidMetadata(
            "workspace_root must not be empty".to_owned(),
        ));
    }
    if metadata.workspace_members.iter().any(String::is_empty) {
        return Err(DependencyCheckError::InvalidMetadata(
            "workspace member IDs must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn workspace_packages(
    metadata: &CargoMetadata,
) -> Result<BTreeMap<&str, &MetadataPackage>, DependencyCheckError> {
    let members = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if members.len() != metadata.workspace_members.len() {
        return Err(DependencyCheckError::InvalidMetadata(
            "workspace_members contains duplicate package IDs".to_owned(),
        ));
    }
    let mut packages = BTreeMap::new();
    for package in &metadata.packages {
        if members.contains(package.id.as_str()) {
            packages.insert(package.id.as_str(), package);
        }
    }
    for member in members {
        if !packages.contains_key(member) {
            return Err(DependencyCheckError::InvalidMetadata(format!(
                "workspace member `{member}` has no package record"
            )));
        }
    }
    Ok(packages)
}

fn relative_manifest_path(
    metadata: &CargoMetadata,
    manifest_path: &Path,
) -> Result<PathBuf, DependencyCheckError> {
    manifest_path
        .strip_prefix(&metadata.workspace_root)
        .map(Path::to_path_buf)
        .map_err(|_| {
            DependencyCheckError::InvalidMetadata(
                "workspace package manifest escaped workspace_root".to_owned(),
            )
        })
}

fn manifest_package_name(
    source: &str,
    relative_path: &str,
) -> Result<String, DependencyCheckError> {
    let mut in_package = false;
    let mut saw_package = false;
    let mut package_name = None;
    for (line_index, raw_line) in source.lines().enumerate() {
        let line_number = line_index + 1;
        let line = strip_toml_comment(raw_line, line_number)
            .map_err(|error| DependencyCheckError::InvalidManifest {
                path: relative_path.to_owned(),
                detail: error.to_string(),
            })?
            .trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_package = line == "[package]";
            if in_package {
                if saw_package {
                    return Err(DependencyCheckError::InvalidManifest {
                        path: relative_path.to_owned(),
                        detail: "duplicate [package] table".to_owned(),
                    });
                }
                saw_package = true;
            }
            continue;
        }
        if !in_package {
            continue;
        }
        let (key, value) = split_assignment(line, line_number).map_err(|error| {
            DependencyCheckError::InvalidManifest {
                path: relative_path.to_owned(),
                detail: error.to_string(),
            }
        })?;
        if key != "name" {
            continue;
        }
        if package_name.is_some() {
            return Err(DependencyCheckError::InvalidManifest {
                path: relative_path.to_owned(),
                detail: "duplicate package.name".to_owned(),
            });
        }
        let name = parse_basic_string(value, line_number).map_err(|error| {
            DependencyCheckError::InvalidManifest {
                path: relative_path.to_owned(),
                detail: error.to_string(),
            }
        })?;
        if name.is_empty() {
            return Err(DependencyCheckError::InvalidManifest {
                path: relative_path.to_owned(),
                detail: "package.name must not be empty".to_owned(),
            });
        }
        package_name = Some(name);
    }
    if !saw_package {
        return Err(DependencyCheckError::InvalidManifest {
            path: relative_path.to_owned(),
            detail: "missing [package] table".to_owned(),
        });
    }
    package_name.ok_or_else(|| DependencyCheckError::InvalidManifest {
        path: relative_path.to_owned(),
        detail: "missing package.name".to_owned(),
    })
}

fn normalize_relative_input_path(path: &Path) -> Result<String, DependencyCheckError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(DependencyCheckError::InvalidManifest {
            path: "<external>".to_owned(),
            detail: "manifest input path must be relative to the workspace".to_owned(),
        });
    }
    Ok(normalize_path(path))
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut pattern_index = 0_usize;
    let mut value_index = 0_usize;
    let mut star_index = None;
    let mut star_value_index = 0_usize;

    while value_index < value.len() {
        let pattern_byte = pattern.get(pattern_index).copied();
        let value_byte = value.get(value_index).copied();
        if pattern_byte == value_byte {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_byte == Some(b'*') {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_value_index = value_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return false;
        }
    }
    while pattern.get(pattern_index) == Some(&b'*') {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}
