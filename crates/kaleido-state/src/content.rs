//! Content-addressed body storage.
//!
//! Section 4.9 keeps bodies out of canonical state and the durable log: only a
//! [`ContentRef`] travels, and the body lives here under its own digest. That
//! is what makes the section 10 redaction rule mechanically checkable rather
//! than a convention — a log line has no field a body could hide in.

use std::fs;
use std::path::{Path, PathBuf};

use kaleido_proto::content::{ContentAvailability, ContentKind, ContentRef, Sensitivity};
use kaleido_proto::ids::ContentId;
use sha2::{Digest, Sha256};

use crate::error::StateError;

/// Directory name for bodies inside the store root.
pub const CONTENT_DIRECTORY: &str = "content";

/// A body store addressed by the digest of its contents.
#[derive(Debug, Clone)]
pub struct ContentStore {
    root: PathBuf,
}

impl ContentStore {
    /// Opens (creating if needed) the body directory under `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StateError> {
        let root = root.as_ref().join(CONTENT_DIRECTORY);
        fs::create_dir_all(&root).map_err(|source| StateError::io(&root, source))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, content_id: &ContentId) -> PathBuf {
        self.root.join(content_id.as_str())
    }

    /// Stores `bytes` and returns the reference canonical state will carry.
    ///
    /// Storing the same bytes twice is a no-op that returns the same reference,
    /// which is what lets a replay converge without duplicating bodies.
    pub fn store(
        &self,
        kind: ContentKind,
        sensitivity: Sensitivity,
        bytes: &[u8],
    ) -> Result<ContentRef, StateError> {
        let hex = hex_digest(bytes);
        let content_id = ContentId::new(hex.clone());
        let path = self.path_for(&content_id);
        if !path.exists() {
            fs::write(&path, bytes).map_err(|source| StateError::io(&path, source))?;
        }
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let reference = ContentRef {
            content_id,
            kind,
            byte_len,
            digest: format!("sha256:{hex}"),
            // Section 4.9: sensitive bodies never carry a preview, and this
            // store is only ever handed bodies from the section 10 list.
            preview: None,
            sensitivity,
            availability: ContentAvailability::Stored,
        };
        reference.validate()?;
        Ok(reference)
    }

    /// Reads a body back, verifying it still matches the recorded digest.
    pub fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, StateError> {
        let path = self.path_for(&reference.content_id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(StateError::ContentMissing {
                    content_id: reference.content_id.clone(),
                });
            }
            Err(source) => return Err(StateError::io(&path, source)),
        };
        if format!("sha256:{}", hex_digest(&bytes)) != reference.digest {
            return Err(StateError::ContentDigestMismatch {
                content_id: reference.content_id.clone(),
            });
        }
        Ok(bytes)
    }

    /// Whether a body is present without reading it.
    pub fn contains(&self, content_id: &ContentId) -> bool {
        self.path_for(content_id).exists()
    }
}

/// Lowercase hexadecimal SHA-256 of `bytes`.
pub fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest.iter() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn store() -> (tempfile::TempDir, ContentStore) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = ContentStore::open(directory.path()).expect("content store");
        (directory, store)
    }

    #[test]
    fn a_stored_reference_carries_metadata_but_never_the_body() {
        let (_directory, store) = store();
        let reference = store
            .store(ContentKind::PlainText, Sensitivity::Sensitive, b"KALEIDO")
            .expect("store body");
        assert_eq!(reference.byte_len, 7);
        assert_eq!(reference.preview, None);
        assert_eq!(reference.availability, ContentAvailability::Stored);
        assert!(reference.digest.starts_with("sha256:"));
        let encoded = serde_json::to_string(&reference).expect("serialise reference");
        assert!(!encoded.contains("KALEIDO"));
    }

    #[test]
    fn identical_bodies_share_one_reference() {
        let (_directory, store) = store();
        let first = store
            .store(ContentKind::PlainText, Sensitivity::Sensitive, b"same")
            .expect("store body");
        let second = store
            .store(ContentKind::PlainText, Sensitivity::Sensitive, b"same")
            .expect("store body again");
        assert_eq!(first, second);
    }

    #[test]
    fn a_tampered_body_is_refused_rather_than_returned() {
        let (_directory, store) = store();
        let reference = store
            .store(ContentKind::PlainText, Sensitivity::Sensitive, b"original")
            .expect("store body");
        let path = store.root().join(reference.content_id.as_str());
        std::fs::write(&path, b"tampered").expect("overwrite body");
        assert!(matches!(
            store.load(&reference),
            Err(StateError::ContentDigestMismatch { .. })
        ));
    }

    #[test]
    fn a_missing_body_is_reported_rather_than_read_as_empty() {
        let (_directory, store) = store();
        let reference = store
            .store(ContentKind::PlainText, Sensitivity::Sensitive, b"gone")
            .expect("store body");
        std::fs::remove_file(store.root().join(reference.content_id.as_str()))
            .expect("remove body");
        assert!(matches!(
            store.load(&reference),
            Err(StateError::ContentMissing { .. })
        ));
    }
}
