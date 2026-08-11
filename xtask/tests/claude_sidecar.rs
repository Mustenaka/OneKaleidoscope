//! These samples exercise the closed OneKaleidoscope sidecar envelope. They
//! are not hand-written Claude SDK upstream DTO fixtures.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::{tempdir, TempDir};
use xtask::fixtures::{verify_claude_sidecar_paths, FixtureVerifyError, Identity};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn broker_bridge_does_not_inherit_user_or_project_permission_rules() -> TestResult {
    let fixtures = repository_claude_fixtures()?;
    let crate_root = fixtures
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Claude fixture root is malformed"))?;
    let bridge = fs::read_to_string(crate_root.join("bridge").join("index.ts"))?;

    assert!(bridge.contains("permissionMode: \"default\""));
    assert!(bridge.contains("settingSources: []"));
    Ok(())
}

#[test]
fn recorded_sdk_success_is_verified_as_acceptance_evidence() -> TestResult {
    let fixtures = repository_claude_fixtures()?;

    let summary = verify_claude_sidecar_paths(&fixtures, &Identity::default())?;

    assert_eq!(summary.files, 1);
    assert_eq!(summary.records, 7);
    assert_eq!(summary.acceptance_files, 1);
    assert_eq!(summary.auth_failure_files, 0);
    Ok(())
}

#[test]
fn successful_capture_metadata_cannot_disclaim_acceptance() -> TestResult {
    let (_root, fixtures) = copied_repository_fixture()?;
    let metadata = fixtures
        .join("sandbox")
        .join("real-sdk-simple-turn.metadata.json");
    let original = fs::read_to_string(&metadata)?;
    let changed = original.replace(
        "\"acceptance_eligible\": true",
        "\"acceptance_eligible\": false",
    );
    if changed == original {
        return Err(io::Error::other("metadata mutation did not change the fixture").into());
    }
    fs::write(metadata, changed)?;

    let error = verification_error(
        verify_claude_sidecar_paths(&fixtures, &Identity::default()),
        "successful evidence must not contradict its acceptance metadata",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "Claude fixture acceptance eligibility contradicts its expected outcome"
            && issue.pointer.as_deref() == Some("/acceptance_eligible")
    }));
    Ok(())
}

#[test]
fn terminal_error_cannot_pass_as_a_successful_capture() -> TestResult {
    let (_root, fixtures) = copied_repository_fixture()?;
    let capture = fixtures.join("sandbox").join("real-sdk-simple-turn.jsonl");
    let original = fs::read_to_string(&capture)?;
    let changed = original.replacen("\"is_error\":false", "\"is_error\":true", 1);
    if changed == original {
        return Err(io::Error::other("result mutation did not change the fixture").into());
    }
    fs::write(capture, changed)?;

    let error = verification_error(
        verify_claude_sidecar_paths(&fixtures, &Identity::default()),
        "a terminal error must contradict success metadata",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "Claude acceptance fixture contains authentication-failure evidence"
            && issue.pointer.as_deref() == Some("/payload/event")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.category == "Claude acceptance fixture is missing the terminal success result"
    }));
    Ok(())
}

#[test]
fn invalid_json_still_reports_raw_secret_leaks() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("fixtures");
    let sandbox = fixtures.join("sandbox");
    fs::create_dir_all(&sandbox)?;
    fs::write(
        sandbox.join("broken.jsonl"),
        "{\"api_key\":\"sk-not-redacted\"\n",
    )?;
    fs::write(
        sandbox.join("broken.metadata.json"),
        concat!(
            "{\n",
            "  \"capture\": \"real_provider\",\n",
            "  \"provider\": \"@anthropic-ai/claude-agent-sdk\",\n",
            "  \"provider_version\": \"0.3.226\",\n",
            "  \"expected_outcome\": \"authentication_failure\",\n",
            "  \"acceptance_eligible\": false\n",
            "}\n"
        ),
    )?;

    let error = verification_error(
        verify_claude_sidecar_paths(&fixtures, &Identity::default()),
        "invalid JSON with a credential marker must fail closed",
    )?;

    assert!(error
        .issues()
        .iter()
        .any(|issue| issue.category == "invalid Claude sidecar JSON"));
    assert!(error
        .issues()
        .iter()
        .any(|issue| issue.category == "leak: secret prefix sk-"));
    Ok(())
}

fn repository_claude_fixtures() -> io::Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .map(|ancestor| {
            ancestor
                .join("crates")
                .join("kaleido-adapter-claude")
                .join("tests")
                .join("fixtures")
        })
        .find(|candidate| {
            candidate
                .join("sandbox")
                .join("real-sdk-simple-turn.jsonl")
                .is_file()
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Claude fixture is missing"))
}

fn copied_repository_fixture() -> io::Result<(TempDir, PathBuf)> {
    let source = repository_claude_fixtures()?.join("sandbox");
    let root = tempdir()?;
    let fixtures = root.path().join("fixtures");
    let sandbox = fixtures.join("sandbox");
    fs::create_dir_all(&sandbox)?;
    for name in [
        "real-sdk-simple-turn.jsonl",
        "real-sdk-simple-turn.metadata.json",
    ] {
        fs::copy(source.join(name), sandbox.join(name))?;
    }
    Ok((root, fixtures))
}

fn verification_error(
    result: Result<xtask::fixtures::ClaudeSidecarVerifySummary, FixtureVerifyError>,
    success_message: &'static str,
) -> io::Result<FixtureVerifyError> {
    match result {
        Err(error) => Ok(error),
        Ok(_) => Err(io::Error::other(success_message)),
    }
}
