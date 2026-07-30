use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceHistoryRecord {
    pub observed_at: String,
    pub tool: String,
    pub version: String,
    pub surface_digests: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AppendSummary {
    pub appended: usize,
    pub deduplicated: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimelineStatus {
    Changed,
    Unchanged,
}

impl TimelineStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Unchanged => "unchanged",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineEntry {
    pub observed_at: String,
    pub version: String,
    pub digest: String,
    pub status: TimelineStatus,
}

#[derive(Debug, Error)]
pub enum SurfaceHistoryError {
    #[error("surface-history I/O failure: {0}")]
    Io(#[from] io::Error),
    #[error("surface-history line {line} is not valid JSON: {source}")]
    InvalidJson {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("surface-history {location} is invalid: {detail}")]
    InvalidRecord { location: String, detail: String },
    #[error("surface-history contains conflicting digests for tool `{tool}` version `{version}`")]
    DigestConflict { tool: String, version: String },
    #[error("surface-history record could not be serialized: {0}")]
    Serialization(serde_json::Error),
    #[error("surface-history path has no usable parent directory")]
    MissingParent,
}

pub fn read_history(path: &Path) -> Result<Vec<SurfaceHistoryRecord>, SurfaceHistoryError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(SurfaceHistoryError::Io(error)),
    };

    let mut records = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let line_number = index + 1;
        if line.is_empty() {
            return Err(SurfaceHistoryError::InvalidRecord {
                location: format!("line {line_number}"),
                detail: "blank JSONL records are not allowed".to_owned(),
            });
        }
        let record = serde_json::from_str::<SurfaceHistoryRecord>(line).map_err(|source| {
            SurfaceHistoryError::InvalidJson {
                line: line_number,
                source,
            }
        })?;
        validate_record(&record, &format!("line {line_number}"))?;
        records.push(record);
    }
    validate_existing_keys(&records)?;
    Ok(records)
}

pub fn append_observations(
    path: &Path,
    observations: &[SurfaceHistoryRecord],
) -> Result<AppendSummary, SurfaceHistoryError> {
    let mut records = read_history(path)?;
    let mut by_key = records
        .iter()
        .map(|record| {
            (
                (record.tool.clone(), record.version.clone()),
                record.surface_digests.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut summary = AppendSummary::default();

    for (index, observation) in observations.iter().enumerate() {
        validate_record(observation, &format!("observation {}", index + 1))?;
        let key = (observation.tool.clone(), observation.version.clone());
        match by_key.get(&key) {
            Some(digests) if digests == &observation.surface_digests => {
                summary.deduplicated += 1;
            }
            Some(_) => {
                return Err(SurfaceHistoryError::DigestConflict {
                    tool: observation.tool.clone(),
                    version: observation.version.clone(),
                });
            }
            None => {
                by_key.insert(key, observation.surface_digests.clone());
                records.push(observation.clone());
                summary.appended += 1;
            }
        }
    }

    if summary.appended != 0 {
        write_records_atomic(path, &records)?;
    }
    Ok(summary)
}

pub fn timeline(
    path: &Path,
    tool: &str,
    entry: &str,
) -> Result<Vec<TimelineEntry>, SurfaceHistoryError> {
    let records = read_history(path)?;
    let mut previous_digest: Option<&str> = None;
    let mut entries = Vec::new();

    for record in &records {
        if record.tool != tool {
            continue;
        }
        let Some(digest) = record.surface_digests.get(entry) else {
            continue;
        };
        let status = match previous_digest {
            Some(previous) if previous == digest => TimelineStatus::Unchanged,
            Some(_) | None => TimelineStatus::Changed,
        };
        entries.push(TimelineEntry {
            observed_at: record.observed_at.clone(),
            version: record.version.clone(),
            digest: digest.clone(),
            status,
        });
        previous_digest = Some(digest);
    }
    Ok(entries)
}

pub fn format_timeline(tool: &str, entry: &str, entries: &[TimelineEntry]) -> String {
    let mut lines = vec![format!("schema history: tool={tool} entry={entry}")];
    if entries.is_empty() {
        lines.push("  no observations".to_owned());
    } else {
        lines.extend(entries.iter().map(|item| {
            format!(
                "  {} version={} digest={} {}",
                item.observed_at,
                item.version,
                item.digest,
                item.status.as_str()
            )
        }));
    }
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn validate_existing_keys(records: &[SurfaceHistoryRecord]) -> Result<(), SurfaceHistoryError> {
    let mut keys = HashMap::new();
    for record in records {
        let key = (record.tool.as_str(), record.version.as_str());
        if keys.insert(key, &record.surface_digests).is_some() {
            return Err(SurfaceHistoryError::InvalidRecord {
                location: format!("tool `{}` version `{}`", record.tool, record.version),
                detail: "duplicate tool+version record".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_record(
    record: &SurfaceHistoryRecord,
    location: &str,
) -> Result<(), SurfaceHistoryError> {
    if DateTime::parse_from_rfc3339(&record.observed_at).is_err() {
        return Err(invalid_record(
            location,
            "`observed_at` must be an RFC 3339 timestamp",
        ));
    }
    if record.tool.trim().is_empty() {
        return Err(invalid_record(location, "`tool` must not be empty"));
    }
    if record.version.trim().is_empty() {
        return Err(invalid_record(location, "`version` must not be empty"));
    }
    if record.surface_digests.is_empty() {
        return Err(invalid_record(
            location,
            "`surface_digests` must not be empty",
        ));
    }
    for (entry, digest) in &record.surface_digests {
        if entry.trim().is_empty() {
            return Err(invalid_record(
                location,
                "`surface_digests` contains an empty entry name",
            ));
        }
        if !is_sha256_digest(digest) {
            return Err(invalid_record(
                location,
                &format!("entry `{entry}` does not contain a lowercase sha256 digest"),
            ));
        }
    }
    Ok(())
}

fn invalid_record(location: &str, detail: &str) -> SurfaceHistoryError {
    SurfaceHistoryError::InvalidRecord {
        location: location.to_owned(),
        detail: detail.to_owned(),
    }
}

fn is_sha256_digest(value: &str) -> bool {
    let Some(hexadecimal) = value.strip_prefix("sha256:") else {
        return false;
    };
    hexadecimal.len() == 64
        && hexadecimal
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_records_atomic(
    path: &Path,
    records: &[SurfaceHistoryRecord],
) -> Result<(), SurfaceHistoryError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(SurfaceHistoryError::MissingParent)?;
    let mut serialized = String::new();
    for record in records {
        let line = serde_json::to_string(record).map_err(SurfaceHistoryError::Serialization)?;
        serialized.push_str(&line);
        serialized.push('\n');
    }

    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(serialized.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| SurfaceHistoryError::Io(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use tempfile::tempdir;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn record(
        observed_at: &str,
        tool: &str,
        version: &str,
        entry: &str,
        digest: &str,
    ) -> SurfaceHistoryRecord {
        SurfaceHistoryRecord {
            observed_at: observed_at.to_owned(),
            tool: tool.to_owned(),
            version: version.to_owned(),
            surface_digests: BTreeMap::from([(entry.to_owned(), digest.to_owned())]),
        }
    }

    #[test]
    fn first_append_writes_one_strict_lf_terminated_json_record() {
        let directory = tempdir().expect("temporary directory must be created");
        let path = directory.path().join("surface-history.jsonl");
        let observation = record(
            "2026-07-30T10:00:00Z",
            "opencode",
            "1.18.9",
            "opencode-event",
            DIGEST_A,
        );

        let summary = append_observations(&path, std::slice::from_ref(&observation))
            .expect("first observation must be appended");

        assert_eq!(
            summary,
            AppendSummary {
                appended: 1,
                deduplicated: 0
            }
        );
        assert_eq!(
            fs::read_to_string(&path).expect("history must be readable"),
            format!(
                "{}\n",
                serde_json::to_string(&observation).expect("record must serialize")
            )
        );
        assert_eq!(
            read_history(&path).expect("history must parse"),
            vec![observation]
        );
    }

    #[test]
    fn identical_tool_version_and_digests_are_idempotent() {
        let directory = tempdir().expect("temporary directory must be created");
        let path = directory.path().join("surface-history.jsonl");
        let first = record(
            "2026-07-30T10:00:00Z",
            "codex",
            "0.146.0",
            "codex-initialize",
            DIGEST_A,
        );
        append_observations(&path, std::slice::from_ref(&first))
            .expect("first append must succeed");
        let before = fs::read(&path).expect("history must be readable");
        let duplicate = SurfaceHistoryRecord {
            observed_at: "2026-07-30T11:00:00Z".to_owned(),
            ..first
        };

        let summary = append_observations(&path, &[duplicate]).expect("duplicate must be a no-op");

        assert_eq!(
            summary,
            AppendSummary {
                appended: 0,
                deduplicated: 1
            }
        );
        assert_eq!(
            fs::read(&path).expect("history must remain readable"),
            before
        );
    }

    #[test]
    fn same_tool_version_with_different_digests_fails_without_partial_batch_write() {
        let directory = tempdir().expect("temporary directory must be created");
        let path = directory.path().join("surface-history.jsonl");
        let original = record(
            "2026-07-30T10:00:00Z",
            "opencode",
            "1.18.9",
            "opencode-event",
            DIGEST_A,
        );
        append_observations(&path, &[original]).expect("initial append must succeed");
        let before = fs::read(&path).expect("history must be readable");
        let new_version = record(
            "2026-07-30T11:00:00Z",
            "codex",
            "0.147.0",
            "codex-initialize",
            DIGEST_A,
        );
        let conflict = record(
            "2026-07-30T11:00:00Z",
            "opencode",
            "1.18.9",
            "opencode-event",
            DIGEST_B,
        );

        let error = append_observations(&path, &[new_version, conflict])
            .expect_err("digest conflict must reject the whole batch");

        assert!(matches!(
            error,
            SurfaceHistoryError::DigestConflict { tool, version }
                if tool == "opencode" && version == "1.18.9"
        ));
        assert_eq!(
            fs::read(&path).expect("history must remain readable"),
            before
        );
    }

    #[test]
    fn malformed_existing_jsonl_fails_without_modifying_the_file() {
        let directory = tempdir().expect("temporary directory must be created");
        let path = directory.path().join("surface-history.jsonl");
        let malformed = b"{\"observed_at\":\n";
        fs::write(&path, malformed).expect("malformed fixture must be written");
        let observation = record(
            "2026-07-30T10:00:00Z",
            "acp",
            "1.18.0",
            "acp-initialize",
            DIGEST_A,
        );

        let error = append_observations(&path, &[observation])
            .expect_err("malformed existing history must fail closed");

        assert!(matches!(
            error,
            SurfaceHistoryError::InvalidJson { line: 1, .. }
        ));
        assert_eq!(
            fs::read(&path).expect("malformed history must remain readable"),
            malformed
        );
    }

    #[test]
    fn timeline_preserves_file_order_and_marks_digest_changes() {
        let directory = tempdir().expect("temporary directory must be created");
        let path = directory.path().join("surface-history.jsonl");
        let records = [
            record(
                "2026-07-30T10:00:00Z",
                "opencode",
                "1.18.8",
                "opencode-event",
                DIGEST_A,
            ),
            record(
                "2026-07-30T11:00:00Z",
                "opencode",
                "1.18.9",
                "opencode-event",
                DIGEST_A,
            ),
            record(
                "2026-07-30T12:00:00Z",
                "codex",
                "0.146.0",
                "codex-initialize",
                DIGEST_A,
            ),
            record(
                "2026-07-30T13:00:00Z",
                "opencode",
                "1.19.0",
                "opencode-event",
                DIGEST_B,
            ),
        ];
        append_observations(&path, &records).expect("timeline fixture must be appended");

        let entries =
            timeline(&path, "opencode", "opencode-event").expect("timeline must be queryable");

        assert_eq!(
            entries,
            vec![
                TimelineEntry {
                    observed_at: "2026-07-30T10:00:00Z".to_owned(),
                    version: "1.18.8".to_owned(),
                    digest: DIGEST_A.to_owned(),
                    status: TimelineStatus::Changed,
                },
                TimelineEntry {
                    observed_at: "2026-07-30T11:00:00Z".to_owned(),
                    version: "1.18.9".to_owned(),
                    digest: DIGEST_A.to_owned(),
                    status: TimelineStatus::Unchanged,
                },
                TimelineEntry {
                    observed_at: "2026-07-30T13:00:00Z".to_owned(),
                    version: "1.19.0".to_owned(),
                    digest: DIGEST_B.to_owned(),
                    status: TimelineStatus::Changed,
                },
            ]
        );
        let rendered = format_timeline("opencode", "opencode-event", &entries);
        assert!(rendered.contains("version=1.18.8"));
        assert!(rendered.contains("version=1.18.9"));
        assert!(rendered.contains("version=1.19.0"));
        assert_eq!(rendered.matches(" changed\n").count(), 2);
        assert_eq!(rendered.matches(" unchanged\n").count(), 1);
    }
}
