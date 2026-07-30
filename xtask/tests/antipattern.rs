#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::tempdir;
use xtask::antipattern::scan_repository_with_inputs;

const RULES: &str = include_str!("../../docs/dependency-rules.toml");

#[derive(Debug)]
struct PackageSpec {
    name: String,
    relative_manifest: PathBuf,
    dependencies: Vec<String>,
}

fn metadata(root: &Path, packages: &[PackageSpec]) -> String {
    let package_values = packages
        .iter()
        .map(|package| {
            let id = format!(
                "path+file:///{}#0.1.0",
                package
                    .relative_manifest
                    .parent()
                    .expect("package manifest must have a parent")
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            json!({
                "id": id,
                "name": package.name,
                "manifest_path": root.join(&package.relative_manifest),
                "dependencies": package
                    .dependencies
                    .iter()
                    .map(|name| json!({"name": name}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let members = package_values
        .iter()
        .filter_map(|package| package.get("id").cloned())
        .collect::<Vec<_>>();
    json!({
        "packages": package_values,
        "workspace_members": members,
    })
    .to_string()
}

fn write_package(
    root: &Path,
    relative_directory: &str,
    name: &str,
    dependencies: &[String],
    source: &str,
) -> PackageSpec {
    let directory = root.join(relative_directory);
    fs::create_dir_all(directory.join("src")).expect("package source directory must be created");
    let dependency_lines = dependencies
        .iter()
        .map(|dependency| format!("{dependency} = \"1\"\n"))
        .collect::<String>();
    fs::write(
        directory.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependency_lines}"
        ),
    )
    .expect("package manifest must be written");
    fs::write(directory.join("src/lib.rs"), source).expect("package source must be written");
    PackageSpec {
        name: name.to_owned(),
        relative_manifest: Path::new(relative_directory).join("Cargo.toml"),
        dependencies: dependencies.to_vec(),
    }
}

#[test]
fn a1_rejects_every_forbidden_direct_dependency_and_accepts_clean_dependencies() {
    let temporary = tempdir().expect("temporary repository must be created");
    let forbidden = vec![
        ["v", "te"].concat(),
        "ansi-parser".to_owned(),
        "strip-ansi-escapes".to_owned(),
        ["term", "wiz"].concat(),
    ];
    let package = write_package(
        temporary.path(),
        "spikes/dependency-probe",
        "dependency-probe",
        &forbidden,
        "pub fn clean() {}\n",
    );

    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");

    assert_eq!(report.violations.len(), 4);
    assert!(report
        .violations
        .iter()
        .all(|violation| violation.rule == "A-1"));

    let clean = write_package(
        temporary.path(),
        "spikes/clean-dependency-probe",
        "clean-dependency-probe",
        &["serde".to_owned()],
        "pub fn clean() {}\n",
    );
    let clean_report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[clean]),
    )
    .expect("clean repository must be scanned");
    assert!(clean_report.violations.is_empty());
}

#[test]
fn a1_source_detection_still_rejects_ansi_parsing_and_accepts_plain_text() {
    let temporary = tempdir().expect("temporary repository must be created");
    let blocked_source = ["pub fn blocked() { let _ = \"", "\\x1", "b[31m\"; }\n"].concat();
    let blocked = write_package(
        temporary.path(),
        "spikes/blocked-source",
        "blocked-source",
        &[],
        &blocked_source,
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[blocked]),
    )
    .expect("repository must be scanned");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.rule == "A-1"));

    let clean_temporary = tempdir().expect("clean temporary repository must be created");
    let clean = write_package(
        clean_temporary.path(),
        "spikes/plain-source",
        "plain-source",
        &[],
        "pub const COLOR: &str = \"green\";\n",
    );
    let clean_report = scan_repository_with_inputs(
        clean_temporary.path(),
        RULES,
        &metadata(clean_temporary.path(), &[clean]),
    )
    .expect("clean repository must be scanned");
    assert!(clean_report.violations.is_empty());
}

#[test]
fn a1_source_detection_preserves_the_existing_terminal_parser_identifier_rule() {
    let temporary = tempdir().expect("temporary repository must be created");
    let parser_name = ["v", "te"].concat();
    let source = format!("use {parser_name}::Parser;\npub fn parse() {{}}\n");
    let package = write_package(
        temporary.path(),
        "spikes/parser-source",
        "parser-source",
        &[],
        &source,
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.rule == "A-1"));
}

#[test]
fn a1_source_detection_rejects_a_raw_ansi_escape_configured_in_the_rule_document() {
    let temporary = tempdir().expect("temporary repository must be created");
    let source = format!(
        "pub const TERMINAL_OUTPUT: &str = \"{}[31mred\";\n",
        '\u{1b}'
    );
    let package = write_package(
        temporary.path(),
        "spikes/raw-escape-source",
        "raw-escape-source",
        &[],
        &source,
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.rule == "A-1"));
}

#[test]
fn a2_rejects_match_comparison_matches_and_if_let_but_not_display_only_literals() {
    let temporary = tempdir().expect("temporary repository must be created");
    let source = r#"
pub fn branch(adapter_name: &str) -> bool {
    let from_match = match adapter_name { "codex" => true, _ => false };
    let from_comparison = adapter_name != "opencode";
    let from_macro = matches!(adapter_name, "claude");
    let from_if_let = if let "codex" = adapter_name { true } else { false };
    println!("{}", "codex");
    from_match || from_comparison || from_macro || from_if_let
}
"#;
    let package = write_package(
        temporary.path(),
        "crates/kaleido-cli",
        "kaleido-cli",
        &[],
        source,
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");

    let a2 = report
        .violations
        .iter()
        .filter(|violation| violation.rule == "A-2")
        .collect::<Vec<_>>();
    assert_eq!(a2.len(), 4);
    assert_eq!(report.agent_name_branch_exemptions, 0);
}

#[test]
fn a2_reasoned_comment_exempts_only_the_adjacent_expression_and_is_counted() {
    let temporary = tempdir().expect("temporary repository must be created");
    let source = r#"
pub fn branch(adapter_name: &str) -> bool {
    // #[allow(kaleido::agent_name_branch)] reason: compatibility diagnostics only
    match adapter_name { "codex" => true, _ => false }
}
"#;
    let package = write_package(
        temporary.path(),
        "crates/kaleido-cli",
        "kaleido-cli",
        &[],
        source,
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");

    assert!(report.violations.is_empty());
    assert_eq!(report.agent_name_branch_exemptions, 1);

    fs::write(
        temporary.path().join("crates/kaleido-cli/src/lib.rs"),
        r#"
pub fn branch(adapter_name: &str) -> bool {
    // #[allow(kaleido::agent_name_branch)] reason:
    match adapter_name { "codex" => true, _ => false }
}
"#,
    )
    .expect("invalid exemption source must be written");
    let invalid = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(
            temporary.path(),
            &[PackageSpec {
                name: "kaleido-cli".to_owned(),
                relative_manifest: PathBuf::from("crates/kaleido-cli/Cargo.toml"),
                dependencies: Vec::new(),
            }],
        ),
    )
    .expect("repository must be scanned");
    assert_eq!(invalid.agent_name_branch_exemptions, 0);
    assert!(invalid
        .violations
        .iter()
        .any(|violation| violation.rule == "A-2"));
}

#[test]
fn a2_scans_the_hostd_tray_ui_package_declared_by_the_rule_document() {
    let temporary = tempdir().expect("temporary repository must be created");
    let package = write_package(
        temporary.path(),
        "crates/kaleido-hostd",
        "kaleido-hostd",
        &[],
        r#"pub fn tray(adapter_name: &str) -> bool {
    adapter_name == "claude"
}
"#,
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.rule == "A-2"));
}

#[test]
fn a4_rejects_upstream_literals_in_proto_but_not_comments_or_identifiers() {
    let temporary = tempdir().expect("temporary repository must be created");
    let source = r#"
// session/update is mentioned only in documentation here.
pub const SESSION_UPDATE_LABEL: &str = "local";
pub const FORBIDDEN: &str = "session/update";
"#;
    let package = write_package(
        temporary.path(),
        "crates/kaleido-proto",
        "kaleido-proto",
        &[],
        source,
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");

    let a4 = report
        .violations
        .iter()
        .filter(|violation| violation.rule == "A-4")
        .collect::<Vec<_>>();
    assert_eq!(a4.len(), 1);
}

#[test]
fn a6_rejects_schema_named_type_definitions_and_accepts_local_types() {
    let temporary = tempdir().expect("temporary repository must be created");
    let schema_directory = temporary.path().join("schemas/codex");
    fs::create_dir_all(&schema_directory).expect("schema directory must be created");
    fs::write(
        schema_directory.join("types.json"),
        r#"{"title":"ThreadStartParams","type":"object"}"#,
    )
    .expect("schema must be written");
    let package = write_package(
        temporary.path(),
        "crates/kaleido-adapter-codex",
        "kaleido-adapter-codex",
        &[],
        "pub struct ThreadStartParams;\npub struct LocalState;\n",
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");

    let a6 = report
        .violations
        .iter()
        .filter(|violation| violation.rule == "A-6")
        .collect::<Vec<_>>();
    assert_eq!(a6.len(), 1);
    assert!(a6
        .first()
        .is_some_and(|violation| violation.detail.contains("ThreadStartParams")));
}

#[test]
fn a6_uses_openapi_component_schema_keys_when_the_document_has_no_type_titles() {
    let temporary = tempdir().expect("temporary repository must be created");
    let schema_directory = temporary.path().join("schemas/opencode");
    fs::create_dir_all(&schema_directory).expect("schema directory must be created");
    fs::write(
        schema_directory.join("openapi.json"),
        r#"{"components":{"schemas":{"PermissionRequest":{"type":"object"}}}}"#,
    )
    .expect("OpenAPI schema must be written");
    let package = write_package(
        temporary.path(),
        "crates/kaleido-adapter-opencode",
        "kaleido-adapter-opencode",
        &[],
        "pub enum PermissionRequest { Pending }\n",
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.rule == "A-6"));
}

#[test]
fn a6_uses_json_schema_definition_keys_when_definitions_have_no_titles() {
    let temporary = tempdir().expect("temporary repository must be created");
    let schema_directory = temporary.path().join("schemas/acp");
    fs::create_dir_all(&schema_directory).expect("schema directory must be created");
    fs::write(
        schema_directory.join("schema.json"),
        r#"{"$defs":{"RequestId":{"type":"string"}}}"#,
    )
    .expect("JSON Schema must be written");
    let package = write_package(
        temporary.path(),
        "crates/kaleido-adapter-acp",
        "kaleido-adapter-acp",
        &[],
        "pub struct RequestId;\n",
    );
    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.rule == "A-6"));
}

#[test]
fn fixture_and_root_schema_sources_are_excluded_without_hiding_nested_source_modules() {
    let temporary = tempdir().expect("temporary repository must be created");
    let package = write_package(
        temporary.path(),
        "crates/example",
        "example",
        &[],
        "pub fn clean() {}\n",
    );
    let blocked = ["to", "do!"].concat();
    let nested_source = temporary.path().join("crates/example/src/schemas/mod.rs");
    fs::create_dir_all(
        nested_source
            .parent()
            .expect("nested source must have a parent"),
    )
    .expect("nested source directory must be created");
    fs::write(&nested_source, format!("fn blocked() {{ {blocked}(); }}\n"))
        .expect("nested source must be written");
    let fixture = temporary
        .path()
        .join("crates/example/tests/fixtures/capture.rs");
    fs::create_dir_all(fixture.parent().expect("fixture must have a parent"))
        .expect("fixture directory must be created");
    fs::write(&fixture, format!("fn fixture() {{ {blocked}(); }}\n"))
        .expect("fixture must be written");
    let schema = temporary.path().join("schemas/generated.rs");
    fs::create_dir_all(schema.parent().expect("schema must have a parent"))
        .expect("schema directory must be created");
    fs::write(&schema, format!("fn schema() {{ {blocked}(); }}\n"))
        .expect("schema source must be written");

    let report = scan_repository_with_inputs(
        temporary.path(),
        RULES,
        &metadata(temporary.path(), &[package]),
    )
    .expect("repository must be scanned");
    assert_eq!(report.violations.len(), 1);
    assert_eq!(
        report
            .violations
            .first()
            .map(|violation| violation.path.as_path()),
        Some(Path::new("crates/example/src/schemas/mod.rs"))
    );
}
