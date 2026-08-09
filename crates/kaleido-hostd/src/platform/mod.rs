//! Host platform identity without treating every Unix-like target as Linux.

use kaleido_proto::host::HostPlatform;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub(crate) fn host_platform() -> Option<HostPlatform> {
    #[cfg(target_os = "linux")]
    {
        return Some(linux::host_platform());
    }
    #[cfg(target_os = "macos")]
    {
        return Some(macos::host_platform());
    }
    #[cfg(target_os = "windows")]
    {
        return Some(windows::host_platform());
    }
    #[allow(unreachable_code)]
    None
}
