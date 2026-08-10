use std::io;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub(super) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(super) fn configure(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

pub(super) fn terminate_tree(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    let taskkill = Command::new("taskkill.exe")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    match taskkill {
        Ok(status) if status.success() => {
            child.wait()?;
            Ok(())
        }
        Ok(_) if child.try_wait()?.is_some() => Ok(()),
        Ok(_) => {
            child.kill()?;
            child.wait()?;
            Err(io::Error::other("process tree termination failed"))
        }
        Err(_) if child.try_wait()?.is_some() => Ok(()),
        Err(error) => {
            child.kill()?;
            child.wait()?;
            Err(error)
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::process::Command;

    #[test]
    fn an_already_exited_child_is_a_successful_tree_termination() {
        let mut child = Command::new("cmd.exe")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("spawn short lived child");
        child.wait().expect("reap child");
        assert!(super::terminate_tree(&mut child).is_ok());
    }
}
