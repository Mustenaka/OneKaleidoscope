//! Owner-only durable storage for opaque credentials and identity material.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

use crate::error::TransportError;
use crate::platform;

pub struct PrivateFileStore {
    path: PathBuf,
}

impl std::fmt::Debug for PrivateFileStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrivateFileStore([redacted path])")
    }
}

impl PrivateFileStore {
    pub fn new(path: PathBuf) -> Result<Self, TransportError> {
        let parent = path.parent().ok_or(TransportError::InsecurePermissions)?;
        platform::prepare_private_directory(parent)
            .map_err(|_| TransportError::InsecurePermissions)?;
        if path.exists() {
            platform::verify_private_path(&path)
                .map_err(|_| TransportError::InsecurePermissions)?;
        }
        Ok(Self { path })
    }

    pub fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, TransportError> {
        if !self.path.exists() {
            return Ok(None);
        }
        platform::verify_private_path(&self.path)
            .map_err(|_| TransportError::InsecurePermissions)?;
        let mut file = fs::File::open(&self.path).map_err(TransportError::from)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.read_to_end(&mut bytes).map_err(TransportError::from)?;
        Ok(Some(bytes))
    }

    pub fn store(&self, bytes: &[u8]) -> Result<(), TransportError> {
        let parent = self
            .path
            .parent()
            .ok_or(TransportError::InsecurePermissions)?;
        platform::prepare_private_directory(parent)
            .map_err(|_| TransportError::InsecurePermissions)?;
        if self.path.exists() {
            platform::verify_private_path(&self.path)
                .map_err(|_| TransportError::InsecurePermissions)?;
        }
        let temporary = temporary_path(parent);
        let mut file =
            platform::secure_private_file(&temporary, true).map_err(TransportError::from)?;
        let result = (|| {
            file.write_all(bytes).map_err(TransportError::from)?;
            file.sync_all().map_err(TransportError::from)?;
            drop(file);
            platform::atomic_replace(&temporary, &self.path).map_err(TransportError::from)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn temporary_path(parent: &Path) -> PathBuf {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    parent.join(format!(".private-{}.tmp", URL_SAFE_NO_PAD.encode(random)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::PrivateFileStore;
    #[cfg(unix)]
    use crate::error::TransportError;

    #[test]
    fn opaque_bytes_are_stable_across_restart_and_debug_hides_the_path() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("private").join("identity.bin");
        let store = PrivateFileStore::new(path.clone()).expect("store");
        store.store(b"stable identity").expect("write");
        assert_eq!(
            PrivateFileStore::new(path.clone())
                .expect("reopen")
                .load()
                .expect("load")
                .expect("bytes")
                .as_slice(),
            b"stable identity"
        );
        assert!(!format!("{store:?}").contains(&path.to_string_lossy().into_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn broad_file_permissions_are_rejected_fail_loud() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempfile::tempdir().expect("directory");
        let parent = directory.path().join("private");
        let path = parent.join("identity.bin");
        let store = PrivateFileStore::new(path.clone()).expect("store");
        store.store(b"identity").expect("write");
        assert_eq!(
            std::fs::metadata(&parent).expect("directory mode").mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).expect("file mode").mode() & 0o777,
            0o600
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("broaden file");
        assert_eq!(
            PrivateFileStore::new(path).err(),
            Some(TransportError::InsecurePermissions)
        );
    }
}
