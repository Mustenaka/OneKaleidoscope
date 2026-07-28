#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

#[path = "../src/forbidden.rs"]
mod forbidden;

use forbidden::scan_repository;
use tempfile::tempdir;

#[test]
fn reports_blocked_macro_with_file_and_line() {
    let temporary = tempdir().expect("temporary repository must be created");
    let source_path = temporary.path().join("crates/example/src/lib.rs");
    fs::create_dir_all(
        source_path
            .parent()
            .expect("source path must have a parent"),
    )
    .expect("source directory must be created");
    let blocked = ["to", "do!"].concat();
    let source = ["fn clean() {}\nfn blocked() { ", &blocked, "(); }\n"].concat();
    fs::write(&source_path, source).expect("blocked source must be written");

    let violations =
        scan_repository(temporary.path()).expect("temporary repository must be scanned");
    let violation = violations
        .first()
        .expect("the blocked source must produce a violation");

    assert_eq!(violations.len(), 1);
    assert_eq!(violation.path, Path::new("crates/example/src/lib.rs"));
    assert_eq!(violation.line, 2);
    assert_eq!(violation.pattern, blocked);
}

#[test]
fn accepts_clean_source() {
    let temporary = tempdir().expect("temporary repository must be created");
    let source_path = temporary.path().join("spikes/example/src/main.rs");
    fs::create_dir_all(
        source_path
            .parent()
            .expect("source path must have a parent"),
    )
    .expect("source directory must be created");
    fs::write(&source_path, "fn main() {}\n").expect("clean source must be written");

    let violations =
        scan_repository(temporary.path()).expect("temporary repository must be scanned");

    assert!(violations.is_empty());
}

#[test]
fn skips_fixture_source_containing_escape_bytes() {
    let temporary = tempdir().expect("temporary repository must be created");
    let fixture_path = temporary
        .path()
        .join("xtask/tests/fixtures/session/capture.rs");
    fs::create_dir_all(
        fixture_path
            .parent()
            .expect("fixture path must have a parent"),
    )
    .expect("fixture directory must be created");
    let mut fixture = b"recorded output: ".to_vec();
    fixture.extend_from_slice(&[0x1b, b'[', b'3', b'1', b'm']);
    fs::write(&fixture_path, fixture).expect("fixture source must be written");

    let violations =
        scan_repository(temporary.path()).expect("temporary repository must be scanned");

    assert!(violations.is_empty());
}
