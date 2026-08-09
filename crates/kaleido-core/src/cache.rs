//! Durable last-good mobile projection cache.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use kaleido_proto::effect::Cursor;
use kaleido_proto::projection::{
    validate_projection_sequence, ProjectionEnvelope, ProjectionKey, PROJECTION_VERSION,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

const CACHE_DIRECTORY: &str = "projection-cache";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheApply {
    Applied,
    RefreshRequired,
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("projection cache I/O failed during {operation}: {kind:?}")]
    Io {
        operation: &'static str,
        kind: std::io::ErrorKind,
    },

    #[error("projection cache data is malformed")]
    Malformed,

    #[error("projection cache contract violation: {0}")]
    Contract(#[from] kaleido_proto::ContractViolation),

    #[error("projection cache encoding failed")]
    Encoding,

    #[error("projection current snapshot cursor does not match its acknowledgement")]
    SnapshotCursorMismatch,
}

#[derive(Debug)]
pub struct ProjectionCache {
    root: PathBuf,
    current: HashMap<ProjectionKey, ProjectionEnvelope>,
}

impl ProjectionCache {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CacheError> {
        let root = root.as_ref().join(CACHE_DIRECTORY);
        fs::create_dir_all(&root).map_err(|error| io("create", error))?;
        let mut paths = fs::read_dir(&root)
            .map_err(|error| io("read", error))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| io("read", error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        });
        paths.sort();

        let mut current: HashMap<ProjectionKey, ProjectionEnvelope> = HashMap::new();
        for path in paths {
            let bytes = fs::read(&path).map_err(|error| io("read", error))?;
            let envelope = serde_json::from_slice::<ProjectionEnvelope>(&bytes)
                .map_err(|_| CacheError::Malformed)?;
            envelope.validate_for_transport()?;
            // #[allow(kaleido::version_branch)] reason: cache invalidation enforces the negotiated projection wire format and never selects a product capability
            if envelope.projection_version != PROJECTION_VERSION {
                continue;
            }
            if path.file_name().and_then(|name| name.to_str())
                != Some(file_name(&envelope)?.as_str())
            {
                return Err(CacheError::Malformed);
            }
            match current.get(&envelope.key) {
                None => {
                    current.insert(envelope.key.clone(), envelope);
                }
                Some(previous) if envelope.cursor == previous.cursor => {
                    if envelope != *previous {
                        return Err(CacheError::Malformed);
                    }
                }
                // Every cache file is a complete, already-validated view. A
                // `CurrentFollows` snapshot may legitimately jump over the
                // retained floor, and a crash can leave both the old and new
                // complete files behind before compaction removes the old
                // one. Cold start therefore selects the greatest complete
                // cursor; live pushes remain strictly contiguous in `apply`.
                Some(previous) if envelope.cursor.seq > previous.cursor.seq => {
                    current.insert(envelope.key.clone(), envelope);
                }
                Some(_) => {}
            }
        }
        Ok(Self { root, current })
    }

    pub fn cached(&self, key: &ProjectionKey) -> Option<&ProjectionEnvelope> {
        self.current.get(key)
    }

    pub fn since(&self, key: &ProjectionKey) -> Option<Cursor> {
        self.cached(key).map(|envelope| envelope.cursor)
    }

    pub fn apply(&mut self, envelope: ProjectionEnvelope) -> Result<CacheApply, CacheError> {
        envelope.validate_for_transport()?;
        // #[allow(kaleido::version_branch)] reason: cache invalidation enforces the negotiated projection wire format and never selects a product capability
        if envelope.projection_version != PROJECTION_VERSION {
            self.invalidate(&envelope.key)?;
            return Ok(CacheApply::RefreshRequired);
        }
        if let Some(previous) = self.current.get(&envelope.key) {
            validate_projection_sequence(
                &envelope.key,
                previous.cursor,
                std::slice::from_ref(&envelope),
            )?;
        }

        self.persist(envelope)?;
        Ok(CacheApply::Applied)
    }

    /// Applies the complete view promised by a validated `CurrentFollows`
    /// acknowledgement. This is the only legal non-contiguous replacement:
    /// resume pushes still go through [`Self::apply`] and must step by one.
    pub fn apply_current(
        &mut self,
        envelope: ProjectionEnvelope,
        expected_cursor: Cursor,
    ) -> Result<CacheApply, CacheError> {
        envelope.validate_for_transport()?;
        if envelope.cursor != expected_cursor {
            return Err(CacheError::SnapshotCursorMismatch);
        }
        // #[allow(kaleido::version_branch)] reason: cache invalidation enforces the negotiated projection wire format and never selects a product capability
        if envelope.projection_version != PROJECTION_VERSION {
            self.invalidate(&envelope.key)?;
            return Ok(CacheApply::RefreshRequired);
        }
        self.persist(envelope)?;
        Ok(CacheApply::Applied)
    }

    fn persist(&mut self, envelope: ProjectionEnvelope) -> Result<(), CacheError> {
        let final_path = self.root.join(file_name(&envelope)?);
        let temporary_path = temporary_path(&final_path)?;
        let encoded = serde_json::to_vec(&envelope).map_err(|_| CacheError::Encoding)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .map_err(|error| io("create", error))?;
        if let Err(error) = file.write_all(&encoded).and_then(|()| file.sync_all()) {
            drop(file);
            drop(fs::remove_file(&temporary_path));
            return Err(io("write", error));
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary_path, &final_path) {
            drop(fs::remove_file(&temporary_path));
            return Err(io("commit", error));
        }
        let key = envelope.key.clone();
        self.current.insert(key.clone(), envelope);
        self.remove_superseded(&key, &final_path)?;
        Ok(())
    }

    fn remove_superseded(&self, key: &ProjectionKey, keep: &Path) -> Result<(), CacheError> {
        let prefix = key_digest(key)?;
        for entry in fs::read_dir(&self.root).map_err(|error| io("read", error))? {
            let path = entry.map_err(|error| io("read", error))?.path();
            let obsolete = path != keep
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".json"));
            if obsolete {
                fs::remove_file(path).map_err(|error| io("compact", error))?;
            }
        }
        Ok(())
    }

    pub fn invalidate(&mut self, key: &ProjectionKey) -> Result<(), CacheError> {
        self.current.remove(key);
        let prefix = key_digest(key)?;
        let entries = fs::read_dir(&self.root).map_err(|error| io("read", error))?;
        for entry in entries {
            let path = entry.map_err(|error| io("read", error))?.path();
            let matches = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".json"));
            if matches {
                fs::remove_file(&path).map_err(|error| io("invalidate", error))?;
            }
        }
        Ok(())
    }
}

fn key_digest(key: &ProjectionKey) -> Result<String, CacheError> {
    let encoded = serde_json::to_vec(key).map_err(|_| CacheError::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn file_name(envelope: &ProjectionEnvelope) -> Result<String, CacheError> {
    Ok(format!(
        "{}-{:020}.json",
        key_digest(&envelope.key)?,
        envelope.cursor.seq
    ))
}

fn temporary_path(final_path: &Path) -> Result<PathBuf, CacheError> {
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(CacheError::Encoding)?;
    for attempt in 0..32_u8 {
        let candidate = final_path.with_file_name(format!("{file_name}.{attempt}.tmp"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(CacheError::Io {
        operation: "create",
        kind: std::io::ErrorKind::AlreadyExists,
    })
}

fn io(operation: &'static str, error: std::io::Error) -> CacheError {
    CacheError::Io {
        operation,
        kind: error.kind(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use kaleido_proto::effect::Cursor;
    use kaleido_proto::host::HostReachability;
    use kaleido_proto::ids::HostId;
    use kaleido_proto::projection::{
        ProjectIndexView, ProjectionEnvelope, ProjectionKey, ProjectionPayload, PROJECTION_VERSION,
    };

    use super::{CacheApply, ProjectionCache};

    fn envelope(cursor: u64) -> ProjectionEnvelope {
        let host_id = HostId::new("host-cache");
        ProjectionEnvelope {
            projection_version: PROJECTION_VERSION,
            key: ProjectionKey::ProjectIndex {
                host_id: host_id.clone(),
            },
            cursor: Cursor { seq: cursor },
            payload: ProjectionPayload::ProjectIndex {
                view: ProjectIndexView {
                    host_id,
                    reachability: HostReachability::Offline,
                    groups: Vec::new(),
                },
            },
        }
    }

    #[test]
    fn current_snapshot_then_contiguous_push_survives_a_cold_start() {
        let directory = tempfile::tempdir().expect("cache directory");
        let mut cache = ProjectionCache::open(directory.path()).expect("open cache");

        assert_eq!(
            cache.apply(envelope(9)).expect("current"),
            CacheApply::Applied
        );
        assert_eq!(
            cache.apply(envelope(10)).expect("next"),
            CacheApply::Applied
        );

        let reloaded = ProjectionCache::open(directory.path()).expect("reload cache");
        assert_eq!(reloaded.since(&envelope(10).key), Some(Cursor { seq: 10 }));
        assert_eq!(
            std::fs::read_dir(directory.path().join("projection-cache"))
                .expect("cache directory")
                .count(),
            1,
            "only the last-good cursor for a projection key is retained"
        );
    }

    #[test]
    fn a_gap_is_rejected_without_replacing_the_last_good_cursor() {
        let directory = tempfile::tempdir().expect("cache directory");
        let mut cache = ProjectionCache::open(directory.path()).expect("open cache");
        cache.apply(envelope(4)).expect("current");

        assert!(cache.apply(envelope(6)).is_err());
        assert_eq!(cache.since(&envelope(4).key), Some(Cursor { seq: 4 }));
    }

    #[test]
    fn a_validated_current_snapshot_may_replace_a_cursor_older_than_the_floor() {
        let directory = tempfile::tempdir().expect("cache directory");
        let mut cache = ProjectionCache::open(directory.path()).expect("open cache");
        cache.apply(envelope(4)).expect("old retained cursor");

        assert_eq!(
            cache
                .apply_current(envelope(9), Cursor { seq: 9 })
                .expect("current snapshot"),
            CacheApply::Applied
        );
        assert_eq!(cache.since(&envelope(9).key), Some(Cursor { seq: 9 }));
    }

    #[test]
    fn cold_start_recovers_if_current_snapshot_compaction_was_interrupted() {
        let directory = tempfile::tempdir().expect("cache directory");
        let cache_root = directory.path().join("projection-cache");
        let mut cache = ProjectionCache::open(directory.path()).expect("open cache");
        let old = envelope(4);
        cache.apply(old.clone()).expect("old current");
        cache
            .apply_current(envelope(9), Cursor { seq: 9 })
            .expect("new current snapshot");

        let interrupted_old = cache_root.join(super::file_name(&old).expect("old file name"));
        std::fs::write(
            interrupted_old,
            serde_json::to_vec(&old).expect("old envelope"),
        )
        .expect("simulate interrupted cleanup");

        let reloaded = ProjectionCache::open(directory.path()).expect("recover cache");
        assert_eq!(reloaded.since(&old.key), Some(Cursor { seq: 9 }));
    }

    #[test]
    fn a_projection_version_change_invalidates_only_that_key() {
        let directory = tempfile::tempdir().expect("cache directory");
        let mut cache = ProjectionCache::open(directory.path()).expect("open cache");
        let valid = envelope(3);
        cache.apply(valid.clone()).expect("current");
        let mut incompatible = valid.clone();
        incompatible.projection_version = PROJECTION_VERSION.saturating_add(1);

        assert_eq!(
            cache.apply(incompatible).expect("refresh decision"),
            CacheApply::RefreshRequired
        );
        assert_eq!(cache.cached(&valid.key), None);
        assert_eq!(
            ProjectionCache::open(directory.path())
                .expect("reload")
                .cached(&valid.key),
            None
        );
    }
}
