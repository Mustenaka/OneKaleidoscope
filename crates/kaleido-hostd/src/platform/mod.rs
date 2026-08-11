//! Host platform identity without treating every Unix-like target as Linux.

use kaleido_proto::host::HostPlatform;
use std::path::{Path, PathBuf};

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

/// Converts an OS-canonical path into the spelling accepted by provider APIs.
/// Windows canonicalization adds a verbatim prefix that JSON APIs normally do
/// not return, so scope comparison must remove it without weakening the
/// already-resolved path boundary.
pub(crate) fn provider_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        return linux::provider_path(path);
    }
    #[cfg(target_os = "macos")]
    {
        return macos::provider_path(path);
    }
    #[cfg(target_os = "windows")]
    {
        return windows::provider_path(path);
    }
    #[allow(unreachable_code)]
    path.to_path_buf()
}
