use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use crate::fixture::{Direction, FixtureError, FixtureSink, Transport};
use crate::platform::{self, ProcessError, ResolvedExecutable};

#[derive(Debug, Error)]
pub enum StdioError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("child stdin was unavailable")]
    MissingStdin,
    #[error("child stdout was unavailable")]
    MissingStdout,
    #[error("child stderr was unavailable")]
    MissingStderr,
    #[error("stdio I/O failure: {0}")]
    Io(#[from] io::Error),
    #[error("child emitted non-UTF-8 JSON line")]
    Utf8,
    #[error("child emitted invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timed out waiting for a protocol message")]
    Timeout,
    #[error("protocol stdout closed before the expected message")]
    Closed,
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    #[error("stdio operation failed ({source}); child cleanup also failed: {cleanup}")]
    CleanupAfterError {
        #[source]
        source: Box<StdioError>,
        cleanup: ProcessError,
    },
}

#[derive(Debug)]
enum ReaderEvent {
    Line(Vec<u8>),
    Error(io::Error),
    Closed,
}

#[derive(Debug)]
pub struct PendingStdioLine {
    raw: String,
}

impl PendingStdioLine {
    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn commit<W: Write>(self, fixture: &mut FixtureSink<W>) -> Result<String, StdioError> {
        fixture.record(Direction::S2c, Transport::Stdio, &self.raw)?;
        Ok(self.raw)
    }
}

#[derive(Debug)]
pub struct StdioTee {
    child: Option<Child>,
    stdin: ChildStdin,
    receiver: Receiver<ReaderEvent>,
}

impl StdioTee {
    pub fn spawn(
        executable: &ResolvedExecutable,
        arguments: &[OsString],
        cwd: &Path,
    ) -> Result<Self, StdioError> {
        let mut child = platform::spawn_fixture(executable, arguments, cwd)?;
        let stdin = require_child_pipe(child.stdin.take(), &mut child, StdioError::MissingStdin)?;
        let stdout =
            require_child_pipe(child.stdout.take(), &mut child, StdioError::MissingStdout)?;
        let stderr =
            require_child_pipe(child.stderr.take(), &mut child, StdioError::MissingStderr)?;
        let (sender, receiver) = mpsc::channel();
        drop(thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => {
                        let _ = sender.send(ReaderEvent::Closed);
                        return;
                    }
                    Ok(_) => {
                        while matches!(line.last(), Some(b'\n' | b'\r')) {
                            line.pop();
                        }
                        if !line.is_empty() && sender.send(ReaderEvent::Line(line)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(ReaderEvent::Error(error));
                        return;
                    }
                }
            }
        }));
        drop(thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut discarded = Vec::new();
            loop {
                discarded.clear();
                match reader.read_until(b'\n', &mut discarded) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
            }
        }));

        Ok(Self {
            child: Some(child),
            stdin,
            receiver,
        })
    }

    pub fn send<W: Write>(
        &mut self,
        raw: &str,
        fixture: &mut FixtureSink<W>,
    ) -> Result<(), StdioError> {
        let _: Value = serde_json::from_str(raw)?;
        fixture.record(Direction::C2s, Transport::Stdio, raw)?;
        self.stdin.write_all(raw.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    pub fn receive<W: Write>(
        &mut self,
        timeout: Duration,
        fixture: &mut FixtureSink<W>,
    ) -> Result<String, StdioError> {
        self.receive_pending(timeout)?.commit(fixture)
    }

    pub fn receive_pending(&mut self, timeout: Duration) -> Result<PendingStdioLine, StdioError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(ReaderEvent::Line(bytes)) => {
                let raw = String::from_utf8(bytes).map_err(|_| StdioError::Utf8)?;
                let _: Value = serde_json::from_str(&raw)?;
                Ok(PendingStdioLine { raw })
            }
            Ok(ReaderEvent::Error(error)) => Err(StdioError::Io(error)),
            Ok(ReaderEvent::Closed) => Err(StdioError::Closed),
            Err(RecvTimeoutError::Timeout) => Err(StdioError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(StdioError::Closed),
        }
    }

    pub fn stop(mut self) -> Result<(), StdioError> {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> Result<(), StdioError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        match platform::terminate_tree(&mut child) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.child = Some(child);
                Err(error.into())
            }
        }
    }
}

impl Drop for StdioTee {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = platform::terminate_tree(child);
        }
    }
}

fn require_child_pipe<T>(
    pipe: Option<T>,
    child: &mut Child,
    missing: StdioError,
) -> Result<T, StdioError> {
    match pipe {
        Some(pipe) => Ok(pipe),
        None => Err(cleanup_after_error(missing, || {
            platform::terminate_tree(child).map(drop)
        })),
    }
}

fn cleanup_after_error(
    source: StdioError,
    cleanup: impl FnOnce() -> Result<(), ProcessError>,
) -> StdioError {
    match cleanup() {
        Ok(()) => source,
        Err(cleanup) => StdioError::CleanupAfterError {
            source: Box::new(source),
            cleanup,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_failure_preserves_the_stdio_error_and_cleanup_error() {
        let cleanup = ProcessError::Terminate(io::Error::other("forced cleanup failure"));
        let mut cleanup_called = false;
        let error = cleanup_after_error(StdioError::MissingStdout, || {
            cleanup_called = true;
            Err(cleanup)
        });

        assert!(cleanup_called, "partial spawn must explicitly run cleanup");
        assert!(matches!(
            &error,
            StdioError::CleanupAfterError {
                source,
                cleanup: ProcessError::Terminate(_),
            } if matches!(source.as_ref(), StdioError::MissingStdout)
        ));
        let message = error.to_string();
        assert!(message.contains("child stdout was unavailable"));
        assert!(message.contains("forced cleanup failure"));
    }
}
