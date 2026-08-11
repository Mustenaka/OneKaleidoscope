use std::io;
use std::process::{Child, Command};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Default)]
pub(crate) struct ProcessTree;

#[cfg(target_os = "windows")]
pub(crate) use windows::ProcessTree;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
#[derive(Debug, Default)]
pub(crate) struct ProcessTree;

pub(crate) fn configure(command: &mut Command) {
    #[cfg(target_os = "linux")]
    linux::configure(command);
    #[cfg(target_os = "macos")]
    macos::configure(command);
    #[cfg(target_os = "windows")]
    windows::configure(command);
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let _ = command;
}

pub(crate) fn attach_tree(child: &Child) -> io::Result<ProcessTree> {
    #[cfg(target_os = "windows")]
    return windows::attach_tree(child);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = child;
        Ok(ProcessTree)
    }
}

pub(crate) fn terminate_tree(tree: &ProcessTree, child: &mut Child) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _ = tree;
    #[cfg(target_os = "linux")]
    return linux::terminate_tree(child);
    #[cfg(target_os = "macos")]
    return macos::terminate_tree(child);
    #[cfg(target_os = "windows")]
    return windows::terminate_tree(tree, child);
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (tree, child);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process tree termination is unsupported on this platform",
        ))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::{self, BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    const CHILD_PID_TIMEOUT: Duration = Duration::from_secs(10);
    const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

    struct LiveProcessTree {
        root: Child,
        process_tree: super::ProcessTree,
        descendant_pid: Option<u32>,
        armed: bool,
    }

    impl LiveProcessTree {
        fn spawn() -> Self {
            let mut command = live_tree_command();
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            super::configure(&mut command);
            let mut root = command.spawn().expect("spawn process-tree root");
            let process_tree = super::attach_tree(&root).expect("attach controlled process tree");
            let stdout = root.stdout.take().expect("take process-tree stdout");
            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                let mut line = String::new();
                let result = BufReader::new(stdout)
                    .read_line(&mut line)
                    .and_then(|_| parse_pid(&line));
                let _ = sender.send(result);
            });
            let mut tree = Self {
                root,
                process_tree,
                descendant_pid: None,
                armed: true,
            };
            let descendant_pid = receiver
                .recv_timeout(CHILD_PID_TIMEOUT)
                .expect("process-tree root must report its descendant promptly")
                .expect("reported descendant PID must be valid");
            tree.descendant_pid = Some(descendant_pid);
            tree
        }

        fn descendant_pid(&self) -> u32 {
            self.descendant_pid
                .expect("live process tree must have a descendant PID")
        }

        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for LiveProcessTree {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            independent_tree_cleanup(self.root.id());
            if let Some(pid) = self.descendant_pid {
                independent_process_cleanup(pid);
            }
            if matches!(self.root.try_wait(), Ok(None)) {
                let _ = self.root.kill();
                let _ = self.root.wait();
            }
        }
    }

    #[test]
    fn a_live_process_tree_terminates_the_root_and_its_descendant() {
        let mut tree = LiveProcessTree::spawn();
        let descendant_pid = tree.descendant_pid();

        assert!(
            tree.root.try_wait().expect("inspect root status").is_none(),
            "the root must still be running before termination"
        );
        assert!(
            process_is_running(descendant_pid).expect("inspect descendant status"),
            "the descendant must still be running before termination"
        );

        super::terminate_tree(&tree.process_tree, &mut tree.root)
            .expect("terminate the complete process tree");

        assert!(
            tree.root.try_wait().expect("inspect root status").is_some(),
            "the root must have exited"
        );
        assert!(
            wait_until_not_running(descendant_pid).expect("wait for descendant exit"),
            "the descendant must have exited"
        );
        tree.disarm();
    }

    fn parse_pid(line: &str) -> io::Result<u32> {
        let pid = line
            .trim()
            .parse()
            .map_err(|_| io::Error::other("process-tree root reported an invalid PID"))?;
        if pid == 0 {
            return Err(io::Error::other(
                "process-tree root reported the reserved zero PID",
            ));
        }
        Ok(pid)
    }

    fn wait_until_not_running(pid: u32) -> io::Result<bool> {
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            if !process_is_running(pid)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn live_tree_command() -> Command {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "sleep 120 & child=$!; printf '%s\\n' \"$child\"; wait \"$child\"",
        ]);
        command
    }

    #[cfg(target_os = "windows")]
    fn live_tree_command() -> Command {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$child = Start-Process -FilePath 'powershell.exe' -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -WindowStyle Hidden -PassThru; [Console]::Out.WriteLine($child.Id); [Console]::Out.Flush(); Wait-Process -Id $child.Id",
        ]);
        command
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn process_is_running(pid: u32) -> io::Result<bool> {
        let output = Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        let state = String::from_utf8_lossy(&output.stdout);
        Ok(output.status.success()
            && state
                .split_whitespace()
                .any(|value| !value.starts_with('Z')))
    }

    #[cfg(target_os = "windows")]
    fn process_is_running(pid: u32) -> io::Result<bool> {
        use std::os::windows::process::CommandExt;

        let output = Command::new("tasklist.exe")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(super::windows::CREATE_NO_WINDOW)
            .output()?;
        Ok(String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn independent_tree_cleanup(root_pid: u32) {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{root_pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn independent_process_cleanup(pid: u32) {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(target_os = "windows")]
    fn independent_tree_cleanup(root_pid: u32) {
        use std::os::windows::process::CommandExt;

        let _ = Command::new("taskkill.exe")
            .args(["/PID", &root_pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(super::windows::CREATE_NO_WINDOW)
            .status();
    }

    #[cfg(target_os = "windows")]
    fn independent_process_cleanup(pid: u32) {
        use std::os::windows::process::CommandExt;

        let _ = Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(super::windows::CREATE_NO_WINDOW)
            .status();
    }
}
