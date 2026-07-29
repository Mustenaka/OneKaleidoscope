use std::cell::RefCell;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use kaleido_recorder::fixture::FixtureSink;
use kaleido_recorder::platform::{self, DiscoveryTarget, ProcessError, ResolvedExecutable};
use kaleido_recorder::redact::Redactor;
use kaleido_recorder::stdio_tee::{StdioError, StdioTee};

#[derive(Clone, Debug, Default)]
struct SharedBuffer(Rc<RefCell<Vec<u8>>>);

impl Write for SharedBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn resolve_script(path: &std::path::Path) -> Result<ResolvedExecutable, io::Error> {
    platform::discover(DiscoveryTarget::Codex, Some(path), None)
        .selected()
        .map(|(_, executable)| executable.clone())
        .ok_or_else(|| io::Error::other("temporary command script did not resolve"))
}

fn assert_cleanup_reaped_root(result: Result<(), StdioError>) -> Result<(), io::Error> {
    match result {
        Ok(()) => Ok(()),
        Err(StdioError::Process(ProcessError::Terminate(error)))
            if error
                .to_string()
                .contains("root process was killed directly")
                && error
                    .to_string()
                    .contains("descendant termination is not guaranteed") =>
        {
            Ok(())
        }
        Err(error) => Err(io::Error::other(format!(
            "unexpected cleanup outcome: {error}"
        ))),
    }
}

#[test]
fn malformed_child_json_is_rejected_before_fixture_write() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let script = directory.path().join("malformed.cmd");
    fs::write(&script, "@echo off\r\necho not-json\r\n")?;
    let executable = resolve_script(&script)?;
    let mut tee = StdioTee::spawn(&executable, &[], directory.path())?;
    let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    let result = tee.receive(Duration::from_secs(2), &mut fixture);
    assert!(matches!(result, Err(StdioError::Json(_))));
    assert!(fixture.into_inner().is_empty());
    assert_cleanup_reaped_root(tee.stop())?;
    Ok(())
}

#[test]
fn pending_line_preserves_raw_order_and_writes_only_on_commit() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let script = directory.path().join("ordered.cmd");
    fs::write(&script, "@echo off\r\necho {\"z\":1,\"a\":2}\r\n")?;
    let executable = resolve_script(&script)?;
    let mut tee = StdioTee::spawn(&executable, &[], directory.path())?;
    let writer = SharedBuffer::default();
    let captured = writer.clone();
    let mut fixture = FixtureSink::new(writer, Redactor::from_pairs([]));

    let pending = tee.receive_pending(Duration::from_secs(2))?;
    assert_eq!(pending.raw(), r#"{"z":1,"a":2}"#);
    assert!(captured.0.borrow().is_empty());
    let raw = pending.commit(&mut fixture)?;
    assert_eq!(raw, r#"{"z":1,"a":2}"#);
    let output = String::from_utf8(captured.0.borrow().clone())?;
    assert!(output.contains(r#""payload":{"z":1,"a":2}"#));
    assert_cleanup_reaped_root(tee.stop())?;
    Ok(())
}

#[test]
fn timeout_cleanup_does_not_wait_for_the_long_running_child() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let script = directory.path().join("wait.cmd");
    fs::write(&script, "@echo off\r\n:wait\r\ngoto wait\r\n")?;
    let executable = resolve_script(&script)?;
    let started = Instant::now();
    let mut tee = StdioTee::spawn(&executable, &[OsString::from("unused")], directory.path())?;
    let mut fixture = FixtureSink::new(Vec::new(), Redactor::from_pairs([]));

    assert!(matches!(
        tee.receive(Duration::from_millis(50), &mut fixture),
        Err(StdioError::Timeout)
    ));
    assert_cleanup_reaped_root(tee.stop())?;
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "process-tree cleanup exceeded its bounded window"
    );
    Ok(())
}
