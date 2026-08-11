use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::ptr;

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

pub(super) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub(crate) struct ProcessTree {
    job: OwnedHandle,
}

pub(super) fn configure(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[allow(unsafe_code)]
pub(super) fn attach_tree(child: &Child) -> io::Result<ProcessTree> {
    // SAFETY: both pointers are null by design (default security attributes,
    // unnamed job); the returned owned handle is checked before adoption.
    let raw_job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if raw_job.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw_job` is a fresh, non-null owned HANDLE returned above and
    // is adopted exactly once by `OwnedHandle`.
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let limits_size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("job limits size is unsupported"))?;
    // SAFETY: `job` is valid; `limits` points to the exact structure selected
    // by `JobObjectExtendedLimitInformation` for the supplied byte length.
    let configured = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            ptr::from_ref(&limits).cast(),
            limits_size,
        )
    };
    if configured == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the child HANDLE belongs to a live `Child`; both handles remain
    // valid for this call and the job handle is retained for the tree lifetime.
    let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
    if assigned == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ProcessTree { job })
}

#[allow(unsafe_code)]
pub(super) fn terminate_tree(tree: &ProcessTree, child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    // SAFETY: the retained job HANDLE is valid and owns the spawned root plus
    // all descendants that did not receive an explicitly permitted breakaway.
    let terminated = unsafe { TerminateJobObject(tree.job.as_raw_handle(), 1) };
    if terminated == 0 {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        return Err(io::Error::last_os_error());
    }
    child.wait()?;
    Ok(())
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
        let tree = super::attach_tree(&child).expect("attach controlled process tree");
        child.wait().expect("reap child");
        assert!(super::terminate_tree(&tree, &mut child).is_ok());
    }
}
