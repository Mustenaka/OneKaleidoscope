//! Content-addressed body storage.
//!
//! Section 4.9 keeps bodies out of canonical state and the durable log: only a
//! [`ContentRef`] travels, and the body lives here under its own digest. That
//! is what makes the section 10 redaction rule mechanically checkable rather
//! than a convention — a log line has no field a body could hide in.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kaleido_proto::content::{
    ContentAvailability, ContentKind, ContentReadChunk, ContentReadRequest, ContentReadResponse,
    ContentRef, ContentUnavailableReason, ContentWriteRequest, ContentWriteResponse, Sensitivity,
};
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::ids::{ContentId, DeviceId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StateError;

/// Directory name for bodies inside the store root.
pub const CONTENT_DIRECTORY: &str = "content";
const OWNERSHIP_FILE: &str = "device-ownership.jsonl";
const OWNERSHIP_FORMAT_VERSION: u32 = 1;
const OWNERSHIP_REWRITE_PREFIX: &str = ".device-ownership-rewrite-";
/// Orphan uploads expire after ten minutes unless a later product policy
/// promotes them. This is deliberately longer than the largest command TTL.
pub const DEVICE_CONTENT_TTL_MS: i64 = 600_000;
/// Maximum simultaneously unexpired uploaded bytes owned by one device.
pub const DEVICE_CONTENT_QUOTA_BYTES: u64 = 8 * 1024 * 1024;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnershipRecord {
    format_version: u32,
    device_digest: String,
    content_ref: ContentRef,
    created_at_ms: i64,
    expires_at_ms: i64,
}

/// A body store addressed by the digest of its contents.
#[derive(Debug, Clone)]
pub struct ContentStore {
    root: PathBuf,
    ownership: BTreeMap<(String, ContentId), OwnershipRecord>,
}

impl ContentStore {
    /// Opens (creating if needed) the body directory under `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StateError> {
        let root = root.as_ref().join(CONTENT_DIRECTORY);
        fs::create_dir_all(&root).map_err(|source| StateError::io(&root, source))?;
        recover_ownership_rewrite(&root)?;
        cleanup_temporary_files(&root)?;
        let ownership = read_ownership(&root)?;
        Ok(Self { root, ownership })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, content_id: &ContentId) -> Result<PathBuf, StateError> {
        if !safe_content_component(content_id.as_str()) {
            return Err(StateError::UnsafeContentId {
                content_id: content_id.clone(),
            });
        }
        Ok(self.root.join(content_id.as_str()))
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
        let path = self.path_for(&content_id)?;
        if path.exists() {
            verify_body(&path, &content_id, bytes.len(), &hex)?;
        } else {
            self.install_body(&path, &content_id, bytes, &hex)?;
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

    fn install_body(
        &self,
        path: &Path,
        content_id: &ContentId,
        bytes: &[u8],
        expected_hex: &str,
    ) -> Result<(), StateError> {
        let (temporary, mut file) = loop {
            let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = self.root.join(format!(
                ".{}.{}.{}.tmp",
                content_id.as_str(),
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => break (candidate, file),
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(StateError::io(&candidate, source)),
            }
        };
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_data()) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(StateError::io(&temporary, source));
        }
        drop(file);

        match fs::hard_link(&temporary, path) {
            Ok(()) => {
                fs::remove_file(&temporary).map_err(|source| StateError::io(&temporary, source))?;
                crate::platform::sync_parent_directory(&self.root)
                    .map_err(|source| StateError::io(&self.root, source))?;
                Ok(())
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary).map_err(|remove| StateError::io(&temporary, remove))?;
                verify_body(path, content_id, bytes.len(), expected_hex)?;
                crate::platform::sync_parent_directory(&self.root)
                    .map_err(|source| StateError::io(&self.root, source))
            }
            Err(source) => {
                let _ = fs::remove_file(&temporary);
                Err(StateError::io(path, source))
            }
        }
    }

    /// Reads a body back, verifying it still matches the recorded digest.
    pub fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, StateError> {
        let path = self.path_for(&reference.content_id)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(StateError::ContentMissing {
                    content_id: reference.content_id.clone(),
                });
            }
            Err(source) => return Err(StateError::io(&path, source)),
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != reference.byte_len
            || format!("sha256:{}", hex_digest(&bytes)) != reference.digest
        {
            return Err(StateError::ContentDigestMismatch {
                content_id: reference.content_id.clone(),
            });
        }
        Ok(bytes)
    }

    /// Whether a body is present without reading it.
    pub fn contains(&self, content_id: &ContentId) -> bool {
        self.path_for(content_id).is_ok_and(|path| path.exists())
    }

    /// Authenticated mobile upload. The control header is never trusted: both
    /// length and digest are recomputed before either body or ownership is
    /// made visible.
    pub fn write_for_device(
        &mut self,
        device_id: &DeviceId,
        request: &ContentWriteRequest,
        bytes: &[u8],
        now_ms: i64,
    ) -> Result<ContentWriteResponse, StateError> {
        self.cleanup_expired_ownership(now_ms)?;
        if request.validate().is_err() {
            return Ok(rejected_write(now_ms));
        }
        let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let actual_digest = format!("sha256:{}", hex_digest(bytes));
        if actual_len != request.byte_len || actual_digest != request.digest {
            return Ok(rejected_write(now_ms));
        }
        let device_digest = device_digest(device_id);
        let content_id = ContentId::new(actual_digest.trim_start_matches("sha256:").to_owned());
        let ownership_key = (device_digest.clone(), content_id.clone());
        let already_owned = self.ownership.get(&ownership_key).is_some_and(|record| {
            record.expires_at_ms > now_ms && record.content_ref.byte_len == actual_len
        });
        let current_bytes = self
            .ownership
            .values()
            .filter(|record| record.device_digest == device_digest && record.expires_at_ms > now_ms)
            .fold(0_u64, |total, record| {
                total.saturating_add(record.content_ref.byte_len)
            });
        if !already_owned
            && current_bytes
                .checked_add(actual_len)
                .is_none_or(|total| total > DEVICE_CONTENT_QUOTA_BYTES)
        {
            return Ok(rejected_write(now_ms));
        }
        let Some(expires_at_ms) = now_ms.checked_add(DEVICE_CONTENT_TTL_MS) else {
            return Ok(rejected_write(now_ms));
        };
        let content_ref =
            self.store(request.content_kind.clone(), Sensitivity::Sensitive, bytes)?;
        let record = OwnershipRecord {
            format_version: OWNERSHIP_FORMAT_VERSION,
            device_digest,
            content_ref: content_ref.clone(),
            created_at_ms: now_ms,
            expires_at_ms,
        };
        append_ownership(&self.root, &record)?;
        self.ownership.insert(ownership_key, record);
        let response = ContentWriteResponse::Stored { content_ref };
        response.validate_for(request)?;
        Ok(response)
    }

    /// Startup cleanup after canonical replay. Only bodies absent from both
    /// canonical state and every active device ownership record are removed.
    pub(crate) fn cleanup_after_replay(
        &mut self,
        now_ms: i64,
        canonical_content_ids: &BTreeSet<ContentId>,
    ) -> Result<(), StateError> {
        self.cleanup_expired_ownership(now_ms)?;
        cleanup_temporary_files(&self.root)?;
        let mut protected = canonical_content_ids.clone();
        protected.extend(
            self.ownership
                .values()
                .map(|record| record.content_ref.content_id.clone()),
        );
        let mut removed = false;
        for entry in
            fs::read_dir(&self.root).map_err(|source| StateError::io(&self.root, source))?
        {
            let path = entry
                .map_err(|source| StateError::io(&self.root, source))?
                .path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if safe_content_component(name) {
                let content_id = ContentId::new(name.to_owned());
                if !protected.contains(&content_id) {
                    fs::remove_file(&path).map_err(|source| StateError::io(&path, source))?;
                    removed = true;
                }
            }
        }
        if removed {
            crate::platform::sync_parent_directory(&self.root)
                .map_err(|source| StateError::io(&self.root, source))?;
        }
        Ok(())
    }

    fn cleanup_expired_ownership(&mut self, now_ms: i64) -> Result<(), StateError> {
        let before = self.ownership.len();
        self.ownership
            .retain(|_, record| record.expires_at_ms > now_ms);
        if self.ownership.len() != before {
            rewrite_ownership(&self.root, &self.ownership)?;
        }
        cleanup_temporary_files(&self.root)
    }

    /// Authenticated, chunked mobile read using server-side metadata lookup;
    /// the caller never fabricates a `ContentRef` from the wire request.
    pub fn read_for_device(
        &self,
        device_id: &DeviceId,
        request: &ContentReadRequest,
        now_ms: i64,
    ) -> Result<ContentReadResponse, StateError> {
        request.validate()?;
        let key = (device_digest(device_id), request.content_id.clone());
        let Some(record) = self.ownership.get(&key) else {
            return Ok(unavailable(
                &request.content_id,
                ContentUnavailableReason::Unauthorized,
            ));
        };
        if record.expires_at_ms <= now_ms {
            return Ok(unavailable(
                &request.content_id,
                ContentUnavailableReason::Evicted,
            ));
        }
        self.read_reference(&record.content_ref, request)
    }

    pub(crate) fn read_reference(
        &self,
        reference: &ContentRef,
        request: &ContentReadRequest,
    ) -> Result<ContentReadResponse, StateError> {
        request.validate()?;
        if reference.content_id != request.content_id {
            return Err(StateError::ContentMetadataMismatch {
                content_id: request.content_id.clone(),
            });
        }
        match reference.availability {
            ContentAvailability::Evicted => {
                return Ok(unavailable(
                    &request.content_id,
                    ContentUnavailableReason::Evicted,
                ));
            }
            ContentAvailability::Inline | ContentAvailability::NeverStored => {
                return Ok(unavailable(
                    &request.content_id,
                    ContentUnavailableReason::NeverStored,
                ));
            }
            ContentAvailability::Stored => {}
        }
        let bytes = match self.load(reference) {
            Ok(bytes) => bytes,
            Err(StateError::ContentMissing { .. }) => {
                return Ok(unavailable(
                    &request.content_id,
                    ContentUnavailableReason::Evicted,
                ));
            }
            Err(StateError::ContentDigestMismatch { .. }) => {
                return Ok(unavailable(
                    &request.content_id,
                    ContentUnavailableReason::DigestMismatch,
                ));
            }
            Err(other) => return Err(other),
        };
        let offset = usize::try_from(request.offset)
            .ok()
            .filter(|offset| *offset <= bytes.len())
            .ok_or(StateError::InvalidContentOffset {
                content_id: request.content_id.clone(),
                offset: request.offset,
            })?;
        let max_bytes = usize::try_from(request.max_bytes).unwrap_or(usize::MAX);
        let end = offset.saturating_add(max_bytes).min(bytes.len());
        let chunk_bytes = bytes
            .get(offset..end)
            .ok_or(StateError::InvalidContentOffset {
                content_id: request.content_id.clone(),
                offset: request.offset,
            })?
            .to_vec();
        let eof = end == bytes.len();
        let next_offset = if eof {
            None
        } else {
            Some(
                u64::try_from(end).map_err(|_| StateError::InvalidContentOffset {
                    content_id: request.content_id.clone(),
                    offset: request.offset,
                })?,
            )
        };
        let response = ContentReadResponse::Chunk {
            chunk: ContentReadChunk {
                content_id: request.content_id.clone(),
                offset: request.offset,
                bytes: chunk_bytes,
                next_offset,
                eof,
                digest: reference.digest.clone(),
            },
        };
        response.validate()?;
        Ok(response)
    }

    /// Fails closed unless this exact reference was uploaded by this device,
    /// remains unexpired, and still matches the stored ownership metadata.
    pub fn validate_device_reference(
        &self,
        device_id: &DeviceId,
        reference: &ContentRef,
        now_ms: i64,
    ) -> Result<(), StateError> {
        reference.ensure_sensitive("device_command.content_ref")?;
        let key = (device_digest(device_id), reference.content_id.clone());
        let record = self
            .ownership
            .get(&key)
            .ok_or_else(|| StateError::ContentUnauthorized {
                content_id: reference.content_id.clone(),
            })?;
        if record.expires_at_ms <= now_ms {
            return Err(StateError::ContentExpired {
                content_id: reference.content_id.clone(),
            });
        }
        if &record.content_ref != reference {
            return Err(StateError::ContentMetadataMismatch {
                content_id: reference.content_id.clone(),
            });
        }
        self.load(reference)?;
        Ok(())
    }
}

fn safe_content_component(component: &str) -> bool {
    component.len() == 64
        && component
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn verify_body(
    path: &Path,
    content_id: &ContentId,
    expected_len: usize,
    expected_hex: &str,
) -> Result<(), StateError> {
    let bytes = fs::read(path).map_err(|source| StateError::io(path, source))?;
    if bytes.len() != expected_len || hex_digest(&bytes) != expected_hex {
        return Err(StateError::ContentDigestMismatch {
            content_id: content_id.clone(),
        });
    }
    Ok(())
}

fn ownership_path(root: &Path) -> PathBuf {
    root.join(OWNERSHIP_FILE)
}

fn cleanup_temporary_files(root: &Path) -> Result<(), StateError> {
    let mut removed = false;
    for entry in fs::read_dir(root).map_err(|source| StateError::io(root, source))? {
        let path = entry.map_err(|source| StateError::io(root, source))?.path();
        let is_temporary = path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"));
        if is_temporary {
            fs::remove_file(&path).map_err(|source| StateError::io(&path, source))?;
            removed = true;
        }
    }
    if removed {
        crate::platform::sync_parent_directory(root)
            .map_err(|source| StateError::io(root, source))?;
    }
    Ok(())
}

fn recover_ownership_rewrite(root: &Path) -> Result<(), StateError> {
    let path = ownership_path(root);
    let mut ready = Vec::new();
    for entry in fs::read_dir(root).map_err(|source| StateError::io(root, source))? {
        let candidate = entry.map_err(|source| StateError::io(root, source))?.path();
        if candidate.is_file()
            && candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(OWNERSHIP_REWRITE_PREFIX) && name.ends_with(".ready")
                })
        {
            ready.push(candidate);
        }
    }
    ready.sort();
    let recover = if path.exists() { None } else { ready.pop() };
    let mut changed = false;
    if let Some(candidate) = recover {
        fs::rename(&candidate, &path).map_err(|source| StateError::io(&candidate, source))?;
        changed = true;
    }
    for candidate in ready {
        fs::remove_file(&candidate).map_err(|source| StateError::io(&candidate, source))?;
        changed = true;
    }
    if changed {
        crate::platform::sync_parent_directory(root)
            .map_err(|source| StateError::io(root, source))?;
    }
    Ok(())
}

fn rewrite_ownership(
    root: &Path,
    ownership: &BTreeMap<(String, ContentId), OwnershipRecord>,
) -> Result<(), StateError> {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let stem = format!(
        "{OWNERSHIP_REWRITE_PREFIX}{}-{sequence:020}",
        std::process::id()
    );
    let temporary = root.join(format!("{stem}.tmp"));
    let ready = root.join(format!("{stem}.ready"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| StateError::io(&temporary, source))?;
    for record in ownership.values() {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        if let Err(source) = file.write_all(line.as_bytes()) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(StateError::io(&temporary, source));
        }
    }
    if let Err(source) = file.sync_data() {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(StateError::io(&temporary, source));
    }
    drop(file);
    fs::rename(&temporary, &ready).map_err(|source| StateError::io(&temporary, source))?;
    crate::platform::sync_parent_directory(root).map_err(|source| StateError::io(root, source))?;

    let path = ownership_path(root);
    if let Err(source) = fs::rename(&ready, &path) {
        if !path.exists() {
            return Err(StateError::io(&ready, source));
        }
        fs::remove_file(&path).map_err(|remove| StateError::io(&path, remove))?;
        crate::platform::sync_parent_directory(root).map_err(|sync| StateError::io(root, sync))?;
        fs::rename(&ready, &path).map_err(|rename| StateError::io(&ready, rename))?;
    }
    crate::platform::sync_parent_directory(root).map_err(|source| StateError::io(root, source))
}

fn append_ownership(root: &Path, record: &OwnershipRecord) -> Result<(), StateError> {
    let path = ownership_path(root);
    let existed = path.exists();
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| StateError::io(&path, source))?;
    file.write_all(line.as_bytes())
        .map_err(|source| StateError::io(&path, source))?;
    file.flush()
        .map_err(|source| StateError::io(&path, source))?;
    file.sync_data()
        .map_err(|source| StateError::io(&path, source))?;
    if !existed {
        crate::platform::sync_parent_directory(root)
            .map_err(|source| StateError::io(root, source))?;
    }
    Ok(())
}

fn read_ownership(
    root: &Path,
) -> Result<BTreeMap<(String, ContentId), OwnershipRecord>, StateError> {
    let path = ownership_path(root);
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(source) => return Err(StateError::io(&path, source)),
    };
    let mut ownership = BTreeMap::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| StateError::io(&path, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<OwnershipRecord>(&line).map_err(|_| {
            StateError::MalformedRecord {
                path: path.clone(),
                line: index + 1,
            }
        })?;
        let digest_valid = record.device_digest.len() == 64
            && record
                .device_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        // #[allow(kaleido::version_branch)] reason: durable ownership format validation rejects incompatible records and never selects a product capability
        if record.format_version != OWNERSHIP_FORMAT_VERSION
            || !digest_valid
            || record.expires_at_ms <= record.created_at_ms
            || !safe_content_component(record.content_ref.content_id.as_str())
            || record.content_ref.digest
                != format!("sha256:{}", record.content_ref.content_id.as_str())
            || record.content_ref.availability != ContentAvailability::Stored
        {
            return Err(StateError::MalformedRecord {
                path: path.clone(),
                line: index + 1,
            });
        }
        record
            .content_ref
            .ensure_sensitive("device_ownership.content_ref")?;
        ownership.insert(
            (
                record.device_digest.clone(),
                record.content_ref.content_id.clone(),
            ),
            record,
        );
    }
    Ok(ownership)
}

fn device_digest(device_id: &DeviceId) -> String {
    let bytes = device_id.as_str().as_bytes();
    let mut scoped = Vec::with_capacity(8 + bytes.len());
    scoped.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    scoped.extend_from_slice(bytes);
    hex_digest(&scoped)
}

fn rejected_write(at_ms: i64) -> ContentWriteResponse {
    ContentWriteResponse::Rejected {
        error: CanonicalError {
            code: ErrorCode::InvalidCommand,
            retriable: false,
            detail_ref: None,
            at_ms,
        },
    }
}

fn unavailable(content_id: &ContentId, reason: ContentUnavailableReason) -> ContentReadResponse {
    ContentReadResponse::Unavailable {
        content_id: content_id.clone(),
        reason,
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
        assert!(std::fs::read_dir(store.root())
            .expect("content directory")
            .all(|entry| !entry
                .expect("content entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[test]
    fn an_existing_corrupt_digest_path_is_never_reported_as_stored() {
        let (_directory, mut store) = store();
        let bytes = b"original";
        let reference = store
            .store(ContentKind::PlainText, Sensitivity::Sensitive, bytes)
            .expect("initial body");
        let path = store.root().join(reference.content_id.as_str());
        std::fs::write(&path, b"tampered").expect("corrupt existing digest path");
        assert!(matches!(
            store.store(ContentKind::PlainText, Sensitivity::Sensitive, bytes),
            Err(StateError::ContentDigestMismatch { .. })
        ));

        let owner = DeviceId::new("device-corrupt-existing");
        let request = write_request(bytes);
        assert!(matches!(
            store.write_for_device(&owner, &request, bytes, 100),
            Err(StateError::ContentDigestMismatch { .. })
        ));
        assert!(store.ownership.is_empty());
    }

    #[test]
    fn unsafe_content_ids_never_escape_the_content_directory() {
        let (_directory, store) = store();
        let reference = ContentRef {
            content_id: ContentId::new("../outside"),
            kind: ContentKind::PlainText,
            byte_len: 1,
            digest: format!("sha256:{}", "0".repeat(64)),
            preview: None,
            sensitivity: Sensitivity::Sensitive,
            availability: ContentAvailability::Stored,
        };
        assert!(matches!(
            store.load(&reference),
            Err(StateError::UnsafeContentId { .. })
        ));
        assert!(!store.contains(&reference.content_id));
    }

    #[test]
    fn stored_bodies_missing_after_retention_are_evicted_not_never_stored() {
        let (_directory, store) = store();
        let reference = store
            .store(ContentKind::PlainText, Sensitivity::Sensitive, b"retained")
            .expect("stored reference");
        std::fs::remove_file(store.root().join(reference.content_id.as_str()))
            .expect("simulate retention eviction");
        let request = ContentReadRequest {
            content_id: reference.content_id.clone(),
            offset: 0,
            max_bytes: 16,
        };
        assert!(matches!(
            store.read_reference(&reference, &request),
            Ok(ContentReadResponse::Unavailable {
                reason: ContentUnavailableReason::Evicted,
                ..
            })
        ));

        let never_stored = ContentRef {
            availability: ContentAvailability::NeverStored,
            ..reference
        };
        assert!(matches!(
            store.read_reference(&never_stored, &request),
            Ok(ContentReadResponse::Unavailable {
                reason: ContentUnavailableReason::NeverStored,
                ..
            })
        ));
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

    fn write_request(bytes: &[u8]) -> ContentWriteRequest {
        ContentWriteRequest {
            content_kind: ContentKind::PlainText,
            byte_len: u64::try_from(bytes.len()).expect("test body length"),
            digest: format!("sha256:{}", hex_digest(bytes)),
        }
    }

    #[test]
    fn device_upload_is_owned_chunked_and_expires() {
        let (_directory, mut store) = store();
        let owner = DeviceId::new("device-owner");
        let other = DeviceId::new("device-other");
        let request = write_request(b"abcdef");
        let response = store
            .write_for_device(&owner, &request, b"abcdef", 100)
            .expect("device upload");
        let content_ref = match response {
            ContentWriteResponse::Stored { content_ref } => Some(content_ref),
            ContentWriteResponse::Rejected { .. } => None,
        }
        .expect("expected stored response");
        assert_eq!(content_ref.sensitivity, Sensitivity::Sensitive);
        assert_eq!(content_ref.preview, None);
        assert_eq!(content_ref.availability, ContentAvailability::Stored);

        let read = ContentReadRequest {
            content_id: content_ref.content_id.clone(),
            offset: 1,
            max_bytes: 3,
        };
        let chunk = match store
            .read_for_device(&owner, &read, 101)
            .expect("owned read")
        {
            ContentReadResponse::Chunk { chunk } => Some(chunk),
            ContentReadResponse::Unavailable { .. } => None,
        }
        .expect("expected chunk");
        assert_eq!(chunk.bytes, b"bcd");
        assert_eq!(chunk.next_offset, Some(4));
        assert!(!chunk.eof);

        assert!(matches!(
            store
                .read_for_device(&other, &read, 101)
                .expect("cross-device refusal"),
            ContentReadResponse::Unavailable {
                reason: ContentUnavailableReason::Unauthorized,
                ..
            }
        ));
        assert!(matches!(
            store
                .read_for_device(&owner, &read, 100 + DEVICE_CONTENT_TTL_MS)
                .expect("expired response"),
            ContentReadResponse::Unavailable {
                reason: ContentUnavailableReason::Evicted,
                ..
            }
        ));
    }

    #[test]
    fn mismatched_upload_and_exhausted_quota_leave_no_owned_body() {
        let (_directory, mut store) = store();
        let owner = DeviceId::new("device-owner");
        let request = write_request(b"correct");
        assert!(matches!(
            store
                .write_for_device(&owner, &request, b"wrong", 100)
                .expect("mismatch response"),
            ContentWriteResponse::Rejected {
                error: CanonicalError {
                    code: ErrorCode::InvalidCommand,
                    ..
                }
            }
        ));
        assert!(store.ownership.is_empty());
        assert!(!store.contains(&ContentId::new(hex_digest(b"wrong"))));

        let device_digest = device_digest(&owner);
        let quota_ref = ContentRef {
            content_id: ContentId::new("quota-placeholder"),
            kind: ContentKind::PlainText,
            byte_len: DEVICE_CONTENT_QUOTA_BYTES,
            digest: format!("sha256:{}", "0".repeat(64)),
            preview: None,
            sensitivity: Sensitivity::Sensitive,
            availability: ContentAvailability::Stored,
        };
        store.ownership.insert(
            (device_digest.clone(), quota_ref.content_id.clone()),
            OwnershipRecord {
                format_version: OWNERSHIP_FORMAT_VERSION,
                device_digest,
                content_ref: quota_ref,
                created_at_ms: 1,
                expires_at_ms: 1_000,
            },
        );
        let one = write_request(b"x");
        assert!(matches!(
            store
                .write_for_device(&owner, &one, b"x", 100)
                .expect("quota response"),
            ContentWriteResponse::Rejected {
                error: CanonicalError {
                    code: ErrorCode::InvalidCommand,
                    ..
                }
            }
        ));
        assert!(!store.contains(&ContentId::new(hex_digest(b"x"))));

        let stale_temporary = store.root().join(".stale-upload.tmp");
        std::fs::write(&stale_temporary, b"partial").expect("stale temporary upload");
        assert!(matches!(
            store
                .write_for_device(&owner, &one, b"x", 1_000)
                .expect("expired quota is reclaimed"),
            ContentWriteResponse::Stored { .. }
        ));
        assert!(!stale_temporary.exists());
        assert_eq!(store.ownership.len(), 1);
        assert_eq!(
            std::fs::read_to_string(ownership_path(store.root()))
                .expect("compacted ownership")
                .lines()
                .count(),
            1
        );
    }
}
