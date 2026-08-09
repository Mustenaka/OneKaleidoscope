use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

pub(crate) fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    // `OpenOptionsExt` is the safe standard-library FFI boundary. Windows
    // requires BACKUP_SEMANTICS to open a directory handle for FlushFileBuffers.
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .sync_all()
}
