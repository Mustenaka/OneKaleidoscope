use std::error::Error;
use std::io::Cursor;

use kaleido_recorder::fixture::FixtureSink;
use kaleido_recorder::redact::Redactor;
use kaleido_recorder::sse_tee::{record_stream, SseError};

#[test]
fn records_crlf_event_and_eof_event_without_reordering_payload() -> Result<(), Box<dyn Error>> {
    let stream = concat!(
        "event: update\r\n",
        "data: {\"type\":\"first\",\"z\":1,\"a\":2}\r\n",
        "\r\n",
        "data: {\"type\":\"last\"}"
    );
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    let count = record_stream(Cursor::new(stream), &mut sink, None)?;
    let output = String::from_utf8(sink.into_inner())?;

    assert_eq!(count, 2);
    assert!(output.contains(r#""payload":{"type":"first","z":1,"a":2}"#));
    assert!(output.contains(r#""payload":{"type":"last"}"#));
    Ok(())
}

#[test]
fn joins_multiple_data_lines_as_specified_by_sse() -> Result<(), Box<dyn Error>> {
    let stream = "data: {\"type\":\"message\",\ndata: \"value\":1}\n\n";
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    let count = record_stream(Cursor::new(stream), &mut sink, None)?;
    let output = String::from_utf8(sink.into_inner())?;

    assert_eq!(count, 1);
    assert!(output.contains("\"payload\":{\"type\":\"message\",\n\"value\":1}"));
    Ok(())
}

#[test]
fn malformed_event_fails_without_installing_a_record() -> Result<(), Box<dyn Error>> {
    let stream = "data: {not-json}\n\n";
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    let Err(error) = record_stream(Cursor::new(stream), &mut sink, None) else {
        return Err("malformed SSE JSON must fail".into());
    };

    assert!(matches!(error, SseError::Fixture(_)));
    assert!(sink.into_inner().is_empty());
    Ok(())
}

#[test]
fn maximum_event_limit_stops_after_requested_count() -> Result<(), Box<dyn Error>> {
    let stream = "data: {\"type\":\"one\"}\n\ndata: {\"type\":\"two\"}\n\n";
    let mut sink = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    let count = record_stream(Cursor::new(stream), &mut sink, Some(1))?;
    let output = String::from_utf8(sink.into_inner())?;

    assert_eq!(count, 1);
    assert!(output.contains(r#""payload":{"type":"one"}"#));
    assert!(!output.contains(r#""payload":{"type":"two"}"#));
    Ok(())
}
