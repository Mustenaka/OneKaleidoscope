//! Durable, per-`ProjectionKey` complete read-model history.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use kaleido_proto::effect::Cursor;
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::projection::{
    decide_projection_subscription, ProjectionEnvelope, ProjectionKey, ProjectionPayload,
    ProjectionSubscribe, ProjectionSubscribeAck, ProjectionSubscribeOutcome, PROJECTION_VERSION,
};

use crate::content::hex_digest;
use crate::error::StateError;

pub const PROJECTION_DIRECTORY: &str = "projections";
pub const DEFAULT_RETENTION_ENTRIES: usize = 256;

/// A stable replay decision and the exact complete views which follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionReplay {
    pub ack: ProjectionSubscribeAck,
    pub envelopes: Vec<ProjectionEnvelope>,
}

/// Persistent complete projection histories, isolated by `ProjectionKey`.
#[derive(Debug)]
pub struct ProjectionJournal {
    root: Option<PathBuf>,
    retention_entries: usize,
    entries: HashMap<ProjectionKey, Vec<ProjectionEnvelope>>,
}

impl ProjectionJournal {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StateError> {
        Self::open_with_retention(root, DEFAULT_RETENTION_ENTRIES)
    }

    pub fn open_with_retention(
        root: impl AsRef<Path>,
        retention_entries: usize,
    ) -> Result<Self, StateError> {
        if retention_entries == 0 {
            return Err(StateError::InvalidProjectionRetention);
        }
        let root = root.as_ref().join(PROJECTION_DIRECTORY);
        fs::create_dir_all(&root).map_err(|source| StateError::io(&root, source))?;
        let mut journal = Self {
            root: Some(root),
            retention_entries,
            entries: HashMap::new(),
        };
        journal.read_all()?;
        Ok(journal)
    }

    pub(crate) fn memory(retention_entries: usize) -> Result<Self, StateError> {
        if retention_entries == 0 {
            return Err(StateError::InvalidProjectionRetention);
        }
        Ok(Self {
            root: None,
            retention_entries,
            entries: HashMap::new(),
        })
    }

    pub fn retention_entries(&self) -> usize {
        self.retention_entries
    }

    pub fn current(&self, key: &ProjectionKey) -> Option<&ProjectionEnvelope> {
        self.entries.get(key).and_then(|entries| entries.last())
    }

    pub fn head(&self, key: &ProjectionKey) -> Option<Cursor> {
        self.current(key).map(|entry| entry.cursor)
    }

    pub fn floor(&self, key: &ProjectionKey) -> Option<Cursor> {
        let entries = self.entries.get(key)?;
        let retained_from = entries.len().saturating_sub(self.retention_entries);
        entries.get(retained_from).map(|entry| entry.cursor)
    }

    /// Appends a full read model only when it differs field-for-field from the
    /// previous payload for this key.
    pub fn record(
        &mut self,
        key: ProjectionKey,
        payload: ProjectionPayload,
    ) -> Result<Option<ProjectionEnvelope>, StateError> {
        payload.validate_for_key(&key)?;
        if self
            .current(&key)
            .is_some_and(|current| current.payload == payload)
        {
            return Ok(None);
        }
        let cursor = match self.head(&key) {
            Some(previous) => previous.next()?,
            None => Cursor::START,
        };
        let envelope = ProjectionEnvelope {
            projection_version: PROJECTION_VERSION,
            key: key.clone(),
            cursor,
            payload,
        };
        envelope.validate_for_transport()?;
        self.append_file(&envelope)?;
        self.entries.entry(key).or_default().push(envelope.clone());
        Ok(Some(envelope))
    }

    /// Reconciles a journal against the history reconstructed from the
    /// canonical log. Persisted history must be an exact prefix; missing tail
    /// entries are the only repair allowed, closing a canonical-log/projection
    /// append crash window without inventing cursors.
    pub(crate) fn reconcile(&mut self, expected: &Self) -> Result<(), StateError> {
        for (key, actual_entries) in &self.entries {
            let Some(expected_entries) = expected.entries.get(key) else {
                return Err(StateError::ProjectionJournalDiverged { key: key.clone() });
            };
            if actual_entries.len() > expected_entries.len()
                || actual_entries
                    .iter()
                    .zip(expected_entries)
                    .any(|(actual, expected)| actual != expected)
            {
                return Err(StateError::ProjectionJournalDiverged { key: key.clone() });
            }
        }
        for (key, expected_entries) in &expected.entries {
            let present = self.entries.get(key).map_or(0, Vec::len);
            for envelope in expected_entries.iter().skip(present) {
                self.append_file(envelope)?;
                self.entries
                    .entry(key.clone())
                    .or_default()
                    .push(envelope.clone());
            }
        }
        Ok(())
    }

    pub fn replay(
        &self,
        request: &ProjectionSubscribe,
        at_ms: i64,
    ) -> Result<ProjectionReplay, StateError> {
        request.key.validate()?;
        let Some(head) = self.head(&request.key) else {
            return Ok(rejected(request, ErrorCode::NotFound, false, at_ms));
        };
        let floor =
            self.floor(&request.key)
                .ok_or_else(|| StateError::ProjectionJournalDiverged {
                    key: request.key.clone(),
                })?;
        let ack = decide_projection_subscription(request, floor, head, at_ms)?;
        let entries = self.entries.get(&request.key).ok_or_else(|| {
            StateError::ProjectionJournalDiverged {
                key: request.key.clone(),
            }
        })?;
        let envelopes = match &ack.outcome {
            ProjectionSubscribeOutcome::CurrentFollows { current_cursor } => entries
                .iter()
                .find(|entry| entry.cursor == *current_cursor)
                .cloned()
                .into_iter()
                .collect(),
            ProjectionSubscribeOutcome::Resumed { from_cursor } => entries
                .iter()
                .filter(|entry| entry.cursor >= *from_cursor)
                .cloned()
                .collect(),
            ProjectionSubscribeOutcome::Rejected { .. } => Vec::new(),
        };
        Ok(ProjectionReplay { ack, envelopes })
    }

    pub(crate) fn checkpoint(&self) -> HashMap<ProjectionKey, usize> {
        self.entries
            .iter()
            .map(|(key, entries)| (key.clone(), entries.len()))
            .collect()
    }

    pub(crate) fn changes_since(
        &self,
        checkpoint: &HashMap<ProjectionKey, usize>,
    ) -> Result<Vec<ProjectionEnvelope>, StateError> {
        let mut changed = self
            .entries
            .iter()
            .flat_map(|(key, entries)| {
                entries
                    .iter()
                    .skip(checkpoint.get(key).copied().unwrap_or(0))
                    .cloned()
            })
            .collect::<Vec<_>>();
        let mut keyed = changed
            .drain(..)
            .map(|entry| Ok((serde_json::to_vec(&entry.key)?, entry)))
            .collect::<Result<Vec<_>, StateError>>()?;
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(keyed.into_iter().map(|(_, entry)| entry).collect())
    }

    fn append_file(&self, envelope: &ProjectionEnvelope) -> Result<(), StateError> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let path = path_for(root, &envelope.key)?;
        let mut line = serde_json::to_string(envelope)?;
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
            .map_err(|source| StateError::io(&path, source))
    }

    fn read_all(&mut self) -> Result<(), StateError> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let mut paths = fs::read_dir(root)
            .map_err(|source| StateError::io(root, source))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| StateError::io(root, source))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        });
        paths.sort();
        for path in paths {
            let entries = read_file(&path)?;
            let Some(first) = entries.first() else {
                continue;
            };
            let key = first.key.clone();
            let expected_path = path_for(root, &key)?;
            if expected_path != path || self.entries.contains_key(&key) {
                return Err(StateError::ProjectionJournalDiverged { key });
            }
            self.entries.insert(key, entries);
        }
        Ok(())
    }
}

fn read_file(path: &Path) -> Result<Vec<ProjectionEnvelope>, StateError> {
    let file = File::open(path).map_err(|source| StateError::io(path, source))?;
    let mut entries = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| StateError::io(path, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope = serde_json::from_str::<ProjectionEnvelope>(&line).map_err(|_| {
            StateError::MalformedRecord {
                path: path.to_path_buf(),
                line: index + 1,
            }
        })?;
        envelope.validate_for_transport()?;
        if let Some(previous) = entries.last() {
            let previous: &ProjectionEnvelope = previous;
            if envelope.key != previous.key {
                return Err(StateError::Contract(
                    kaleido_proto::ContractViolation::MixedProjectionKeys,
                ));
            }
            if envelope.cursor == previous.cursor {
                return Err(StateError::Contract(
                    kaleido_proto::ContractViolation::CursorRepeated {
                        cursor: envelope.cursor.seq,
                    },
                ));
            }
            let expected = previous.cursor.next()?;
            if envelope.cursor != expected {
                return Err(StateError::Contract(
                    kaleido_proto::ContractViolation::CursorGap {
                        expected: expected.seq,
                        found: envelope.cursor.seq,
                    },
                ));
            }
        // #[allow(kaleido::version_branch)] reason: persisted transport envelope integrity requires the first cursor to equal the protocol start sentinel and never selects a product capability
        } else if envelope.cursor != Cursor::START {
            return Err(StateError::Contract(
                kaleido_proto::ContractViolation::CursorGap {
                    expected: Cursor::START.seq,
                    found: envelope.cursor.seq,
                },
            ));
        }
        entries.push(envelope);
    }
    Ok(entries)
}

fn path_for(root: &Path, key: &ProjectionKey) -> Result<PathBuf, StateError> {
    let encoded = serde_json::to_vec(key)?;
    Ok(root.join(format!("{}.jsonl", hex_digest(&encoded))))
}

fn rejected(
    request: &ProjectionSubscribe,
    code: ErrorCode,
    retriable: bool,
    at_ms: i64,
) -> ProjectionReplay {
    ProjectionReplay {
        ack: ProjectionSubscribeAck {
            key: request.key.clone(),
            outcome: ProjectionSubscribeOutcome::Rejected {
                error: CanonicalError {
                    code,
                    retriable,
                    detail_ref: None,
                    at_ms,
                },
            },
        },
        envelopes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use kaleido_proto::host::HostReachability;
    use kaleido_proto::ids::HostId;
    use kaleido_proto::projection::ProjectIndexView;
    use kaleido_proto::ContractViolation;

    fn key() -> ProjectionKey {
        ProjectionKey::ProjectIndex {
            host_id: HostId::new("hst_projection_test"),
        }
    }

    fn payload(reachability: HostReachability) -> ProjectionPayload {
        ProjectionPayload::ProjectIndex {
            view: ProjectIndexView {
                host_id: HostId::new("hst_projection_test"),
                reachability,
                groups: Vec::new(),
            },
        }
    }

    #[test]
    fn retention_selects_resume_or_current_without_crossing_keys() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut journal =
            ProjectionJournal::open_with_retention(directory.path(), 2).expect("journal");
        for reachability in [
            HostReachability::Offline,
            HostReachability::LanDirect,
            HostReachability::PeerToPeer,
            HostReachability::Relayed,
        ] {
            journal
                .record(key(), payload(reachability))
                .expect("append projection");
        }

        let stale = journal
            .replay(
                &ProjectionSubscribe {
                    key: key(),
                    since: Some(Cursor::START),
                },
                10,
            )
            .expect("stale replay");
        assert!(matches!(
            stale.ack.outcome,
            ProjectionSubscribeOutcome::CurrentFollows {
                current_cursor: Cursor { seq: 3 }
            }
        ));
        assert_eq!(stale.envelopes.len(), 1);

        let resumed = journal
            .replay(
                &ProjectionSubscribe {
                    key: key(),
                    since: Some(Cursor { seq: 1 }),
                },
                10,
            )
            .expect("retained replay");
        assert!(matches!(
            resumed.ack.outcome,
            ProjectionSubscribeOutcome::Resumed {
                from_cursor: Cursor { seq: 2 }
            }
        ));
        assert_eq!(
            resumed
                .envelopes
                .iter()
                .map(|entry| entry.cursor.seq)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );

        let ahead = journal
            .replay(
                &ProjectionSubscribe {
                    key: key(),
                    since: Some(Cursor { seq: 4 }),
                },
                10,
            )
            .expect("ahead response");
        assert!(matches!(
            ahead.ack.outcome,
            ProjectionSubscribeOutcome::Rejected {
                error: CanonicalError {
                    code: ErrorCode::CursorGap,
                    ..
                }
            }
        ));
    }

    #[test]
    fn append_overflow_and_persisted_gap_fail_closed() {
        let mut memory = ProjectionJournal::memory(2).expect("memory journal");
        memory.entries.insert(
            key(),
            vec![ProjectionEnvelope {
                projection_version: PROJECTION_VERSION,
                key: key(),
                cursor: Cursor { seq: u64::MAX },
                payload: payload(HostReachability::Offline),
            }],
        );
        assert!(matches!(
            memory.record(key(), payload(HostReachability::LanDirect)),
            Err(StateError::Contract(ContractViolation::CursorOverflow))
        ));

        let directory = tempfile::tempdir().expect("temporary directory");
        let mut persisted = ProjectionJournal::open(directory.path()).expect("journal");
        let first = persisted
            .record(key(), payload(HostReachability::Offline))
            .expect("first append")
            .expect("first entry");
        let mut skipped = first;
        skipped.cursor = Cursor { seq: 2 };
        skipped.payload = payload(HostReachability::LanDirect);
        let root = directory.path().join(PROJECTION_DIRECTORY);
        let path = fs::read_dir(&root)
            .expect("projection directory")
            .next()
            .expect("projection file")
            .expect("directory entry")
            .path();
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open journal file");
        writeln!(
            file,
            "{}",
            serde_json::to_string(&skipped).expect("encode skipped")
        )
        .expect("append skipped");
        file.sync_data().expect("sync skipped record");
        assert!(matches!(
            ProjectionJournal::open(directory.path()),
            Err(StateError::Contract(ContractViolation::CursorGap {
                expected: 1,
                found: 2
            }))
        ));
    }
}
