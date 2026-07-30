use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};

use proc_macro2::{TokenStream, TokenTree};
use serde::Deserialize;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    BinOp, ExprBinary, ExprLet, ExprMacro, ExprMatch, ExprMethodCall, File, ItemEnum, ItemStruct,
    ItemType, ItemUnion, LitByteStr, LitStr,
};
use thiserror::Error;

use crate::deps::{
    parse_restricted_toml, RestrictedTable, RestrictedToml, RestrictedTomlError, RestrictedValue,
};

const RULES_PATH: &str = "docs/dependency-rules.toml";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Violation {
    pub path: PathBuf,
    pub line: usize,
    pub rule: String,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}: {}: {}",
            self.path.to_string_lossy().replace('\\', "/"),
            self.line,
            self.rule,
            self.detail
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanReport {
    pub violations: Vec<Violation>,
    pub agent_name_branch_exemptions: usize,
}

#[derive(Debug, Error)]
pub enum AntipatternError {
    #[error("could not read antipattern input: {0}")]
    Io(#[from] io::Error),
    #[error("dependency rule document is invalid: {0}")]
    Rules(#[from] RestrictedTomlError),
    #[error("dependency rule document is invalid: {0}")]
    InvalidRules(String),
    #[error("cargo metadata is invalid: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("cargo metadata failed with status {status}")]
    MetadataCommand { status: ExitStatus },
    #[error("workspace package `{package}` has a manifest outside the repository")]
    ManifestOutsideRepository { package: String },
    #[error("{path} is not valid Rust source: {source}")]
    RustSyntax { path: PathBuf, source: syn::Error },
    #[error("{path} is not valid JSON: {source}")]
    SchemaJson {
        path: PathBuf,
        source: serde_json::Error,
    },
}

#[derive(Debug)]
struct AntipatternRules {
    a1: A1Rules,
    a2: A2Rules,
    a4: A4Rules,
    a6: A6Rules,
}

#[derive(Debug)]
struct A1Rules {
    forbidden_dependencies: Vec<String>,
    source_patterns: Vec<String>,
}

#[derive(Debug)]
struct A2Rules {
    ui_crates: Vec<String>,
    agent_names: BTreeSet<String>,
    exemption_prefix: String,
}

#[derive(Debug)]
struct A4Rules {
    crate_name: String,
    forbidden_literals: Vec<String>,
}

#[derive(Debug)]
struct A6Rules {
    sources: Vec<A6Source>,
}

#[derive(Debug)]
struct A6Source {
    crate_pattern: String,
    schema_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<MetadataPackage>,
    workspace_members: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct MetadataPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetadataDependency>,
}

#[derive(Clone, Debug, Deserialize)]
struct MetadataDependency {
    name: String,
}

#[derive(Debug)]
struct WorkspacePackage {
    name: String,
    manifest_relative: PathBuf,
    dependencies: Vec<String>,
}

#[derive(Debug)]
struct SourceFile {
    relative: PathBuf,
    absolute: PathBuf,
}

#[derive(Debug)]
struct BytePattern {
    needle: Vec<u8>,
    rule: &'static str,
    label: String,
}

pub fn scan_repository(root: &Path) -> Result<ScanReport, AntipatternError> {
    let rules = fs::read_to_string(root.join(RULES_PATH))?;
    let metadata = cargo_metadata(root)?;
    scan_repository_with_inputs(root, &rules, &metadata)
}

pub fn scan_repository_with_inputs(
    root: &Path,
    rules_toml: &str,
    metadata_json: &str,
) -> Result<ScanReport, AntipatternError> {
    let rules = antipattern_rules(rules_toml)?;
    let metadata: CargoMetadata = serde_json::from_str(metadata_json)?;
    let packages = workspace_packages(root, metadata)?;
    let mut report = ScanReport::default();

    scan_generic_source_rules(root, &rules.a1.source_patterns, &mut report.violations)?;
    scan_a1_dependencies(root, &packages, &rules.a1, &mut report.violations)?;
    scan_a2(root, &packages, &rules.a2, &mut report)?;
    scan_a4(root, &packages, &rules.a4, &mut report.violations)?;
    scan_a6(root, &packages, &rules.a6, &mut report.violations)?;

    report.violations.sort();
    report.violations.dedup();
    Ok(report)
}

fn antipattern_rules(source: &str) -> Result<AntipatternRules, AntipatternError> {
    let document = parse_restricted_toml(source)?;
    let a1_table = required_table(&document, &["antipatterns", "a1"])?;
    reject_unknown_keys(
        a1_table,
        &["forbidden_dependencies", "source_patterns"],
        "[antipatterns.a1]",
    )?;
    let a2_table = required_table(&document, &["antipatterns", "a2"])?;
    reject_unknown_keys(
        a2_table,
        &["ui_crates", "agent_names", "exemption_prefix"],
        "[antipatterns.a2]",
    )?;
    let a4_table = required_table(&document, &["antipatterns", "a4"])?;
    reject_unknown_keys(
        a4_table,
        &["crate_name", "forbidden_literals"],
        "[antipatterns.a4]",
    )?;
    if let Some(a6_table) = document.table(&["antipatterns", "a6"]) {
        reject_unknown_keys(a6_table, &[], "[antipatterns.a6]")?;
    }

    let source_tables = document
        .array_tables(&["antipatterns", "a6", "sources"])
        .ok_or_else(|| {
            AntipatternError::InvalidRules(
                "missing `[[antipatterns.a6.sources]]` declarations".to_owned(),
            )
        })?;
    if source_tables.is_empty() {
        return Err(AntipatternError::InvalidRules(
            "`[[antipatterns.a6.sources]]` must not be empty".to_owned(),
        ));
    }
    let sources = source_tables
        .iter()
        .map(|table| {
            reject_unknown_keys(
                table,
                &["crate_pattern", "schema_paths"],
                "[[antipatterns.a6.sources]]",
            )?;
            Ok(A6Source {
                crate_pattern: required_string(
                    table,
                    "crate_pattern",
                    "[[antipatterns.a6.sources]]",
                )?,
                schema_paths: required_string_array(
                    table,
                    "schema_paths",
                    "[[antipatterns.a6.sources]]",
                )?,
            })
        })
        .collect::<Result<Vec<_>, AntipatternError>>()?;

    Ok(AntipatternRules {
        a1: A1Rules {
            forbidden_dependencies: required_string_array(
                a1_table,
                "forbidden_dependencies",
                "[antipatterns.a1]",
            )?,
            source_patterns: required_string_array(
                a1_table,
                "source_patterns",
                "[antipatterns.a1]",
            )?,
        },
        a2: A2Rules {
            ui_crates: required_string_array(a2_table, "ui_crates", "[antipatterns.a2]")?,
            agent_names: required_string_array(a2_table, "agent_names", "[antipatterns.a2]")?
                .into_iter()
                .collect(),
            exemption_prefix: required_string(a2_table, "exemption_prefix", "[antipatterns.a2]")?,
        },
        a4: A4Rules {
            crate_name: required_string(a4_table, "crate_name", "[antipatterns.a4]")?,
            forbidden_literals: required_string_array(
                a4_table,
                "forbidden_literals",
                "[antipatterns.a4]",
            )?,
        },
        a6: A6Rules { sources },
    })
}

fn required_table<'a>(
    document: &'a RestrictedToml,
    path: &[&str],
) -> Result<&'a RestrictedTable, AntipatternError> {
    document.table(path).ok_or_else(|| {
        AntipatternError::InvalidRules(format!("missing table `[{}]`", path.join(".")))
    })
}

fn reject_unknown_keys(
    table: &RestrictedTable,
    allowed: &[&str],
    context: &str,
) -> Result<(), AntipatternError> {
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(AntipatternError::InvalidRules(format!(
            "{context} contains unsupported key `{key}`"
        )));
    }
    Ok(())
}

fn required_string(
    table: &RestrictedTable,
    key: &str,
    context: &str,
) -> Result<String, AntipatternError> {
    match table.get(key) {
        Some(RestrictedValue::String(value)) => Ok(value.clone()),
        Some(_) => Err(AntipatternError::InvalidRules(format!(
            "{context}.{key} must be a string"
        ))),
        None => Err(AntipatternError::InvalidRules(format!(
            "{context} is missing `{key}`"
        ))),
    }
}

fn required_string_array(
    table: &RestrictedTable,
    key: &str,
    context: &str,
) -> Result<Vec<String>, AntipatternError> {
    match table.get(key) {
        Some(RestrictedValue::StringArray(values)) => Ok(values.clone()),
        Some(_) => Err(AntipatternError::InvalidRules(format!(
            "{context}.{key} must be an array of strings"
        ))),
        None => Err(AntipatternError::InvalidRules(format!(
            "{context} is missing `{key}`"
        ))),
    }
}

fn cargo_metadata(root: &Path) -> Result<String, AntipatternError> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(AntipatternError::MetadataCommand {
            status: output.status,
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn workspace_packages(
    root: &Path,
    metadata: CargoMetadata,
) -> Result<Vec<WorkspacePackage>, AntipatternError> {
    let mut packages = Vec::new();
    for package in metadata.packages {
        if !metadata.workspace_members.contains(&package.id) {
            continue;
        }
        let relative = package.manifest_path.strip_prefix(root).map_err(|_| {
            AntipatternError::ManifestOutsideRepository {
                package: package.name.clone(),
            }
        })?;
        packages.push(WorkspacePackage {
            name: package.name,
            manifest_relative: relative.to_path_buf(),
            dependencies: package
                .dependencies
                .into_iter()
                .map(|dependency| dependency.name)
                .collect(),
        });
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

fn scan_generic_source_rules(
    root: &Path,
    a1_source_patterns: &[String],
    violations: &mut Vec<Violation>,
) -> Result<(), AntipatternError> {
    let patterns = generic_patterns(a1_source_patterns);
    for scope in ["crates", "spikes", "xtask"] {
        let scope_path = root.join(scope);
        match fs::metadata(&scope_path) {
            Ok(metadata) if metadata.is_dir() => {
                for source in rust_files(root, &scope_path)? {
                    scan_byte_patterns(&source, &patterns, violations)?;
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn scan_byte_patterns(
    source: &SourceFile,
    patterns: &[BytePattern],
    violations: &mut Vec<Violation>,
) -> Result<(), AntipatternError> {
    let contents = fs::read(&source.absolute)?;
    for pattern in patterns {
        if pattern.needle.is_empty() {
            continue;
        }
        for (offset, window) in contents.windows(pattern.needle.len()).enumerate() {
            if window == pattern.needle {
                violations.push(Violation {
                    path: source.relative.clone(),
                    line: line_for_offset(&contents, offset),
                    rule: pattern.rule.to_owned(),
                    detail: format!("forbidden source pattern `{}`", pattern.label),
                });
            }
        }
    }
    Ok(())
}

fn generic_patterns(a1_source_patterns: &[String]) -> Vec<BytePattern> {
    let mut patterns = vec![
        text_pattern(&["to", "do!"], "SOURCE"),
        text_pattern(&["unimple", "mented!"], "SOURCE"),
        text_pattern(&["// TO", "DO"], "SOURCE"),
        text_pattern(&["// FIX", "ME"], "SOURCE"),
        text_pattern(&["#[ig", "nore]"], "SOURCE"),
    ];
    patterns.extend(a1_source_patterns.iter().map(|pattern| BytePattern {
        needle: pattern.as_bytes().to_vec(),
        rule: "A-1",
        label: pattern.clone(),
    }));
    patterns
}

fn text_pattern(parts: &[&str], rule: &'static str) -> BytePattern {
    let label = parts.concat();
    BytePattern {
        needle: label.as_bytes().to_vec(),
        rule,
        label,
    }
}

fn scan_a1_dependencies(
    root: &Path,
    packages: &[WorkspacePackage],
    rules: &A1Rules,
    violations: &mut Vec<Violation>,
) -> Result<(), AntipatternError> {
    for package in packages {
        let manifest = root.join(&package.manifest_relative);
        let manifest_contents = fs::read(&manifest)?;
        for dependency in &package.dependencies {
            if rules
                .forbidden_dependencies
                .iter()
                .any(|pattern| wildcard_matches(pattern, dependency))
            {
                violations.push(Violation {
                    path: package.manifest_relative.clone(),
                    line: dependency_line(&manifest_contents, dependency),
                    rule: "A-1".to_owned(),
                    detail: format!("forbidden terminal/ANSI dependency `{dependency}`"),
                });
            }
        }
    }
    Ok(())
}

fn dependency_line(manifest: &[u8], dependency: &str) -> usize {
    let needle = dependency.as_bytes();
    if needle.is_empty() {
        return 1;
    }
    manifest
        .windows(needle.len())
        .position(|window| window == needle)
        .map_or(1, |offset| line_for_offset(manifest, offset))
}

fn scan_a2(
    root: &Path,
    packages: &[WorkspacePackage],
    rules: &A2Rules,
    report: &mut ScanReport,
) -> Result<(), AntipatternError> {
    for package in packages {
        if !rules
            .ui_crates
            .iter()
            .any(|pattern| wildcard_matches(pattern, &package.name))
        {
            continue;
        }
        for source in package_source_files(root, package)? {
            let contents = fs::read_to_string(&source.absolute)?;
            let syntax = parse_rust(&source.relative, &contents)?;
            let mut visitor = A2Visitor::new(&rules.agent_names);
            visitor.visit_file(&syntax);
            let lines = contents.lines().collect::<Vec<_>>();
            for hit in visitor.hits {
                if has_reasoned_exemption(&lines, hit.line, &rules.exemption_prefix) {
                    report.agent_name_branch_exemptions += 1;
                } else {
                    report.violations.push(Violation {
                        path: source.relative.clone(),
                        line: hit.line,
                        rule: "A-2".to_owned(),
                        detail: format!(
                            "UI branches on agent name(s) {}; use capabilities() or add a \
                             reasoned A-2 exemption comment",
                            hit.names.join(", ")
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

fn has_reasoned_exemption(lines: &[&str], expression_line: usize, prefix: &str) -> bool {
    let Some(previous) = expression_line
        .checked_sub(2)
        .and_then(|index| lines.get(index))
    else {
        return false;
    };
    let trimmed = previous.trim();
    trimmed
        .strip_prefix(prefix)
        .is_some_and(|reason| !reason.trim().is_empty())
}

fn scan_a4(
    root: &Path,
    packages: &[WorkspacePackage],
    rules: &A4Rules,
    violations: &mut Vec<Violation>,
) -> Result<(), AntipatternError> {
    let Some(package) = packages
        .iter()
        .find(|package| package.name == rules.crate_name)
    else {
        return Ok(());
    };
    for source in package_source_files(root, package)? {
        let contents = fs::read_to_string(&source.absolute)?;
        let syntax = parse_rust(&source.relative, &contents)?;
        let mut visitor = StringLiteralVisitor::default();
        visitor.visit_file(&syntax);
        for literal in visitor.literals {
            for forbidden in &rules.forbidden_literals {
                if literal.value.contains(forbidden) {
                    violations.push(Violation {
                        path: source.relative.clone(),
                        line: literal.line,
                        rule: "A-4".to_owned(),
                        detail: format!(
                            "UACP contract reuses upstream discriminator `{forbidden}`"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

fn scan_a6(
    root: &Path,
    packages: &[WorkspacePackage],
    rules: &A6Rules,
    violations: &mut Vec<Violation>,
) -> Result<(), AntipatternError> {
    let mut schema_names_by_source = BTreeMap::<usize, BTreeSet<String>>::new();
    for package in packages {
        for (source_index, source_rules) in rules.sources.iter().enumerate() {
            if !wildcard_matches(&source_rules.crate_pattern, &package.name) {
                continue;
            }
            let names = if let Some(names) = schema_names_by_source.get(&source_index) {
                names.clone()
            } else {
                let names = schema_type_names(root, &source_rules.schema_paths)?;
                schema_names_by_source.insert(source_index, names.clone());
                names
            };
            for source in package_source_files(root, package)? {
                let contents = fs::read_to_string(&source.absolute)?;
                let syntax = parse_rust(&source.relative, &contents)?;
                let mut visitor = TypeDefinitionVisitor::new(&names);
                visitor.visit_file(&syntax);
                for definition in visitor.definitions {
                    violations.push(Violation {
                        path: source.relative.clone(),
                        line: definition.line,
                        rule: "A-6".to_owned(),
                        detail: format!(
                            "handwritten upstream type `{}` matches a pinned schema type",
                            definition.name
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

fn schema_type_names(
    root: &Path,
    schema_paths: &[String],
) -> Result<BTreeSet<String>, AntipatternError> {
    let mut names = BTreeSet::new();
    for schema_path in schema_paths {
        let absolute = root.join(schema_path);
        match fs::metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => {
                for file in json_files(root, &absolute)? {
                    let bytes = fs::read(&file.absolute)?;
                    let value =
                        serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|source| {
                            AntipatternError::SchemaJson {
                                path: file.relative.clone(),
                                source,
                            }
                        })?;
                    collect_schema_titles(&value, &mut names);
                    collect_named_schema_keys(&value, &mut names);
                }
            }
            Ok(metadata) if metadata.is_file() => {
                let relative = absolute
                    .strip_prefix(root)
                    .map_err(|_| io::Error::other("schema path escaped repository root"))?
                    .to_path_buf();
                let bytes = fs::read(&absolute)?;
                let value =
                    serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|source| {
                        AntipatternError::SchemaJson {
                            path: relative,
                            source,
                        }
                    })?;
                collect_schema_titles(&value, &mut names);
                collect_named_schema_keys(&value, &mut names);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(names)
}

fn collect_schema_titles(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(title) = object.get("title").and_then(serde_json::Value::as_str) {
                insert_schema_identifier(title, names);
            }
            for nested in object.values() {
                collect_schema_titles(nested, names);
            }
        }
        serde_json::Value::Array(array) => {
            for nested in array {
                collect_schema_titles(nested, names);
            }
        }
        _ => {}
    }
}

fn collect_named_schema_keys(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for keyword in ["$defs", "definitions"] {
                if let Some(definitions) =
                    object.get(keyword).and_then(serde_json::Value::as_object)
                {
                    for name in definitions.keys() {
                        insert_schema_identifier(name, names);
                    }
                }
            }
            if let Some(schemas) = object
                .get("components")
                .and_then(|components| components.get("schemas"))
                .and_then(serde_json::Value::as_object)
            {
                for name in schemas.keys() {
                    insert_schema_identifier(name, names);
                }
            }
            for nested in object.values() {
                collect_named_schema_keys(nested, names);
            }
        }
        serde_json::Value::Array(array) => {
            for nested in array {
                collect_named_schema_keys(nested, names);
            }
        }
        _ => {}
    }
}

fn insert_schema_identifier(name: &str, names: &mut BTreeSet<String>) {
    if syn::parse_str::<syn::Ident>(name).is_ok() {
        names.insert(name.to_owned());
    }
}

fn parse_rust(path: &Path, contents: &str) -> Result<File, AntipatternError> {
    syn::parse_file(contents).map_err(|source| AntipatternError::RustSyntax {
        path: path.to_path_buf(),
        source,
    })
}

fn package_source_files(
    root: &Path,
    package: &WorkspacePackage,
) -> Result<Vec<SourceFile>, AntipatternError> {
    let Some(package_root) = package
        .manifest_relative
        .parent()
        .map(|parent| root.join(parent))
    else {
        return Ok(Vec::new());
    };
    let source_root = package_root.join("src");
    match fs::metadata(&source_root) {
        Ok(metadata) if metadata.is_dir() => rust_files(root, &source_root),
        Ok(_) => Ok(Vec::new()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn rust_files(root: &Path, directory: &Path) -> Result<Vec<SourceFile>, AntipatternError> {
    files_with_extension(root, directory, OsStr::new("rs"), true)
}

fn json_files(root: &Path, directory: &Path) -> Result<Vec<SourceFile>, AntipatternError> {
    files_with_extension(root, directory, OsStr::new("json"), false)
}

fn files_with_extension(
    root: &Path,
    directory: &Path,
    extension: &OsStr,
    apply_exclusions: bool,
) -> Result<Vec<SourceFile>, AntipatternError> {
    let mut files = Vec::new();
    visit_directory(root, directory, extension, apply_exclusions, &mut files)?;
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(files)
}

fn visit_directory(
    root: &Path,
    directory: &Path,
    extension: &OsStr,
    apply_exclusions: bool,
    files: &mut Vec<SourceFile>,
) -> Result<(), AntipatternError> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let absolute = entry.path();
        let relative = absolute
            .strip_prefix(root)
            .map_err(|_| io::Error::other("scanner path escaped repository root"))?;
        if apply_exclusions && excluded(relative) {
            continue;
        }
        if file_type.is_dir() {
            visit_directory(root, &absolute, extension, apply_exclusions, files)?;
        } else if file_type.is_file()
            && absolute
                .extension()
                .is_some_and(|candidate| candidate == extension)
        {
            files.push(SourceFile {
                relative: relative.to_path_buf(),
                absolute,
            });
        }
    }
    Ok(())
}

fn excluded(relative: &Path) -> bool {
    let mut components = relative.components().filter_map(|component| {
        if let Component::Normal(name) = component {
            Some(name)
        } else {
            None
        }
    });
    let first = components.next();
    if first.is_some_and(|name| {
        name == OsStr::new("target") || name == OsStr::new("schemas") || name == OsStr::new(".git")
    }) {
        return true;
    }
    let mut previous_was_tests = first == Some(OsStr::new("tests"));
    for name in components {
        if previous_was_tests && name == OsStr::new("fixtures") {
            return true;
        }
        previous_was_tests = name == OsStr::new("tests");
    }
    false
}

fn line_for_offset(contents: &[u8], offset: usize) -> usize {
    contents
        .iter()
        .take(offset)
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

fn wildcard_matches(pattern: &str, candidate: &str) -> bool {
    let pattern = pattern.as_bytes();
    let candidate = candidate.as_bytes();
    let mut pattern_index = 0;
    let mut candidate_index = 0;
    let mut star_index = None;
    let mut star_candidate_index = 0;

    while let Some(candidate_byte) = candidate.get(candidate_index) {
        match pattern.get(pattern_index) {
            Some(pattern_byte) if pattern_byte == candidate_byte => {
                pattern_index += 1;
                candidate_index += 1;
            }
            Some(b'*') => {
                star_index = Some(pattern_index);
                pattern_index += 1;
                star_candidate_index = candidate_index;
            }
            _ => {
                let Some(star) = star_index else {
                    return false;
                };
                star_candidate_index += 1;
                candidate_index = star_candidate_index;
                pattern_index = star + 1;
            }
        }
    }
    while pattern.get(pattern_index) == Some(&b'*') {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[derive(Debug)]
struct A2Hit {
    line: usize,
    names: Vec<String>,
}

struct A2Visitor<'a> {
    agent_names: &'a BTreeSet<String>,
    hits: Vec<A2Hit>,
}

impl<'a> A2Visitor<'a> {
    fn new(agent_names: &'a BTreeSet<String>) -> Self {
        Self {
            agent_names,
            hits: Vec::new(),
        }
    }

    fn record(&mut self, line: usize, names: BTreeSet<String>) {
        if names.is_empty() {
            return;
        }
        self.hits.push(A2Hit {
            line,
            names: names.into_iter().collect(),
        });
    }
}

impl<'ast> Visit<'ast> for A2Visitor<'_> {
    fn visit_expr_binary(&mut self, expression: &'ast ExprBinary) {
        if matches!(expression.op, BinOp::Eq(_) | BinOp::Ne(_)) {
            let mut collector = AgentLiteralCollector::new(self.agent_names);
            collector.visit_expr(&expression.left);
            collector.visit_expr(&expression.right);
            let names = collector.names;
            self.record(expression.span().start().line, names);
        }
        visit::visit_expr_binary(self, expression);
    }

    fn visit_expr_match(&mut self, expression: &'ast ExprMatch) {
        let mut names = BTreeSet::new();
        for arm in &expression.arms {
            let mut collector = AgentLiteralCollector::new(self.agent_names);
            collector.visit_pat(&arm.pat);
            names.extend(collector.names);
        }
        self.record(expression.span().start().line, names);
        visit::visit_expr_match(self, expression);
    }

    fn visit_expr_let(&mut self, expression: &'ast ExprLet) {
        let mut collector = AgentLiteralCollector::new(self.agent_names);
        collector.visit_pat(&expression.pat);
        collector.visit_expr(&expression.expr);
        self.record(expression.span().start().line, collector.names);
        visit::visit_expr_let(self, expression);
    }

    fn visit_expr_macro(&mut self, expression: &'ast ExprMacro) {
        if expression.mac.path.is_ident("matches") {
            let names = agent_literals_in_tokens(&expression.mac.tokens, self.agent_names);
            self.record(expression.span().start().line, names);
        }
        visit::visit_expr_macro(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast ExprMethodCall) {
        let method = expression.method.to_string();
        if method == "eq" || method == "ne" {
            let mut collector = AgentLiteralCollector::new(self.agent_names);
            collector.visit_expr(&expression.receiver);
            for argument in &expression.args {
                collector.visit_expr(argument);
            }
            self.record(expression.span().start().line, collector.names);
        }
        visit::visit_expr_method_call(self, expression);
    }
}

struct AgentLiteralCollector<'a> {
    agent_names: &'a BTreeSet<String>,
    names: BTreeSet<String>,
}

impl<'a> AgentLiteralCollector<'a> {
    fn new(agent_names: &'a BTreeSet<String>) -> Self {
        Self {
            agent_names,
            names: BTreeSet::new(),
        }
    }
}

impl<'ast> Visit<'ast> for AgentLiteralCollector<'_> {
    fn visit_lit_str(&mut self, literal: &'ast LitStr) {
        let value = literal.value();
        if self.agent_names.contains(&value) {
            self.names.insert(value);
        }
    }
}

fn agent_literals_in_tokens(
    tokens: &TokenStream,
    agent_names: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_token_literals(tokens.clone(), agent_names, &mut names);
    names
}

fn collect_token_literals(
    tokens: TokenStream,
    agent_names: &BTreeSet<String>,
    names: &mut BTreeSet<String>,
) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => {
                collect_token_literals(group.stream(), agent_names, names);
            }
            TokenTree::Literal(literal) => {
                if let Ok(value) = syn::parse_str::<LitStr>(&literal.to_string()) {
                    let value = value.value();
                    if agent_names.contains(&value) {
                        names.insert(value);
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
struct StringLiteral {
    line: usize,
    value: String,
}

#[derive(Default)]
struct StringLiteralVisitor {
    literals: Vec<StringLiteral>,
}

impl<'ast> Visit<'ast> for StringLiteralVisitor {
    fn visit_lit_str(&mut self, literal: &'ast LitStr) {
        self.literals.push(StringLiteral {
            line: literal.span().start().line,
            value: literal.value(),
        });
    }

    fn visit_lit_byte_str(&mut self, literal: &'ast LitByteStr) {
        if let Ok(value) = String::from_utf8(literal.value()) {
            self.literals.push(StringLiteral {
                line: literal.span().start().line,
                value,
            });
        }
    }
}

#[derive(Debug)]
struct TypeDefinition {
    line: usize,
    name: String,
}

struct TypeDefinitionVisitor<'a> {
    schema_names: &'a BTreeSet<String>,
    definitions: Vec<TypeDefinition>,
}

impl<'a> TypeDefinitionVisitor<'a> {
    fn new(schema_names: &'a BTreeSet<String>) -> Self {
        Self {
            schema_names,
            definitions: Vec::new(),
        }
    }

    fn record(&mut self, name: &syn::Ident) {
        let name_text = name.to_string();
        if self.schema_names.contains(&name_text) {
            self.definitions.push(TypeDefinition {
                line: name.span().start().line,
                name: name_text,
            });
        }
    }
}

impl<'ast> Visit<'ast> for TypeDefinitionVisitor<'_> {
    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        self.record(&item.ident);
        visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        self.record(&item.ident);
        visit::visit_item_enum(self, item);
    }

    fn visit_item_union(&mut self, item: &'ast ItemUnion) {
        self.record(&item.ident);
        visit::visit_item_union(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        self.record(&item.ident);
        visit::visit_item_type(self, item);
    }
}
