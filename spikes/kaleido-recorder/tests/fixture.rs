use std::error::Error;
use std::io::{self, Write};
use std::time::Duration;

use kaleido_recorder::fixture::{
    http_response_payload, Direction, FixtureError, FixtureSink, Transport,
};
use kaleido_recorder::redact::Redactor;

#[test]
fn record_preserves_payload_key_order_and_uses_relative_time() -> Result<(), Box<dyn Error>> {
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    sink.record_at(
        Duration::from_millis(42),
        Direction::S2c,
        Transport::Stdio,
        r#"{"z":1,"a":2}"#,
    )?;

    let line = String::from_utf8(sink.into_inner())?;
    assert_eq!(
        line,
        "{\"ts_ms\":42,\"dir\":\"s2c\",\"transport\":\"stdio\",\"payload\":{\"z\":1,\"a\":2}}\n"
    );
    Ok(())
}

#[test]
fn record_redacts_secret_before_writing_any_bytes() -> Result<(), Box<dyn Error>> {
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    sink.record_at(
        Duration::ZERO,
        Direction::C2s,
        Transport::Stdio,
        r#"{"token":"sk-test123"}"#,
    )?;

    let output = String::from_utf8(sink.into_inner())?;
    assert!(!output.contains("sk-test123"));
    assert!(output.contains("<REDACTED_TOKEN>"));
    Ok(())
}

#[test]
fn record_replaces_unmapped_absolute_path_before_writing() -> Result<(), Box<dyn Error>> {
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    sink.record_at(
        Duration::ZERO,
        Direction::C2s,
        Transport::Stdio,
        r#"{"path":"D:\\outside\\secret.txt"}"#,
    )?;

    let output = String::from_utf8(sink.into_inner())?;
    assert!(!output.contains(r#"D:\\outside\\secret.txt"#));
    assert_eq!(
        output,
        "{\"ts_ms\":0,\"dir\":\"c2s\",\"transport\":\"stdio\",\"payload\":{\"path\":\"<OUTSIDE_PATH>\"}}\n"
    );
    Ok(())
}

#[test]
fn record_rejects_non_object_payload() -> Result<(), Box<dyn Error>> {
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    let Err(error) = sink.record_at(Duration::ZERO, Direction::S2c, Transport::Sse, "null") else {
        return Err("fixture payload must be rejected".into());
    };

    assert!(matches!(error, FixtureError::PayloadShape));
    assert!(sink.into_inner().is_empty());
    Ok(())
}

#[test]
fn record_rejects_non_string_sensitive_fields_before_writing() -> Result<(), Box<dyn Error>> {
    for payload in [
        r#"{"authorization":{"scheme":"opaque","credential":"plain"}}"#,
        r#"{"authorization":["plain"]}"#,
        r#"{"authorization":null}"#,
        r#"{"authorization":17}"#,
        r#"{"authorization":false}"#,
        r#"{"api_key":{"credential":"plain"}}"#,
        r#"{"api_key":["plain"]}"#,
        r#"{"api_key":null}"#,
        r#"{"api_key":17}"#,
        r#"{"api_key":false}"#,
    ] {
        let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

        let Err(error) = sink.record_at(Duration::ZERO, Direction::S2c, Transport::Stdio, payload)
        else {
            return Err(format!("sensitive shape was accepted: {payload}").into());
        };

        assert!(matches!(error, FixtureError::Leak { .. }));
        assert!(sink.into_inner().is_empty());
    }
    Ok(())
}

#[test]
fn writer_failure_is_propagated() -> Result<(), Box<dyn Error>> {
    let mut sink = FixtureSink::new(FailingWriter, Redactor::from_pairs([]));

    let Err(error) = sink.record_at(
        Duration::ZERO,
        Direction::C2s,
        Transport::Http,
        r#"{"method":"GET","path":"/session","status":null,"content_type":"application/json","body":null}"#,
    ) else {
        return Err("writer failure must be propagated".into());
    };

    assert!(matches!(error, FixtureError::Write(_)));
    Ok(())
}

#[test]
fn http_envelope_retains_raw_scalar_body() -> Result<(), Box<dyn Error>> {
    let payload = http_response_payload("GET", "/global/health", 200, "application/json", "null")?;

    assert_eq!(
        payload,
        r#"{"method":"GET","path":"/global/health","status":200,"content_type":"application/json","body":null}"#
    );
    Ok(())
}

#[derive(Debug)]
struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected writer failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
