use std::io;
use std::process::{Child, Command};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

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

pub(crate) fn terminate_tree(child: &mut Child) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    return linux::terminate_tree(child);
    #[cfg(target_os = "macos")]
    return macos::terminate_tree(child);
    #[cfg(target_os = "windows")]
    return windows::terminate_tree(child);
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = child;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process tree termination is unsupported on this platform",
        ))
    }
}
