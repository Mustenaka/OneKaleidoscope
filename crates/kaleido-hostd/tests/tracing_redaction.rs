#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Section 10 applied to `tracing` output.
//!
//! This lives in its own test binary on purpose. `tracing` caches, process
//! wide, whether a call site is of interest to anyone; a call site first
//! reached while no subscriber is installed stays disabled afterwards. Sharing
//! a binary with tests that replay without a subscriber would therefore capture
//! nothing and pass for the wrong reason.

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use kaleido_adapter::IdentityMint;
use kaleido_hostd::slice::{self, ApprovalDecision, RunRequest};

use support::{forbidden_strings, replay, FixtureRuntime, FIXTURES};

#[derive(Clone, Debug, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl CapturedLog {
    fn text(&self) -> String {
        self.0
            .lock()
            .map(|buffer| String::from_utf8_lossy(&buffer).into_owned())
            .unwrap_or_default()
    }
}

impl Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut buffer) = self.0.lock() {
            buffer.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLog {
    type Writer = CapturedLog;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn tracing_output_never_carries_a_body_a_path_or_an_upstream_identifier() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let live_path = directory.path().to_string_lossy().into_owned();
    let live_prompt = "LIVE TRACE SECRET PROMPT";
    let live_steer = "LIVE TRACE SECRET STEER";
    let command_id = IdentityMint::new("hostd-live-tracing").command_id("submit");
    let mut runtime = FixtureRuntime::new("03-permission-approve.jsonl", command_id.clone());
    let identity = runtime.identity();
    let mut request = RunRequest::new(
        "unused-by-fixture-runtime",
        directory.path(),
        directory.path().join("live-log"),
        live_prompt,
    );
    request.decide_first_approval = Some(ApprovalDecision::Accept);
    request.enqueue_steer = Some(live_steer.to_owned());
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for name in FIXTURES {
            let _replayed = replay(name);
        }
        slice::run_with_session(&request, &mut runtime, identity, command_id)
            .expect("run the live composition path");
    });

    let text = captured.text();
    // Without this the test would pass on an empty buffer, which proves
    // nothing about what the code would have logged.
    assert!(
        text.contains("appended a state transition"),
        "the replay must actually emit trace events; captured {} byte(s)",
        text.len()
    );
    assert!(text.contains("replayed a recorded transcript"));
    assert!(text.contains("completed a live diagnostic session"));
    for forbidden in forbidden_strings() {
        assert!(
            !text.contains(forbidden),
            "`{forbidden}` appears in tracing output"
        );
    }
    for forbidden in [live_prompt, live_steer, live_path.as_str()] {
        assert!(
            !text.contains(forbidden),
            "`{forbidden}` appears in live tracing output"
        );
    }
}
