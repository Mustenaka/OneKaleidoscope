use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Debug)]
pub(crate) enum Receive {
    Line(Vec<u8>),
    EndOfStream(Option<i64>),
    TimedOut,
}

enum ReaderEvent {
    Line(Vec<u8>),
    EndOfStream,
}

#[derive(Debug)]
pub(crate) struct ChildTransport {
    child: Child,
    stdin: Option<ChildStdin>,
    receiver: Receiver<ReaderEvent>,
    reader: Option<JoinHandle<()>>,
}

impl ChildTransport {
    pub(crate) fn spawn(node: &Path, bridge: &Path, current_dir: &Path) -> io::Result<Self> {
        let mut command = Command::new(node);
        command
            .arg("--experimental-strip-types")
            .arg(bridge)
            .current_dir(current_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure_platform(&mut command);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("bridge stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("bridge stdout unavailable"))?;
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => {
                        let _ = sender.send(ReaderEvent::EndOfStream);
                        break;
                    }
                    Ok(_) => {
                        while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                            line.pop();
                        }
                        if !line.is_empty() && sender.send(ReaderEvent::Line(line)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            receiver,
            reader: Some(reader),
        })
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "bridge stdin closed"))?;
        stdin.write_all(bytes)?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    pub(crate) fn receive(&mut self, timeout: Duration) -> io::Result<Receive> {
        match self.receiver.recv_timeout(timeout) {
            Ok(ReaderEvent::Line(line)) => Ok(Receive::Line(line)),
            Ok(ReaderEvent::EndOfStream) => Ok(Receive::EndOfStream(self.exit_code()?)),
            Err(RecvTimeoutError::Timeout) => Ok(Receive::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Ok(Receive::EndOfStream(self.exit_code()?)),
        }
    }

    pub(crate) fn try_receive(&mut self) -> io::Result<Option<Receive>> {
        match self.receiver.try_recv() {
            Ok(ReaderEvent::Line(line)) => Ok(Some(Receive::Line(line))),
            Ok(ReaderEvent::EndOfStream) => Ok(Some(Receive::EndOfStream(self.exit_code()?))),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Ok(Some(Receive::EndOfStream(self.exit_code()?))),
        }
    }

    fn exit_code(&mut self) -> io::Result<Option<i64>> {
        Ok(self
            .child
            .try_wait()?
            .and_then(|status| status.code().map(i64::from)))
    }

    pub(crate) fn terminate(&mut self) -> io::Result<()> {
        self.stdin.take();
        if self.child.try_wait()?.is_none() {
            terminate_process_tree(&mut self.child)?;
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(())
    }
}

#[cfg(windows)]
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    use std::process::Command;

    let pid = child.id().to_string();
    let mut command = Command::new("taskkill.exe");
    command
        .args(["/PID", pid.as_str(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_platform(&mut command);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        child.kill()
    }
}

#[cfg(not(windows))]
fn terminate_process_tree(child: &mut Child) -> io::Result<()> {
    child.kill()
}

#[cfg(windows)]
fn configure_platform(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn configure_platform(_command: &mut Command) {}
