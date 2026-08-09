use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(crate) fn prepare_private_directory(path: &Path) -> io::Result<()> {
    if path.exists() {
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private directory permissions are not owner-only",
            ));
        }
        return Ok(());
    }
    fs::create_dir_all(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o700))
}

pub(crate) fn secure_private_file(path: &Path, create_new: bool) -> io::Result<File> {
    if path.exists() {
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private file permissions are not owner-only",
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).mode(0o600);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    options.open(path)
}

pub(crate) fn verify_private_path(path: &Path) -> io::Result<()> {
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode == 0o600 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private file permissions are not owner-only",
        ))
    }
}

pub(crate) fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "private file has no parent"))?;
    File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::prepare_private_directory;

    #[test]
    fn rejects_a_broadened_directory() {
        let path = std::env::temp_dir().join(format!(
            "kaleido-transport-permissions-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("permissions");
        assert!(prepare_private_directory(&path).is_err());
        fs::remove_dir(&path).expect("cleanup");
    }
}
