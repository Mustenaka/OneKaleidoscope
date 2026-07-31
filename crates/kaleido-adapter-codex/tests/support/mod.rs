#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used, dead_code)]
//! Shared helpers for the adapter's fixture-driven tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kaleido_adapter::content::{ContentAccess, ContentAccessError};
use kaleido_adapter_codex::{parse_transcript, CodexReducer, ReducerConfig, Transcript};
use kaleido_proto::capability::EvidenceSource;
use kaleido_proto::content::{ContentAvailability, ContentKind, ContentRef, Sensitivity};
use kaleido_proto::host::{HostPlatform, LaunchSurface};
use kaleido_proto::ids::ContentId;
use kaleido_proto::turn::TurnOrigin;

/// Base instant for frames that carry only a relative offset.
pub const BASE_AT_MS: i64 = 1_785_378_000_000;

/// An in-memory body store, so a reducer test needs no filesystem.
#[derive(Debug, Default)]
pub struct MemoryContent {
    bodies: BTreeMap<String, Vec<u8>>,
}

impl MemoryContent {
    pub fn text_of(&self, reference: &ContentRef) -> String {
        self.bodies
            .get(reference.content_id.as_str())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.bodies.len()
    }
}

impl ContentAccess for MemoryContent {
    fn store(
        &mut self,
        kind: ContentKind,
        sensitivity: Sensitivity,
        bytes: &[u8],
    ) -> Result<ContentRef, ContentAccessError> {
        let digest = digest_hex(bytes);
        self.bodies.insert(digest.clone(), bytes.to_vec());
        let reference = ContentRef {
            content_id: ContentId::new(digest.clone()),
            kind,
            byte_len: bytes.len() as u64,
            digest: format!("sha256:{digest}"),
            preview: None,
            sensitivity,
            availability: ContentAvailability::Stored,
        };
        reference.validate()?;
        Ok(reference)
    }

    fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, ContentAccessError> {
        self.bodies
            .get(reference.content_id.as_str())
            .cloned()
            .ok_or_else(|| ContentAccessError::Missing {
                content_id: reference.content_id.clone(),
            })
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    // A test store only needs a collision-resistant key, and reusing the
    // production digest would hide a bug in it.
    let mut state: u128 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        state ^= u128::from(*byte);
        state = state.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{state:032x}{:032x}", bytes.len())
}

pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("crate sits two levels below the repository root")
}

pub fn fixture_path(name: &str) -> PathBuf {
    repository_root()
        .join("tests")
        .join("fixtures")
        .join("codex")
        .join(name)
}

/// Every committed Codex recording this slice reduces.
pub const FIXTURES: [&str; 3] = [
    "01-simple-turn.jsonl",
    "03-permission-approve.jsonl",
    "04-permission-deny.jsonl",
];

pub fn load_transcript(name: &str) -> Transcript {
    let raw = std::fs::read_to_string(fixture_path(name)).expect("read committed fixture");
    parse_transcript(&raw).expect("parse committed fixture")
}

pub fn reducer() -> CodexReducer {
    CodexReducer::new(ReducerConfig {
        host_display_name: "test-host".to_owned(),
        host_platform: HostPlatform::Windows,
        project_display_name: "test-project".to_owned(),
        identity_salt: "test-host".to_owned(),
        evidence: EvidenceSource::RecordedFixture,
        launch_surface: LaunchSurface::BrokerLaunched,
        turn_origin: TurnOrigin::LocalSurface,
        base_at_ms: BASE_AT_MS,
        runtime_version_label: None,
    })
}
