use std::io;
use std::process::{Child, Command};

pub(super) fn configure(_command: &mut Command) {}

pub(super) fn terminate_tree(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
    }
    Ok(())
}
