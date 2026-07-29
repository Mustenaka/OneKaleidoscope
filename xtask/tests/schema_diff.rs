#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;
use xtask::schema::{semantic_changes, verify_semantic_match};

#[test]
fn identical_json_returns_success() {
    let expected = tempdir().expect("expected snapshot directory must be created");
    let actual = tempdir().expect("actual snapshot directory must be created");
    write_json(expected.path(), "codex/message.json", r#"{"kind":"turn"}"#);
    write_json(actual.path(), "codex/message.json", r#"{"kind":"turn"}"#);

    verify_semantic_match(expected.path(), actual.path())
        .expect("identical JSON must return success");
}

#[test]
fn reports_field_addition_removal_and_change_with_json_pointers() {
    let expected = tempdir().expect("expected snapshot directory must be created");
    let actual = tempdir().expect("actual snapshot directory must be created");
    write_json(
        expected.path(),
        "opencode/openapi.json",
        r#"{"a/b~c":0,"changed":"before","items":[{"id":"first"},{"id":"second"}],"removed":true,"stable":1}"#,
    );
    write_json(
        actual.path(),
        "opencode/openapi.json",
        r#"{"a/b~c":1,"added":true,"changed":"after","items":[{"id":"second"},{"id":"first"}],"stable":1}"#,
    );
    write_json(expected.path(), "codex/removed.json", r#"{"present":true}"#);
    write_json(actual.path(), "codex/added.json", r#"{"present":true}"#);

    let changes =
        semantic_changes(expected.path(), actual.path()).expect("valid JSON must be compared");
    let rendered: Vec<String> = changes.iter().map(ToString::to_string).collect();

    assert_eq!(
        rendered,
        vec![
            "codex/added.json# (added)",
            "codex/removed.json# (removed)",
            "opencode/openapi.json#/added (added)",
            "opencode/openapi.json#/a~1b~0c (changed)",
            "opencode/openapi.json#/changed (changed)",
            "opencode/openapi.json#/items/0/id (changed)",
            "opencode/openapi.json#/items/1/id (changed)",
            "opencode/openapi.json#/removed (removed)",
        ]
    );
    let error = verify_semantic_match(expected.path(), actual.path())
        .expect_err("semantic drift must fail verification");
    assert_eq!(error.exit_code(), 1);
}

#[test]
fn object_key_order_does_not_count_as_drift() {
    let expected = tempdir().expect("expected snapshot directory must be created");
    let actual = tempdir().expect("actual snapshot directory must be created");
    write_json(
        expected.path(),
        "acp/schema.json",
        r#"{"outer":{"alpha":1,"beta":2},"tail":true}"#,
    );
    write_json(
        actual.path(),
        "acp/schema.json",
        r#"{"tail":true,"outer":{"beta":2,"alpha":1}}"#,
    );

    let changes =
        semantic_changes(expected.path(), actual.path()).expect("valid JSON must be compared");

    assert!(changes.is_empty());
}

#[test]
fn invalid_json_returns_three_without_exposing_an_absolute_path() {
    let expected = tempdir().expect("expected snapshot directory must be created");
    let actual = tempdir().expect("actual snapshot directory must be created");
    write_json(expected.path(), "codex/broken.json", r#"{"incomplete":"#);
    write_json(actual.path(), "codex/broken.json", r#"{"valid":true}"#);

    let error = verify_semantic_match(expected.path(), actual.path())
        .expect_err("invalid snapshot JSON must fail verification");

    assert_eq!(error.exit_code(), 3);
    let rendered = error.to_string();
    assert!(rendered.contains("<external>/broken.json"));
    assert!(!rendered.contains(&expected.path().to_string_lossy().to_string()));
}

#[test]
fn missing_upstream_tool_exits_two_for_diff_and_refresh() {
    let empty_path = tempdir().expect("empty executable path must be created");
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be inside the workspace");
    let schemas = workspace.join("schemas");
    let before = read_tree(&schemas);

    for subcommand in ["diff", "refresh"] {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["schema", subcommand])
            .env("PATH", empty_path.path())
            .output()
            .expect("xtask must start");

        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("required tool `codex` is unavailable"));
        assert!(stderr.contains("@openai/codex@0.146.0"));
        assert_eq!(
            read_tree(&schemas),
            before,
            "failed {subcommand} must not modify the committed snapshot"
        );
    }
}

fn write_json(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("JSON path must have a parent"))
        .expect("JSON parent directory must be created");
    fs::write(path, contents).expect("JSON fixture must be written");
}

fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    visit_tree(root, root, &mut files);
    files
}

fn visit_tree(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    let mut entries = fs::read_dir(directory)
        .expect("snapshot directory must be readable")
        .collect::<Result<Vec<_>, _>>()
        .expect("snapshot entries must be readable");
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let file_type = entry
            .file_type()
            .expect("snapshot metadata must be readable");
        let path = entry.path();
        if file_type.is_dir() {
            visit_tree(root, &path, files);
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("snapshot path must remain below its root")
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(
                relative,
                fs::read(path).expect("snapshot file must be readable"),
            );
        }
    }
}
