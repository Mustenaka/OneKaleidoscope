#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

#[path = "../src/error.rs"]
pub mod error;
#[path = "../src/probe.rs"]
pub mod probe;
#[path = "../src/record.rs"]
pub mod record;

use iroh::{EndpointAddr, SecretKey};
use tempfile::tempdir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_spike-iroh")
}

#[test]
fn invalid_ticket_exits_20_with_concrete_error_and_does_not_panic() {
    let temp = tempdir().expect("temporary result directory must be created");
    let results = temp.path().join("invalid-ticket.jsonl");

    let output = Command::new(binary())
        .args([
            "dial",
            "not-base64!!",
            "--label",
            "4g-invalid",
            "--window-secs",
            "1",
            "--out",
        ])
        .arg(&results)
        .output()
        .expect("spike-iroh process must start");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(20));
    assert!(stderr.contains("ERROR InvalidTicketBase64:"));
    assert!(!stderr.contains("panicked"));
    assert!(matches!(
        probe::decode_ticket("not-base64!!"),
        Err(error::SpikeError::InvalidTicketBase64(_))
    ));

    let mut warnings = Vec::new();
    let records =
        record::read_records(&results, &mut warnings).expect("failure record must be readable");
    assert_eq!(records.len(), 1);
    let record = records
        .first()
        .expect("invalid ticket must persist one failure record");
    assert!(!record.connect_ok);
    assert!(record
        .error
        .as_deref()
        .is_some_and(|error| error.starts_with("InvalidTicketBase64:")));
    assert!(warnings.is_empty());
}

#[test]
fn valid_ticket_for_missing_endpoint_times_out_and_still_writes_failure_record() {
    let temp = tempdir().expect("temporary result directory must be created");
    let results = temp.path().join("missing-endpoint.jsonl");
    let missing = EndpointAddr::new(SecretKey::generate().public());
    let ticket = probe::encode_ticket(&missing).expect("syntactically valid ticket must encode");

    let started = Instant::now();
    let output = Command::new(binary())
        .arg("dial")
        .arg(ticket)
        .args(["--label", "4g-missing", "--window-secs", "1", "--out"])
        .arg(&results)
        .output()
        .expect("spike-iroh process must start");

    assert_eq!(output.status.code(), Some(20));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the command must honor its finite connection timeout"
    );

    let mut warnings = Vec::new();
    let records =
        record::read_records(&results, &mut warnings).expect("failure record must be readable");
    assert_eq!(records.len(), 1);
    let record = records
        .first()
        .expect("missing endpoint must persist one failure record");
    assert!(!record.connect_ok);
    assert_eq!(
        record.remote_endpoint_id.as_deref(),
        Some(missing.id.to_string().as_str())
    );
    assert!(record.error.is_some());
    assert!(warnings.is_empty());
}

#[test]
fn summarize_reports_corrupt_line_number_and_keeps_valid_records() {
    let temp = tempdir().expect("temporary result directory must be created");
    let results = temp.path().join("mixed.jsonl");
    let mut first = probe::new_record("dial", "4g", 30);
    first.connect_ok = true;
    first.ended_direct = true;
    first.selected_path_at_end = Some("ip".to_owned());
    first.direct_path_selected_ms = Some(1_000);
    let mut second = probe::new_record("dial", "4g", 30);
    second.connect_ok = true;
    second.selected_path_at_end = Some("relay".to_owned());
    let contents = format!(
        "{}\n{{this is not json}}\n{}\n",
        serde_json::to_string(&first).expect("first record must serialize"),
        serde_json::to_string(&second).expect("second record must serialize")
    );
    fs::write(&results, contents).expect("mixed JSONL fixture must be written");

    let output = Command::new(binary())
        .arg("summarize")
        .arg(&results)
        .output()
        .expect("spike-iroh process must start");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("runs=2"));
    assert!(stdout.contains("direct=1 (50.0%)"));
    assert!(stderr.contains("line 2"));
    assert!(!stderr.contains("panicked"));
}
