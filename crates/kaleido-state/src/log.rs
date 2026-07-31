//! The append-only durable log.
//!
//! One file per [`StreamKey`], one JSON [`LogRecord`] per line. Section 5.2
//! makes a cursor gap log corruption rather than a warning, so reading a stream
//! back always runs [`verify_contiguous`] before any record is applied.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use kaleido_proto::effect::{verify_contiguous, Cursor, LogRecord, StreamKey};

use crate::error::StateError;

/// Directory name for stream files inside the store root.
pub const STREAM_DIRECTORY: &str = "streams";

/// Append-only storage for canonical state transitions.
#[derive(Debug, Clone)]
pub struct StreamLog {
    root: PathBuf,
}

impl StreamLog {
    /// Opens (creating if needed) the stream directory under `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StateError> {
        let root = root.as_ref().join(STREAM_DIRECTORY);
        fs::create_dir_all(&root).map_err(|source| StateError::io(&root, source))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, stream: &StreamKey) -> PathBuf {
        self.root.join(stream_file_name(stream))
    }

    /// Appends one record, flushing before returning.
    ///
    /// Section 5.2 only allows a cursor to be assigned after a durable append
    /// succeeds, so the caller must treat an error here as "the cursor was
    /// never used".
    pub fn append(&self, record: &LogRecord) -> Result<(), StateError> {
        let path = self.path_for(&record.stream);
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
        Ok(())
    }

    /// Reads every stream, verifies each one, and returns a deterministic
    /// global order.
    ///
    /// Cross-stream ordering is not guaranteed by the contract (section 5.2),
    /// so the store makes its own: append timestamps are strictly increasing
    /// per store, and the file name plus cursor break any remaining tie.
    pub fn read_all(&self) -> Result<Vec<LogRecord>, StateError> {
        let mut per_stream = self.read_streams()?;
        let mut records = Vec::new();
        for (_, stream_records) in per_stream.iter_mut() {
            records.append(stream_records);
        }
        records.sort_by(|left, right| {
            left.appended_at_ms
                .cmp(&right.appended_at_ms)
                .then_with(|| stream_file_name(&left.stream).cmp(&stream_file_name(&right.stream)))
                .then_with(|| left.cursor.cmp(&right.cursor))
        });
        Ok(records)
    }

    /// Reads each stream file separately, verifying contiguity within it.
    pub fn read_streams(&self) -> Result<BTreeMap<String, Vec<LogRecord>>, StateError> {
        let mut streams = BTreeMap::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(streams);
            }
            Err(source) => return Err(StateError::io(&self.root, source)),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| StateError::io(&self.root, source))?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                paths.push(path);
            }
        }
        paths.sort();
        for path in paths {
            let records = read_stream_file(&path)?;
            verify_contiguous(&records)?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            streams.insert(name, records);
        }
        Ok(streams)
    }

    /// The highest cursor recorded for each stream, for resuming appends.
    pub fn stream_heads(&self) -> Result<Vec<(StreamKey, Cursor)>, StateError> {
        let mut heads = Vec::new();
        for (_, records) in self.read_streams()? {
            if let Some(last) = records.last() {
                heads.push((last.stream.clone(), last.cursor));
            }
        }
        Ok(heads)
    }
}

fn read_stream_file(path: &Path) -> Result<Vec<LogRecord>, StateError> {
    let file = File::open(path).map_err(|source| StateError::io(path, source))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|source| StateError::io(path, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let record =
            serde_json::from_str::<LogRecord>(&line).map_err(|_| StateError::MalformedRecord {
                path: path.to_path_buf(),
                line: index + 1,
            })?;
        records.push(record);
    }
    Ok(records)
}

/// A filesystem-safe file name for one stream.
///
/// Only broker-assigned canonical identifiers reach this function, and section
/// 10 permits those in ordinary storage; upstream identifiers never do.
pub fn stream_file_name(stream: &StreamKey) -> String {
    let (kind, identifier) = match stream {
        StreamKey::Host { host_id } => ("host", host_id.as_str()),
        StreamKey::Project { project_id } => ("project", project_id.as_str()),
        StreamKey::Session { session_id } => ("session", session_id.as_str()),
        StreamKey::Workflow { workflow_id } => ("workflow", workflow_id.as_str()),
    };
    let mut sanitised = String::with_capacity(identifier.len());
    for character in identifier.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            sanitised.push(character);
        } else {
            sanitised.push('_');
        }
    }
    format!("{kind}-{sanitised}.jsonl")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use kaleido_proto::effect::{DiagnosticCode, DiagnosticRecord, StateEffect};
    use kaleido_proto::ids::SessionId;
    use kaleido_proto::ContractViolation;

    fn diagnostic(count: u64) -> StateEffect {
        StateEffect::DiagnosticRecorded {
            diagnostic: DiagnosticRecord {
                runtime_id: None,
                session_id: None,
                code: DiagnosticCode::UnknownUpstreamMessage,
                count,
                first_at_ms: 1,
                last_at_ms: 2,
                detail_ref: None,
            },
        }
    }

    fn record(seq: u64, appended_at_ms: i64) -> LogRecord {
        LogRecord {
            cursor: Cursor { seq },
            stream: StreamKey::Session {
                session_id: SessionId::new("ses_0123456789abcdef"),
            },
            appended_at_ms,
            effect: diagnostic(seq),
        }
    }

    #[test]
    fn records_round_trip_through_the_log() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = StreamLog::open(directory.path()).expect("open log");
        log.append(&record(0, 10)).expect("append first");
        log.append(&record(1, 11)).expect("append second");
        let records = log.read_all().expect("read back");
        assert_eq!(records.len(), 2);
        assert_eq!(records.first().map(|entry| entry.cursor.seq), Some(0));
        assert_eq!(records.get(1).map(|entry| entry.cursor.seq), Some(1));
    }

    #[test]
    fn a_skipped_cursor_is_reported_as_a_gap() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = StreamLog::open(directory.path()).expect("open log");
        log.append(&record(0, 10)).expect("append first");
        log.append(&record(2, 11)).expect("append skipped");
        assert!(matches!(
            log.read_all(),
            Err(StateError::Contract(ContractViolation::CursorGap {
                expected: 1,
                found: 2
            }))
        ));
    }

    #[test]
    fn a_corrupt_line_is_reported_with_its_position() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = StreamLog::open(directory.path()).expect("open log");
        let first = record(0, 10);
        log.append(&first).expect("append first");
        let path = log.path_for(&first.stream);
        let mut contents = std::fs::read_to_string(&path).expect("read log");
        contents.push_str("{ this is not a record }\n");
        std::fs::write(&path, contents).expect("write log");
        assert!(matches!(
            log.read_all(),
            Err(StateError::MalformedRecord { line: 2, .. })
        ));
    }
}
