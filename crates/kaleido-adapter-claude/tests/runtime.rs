#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kaleido_adapter::content::ContentAccess;
use kaleido_adapter::session::{ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest};
use kaleido_adapter_claude::{ClaudeRuntimeConfig, ClaudeRuntimeSession};
use kaleido_proto::capability::Capability;
use kaleido_proto::content::{ContentKind, Sensitivity};
use kaleido_proto::effect::StateEffect;

use support::{MemoryContent, BASE_AT_MS};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

fn isolated_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "onekaleidoscope-claude-{label}-{}-{}",
        std::process::id(),
        NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).expect("isolated test root is created");
    root
}

fn runtime(resume_session: Option<&str>) -> ClaudeRuntimeSession {
    ClaudeRuntimeSession::new(ClaudeRuntimeConfig {
        node_executable: PathBuf::from("node"),
        bridge_script: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("fake-sidecar.mjs"),
        reducer: reducer_config(),
        request_timeout: Duration::from_secs(5),
        resume_session: resume_session.map(str::to_owned),
    })
}

fn reducer_config() -> kaleido_adapter_claude::ReducerConfig {
    kaleido_adapter_claude::ReducerConfig {
        host_display_name: "test-host".to_owned(),
        host_platform: kaleido_proto::host::HostPlatform::Windows,
        project_display_name: "test-project".to_owned(),
        identity_salt: "test-host".to_owned(),
        evidence: kaleido_proto::capability::EvidenceSource::RecordedFixture,
        launch_surface: kaleido_proto::host::LaunchSurface::BrokerLaunched,
        turn_origin: kaleido_proto::turn::TurnOrigin::LocalSurface,
        base_at_ms: BASE_AT_MS,
        runtime_version_label: Some("2.1.226".to_owned()),
    }
}

fn start_request(
    runtime: &ClaudeRuntimeSession,
    root: &Path,
    content: &mut MemoryContent,
) -> SessionStartRequest {
    let root_ref = content
        .store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            root.to_string_lossy().as_bytes(),
        )
        .expect("root reference is stored");
    SessionStartRequest {
        project_id: runtime.project_id().clone(),
        project_binding_id: runtime.project_binding_id().clone(),
        runtime_id: runtime.runtime_id().clone(),
        project_root_ref: root_ref,
    }
}

#[test]
fn explicit_error_cleans_up_and_the_same_runtime_can_retry() {
    let root = isolated_root("retry");
    let mut runtime = runtime(Some("retry-once"));
    let mut content = MemoryContent::default();
    let request = start_request(&runtime, &root, &mut content);

    assert!(matches!(
        runtime.start(&request, &mut content),
        Err(RuntimeSessionError::ProtocolViolation { .. })
    ));
    runtime
        .start(&request, &mut content)
        .expect("failed start leaves the actor retryable");
    runtime.close().expect("close acknowledgement is consumed");
    fs::remove_dir_all(&root).expect("isolated retry root is removed");
}

#[test]
fn resume_returns_on_ready_without_fabricating_an_eager_resume_event() {
    let root = isolated_root("resume");
    let mut runtime = runtime(None);
    let mut content = MemoryContent::default();
    let request = start_request(&runtime, &root, &mut content);

    runtime
        .resume("provider-session", &request, &mut content)
        .expect("ready is sufficient to establish the provisional route");
    assert!(!runtime
        .capability_probe()
        .is_proven(Capability::HistoryResume));
    runtime.close().expect("close handshake succeeds");
    fs::remove_dir_all(&root).expect("isolated resume root is removed");
}

#[test]
fn history_runtime_uses_the_exact_discovery_scope_and_bounded_page() {
    let root = isolated_root("history");
    let mut runtime = runtime(None);
    let mut content = MemoryContent::default();

    runtime
        .discover(&root, &mut content)
        .expect("official discovery transport returns a closed session list");
    let session_id = runtime
        .reducer()
        .discovered_sessions()
        .first()
        .expect("fixture discovery returns one session")
        .id
        .clone();
    let effects = runtime
        .read_history(&session_id, &root, 0, 10, &mut content)
        .expect("exact bounded history page is accepted");
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, StateEffect::ItemUpserted { .. })));
    assert!(runtime
        .capability_probe()
        .is_proven(Capability::HistoryRead));

    let other_root = isolated_root("wrong-history");
    assert!(matches!(
        runtime.read_history(&session_id, &other_root, 0, 10, &mut content),
        Err(RuntimeSessionError::ProtocolViolation { .. })
    ));
    assert!(matches!(
        runtime.read_history(&session_id, &root, 0, 101, &mut content),
        Err(RuntimeSessionError::ProtocolViolation { .. })
    ));
    fs::remove_dir_all(&other_root).expect("isolated wrong-scope root is removed");
    fs::remove_dir_all(&root).expect("isolated history root is removed");
}
