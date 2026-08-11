//! Verifier tests use deliberately valid/invalid unit samples in temporary
//! directories. They are not agent contract fixtures.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use xtask::fixtures::{
    verify_claude_sidecar_paths, verify_paths, FixtureVerifyError, Identity, VerifySummary,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn no_fixture_files_reports_zero_without_loading_schemas() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    fs::create_dir_all(&fixtures)?;

    let summary = verify_paths(
        &fixtures,
        &root.path().join("missing-schemas"),
        &fixtures.join("sandbox"),
        &Identity::default(),
    )?;

    assert_eq!(summary.files, 0);
    assert_eq!(summary.records, 0);
    Ok(())
}

#[test]
fn jsonl_outside_a_recognized_agent_directory_is_not_silently_skipped() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "unknown/nested/record.jsonl",
        "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{}}\n",
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "all fixture JSONL files must be classified and verified",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "fixture JSONL is outside a recognized agent directory"
            && issue
                .file
                .ends_with("tests/fixtures/unknown/nested/record.jsonl")
    }));
    Ok(())
}

#[test]
fn fixture_jsonl_symlink_is_rejected_instead_of_skipped() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    let codex = fixtures.join("codex");
    fs::create_dir_all(&codex)?;
    let target = root.path().join("link-target");
    fs::create_dir_all(&target)?;
    fs::write(
        target.join("record.jsonl"),
        "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{}}\n",
    )?;
    create_fixture_link(&target, &codex.join("linked.jsonl"))?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "a fixture symlink must fail before schemas are loaded",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "fixture symlink is not allowed"
            && issue.file.ends_with("tests/fixtures/codex/linked.jsonl")
    }));
    Ok(())
}

#[test]
fn top_level_agent_fixture_link_is_rejected() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    fs::create_dir_all(&fixtures)?;
    let target = root.path().join("external-codex");
    fs::create_dir_all(&target)?;
    fs::write(
        target.join("record.jsonl"),
        "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{}}\n",
    )?;
    create_fixture_link(&target, &fixtures.join("codex"))?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "a top-level agent fixture link must fail before it is traversed",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "fixture symlink is not allowed"
            && issue.file.ends_with("tests/fixtures/codex")
    }));
    Ok(())
}

#[test]
fn leak_is_reported_with_relative_file_and_line_without_echoing_secret() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/leak.jsonl",
        r#"{"ts_ms":0,"dir":"c2s","transport":"stdio","payload":{"token":"sk-test123"}}"#,
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "a secret prefix must fail verification",
    )?;
    let rendered = error.to_string();

    assert!(rendered.contains("tests/fixtures/codex/leak.jsonl:1"));
    assert!(rendered.contains("leak: secret prefix sk-"));
    assert!(!rendered.contains("sk-test123"));
    assert!(!rendered.contains(&root.path().to_string_lossy().to_string()));
    Ok(())
}

#[test]
fn leak_scan_precedes_json_parsing_for_a_malformed_line() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/malformed-leak.jsonl",
        r#"{"payload":"sk-test123""#,
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "a malformed line containing a secret must fail",
    )?;

    assert_eq!(
        error.issues().first().map(|issue| issue.category.as_str()),
        Some("leak: secret prefix sk-")
    );
    assert!(error
        .issues()
        .iter()
        .any(|issue| issue.category == "invalid JSON"));
    assert!(!error.to_string().contains("sk-test123"));
    Ok(())
}

#[test]
fn sensitive_data_in_http_path_is_reported_by_pointer() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/path-leak.jsonl",
        r#"{"ts_ms":0,"dir":"c2s","transport":"http","payload":{"method":"GET","path":"/global/health?api_key=sk%2Dtest123","status":null,"content_type":"application/json","body":null}}"#,
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "a URL-encoded secret in the HTTP path must fail verification",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: secret prefix sk-"
            && issue.pointer.as_deref() == Some("/payload/path")
    }));
    assert!(!error.to_string().contains("sk-test123"));
    Ok(())
}

#[test]
fn percent_encoded_secret_in_http_route_segment_is_rejected() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/encoded-route-secret.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"DELETE\",\"path\":\"/auth/sk%2Dtest123\",\"status\":null,\"content_type\":\"application/json\",\"body\":null}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"http\",\"payload\":{\"method\":\"DELETE\",\"path\":\"/auth/sk%2Dtest123\",\"status\":200,\"content_type\":\"application/json\",\"body\":true}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "a percent-encoded route segment must not hide a secret prefix",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.line == 1
            && issue.category == "leak: secret prefix sk-"
            && issue.pointer.as_deref() == Some("/payload/path")
    }));
    Ok(())
}

#[test]
fn opencode_http_body_path_object_is_scanned_as_filesystem_data() -> TestResult {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let recorded_path = manifest
        .ancestors()
        .map(|ancestor| {
            ancestor
                .join("tests/fixtures")
                .join("opencode/08-session-load.jsonl")
        })
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "recorded OpenCode session-load fixture is missing",
            )
        })?;
    let recorded = fs::read_to_string(recorded_path)?;
    let mut lines = recorded.lines();
    let request = lines.nth(4).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "recorded OpenCode fixture has no message-list request",
        )
    })?;
    let response = lines.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "recorded OpenCode fixture has no message-list response",
        )
    })?;
    let external = r"Z:\\kaleido-outside\\secret";
    let repro = format!("{request}\n{}\n", response.replace("<SANDBOX>", external));

    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(&fixtures, "opencode/http-body-path-object.jsonl", &repro)?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "filesystem paths nested under an HTTP body path object must not use the route exemption",
    )?;

    for pointer in [
        "/payload/body/1/info/path/cwd",
        "/payload/body/1/info/path/root",
    ] {
        assert!(error.issues().iter().any(|issue| {
            issue.line == 2
                && issue.category == "leak: absolute path outside fixture sandbox"
                && issue.pointer.as_deref() == Some(pointer)
        }));
    }
    Ok(())
}

#[test]
fn strict_outer_shape_and_monotonic_timestamp_are_enforced() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/shape.jsonl",
        concat!(
            "{\"ts_ms\":5,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{}}\n",
            "{\"ts_ms\":4,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{}}\n",
            "{\"ts_ms\":6,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{},\"extra\":true}\n"
        ),
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "structural violations must fail before schemas are loaded",
    )?;
    let issues = error.issues();

    assert!(issues.iter().any(|issue| {
        issue.line == 2
            && issue.category == "ts_ms must be non-decreasing within a fixture"
            && issue.pointer.as_deref() == Some("/ts_ms")
    }));
    assert!(issues.iter().any(|issue| {
        issue.line == 3
            && issue.category == "outer object must contain exactly ts_ms, dir, transport, payload"
    }));
    Ok(())
}

#[test]
fn username_home_sensitive_field_and_outside_path_are_reported_by_pointer() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/privacy.jsonl",
        r#"{"ts_ms":0,"dir":"c2s","transport":"stdio","payload":{"cwd":"C:\\Users\\Alice\\private","headers":{"authorization":"secret"}}}"#,
    )?;
    let identity = Identity::new(
        Some("Alice".to_owned()),
        Some(PathBuf::from(r"C:\Users\Alice")),
    );

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &identity,
        ),
        "privacy leaks must fail before schemas are loaded",
    )?;
    let issues = error.issues();

    assert!(issues.iter().any(|issue| {
        issue.category == "leak: current username"
            && issue.pointer.as_deref() == Some("/payload/cwd")
    }));
    assert!(issues.iter().any(|issue| {
        issue.category == "leak: home directory" && issue.pointer.as_deref() == Some("/payload/cwd")
    }));
    assert!(issues.iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/cwd")
    }));
    assert!(issues.iter().any(|issue| {
        issue.category == "leak: unredacted sensitive field"
            && issue.pointer.as_deref() == Some("/payload/headers/authorization")
    }));
    assert!(!error.to_string().contains("Alice"));
    assert!(!error.to_string().contains("secret"));
    Ok(())
}

#[test]
fn unc_and_unix_absolute_paths_are_rejected() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/absolute-paths.jsonl",
        r#"{"ts_ms":0,"dir":"c2s","transport":"stdio","payload":{"network":"\\\\server\\share\\secret.txt","unix":"/mnt/data/secret.txt","home":"/home/user/private.txt","windows":"C:\\Users\\x\\private.txt","single":"/name"}}"#,
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "UNC and Unix absolute paths outside the sandbox must fail",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/network")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/unix")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/home")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/windows")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/single")
    }));
    Ok(())
}

#[test]
fn embedded_paths_after_punctuation_and_unicode_space_are_rejected() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/embedded-absolute-paths.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{",
            "\"paren_windows\":\"cat(Z:\\\\outside\\\\secret.txt)\",",
            "\"paren_unix\":\"cat(/mnt/data/secret.txt)\",",
            "\"single_unix\":\"cat /secret\",",
            "\"unicode_space\":\"run\\u00a0Z:\\\\outside\\\\secret.txt\",",
            "\"file_uri\":\"file:/mnt/data/secret.txt\"}}\n"
        ),
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::default(),
        ),
        "punctuation and Unicode whitespace must not hide absolute paths",
    )?;

    for pointer in [
        "/payload/paren_windows",
        "/payload/paren_unix",
        "/payload/single_unix",
        "/payload/unicode_space",
        "/payload/file_uri",
    ] {
        assert!(error.issues().iter().any(|issue| {
            issue.category == "leak: absolute path outside fixture sandbox"
                && issue.pointer.as_deref() == Some(pointer)
        }));
    }
    Ok(())
}

#[test]
fn username_named_json_key_and_embedded_secret_like_word_are_not_leaks() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/benign-identifiers.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{",
            "\"root\":\"tree\",\"task\":\"task-based\",\"token\":\"sk-test123\"}}\n"
        ),
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &fixtures.join("sandbox"),
            &Identity::new(Some("root".to_owned()), None),
        ),
        "the deliberate token must fail before schemas are loaded",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: secret prefix sk-"
            && issue.pointer.as_deref() == Some("/payload/token")
    }));
    assert!(!error
        .issues()
        .iter()
        .any(|issue| issue.category == "leak: current username"));
    assert!(!error.issues().iter().any(|issue| {
        issue.category == "leak: secret prefix sk-"
            && issue.pointer.as_deref() == Some("/payload/task")
    }));
    Ok(())
}

#[test]
fn sandbox_dot_segment_traversal_is_rejected_for_both_separators() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    let sandbox = fixtures.join("sandbox");
    let sandbox_forward = sandbox.to_string_lossy().replace('\\', "/");
    let sandbox_backward = sandbox_forward.replace('/', "\\");
    let unix_escape = format!("{sandbox_forward}/safe/./../../secret.txt");
    let windows_escape = format!(r"{sandbox_backward}\safe\.\..\..\secret.txt");
    let unix_escape_at_end = format!("{sandbox_forward}/..");
    let windows_escape_at_end = format!(r"{sandbox_backward}\..");
    let unix_inside = format!("{sandbox_forward}/safe/../inside.txt");
    let windows_inside = format!(r"{sandbox_backward}\safe\..\inside.txt");
    let payload = serde_json::json!({
        "unix": unix_escape,
        "windows": windows_escape,
        "unix_end": unix_escape_at_end,
        "windows_end": windows_escape_at_end,
        "placeholder": "<SANDBOX>/safe/./../../secret.txt",
        "placeholder_end": r"<SANDBOX>\..",
        "unix_inside": unix_inside,
        "windows_inside": windows_inside,
        "placeholder_inside": "<SANDBOX>/safe/../inside.txt"
    });
    let record = serde_json::json!({
        "ts_ms": 0,
        "dir": "c2s",
        "transport": "stdio",
        "payload": payload
    });
    write_fixture(
        &fixtures,
        "codex/traversal.jsonl",
        &serde_json::to_string(&record)?,
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &root.path().join("missing-schemas"),
            &sandbox,
            &Identity::default(),
        ),
        "sandbox traversal must fail before schemas are loaded",
    )?;

    for pointer in [
        "/payload/unix",
        "/payload/windows",
        "/payload/unix_end",
        "/payload/windows_end",
        "/payload/placeholder",
        "/payload/placeholder_end",
    ] {
        assert!(error.issues().iter().any(|issue| {
            issue.category == "leak: absolute path outside fixture sandbox"
                && issue.pointer.as_deref() == Some(pointer)
        }));
    }
    for pointer in [
        "/payload/unix_inside",
        "/payload/windows_inside",
        "/payload/placeholder_inside",
    ] {
        assert!(!error.issues().iter().any(|issue| {
            issue.category == "leak: absolute path outside fixture sandbox"
                && issue.pointer.as_deref() == Some(pointer)
        }));
    }
    Ok(())
}

#[test]
fn single_leading_backslash_root_outside_sandbox_is_reported_as_a_leak() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    let sandbox = PathBuf::from(r"\kaleido-t102-sandbox");
    let request = serde_json::json!({
        "ts_ms": 0,
        "dir": "c2s",
        "transport": "http",
        "payload": {
            "method": "GET",
            "path": "/global/health",
            "status": null,
            "content_type": "application/json",
            "body": null
        }
    });
    let response = serde_json::json!({
        "ts_ms": 1,
        "dir": "s2c",
        "transport": "http",
        "payload": {
            "method": "GET",
            "path": "/global/health",
            "status": 200,
            "content_type": "application/json",
            "body": {
                "healthy": true,
                "version": r"open(\foo\secret.txt)"
            }
        }
    });
    write_fixture(
        &fixtures,
        "opencode/single-backslash-root.jsonl",
        &format!(
            "{}\n{}\n",
            serde_json::to_string(&request)?,
            serde_json::to_string(&response)?
        ),
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &repository_schemas()?,
            &sandbox,
            &Identity::default(),
        ),
        "the rooted backslash path must be rejected",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.line == 2
            && issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/body/version")
    }));
    Ok(())
}

#[test]
fn single_leading_backslash_root_inside_sandbox_is_not_reported() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    let sandbox = PathBuf::from(r"\kaleido-t102-sandbox");
    let request = serde_json::json!({
        "ts_ms": 0,
        "dir": "c2s",
        "transport": "http",
        "payload": {
            "method": "GET",
            "path": "/global/health",
            "status": null,
            "content_type": "application/json",
            "body": null
        }
    });
    let response = serde_json::json!({
        "ts_ms": 1,
        "dir": "s2c",
        "transport": "http",
        "payload": {
            "method": "GET",
            "path": "/global/health",
            "status": 200,
            "content_type": "application/json",
            "body": {
                "healthy": true,
                "version": r"\kaleido-t102-sandbox\safe-\inside.txt"
            }
        }
    });
    write_fixture(
        &fixtures,
        "opencode/single-backslash-inside.jsonl",
        &format!(
            "{}\n{}\n",
            serde_json::to_string(&request)?,
            serde_json::to_string(&response)?
        ),
    )?;

    let summary = verify_paths(
        &fixtures,
        &repository_schemas()?,
        &sandbox,
        &Identity::default(),
    )?;

    assert_eq!(summary.files, 1);
    assert_eq!(summary.records, 2);
    assert_eq!(summary.opencode_files, 1);
    Ok(())
}

#[test]
fn unix_sandbox_comparison_remains_case_sensitive() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/unix-case.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"/tmp/kaleido/secret.txt\",\"version\":\"/tmp/Kaleido/inside.txt\"}}}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"result\":{\"codexHome\":\"<HOME>\",\"platformFamily\":\"linux\",\"platformOs\":\"linux\",\"userAgent\":\"test\"}}}\n"
        ),
    )?;

    let error = verification_error(
        verify_paths(
            &fixtures,
            &repository_schemas()?,
            Path::new("/tmp/Kaleido"),
            &Identity::default(),
        ),
        "Unix paths that differ from the sandbox only by case must remain outside",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/params/clientInfo/name")
    }));
    assert!(!error.issues().iter().any(|issue| {
        issue.category == "leak: absolute path outside fixture sandbox"
            && issue.pointer.as_deref() == Some("/payload/params/clientInfo/version")
    }));
    Ok(())
}

#[test]
fn valid_codex_request_passes_method_schema_validation() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/valid.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"Use /design-sync or /loop 5m /foo; see https://example.test/home/user/x and references/palette.md\",\"version\":\"1\"},\"api_key\":\"<REDACTED_TOKEN>\",\"authorization\":\"<REDACTED_TOKEN>\"}}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"result\":{\"codexHome\":\"<HOME>\",\"platformFamily\":\"windows\",\"platformOs\":\"windows\",\"userAgent\":\"test\"}}}\n"
        ),
    )?;

    let summary = verify_with_repository_schemas(&fixtures)??;

    assert_eq!(summary.files, 1);
    assert_eq!(summary.records, 2);
    assert_eq!(summary.codex_files, 1);
    Ok(())
}

#[test]
fn codex_schema_error_points_to_missing_required_field() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/invalid.jsonl",
        r#"{"ts_ms":0,"dir":"c2s","transport":"stdio","payload":{"id":1,"method":"initialize","params":{}}}"#,
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "missing clientInfo must fail Codex schema validation",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "Codex method schema validation failed"
            && issue.pointer.as_deref() == Some("/payload/params/clientInfo")
    }));
    Ok(())
}

#[test]
fn unknown_method_and_unknown_response_id_are_errors() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/unknown.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"method\":\"unknown/method\",\"params\":{}}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"id\":999,\"result\":{}}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "unknown methods and response ids must fail",
    )?;

    assert!(error
        .issues()
        .iter()
        .any(|issue| issue.category == "unknown Codex method"));
    assert!(error
        .issues()
        .iter()
        .any(|issue| issue.category == "unknown Codex response id"));
    Ok(())
}

#[test]
fn bidirectional_json_rpc_may_reuse_the_same_request_id() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/bidirectional-ids.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"verifier-test\",\"version\":\"1\"}}}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"method\":\"attestation/generate\",\"params\":{}}}\n",
            "{\"ts_ms\":2,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"error\":{\"code\":-32000,\"message\":\"test error\"}}}\n",
            "{\"ts_ms\":3,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"error\":{\"code\":-32000,\"message\":\"test error\"}}}\n"
        ),
    )?;

    let summary = verify_with_repository_schemas(&fixtures)??;

    assert_eq!(summary.files, 1);
    assert_eq!(summary.records, 4);
    Ok(())
}

#[test]
fn valid_acp_request_uses_exact_method_definition() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "acp-claude/valid.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1}}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1}}}\n"
        ),
    )?;

    let summary = verify_with_repository_schemas(&fixtures)??;

    assert_eq!(summary.acp_files, 1);
    assert_eq!(summary.records, 2);
    Ok(())
}

#[test]
fn acp_unknown_method_and_response_id_are_errors() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "acp-claude/unknown.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"unknown/method\",\"params\":{}}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "unknown ACP methods and response ids must fail",
    )?;

    assert!(error
        .issues()
        .iter()
        .any(|issue| issue.category == "unknown ACP method"));
    assert!(error
        .issues()
        .iter()
        .any(|issue| issue.category == "unknown ACP response id"));
    Ok(())
}

#[test]
fn acp_method_params_must_be_present_and_non_null() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "acp-claude/missing-params.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1}}}\n",
            "{\"ts_ms\":2,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":null}}\n",
            "{\"ts_ms\":3,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"protocolVersion\":1}}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "missing and null ACP params must fail closed",
    )?;

    for line in [1, 3] {
        assert!(error.issues().iter().any(|issue| {
            issue.line == line
                && issue.category == "ACP method params must be present and non-null"
                && issue.pointer.as_deref() == Some("/payload/params")
        }));
    }
    Ok(())
}

#[test]
fn unmatched_codex_and_acp_requests_fail_at_end_of_file() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "codex/unmatched.jsonl",
        "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"verifier-test\",\"version\":\"1\"}}}}\n",
    )?;
    write_fixture(
        &fixtures,
        "acp-claude/unmatched.jsonl",
        "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1}}}\n",
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "outstanding JSON-RPC requests must fail at end of file",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "Codex request has no matching response"
            && issue.pointer.as_deref() == Some("/payload/id")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.category == "ACP request has no matching response"
            && issue.pointer.as_deref() == Some("/payload/id")
    }));
    Ok(())
}

#[test]
fn opencode_http_operation_and_response_body_are_schema_checked() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/health.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/global/health\",\"status\":null,\"content_type\":\"application/json\",\"body\":null}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/global/health\",\"status\":200,\"content_type\":\"application/json; charset=utf-8\",\"body\":{\"healthy\":true,\"version\":\"1\"}}}\n",
            "{\"ts_ms\":2,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/global/health\",\"status\":null,\"content_type\":\"application/json\",\"body\":null}}\n",
            "{\"ts_ms\":3,\"dir\":\"s2c\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/global/health\",\"status\":200,\"content_type\":\"application/json\",\"body\":{\"healthy\":true}}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "the second health response is missing a required field",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.line == 4
            && issue.category == "OpenCode response body schema validation failed"
            && issue.pointer.as_deref() == Some("/payload/body/version")
    }));
    assert!(!error
        .issues()
        .iter()
        .any(|issue| matches!(issue.line, 1..=3)));
    Ok(())
}

#[test]
fn opencode_http_response_without_request_is_rejected() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/unmatched-response.jsonl",
        "{\"ts_ms\":0,\"dir\":\"s2c\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/global/health\",\"status\":200,\"content_type\":\"application/json\",\"body\":{\"healthy\":true,\"version\":\"1\"}}}\n",
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "an OpenCode HTTP response without a request must fail",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "OpenCode HTTP response has no matching request"
            && issue.pointer.as_deref() == Some("/payload/path")
    }));
    Ok(())
}

#[test]
fn opencode_required_path_and_query_parameters_are_checked() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/parameters.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/experimental/tool?provider=test\",\"status\":null,\"content_type\":\"application/json\",\"body\":null}}\n",
            "{\"ts_ms\":1,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/session/not-a-session\",\"status\":null,\"content_type\":\"application/json\",\"body\":null}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "missing query parameters and invalid path parameters must fail",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.line == 1
            && issue.category == "OpenCode required query parameter is missing"
            && issue.pointer.as_deref() == Some("/payload/path")
    }));
    assert!(error.issues().iter().any(|issue| {
        issue.line == 2
            && issue.category == "OpenCode path parameter schema validation failed"
            && issue.pointer.as_deref() == Some("/payload/path")
    }));
    Ok(())
}

#[test]
fn opencode_operation_without_request_body_rejects_empty_object() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/undeclared-body.jsonl",
        "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/global/health\",\"status\":null,\"content_type\":\"application/json\",\"body\":{}}}\n",
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "an empty object is still a body when the operation declares none",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "HTTP operation does not declare a body"
            && issue.pointer.as_deref() == Some("/payload/body")
    }));
    Ok(())
}

#[test]
fn opencode_sse_payload_is_checked_against_event_schema() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/events.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/event\",\"status\":null,\"content_type\":\"\",\"body\":null}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"sse\",\"payload\":{\"id\":\"evt_test\",\"type\":\"catalog.updated\",\"properties\":{}}}\n",
            "{\"ts_ms\":2,\"dir\":\"s2c\",\"transport\":\"sse\",\"payload\":{\"id\":\"evt_test\",\"type\":\"not.a.real.event\",\"properties\":{}}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "an unknown SSE event variant must fail schema validation",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.line == 3
            && issue.category == "OpenCode SSE Event schema validation failed"
            && issue.pointer.as_deref() == Some("/payload/type")
    }));
    assert!(!error
        .issues()
        .iter()
        .any(|issue| matches!(issue.line, 1 | 2)));
    Ok(())
}

#[test]
fn opencode_sse_schema_follows_the_pending_stream_endpoint() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/stream-schemas.jsonl",
        concat!(
            "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/event\",\"status\":null,\"content_type\":\"\",\"body\":null}}\n",
            "{\"ts_ms\":1,\"dir\":\"s2c\",\"transport\":\"sse\",\"payload\":{\"id\":\"evt_test\",\"type\":\"catalog.updated\",\"properties\":{}}}\n",
            "{\"ts_ms\":2,\"dir\":\"c2s\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/global/event\",\"status\":null,\"content_type\":\"\",\"body\":null}}\n",
            "{\"ts_ms\":3,\"dir\":\"s2c\",\"transport\":\"sse\",\"payload\":{\"id\":\"evt_test\",\"type\":\"catalog.updated\",\"properties\":{}}}\n"
        ),
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "a /global/event frame must be checked as GlobalEvent rather than Event",
    )?;

    for pointer in ["/payload/directory", "/payload/payload"] {
        assert!(error.issues().iter().any(|issue| {
            issue.line == 4
                && issue.category == "OpenCode SSE Event schema validation failed"
                && issue.pointer.as_deref() == Some(pointer)
        }));
    }
    assert!(!error
        .issues()
        .iter()
        .any(|issue| matches!(issue.line, 1..=3)));
    Ok(())
}

#[test]
fn opencode_sse_without_a_stream_request_is_rejected() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/unmatched-event.jsonl",
        "{\"ts_ms\":0,\"dir\":\"s2c\",\"transport\":\"sse\",\"payload\":{\"id\":\"evt_test\",\"type\":\"catalog.updated\",\"properties\":{}}}\n",
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "an SSE frame without an endpoint context must fail closed",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "OpenCode SSE event has no matching stream request"
            && issue.pointer.as_deref() == Some("/payload")
    }));
    Ok(())
}

#[test]
fn opencode_unknown_http_operation_is_rejected() -> TestResult {
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(
        &fixtures,
        "opencode/unknown.jsonl",
        "{\"ts_ms\":0,\"dir\":\"s2c\",\"transport\":\"http\",\"payload\":{\"method\":\"GET\",\"path\":\"/not-a-real-operation\",\"status\":200,\"content_type\":\"application/json\",\"body\":{}}}\n",
    )?;

    let error = verification_error(
        verify_with_repository_schemas(&fixtures)?,
        "an unknown HTTP operation must fail",
    )?;

    assert!(error.issues().iter().any(|issue| {
        issue.category == "unknown or ambiguous OpenCode HTTP operation"
            && issue.pointer.as_deref() == Some("/payload/path")
    }));
    Ok(())
}

#[test]
fn recorded_fixtures_pass_repository_schemas() -> TestResult {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_root = manifest
        .ancestors()
        .map(|ancestor| ancestor.join("tests/fixtures"))
        .find(|candidate| candidate.join("acp-claude/06-cancel.jsonl").is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "recorded fixtures are missing"))?;
    let acp = fs::read_to_string(fixture_root.join("acp-claude/06-cancel.jsonl"))?;
    let opencode = fs::read_to_string(fixture_root.join("opencode/08-session-load.jsonl"))?;
    let root = tempdir()?;
    let fixtures = root.path().join("tests/fixtures");
    write_fixture(&fixtures, "acp-claude/06-cancel.jsonl", &acp)?;
    write_fixture(&fixtures, "opencode/08-session-load.jsonl", &opencode)?;

    let summary = verify_with_repository_schemas(&fixtures)??;

    assert_eq!(summary.files, 2);
    assert_eq!(summary.records, 14);
    assert_eq!(summary.acp_files, 1);
    assert_eq!(summary.opencode_files, 1);
    Ok(())
}

#[test]
fn recorded_claude_success_uses_closed_sdk_events_and_is_acceptance() -> TestResult {
    let fixture_root = repository_claude_fixtures()?;
    let summary = verify_claude_sidecar_paths(&fixture_root, &Identity::default())?;

    assert_eq!(summary.files, 1);
    assert_eq!(summary.records, 7);
    assert_eq!(summary.acceptance_files, 1);
    assert_eq!(summary.auth_failure_files, 0);
    Ok(())
}

#[test]
fn claude_acceptance_fixture_rejects_a_terminal_error_mutation() -> TestResult {
    let source_root = repository_claude_fixtures()?;
    let root = tempdir()?;
    let fixtures = root.path().join("fixtures");
    let sandbox = fixtures.join("sandbox");
    fs::create_dir_all(&sandbox)?;
    let recording = fs::read_to_string(source_root.join("sandbox/real-sdk-simple-turn.jsonl"))?;
    let mutated = recording.replace("\"is_error\":false", "\"is_error\":true");
    fs::write(sandbox.join("real-sdk-simple-turn.jsonl"), mutated)?;
    fs::copy(
        source_root.join("sandbox/real-sdk-simple-turn.metadata.json"),
        sandbox.join("real-sdk-simple-turn.metadata.json"),
    )?;

    let error = verification_error(
        verify_claude_sidecar_paths(&fixtures, &Identity::default()),
        "a terminal error must not pass as acceptance evidence",
    )?;
    assert!(error.issues().iter().any(|issue| {
        issue.category == "Claude acceptance fixture contains authentication-failure evidence"
            && issue.pointer.as_deref() == Some("/payload/event")
    }));
    Ok(())
}

fn verify_with_repository_schemas(
    fixtures: &Path,
) -> io::Result<Result<VerifySummary, FixtureVerifyError>> {
    Ok(verify_paths(
        fixtures,
        &repository_schemas()?,
        &fixtures.join("sandbox"),
        &Identity::default(),
    ))
}

fn repository_schemas() -> io::Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .map(|ancestor| ancestor.join("schemas"))
        .find(|candidate| candidate.join("acp/schema.json").is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "repository schema snapshots are unavailable to verifier tests",
            )
        })
}

fn repository_claude_fixtures() -> io::Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .map(|ancestor| ancestor.join("crates/kaleido-adapter-claude/tests/fixtures"))
        .find(|candidate| {
            candidate
                .join("sandbox/real-sdk-simple-turn.jsonl")
                .is_file()
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Claude fixtures are missing"))
}

fn write_fixture(root: &Path, relative: &str, contents: &str) -> io::Result<()> {
    let path = root.join(relative);
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture path must have a parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    fs::write(path, contents)
}

fn verification_error<T>(
    result: Result<T, FixtureVerifyError>,
    success_message: &'static str,
) -> io::Result<FixtureVerifyError> {
    match result {
        Err(error) => Ok(error),
        Ok(_) => Err(io::Error::other(success_message)),
    }
}

#[cfg(unix)]
fn create_fixture_link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_fixture_link(target: &Path, link: &Path) -> std::io::Result<()> {
    use std::process::{Command, Stdio};

    let link = link.to_string_lossy().replace('/', "\\");
    let target = target.to_string_lossy().replace('/', "\\");
    let command = format!(r#"mklink /J "{link}" "{target}""#);
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J", &link, &target])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "mklink /J command `{command}` failed with {}: {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}
