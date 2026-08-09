#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used, dead_code)]
//! Shared helpers for the host daemon's integration tests.

use std::path::{Path, PathBuf};

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::content::ContentAccess;
use kaleido_adapter::session::{ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest};
use kaleido_adapter::IdentityMint;
use kaleido_adapter_codex::{parse_transcript, CodexReducer, ReducerConfig, Transcript};
use kaleido_hostd::slice::{self, ReplayRequest};
use kaleido_hostd::slice::{RunSessionIdentity, REPLAY_BASE_AT_MS};
use kaleido_proto::attention::AttentionResponse;
use kaleido_proto::capability::EvidenceSource;
use kaleido_proto::content::{ContentKind, ContentRef, Sensitivity};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::{HostPlatform, LaunchSurface};
use kaleido_proto::ids::CommandId;
use kaleido_proto::ids::SessionId;
use kaleido_proto::turn::TurnOrigin;
use kaleido_state::CanonicalState;

pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("crate sits two levels below the repository root")
}

pub fn fixture(name: &str) -> PathBuf {
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

pub struct Replayed {
    pub directory: tempfile::TempDir,
    pub log_dir: PathBuf,
    pub state: CanonicalState,
    pub session_id: SessionId,
}

pub fn replay(name: &str) -> Replayed {
    let directory = tempfile::tempdir().expect("temporary directory");
    let log_dir = directory.path().join("log");
    let request = ReplayRequest::new(fixture(name), &log_dir);
    let (outcome, store) = slice::replay_into_store(&request).expect("replay the recording");
    Replayed {
        directory,
        log_dir,
        state: store.state().clone(),
        session_id: outcome.session_id,
    }
}

/// Strings the recordings contain that section 10 forbids in an ordinary log.
pub fn forbidden_strings() -> Vec<&'static str> {
    vec![
        // Message and reasoning bodies.
        "KALEIDO SIMPLE TURN",
        "KALEIDO PERMISSION PROBE",
        "Reply with exactly this plain text",
        "Use the file-editing tool",
        // A filesystem path and a diff fragment.
        "editable.txt",
        "<SANDBOX>",
        // Raw upstream identifiers, from all three recordings.
        "019fb0d8-5af0-7d22-a53f-daf0d7c4c510",
        "019fb1ab-b957-7360-9092-8bbb9c1ae8b4",
        "019fb1e8-27db-7bf1-b55c-7336590804f4",
        "call_2U5LK993kimYZdKFejO2dr32",
        "call_VPQYSRFTv9gqoAPa1eKIzBIv",
        "msg_0ea24d87ed904bf7016a6ab665968c819a922fd5c2035d7bbc",
        "rs_0b873888fe560d28016a6aec81038c8198a25ddefd4799c2cf",
        // The upstream method vocabulary has no place in canonical records.
        "item/agentMessage/delta",
        "thread/status/changed",
        "requestApproval",
    ]
}

/// A provider session driven by a committed Codex recording.
///
/// The first six frames are the recorded initialize/thread-start exchange, the
/// seventh is turn/start, and subsequent server frames are drained one at a
/// time. Approval replies consume the next recorded client frame. This makes
/// the host composition path deterministic while retaining real upstream wire
/// evidence.
pub struct FixtureRuntime {
    reducer: CodexReducer,
    transcript: Transcript,
    wire_payloads: Vec<serde_json::Value>,
    next_frame: usize,
    exit_on_drain: bool,
}

impl FixtureRuntime {
    pub fn new(name: &str, command_id: CommandId) -> Self {
        let raw = std::fs::read_to_string(fixture(name)).expect("read fixture");
        let transcript = parse_transcript(&raw).expect("parse fixture");
        let wire_payloads = raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .expect("fixture line is JSON")
                    .get("payload")
                    .cloned()
                    .expect("fixture line has payload")
            })
            .collect();
        let reducer = CodexReducer::new(ReducerConfig {
            host_display_name: "kaleido-host".to_owned(),
            host_platform: HostPlatform::Windows,
            project_display_name: "kaleido-slice".to_owned(),
            identity_salt: "kaleido-host".to_owned(),
            evidence: EvidenceSource::ObservedInTraffic,
            launch_surface: LaunchSurface::BrokerLaunched,
            turn_origin: TurnOrigin::RemoteCommand { command_id },
            base_at_ms: REPLAY_BASE_AT_MS,
            runtime_version_label: Some("codex-cli 0.146.0".to_owned()),
        });
        Self {
            reducer,
            transcript,
            wire_payloads,
            next_frame: 0,
            exit_on_drain: false,
        }
    }

    pub fn exiting(name: &str, command_id: CommandId) -> Self {
        let mut runtime = Self::new(name, command_id);
        runtime.exit_on_drain = true;
        runtime
    }

    pub fn identity(&self) -> RunSessionIdentity {
        let mint = IdentityMint::new("kaleido-host");
        let runtime_id = mint.runtime_id("kaleido-host|app-server");
        let project_id = mint.project_id("kaleido-slice");
        let project_binding_id = mint.project_binding_id(&format!("kaleido-slice|{runtime_id}"));
        RunSessionIdentity {
            project_id,
            project_binding_id,
            runtime_id,
        }
    }

    fn reduce_next(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let frame = self
            .transcript
            .frames()
            .get(self.next_frame)
            .cloned()
            .ok_or_else(protocol_violation)?;
        self.next_frame += 1;
        self.reducer
            .ingest_frame(&frame, content)
            .map_err(|_| protocol_violation())
    }

    fn reduce_until(
        &mut self,
        end: usize,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let mut effects = Vec::new();
        while self.next_frame < end {
            effects.extend(self.reduce_next(content)?);
        }
        Ok(effects)
    }
}

impl ProviderRuntimeSession for FixtureRuntime {
    fn start(
        &mut self,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let expected = self.identity();
        if request.project_id != expected.project_id
            || request.project_binding_id != expected.project_binding_id
            || request.runtime_id != expected.runtime_id
            || request.project_root_ref.kind != ContentKind::FilePath
            || request.project_root_ref.sensitivity != Sensitivity::Sensitive
            || content.load(&request.project_root_ref)?.is_empty()
        {
            return Err(protocol_violation());
        }
        self.reduce_until(6, content)
    }

    fn submit_prompt(
        &mut self,
        command_id: &CommandId,
        body: &ContentRef,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        body.ensure_sensitive("fixture.prompt")
            .map_err(RuntimeSessionError::Contract)?;
        if content.load(body)?.is_empty() {
            return Err(protocol_violation());
        }
        let request_id = self
            .wire_payloads
            .get(self.next_frame)
            .filter(|payload| {
                payload.get("method").and_then(serde_json::Value::as_str) == Some("turn/start")
            })
            .and_then(|payload| payload.get("id"))
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(protocol_violation)?;
        let response_end = self
            .wire_payloads
            .iter()
            .enumerate()
            .skip(self.next_frame + 1)
            .find_map(|(index, payload)| {
                (payload.get("id").and_then(serde_json::Value::as_i64) == Some(request_id)
                    && payload.get("result").is_some())
                .then_some(index + 1)
            })
            .ok_or_else(protocol_violation)?;
        if !self
            .reducer
            .register_local_turn_start(request_id, command_id)
        {
            return Err(protocol_violation());
        }
        let reduced = self.reduce_until(response_end, content);
        if reduced.is_err() {
            self.reducer.cancel_local_turn_start(request_id);
        }
        reduced
    }

    fn respond_attention(
        &mut self,
        response: &AttentionResponse,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        let expected_decision = self
            .wire_payloads
            .get(self.next_frame)
            .and_then(|payload| payload.pointer("/result/decision"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(protocol_violation)?;
        if response.option_id.as_deref() != Some(expected_decision) {
            return Err(protocol_violation());
        }
        self.reduce_next(content)
    }

    fn drain_effects(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if self.exit_on_drain {
            self.exit_on_drain = false;
            return self
                .reducer
                .process_exited(Some(23), REPLAY_BASE_AT_MS + 2_000)
                .map_err(|_| protocol_violation());
        }
        if self.next_frame == self.transcript.len() {
            return Ok(Vec::new());
        }
        self.reduce_next(content)
    }

    fn close(&mut self) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.reducer
            .clean_disconnected(REPLAY_BASE_AT_MS + 120_000)
            .map_err(|_| protocol_violation())
    }

    fn capability_probe(&self) -> CapabilityProbe {
        self.reducer.capability_probe()
    }
}

fn protocol_violation() -> RuntimeSessionError {
    RuntimeSessionError::ProtocolViolation {
        detail: "fixture session sequence mismatch".to_owned(),
    }
}
