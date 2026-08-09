//! Narrow platform filesystem durability helpers.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::sync_parent_directory;
#[cfg(target_os = "macos")]
pub(crate) use macos::sync_parent_directory;
#[cfg(target_os = "windows")]
pub(crate) use windows::sync_parent_directory;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn sync_parent_directory(path: &std::path::Path) -> std::io::Result<()> {
    // Android and other Unix-like targets support syncing an opened directory.
    // Keeping this as the explicit default prevents treating Android as Linux.
    std::fs::File::open(path)?.sync_all()
}
