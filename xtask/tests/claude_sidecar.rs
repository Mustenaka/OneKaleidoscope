//! These samples exercise the closed OneKaleidoscope sidecar envelope. They
//! are not hand-written Claude SDK upstream DTO fixtures.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::{tempdir, TempDir};
use xtask::fixtures::{verify_claude_sidecar_paths, FixtureVerifyError, Identity};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn recorded_sdk_authentication_failure_is_verified_as_failure_only() -> TestResult {
    let fixtures = repository_claude_fixtures()?;

    let summary = verify_claude_sidecar_paths(&fixtures, &Identity::default())?;

    assert_eq!(summary.files, 1);
    assert_eq!(summary.records, 6);
    assert_eq!(summary.auth_failure_files, 1);
    Ok(())
}

#[test]
fn authentication_failure_metadata_cannot_claim_acceptance() -> TestResult {
    let (_root, fixtures) = copied_repository_fixture()?;
    let metadata = fixtures
        .join("sandbox")
        .join("real-sdk-simple-turn.metadata.json");
    let original = fs::read_to_string(&metadata)?;
    let changed = original.replace(
        "\"acceptance_eligible\": false",
        "\"acceptance_eligible\": true",
    );
    if changed == original {
        return Err(io::Error::other("metadata mutation did not change the fixture").into());
    }
    fs::write(metadata, changed)?;

    let error = verification_error(
        verify_claude_sidecar_paths(&fixtures, &Identity::default()),
        "an authentication failure must never become acceptance evidence",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "authentication-failure fixture must not be acceptance eligible"
            && issue.pointer.as_deref() == Some("/acceptance_eligible")
    }));
    Ok(())
}

#[test]
fn successful_result_cannot_pass_as_an_authentication_failure_capture() -> TestResult {
    let (_root, fixtures) = copied_repository_fixture()?;
    let capture = fixtures.join("sandbox").join("real-sdk-simple-turn.jsonl");
    let original = fs::read_to_string(&capture)?;
    let changed = original.replacen("\"is_error\":true", "\"is_error\":false", 1);
    if changed == original {
        return Err(io::Error::other("result mutation did not change the fixture").into());
    }
    fs::write(capture, changed)?;

    let error = verification_error(
        verify_claude_sidecar_paths(&fixtures, &Identity::default()),
        "a successful result must contradict failure-only metadata",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "authentication-failure fixture contains a successful result"
            && issue.pointer.as_deref() == Some("/payload/event/is_error")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.category == "Claude failure fixture is missing the terminal API-error result"
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
