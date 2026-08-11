use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub(super) fn configure(command: &mut Command) {
    command.process_group(0);
}

pub(super) fn terminate_tree(child: &mut Child) -> io::Result<()> {
    let process_group = child.id();
    let root_was_running = child.try_wait()?.is_none();
    let status = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{process_group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if !status.success() && root_was_running && child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
        return Err(io::Error::other("process group termination failed"));
    }
    if child.try_wait()?.is_none() {
        child.wait()?;
    }
    Ok(())
}
