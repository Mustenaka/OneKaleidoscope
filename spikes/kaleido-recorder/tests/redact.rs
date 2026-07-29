use std::env;
use std::error::Error;
use std::path::Path;

use kaleido_recorder::redact::{detect_leaks, LeakKind, RedactionError, Redactor};
use serde_json::Value;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn current_username_is_detected() -> TestResult {
    let username = env::var("USERNAME").or_else(|_| env::var("USER"))?;
    let raw = format!(r#"{{"message":"owned by {username}"}}"#);
    let value: Value = serde_json::from_str(&raw)?;

    let leaks = detect_leaks(&raw, &value);

    assert!(leaks.contains(&LeakKind::Username));
    Ok(())
}

#[test]
fn secret_prefix_is_detected() -> TestResult {
    let raw = r#"{"token":"sk-test123"}"#;
    let value: Value = serde_json::from_str(raw)?;

    let leaks = detect_leaks(raw, &value);

    assert!(leaks.contains(&LeakKind::SecretPrefix("sk-")));
    Ok(())
}

#[test]
fn clean_payload_is_not_reported() -> TestResult {
    let raw = r#"{"message":"fixture-safe text","path":"<SANDBOX>/notes.txt"}"#;
    let value: Value = serde_json::from_str(raw)?;

    assert!(detect_leaks(raw, &value).is_empty());
    Ok(())
}

#[test]
fn replacement_is_deterministic() {
    let redactor = Redactor::from_pairs([
        ("fixture-home/sample".to_owned(), "<HOME>"),
        ("sample".to_owned(), "<USER>"),
    ]);
    let input = r#"{"first":"fixture-home/sample","second":"sample"}"#;

    let first = redactor.redact(input);
    let second = redactor.redact(input);

    assert_eq!(first, second);
    assert_eq!(first, r#"{"first":"<HOME>","second":"<USER>"}"#);
}

#[test]
fn available_commands_update_is_replaced_with_a_deterministic_count_summary() {
    let redactor = Redactor::from_pairs([]);
    let before = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture-session","update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"private-one","description":"first","input":{"hint":"[one]"}},{"name":"private-two","description":"second","input":{"hint":"[two]"}}],"tail":"unchanged"}}}"#;
    let after = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture-session","update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"<REDACTED_COMMAND>","description":"<REDACTED_COMMAND>","_meta":{"kaleidoRedaction":"available_commands_update","originalCount":2}}],"tail":"unchanged"}}}"#;

    let first = redactor.redact(before);
    let second = redactor.redact(before);

    assert_eq!(first, after);
    assert_eq!(second, after);
    assert_eq!(redactor.redact(after), after);
}

#[test]
fn legacy_command_summary_migrates_without_losing_the_original_count() {
    let redactor = Redactor::from_pairs([]);
    let legacy = r#"{"method":"session/update","params":{"update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"<REDACTED_COMMAND>","count":58}]}}}"#;
    let expected = r#"{"method":"session/update","params":{"update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"<REDACTED_COMMAND>","description":"<REDACTED_COMMAND>","_meta":{"kaleidoRedaction":"available_commands_update","originalCount":58}}]}}}"#;

    assert_eq!(redactor.redact(legacy), expected);
}

#[test]
fn available_commands_redaction_does_not_escape_its_approved_protocol_scope() {
    let redactor = Redactor::from_pairs([]);
    let different_update = r#"{"method":"session/update","params":{"update":{"sessionUpdate":"plan","availableCommands":[{"name":"must-stay"}]}}}"#;
    let different_method = r#"{"method":"fixture/example","params":{"update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"must-stay"}]}}}"#;
    let sibling = r#"{"method":"session/update","params":{"other":{"availableCommands":[{"name":"sibling-must-stay"}]},"update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"private-one","description":"first"}]}}}"#;
    let sibling_expected = r#"{"method":"session/update","params":{"other":{"availableCommands":[{"name":"sibling-must-stay"}]},"update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"<REDACTED_COMMAND>","description":"<REDACTED_COMMAND>","_meta":{"kaleidoRedaction":"available_commands_update","originalCount":1}}]}}}"#;

    assert_eq!(redactor.redact(different_update), different_update);
    assert_eq!(redactor.redact(different_method), different_method);
    assert_eq!(redactor.redact(sibling), sibling_expected);
}

#[test]
fn malformed_available_commands_update_fails_closed() {
    let redactor = Redactor::from_pairs([]);
    for malformed in [
        r#"{"method":"session/update","params":{"update":{"sessionUpdate":"available_commands_update"}}}"#,
        r#"{"method":"session/update","params":{"update":{"sessionUpdate":"available_commands_update","availableCommands":{"name":"wrong"}}}}"#,
        r#"{"method":"session/update","params":{"update":{"sessionUpdate":"available_commands_update","availableCommands":[],"availableCommands":[]}}}"#,
    ] {
        assert_eq!(
            redactor.try_redact(malformed),
            Err(RedactionError::AvailableCommandsShape)
        );
        assert_eq!(redactor.redact(malformed), "<REDACTION_ERROR>");
    }
}

#[test]
fn repository_fixture_sandbox_is_replaced_before_absolute_path_scanning() -> TestResult {
    let sandbox = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("workspace root must be available")?
        .join("tests/fixtures/sandbox")
        .canonicalize()?;
    let redactor = Redactor::for_environment(&sandbox);
    let forward = sandbox.to_string_lossy().replace('\\', "/");
    let input = serde_json::to_string(&serde_json::json!({"directory": forward}))?;
    let api_input = r#"{"directory":"D:/Work/Code/Cross/OneKaleidoscope/tests/fixtures/sandbox"}"#;

    assert_eq!(redactor.redact(&input), r#"{"directory":"<SANDBOX>"}"#);
    assert_eq!(redactor.redact(api_input), r#"{"directory":"<SANDBOX>"}"#);
    Ok(())
}

#[test]
fn sensitive_fields_and_bearer_values_are_replaced_without_reordering() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"z":1,"authorization":"Bearer abc.def","api_key":"value","a":"unchanged"}"#;

    let redacted = redactor.redact(input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(
        redacted,
        r#"{"z":1,"authorization":"<REDACTED_TOKEN>","api_key":"<REDACTED_TOKEN>","a":"unchanged"}"#
    );
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn non_string_sensitive_fields_are_not_modified_and_are_rejected() -> TestResult {
    for (field, input) in [
        (
            "authorization",
            r#"{"authorization":{"scheme":"opaque","credential":"plain"},"tail":"unchanged"}"#,
        ),
        (
            "authorization",
            r#"{"authorization":["plain"],"tail":"unchanged"}"#,
        ),
        (
            "authorization",
            r#"{"authorization":null,"tail":"unchanged"}"#,
        ),
        (
            "authorization",
            r#"{"authorization":17,"tail":"unchanged"}"#,
        ),
        (
            "authorization",
            r#"{"authorization":false,"tail":"unchanged"}"#,
        ),
        (
            "api_key",
            r#"{"api_key":{"credential":"plain"},"tail":"unchanged"}"#,
        ),
        ("api_key", r#"{"api_key":["plain"],"tail":"unchanged"}"#),
        ("api_key", r#"{"api_key":null,"tail":"unchanged"}"#),
        ("api_key", r#"{"api_key":17,"tail":"unchanged"}"#),
        ("api_key", r#"{"api_key":false,"tail":"unchanged"}"#),
    ] {
        let redactor = Redactor::from_pairs([]);
        let redacted = redactor.redact(input);
        let value: Value = serde_json::from_str(&redacted)?;

        assert_eq!(redacted, input, "{field} shape was modified");
        assert!(
            detect_leaks(&redacted, &value).contains(&LeakKind::SensitiveField(field)),
            "{field} shape was not rejected"
        );
    }
    Ok(())
}

#[test]
fn secret_prefix_redaction_is_case_insensitive_and_keeps_json_valid() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"one":"SK-AbC+/=","two":"GHP_XyZ~","three":"BEARER abc.def"}"#;

    let redacted = redactor.redact(input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(
        redacted,
        r#"{"one":"<REDACTED_TOKEN>","two":"<REDACTED_TOKEN>","three":"<REDACTED_TOKEN>"}"#
    );
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn secret_prefix_redaction_does_not_modify_an_embedded_ordinary_substring() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input =
        r#"{"task":"task-based","mask":"flask-test","token":"sk-test123","auth":"Bearer abc"}"#;

    let redacted = redactor.redact(input);

    assert_eq!(
        redacted,
        r#"{"task":"task-based","mask":"flask-test","token":"<REDACTED_TOKEN>","auth":"<REDACTED_TOKEN>"}"#
    );
    let value: Value = serde_json::from_str(&redacted)?;
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn username_replacement_does_not_modify_an_identically_named_json_key() {
    let redactor = Redactor::from_pairs([("root".to_owned(), "<USER>")]);
    let input = r#"{"root":"tree","owner":"root"}"#;

    assert_eq!(
        redactor.redact(input),
        r#"{"root":"tree","owner":"<USER>"}"#
    );
}

#[test]
fn embedded_single_segment_unix_path_is_detected_and_redacted() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let raw = r#"{"command":"cat /secret"}"#;
    let value: Value = serde_json::from_str(raw)?;

    assert!(detect_leaks(raw, &value)
        .iter()
        .any(|leak| matches!(leak, LeakKind::OutsideSandboxPath(_))));
    assert_eq!(redactor.redact(raw), r#"{"command":"<OUTSIDE_PATH>"}"#);
    Ok(())
}

#[test]
fn outside_absolute_path_is_detected_inside_tool_argument() -> TestResult {
    let raw = r#"{"command":"read D:\\private\\secret.txt"}"#;
    let value: Value = serde_json::from_str(raw)?;

    let leaks = detect_leaks(raw, &value);

    assert!(leaks
        .iter()
        .any(|kind| matches!(kind, LeakKind::OutsideSandboxPath(_))));
    Ok(())
}

#[test]
fn whole_outside_path_is_redacted_without_reordering_other_fields() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"z":1,"cwd":"D:\\runtime\\outside","a":"unchanged"}"#;

    let redacted = redactor.redact(input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(
        redacted,
        r#"{"z":1,"cwd":"<OUTSIDE_PATH>","a":"unchanged"}"#
    );
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn sandbox_traversal_is_redacted_before_the_sandbox_prefix() -> TestResult {
    let sandbox = tempfile::tempdir()?;
    let redactor = Redactor::for_environment(sandbox.path());
    let sandbox_forward = sandbox.path().to_string_lossy().replace('\\', "/");
    let sandbox_backward = sandbox_forward.replace('/', "\\");
    let unix_escape = format!("{sandbox_forward}/./../secret.txt");
    let windows_escape = format!(r"{sandbox_backward}\.\..\secret.txt");
    let unix_inside = format!("{sandbox_forward}/safe/../inside.txt");
    let windows_inside = format!(r"{sandbox_backward}\safe\..\inside.txt");
    let input = format!(
        r#"{{"z":1,"unix":{},"windows":{},"unix_inside":{},"windows_inside":{},"a":"unchanged"}}"#,
        serde_json::to_string(&unix_escape)?,
        serde_json::to_string(&windows_escape)?,
        serde_json::to_string(&unix_inside)?,
        serde_json::to_string(&windows_inside)?
    );

    let redacted = redactor.redact(&input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(
        redacted,
        r#"{"z":1,"unix":"<OUTSIDE_PATH>","windows":"<OUTSIDE_PATH>","unix_inside":"<SANDBOX>/safe/../inside.txt","windows_inside":"<SANDBOX>\\safe\\..\\inside.txt","a":"unchanged"}"#
    );
    assert_eq!(redactor.redact(&unix_escape), "<OUTSIDE_PATH>");
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn redacted_sandbox_placeholder_cannot_hide_traversal() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"z":1,"cwd":"<SANDBOX>/safe/./../../secret.txt","a":"unchanged"}"#;
    let original: Value = serde_json::from_str(input)?;

    assert!(detect_leaks(input, &original)
        .iter()
        .any(|kind| matches!(kind, LeakKind::OutsideSandboxPath(_))));

    let redacted = redactor.redact(input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(
        redacted,
        r#"{"z":1,"cwd":"<OUTSIDE_PATH>","a":"unchanged"}"#
    );
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn string_containing_an_outside_path_is_replaced_as_one_redaction_unit() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"command":"read D:\\private\\secret.txt and summarize"}"#;

    let redacted = redactor.redact(input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(redacted, r#"{"command":"<OUTSIDE_PATH>"}"#);
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn unicode_before_an_outside_path_does_not_hide_the_path() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"description":"示例 D:\\Program Files\\agent\\bin.exe"}"#;

    let redacted = redactor.redact(input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(redacted, r#"{"description":"<OUTSIDE_PATH>"}"#);
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}

#[test]
fn generic_unix_paths_are_redacted_deterministically_without_reordering() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"z":1,"root":"/name","standalone_command":"/design-sync","prose":"inspect /a/b now","mnt":"/mnt/private/file","opt":"/opt/agent/bin","srv":"/srv/kaleido/data","admin":"/root/.claude/state","workspace":"/workspace/project/file","custom":"/volume/cache/item","a":"unchanged"}"#;
    let expected = r#"{"z":1,"root":"<OUTSIDE_PATH>","standalone_command":"<OUTSIDE_PATH>","prose":"<OUTSIDE_PATH>","mnt":"<OUTSIDE_PATH>","opt":"<OUTSIDE_PATH>","srv":"<OUTSIDE_PATH>","admin":"<OUTSIDE_PATH>","workspace":"<OUTSIDE_PATH>","custom":"<OUTSIDE_PATH>","a":"unchanged"}"#;

    let first = redactor.redact(input);
    let second = redactor.redact(input);
    let value: Value = serde_json::from_str(&first)?;

    assert_eq!(first, second);
    assert_eq!(first, expected);
    assert!(detect_leaks(&first, &value).is_empty());
    Ok(())
}

#[test]
fn generic_unix_paths_are_detected_before_redaction() -> TestResult {
    for (field, text) in [
        ("root", "/name"),
        ("standalone_command", "/design-sync"),
        ("prose", "inspect /a/b now"),
        ("mnt", "/mnt/private/file"),
        ("opt", "/opt/agent/bin"),
        ("srv", "/srv/kaleido/data"),
        ("admin", "/root/.claude/state"),
        ("workspace", "/workspace/project/file"),
        ("custom", "/volume/cache/item"),
    ] {
        let value = serde_json::json!({ field: text });
        let raw = serde_json::to_string(&value)?;
        let leaks = detect_leaks(&raw, &value);

        assert!(
            leaks.contains(&LeakKind::OutsideSandboxPath(field.to_owned())),
            "expected an absolute-path finding for {text}"
        );
    }
    Ok(())
}

#[test]
fn slash_commands_urls_and_relative_paths_are_not_paths() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"command":"Use /design-sync","loop":"/loop 5m /foo","url":"https://example.test/a/b","reference":"references/palette.md","relative":"ordinary a/b","embedded":"Use /code-review then /config"}"#;
    let value: Value = serde_json::from_str(input)?;

    assert_eq!(redactor.redact(input), input);
    assert!(detect_leaks(input, &value).is_empty());
    Ok(())
}

#[test]
fn http_envelope_route_is_preserved_while_other_fields_are_redacted() -> TestResult {
    let redactor = Redactor::from_pairs([]);
    let input = r#"{"method":"GET","path":"/global/health","status":200,"content_type":"application/json","body":{"cwd":"/srv/private/data"}}"#;
    let expected = r#"{"method":"GET","path":"/global/health","status":200,"content_type":"application/json","body":{"cwd":"<OUTSIDE_PATH>"}}"#;

    let redacted = redactor.redact(input);
    let value: Value = serde_json::from_str(&redacted)?;

    assert_eq!(redacted, expected);
    assert!(detect_leaks(&redacted, &value).is_empty());
    Ok(())
}
