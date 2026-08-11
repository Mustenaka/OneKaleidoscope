//! The offline vertical slice: recorded traffic in, canonical state out.
//!
//! `replay` runs a committed recording through the same decoder, reducer and
//! store a live process would use, and starts no process at all. That is what
//! makes every assertion about this path deterministic: the only inputs are
//! bytes already in the repository.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::session::{ProviderRuntimeSession, SessionStartRequest};
use kaleido_adapter::IdentityMint;
use kaleido_adapter_codex::{
    parse_transcript, CodexReducer, CodexRuntimeConfig, CodexRuntimeSession, CodexSandboxMode,
    ReducerConfig, Transcript,
};
use kaleido_proto::attention::{
    AttentionResponse, AttentionState, AttentionSubject, DecisionSemantics,
};
use kaleido_proto::capability::EvidenceSource;
use kaleido_proto::command::{Actor, Command, CommandEnvelope};
use kaleido_proto::content::{ContentKind, Sensitivity};
use kaleido_proto::host::{HostPlatform, LaunchSurface};
use kaleido_proto::ids::{CommandId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId};
use kaleido_proto::projection::ProjectionPayload;
use kaleido_proto::queue::{QueueIntent, QueueState};
use kaleido_proto::session::{LiveBinding, SessionStatus};
use kaleido_proto::turn::TurnOrigin;
use kaleido_state::{CanonicalStore, ClockSource, ProjectionName};

use crate::content::StoreContentAccess;
use crate::error::HostdError;

/// Base instant for frames that carry only a relative offset.
///
/// Replay must be reproducible, so the clock is a constant rather than the wall
/// clock. It sits just before the recorded window so synthetic and recorded
/// instants stay in the same range and order sensibly.
pub const REPLAY_BASE_AT_MS: i64 = 1_785_378_000_000;

/// The seven projections a Codex session can actually materialize in R3.
///
/// `WorkflowBoard` remains a valid transport/protocol key, but a Codex fixture
/// with no workflow must not invent one just to satisfy a diagnostic `all`.
pub const R3_CODEX_PROJECTIONS: [ProjectionName; 7] = [
    ProjectionName::ProjectIndex,
    ProjectionName::SessionIndex,
    ProjectionName::Transcript,
    ProjectionName::LiveActivity,
    ProjectionName::InputQueue,
    ProjectionName::AttentionInbox,
    ProjectionName::RuntimeCapability,
];

/// What to replay and where to put the result.
#[derive(Debug, Clone)]
pub struct ReplayRequest {
    pub fixture: PathBuf,
    pub log_dir: PathBuf,
    pub base_at_ms: i64,
    pub host_display_name: String,
    pub project_display_name: String,
}

impl ReplayRequest {
    pub fn new(fixture: impl Into<PathBuf>, log_dir: impl Into<PathBuf>) -> Self {
        Self {
            fixture: fixture.into(),
            log_dir: log_dir.into(),
            base_at_ms: REPLAY_BASE_AT_MS,
            host_display_name: "kaleido-host".to_owned(),
            project_display_name: "kaleido-slice".to_owned(),
        }
    }
}

/// What a replay produced.
#[derive(Debug, Clone)]
pub struct ReplayOutcome {
    pub session_id: SessionId,
    pub frames: usize,
    pub effects: usize,
    pub records: usize,
    pub probe: CapabilityProbe,
}

/// The decision the diagnostic runner should make for the first file-change
/// approval it observes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Accept,
    Decline,
}

/// Inputs for one broker-owned live Codex session.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub executable: PathBuf,
    pub project_root: PathBuf,
    pub log_dir: PathBuf,
    pub prompt: String,
    pub decide_first_approval: Option<ApprovalDecision>,
    pub enqueue_steer: Option<String>,
    pub timeout: Duration,
}

impl RunRequest {
    pub fn new(
        executable: impl Into<PathBuf>,
        project_root: impl Into<PathBuf>,
        log_dir: impl Into<PathBuf>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            executable: executable.into(),
            project_root: project_root.into(),
            log_dir: log_dir.into(),
            prompt: prompt.into(),
            decide_first_approval: None,
            enqueue_steer: None,
            timeout: Duration::from_secs(120),
        }
    }
}

/// Provider-neutral identity needed by the composition root to start a
/// concrete runtime session.
#[derive(Debug, Clone)]
pub struct RunSessionIdentity {
    pub project_id: ProjectId,
    pub project_binding_id: ProjectBindingId,
    pub runtime_id: ProviderRuntimeId,
}

/// Machine-readable evidence emitted by `slice run`.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub session_id: SessionId,
    pub report_json: String,
}

#[derive(Debug, Default)]
struct LiveEvidence {
    session_index_while_observing: Option<serde_json::Value>,
    session_index_while_controlling: Option<serde_json::Value>,
    runtime_capability_while_controlling: Option<serde_json::Value>,
    live_activity_while_streaming: Option<serde_json::Value>,
    steer_delivery_ever_observed: bool,
}

/// Replays a recorded transcript into a fresh store.
pub fn replay(request: &ReplayRequest) -> Result<ReplayOutcome, HostdError> {
    replay_into_store(request).map(|(outcome, _)| outcome)
}

/// Replays a recorded transcript and hands back the store it built.
pub fn replay_into_store(
    request: &ReplayRequest,
) -> Result<(ReplayOutcome, CanonicalStore), HostdError> {
    let raw = std::fs::read_to_string(&request.fixture)
        .map_err(|source| HostdError::io(&request.fixture, source))?;
    let transcript = parse_transcript(&raw)?;
    let mut store = CanonicalStore::open(
        &request.log_dir,
        ClockSource::Fixed {
            at_ms: request.base_at_ms,
        },
    )?;
    let mut reducer = CodexReducer::new(ReducerConfig {
        host_display_name: request.host_display_name.clone(),
        host_platform: host_platform()?,
        project_display_name: request.project_display_name.clone(),
        identity_salt: request.host_display_name.clone(),
        // A recording proves the shape of the protocol, not that anything is
        // attached now, so the session cannot claim a live binding.
        evidence: EvidenceSource::RecordedFixture,
        launch_surface: LaunchSurface::BrokerLaunched,
        turn_origin: TurnOrigin::LocalSurface,
        base_at_ms: request.base_at_ms,
        runtime_version_label: None,
    });
    let mut access = StoreContentAccess::new(store.content().clone());
    let effects = reducer.ingest(&transcript, &mut access)?;
    let records = store.apply_all(&effects)?;
    let session_id = reducer.session_id().cloned().ok_or(HostdError::NoSession)?;
    // Building the snapshot re-validates every reference the session claims to
    // resolve, so a reducer mistake fails here rather than on a reader.
    store.session_snapshot(&session_id)?;
    tracing::info!(
        target: "kaleido.slice",
        frames = transcript.len(),
        effects = effects.len(),
        records = records.len(),
        session = %session_id,
        "replayed a recorded transcript"
    );
    Ok((
        ReplayOutcome {
            session_id,
            frames: transcript.len(),
            effects: effects.len(),
            records: records.len(),
            probe: reducer.capability_probe(),
        },
        store,
    ))
}

/// Starts a real Codex app-server process and runs one diagnostic turn.
pub fn run(request: &RunRequest) -> Result<RunOutcome, HostdError> {
    let issued_at_ms = system_time_ms();
    let mint = IdentityMint::new("kaleido-host");
    let submit_command_id = mint.command_id(&format!("slice-run-submit|{issued_at_ms}"));
    let reducer = ReducerConfig {
        host_display_name: "kaleido-host".to_owned(),
        host_platform: host_platform()?,
        project_display_name: "kaleido-slice".to_owned(),
        identity_salt: "kaleido-host".to_owned(),
        evidence: EvidenceSource::ObservedInTraffic,
        launch_surface: LaunchSurface::BrokerLaunched,
        turn_origin: TurnOrigin::RemoteCommand {
            command_id: submit_command_id.clone(),
        },
        base_at_ms: issued_at_ms,
        // The CLI version check is an acceptance precondition. App-server
        // traffic in this slice does not itself prove a version label.
        runtime_version_label: None,
    };
    let sandbox = if request.decide_first_approval.is_some() {
        CodexSandboxMode::ReadOnly
    } else {
        CodexSandboxMode::WorkspaceWrite
    };
    let mut runtime = CodexRuntimeSession::new(CodexRuntimeConfig {
        executable: request.executable.clone(),
        reducer,
        sandbox,
        request_timeout: request.timeout.min(Duration::from_secs(30)),
    });
    let identity = RunSessionIdentity {
        project_id: runtime.project_id().clone(),
        project_binding_id: runtime.project_binding_id().clone(),
        runtime_id: runtime.runtime_id().clone(),
    };
    run_with_session(request, &mut runtime, identity, submit_command_id)
        .map_err(HostdError::redact_live_path)
}

/// Runs the provider-neutral broker orchestration against one runtime session.
///
/// This is public so the live composition path can be contract-tested with the
/// committed, real Codex recordings without requiring a login or network.
pub fn run_with_session(
    request: &RunRequest,
    runtime: &mut dyn ProviderRuntimeSession,
    identity: RunSessionIdentity,
    submit_command_id: CommandId,
) -> Result<RunOutcome, HostdError> {
    let mut store = CanonicalStore::open(&request.log_dir, ClockSource::System)?;
    let canonical_project_root = std::fs::canonicalize(&request.project_root)
        .map_err(|_| HostdError::ProjectRootUnavailable)?;
    let project_root = canonical_project_root.to_string_lossy();
    let project_root_ref = store.store_content(
        ContentKind::FilePath,
        Sensitivity::Sensitive,
        project_root.as_bytes(),
    )?;
    drop(project_root);
    let mut access = StoreContentAccess::new(store.content().clone());
    let start = SessionStartRequest {
        project_id: identity.project_id,
        project_binding_id: identity.project_binding_id,
        runtime_id: identity.runtime_id,
        project_root_ref,
    };
    let mut evidence = LiveEvidence::default();
    let effects = runtime.start(&start, &mut access)?;
    apply_live_effects(&mut store, &effects, &mut evidence)?;
    let session_id = only_session_id(&store)?;
    store.session_snapshot(&session_id)?;

    let prompt_ref = store.store_content(
        ContentKind::PlainText,
        Sensitivity::Sensitive,
        request.prompt.as_bytes(),
    )?;
    let prompt_envelope = CommandEnvelope {
        command_id: submit_command_id,
        idempotency_key: "slice-run-submit".to_owned(),
        actor: Actor::Broker,
        issued_at_ms: system_time_ms(),
        expires_at_ms: None,
        body: Command::SubmitPrompt {
            session_id: session_id.clone(),
            body: prompt_ref.clone(),
        },
    };
    reject_if_command_failed(store.submit_command(&prompt_envelope, system_time_ms())?)?;
    let effects = runtime.submit_prompt(&prompt_envelope.command_id, &prompt_ref, &mut access)?;
    apply_live_effects(&mut store, &effects, &mut evidence)?;
    store.session_snapshot(&session_id)?;

    if let Some(steer) = &request.enqueue_steer {
        let steer_ref = store.store_content(
            ContentKind::PlainText,
            Sensitivity::Sensitive,
            steer.as_bytes(),
        )?;
        let steer_command_id = IdentityMint::new("kaleido-host")
            .command_id(&format!("slice-run-steer|{}", system_time_ms()));
        let envelope = CommandEnvelope {
            command_id: steer_command_id,
            idempotency_key: "slice-run-steer".to_owned(),
            actor: Actor::Broker,
            issued_at_ms: system_time_ms(),
            expires_at_ms: None,
            body: Command::EnqueueInput {
                session_id: session_id.clone(),
                body: steer_ref,
                intent: QueueIntent::SteerActiveTurn,
            },
        };
        reject_if_command_failed(store.submit_command(&envelope, system_time_ms())?)?;
        // Deliberately no runtime call. EnqueueInput is broker-owned and cannot
        // become DeliveredAsSteer without an observed runtime acknowledgement.
        latch_live_evidence(&store, &session_id, &mut evidence)?;
        store.session_snapshot(&session_id)?;
    }

    let deadline = Instant::now() + request.timeout;
    let mut approval_decided = false;
    loop {
        if let Some(decision) = request.decide_first_approval {
            if !approval_decided {
                approval_decided = answer_first_approval(
                    &mut store,
                    runtime,
                    &mut access,
                    decision,
                    &mut evidence,
                )?;
            }
        }
        if turn_is_terminal(&store, &session_id) || session_is_offline(&store, &session_id) {
            break;
        }
        if Instant::now() >= deadline {
            let close_effects = runtime.close()?;
            apply_live_effects(&mut store, &close_effects, &mut evidence)?;
            store.session_snapshot(&session_id)?;
            return Err(HostdError::LiveTimeout);
        }
        let effects = runtime.drain_effects(&mut access)?;
        apply_live_effects(&mut store, &effects, &mut evidence)?;
        if effects.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let process_exited = session_is_offline(&store, &session_id);
    if !process_exited {
        let close_effects = runtime.close()?;
        apply_live_effects(&mut store, &close_effects, &mut evidence)?;
    }
    store.session_snapshot(&session_id)?;
    render_run_outcome(&store, session_id, evidence, process_exited)
}

fn apply_live_effects(
    store: &mut CanonicalStore,
    effects: &[kaleido_proto::effect::StateEffect],
    evidence: &mut LiveEvidence,
) -> Result<(), HostdError> {
    for effect in effects {
        store.apply(effect)?;
        if let Ok(session_id) = only_session_id(store) {
            // Sampling after every effect is essential: an in-progress item
            // can start and finish within one drained batch.
            latch_live_evidence(store, &session_id, evidence)?;
        }
    }
    Ok(())
}

fn latch_live_evidence(
    store: &CanonicalStore,
    session_id: &SessionId,
    evidence: &mut LiveEvidence,
) -> Result<(), HostdError> {
    if evidence.session_index_while_observing.is_none() {
        let projection = store.projection(ProjectionName::SessionIndex, Some(session_id))?;
        let observing = match &projection.payload {
            ProjectionPayload::SessionIndex { view } => view
                .active
                .iter()
                .chain(view.history.iter())
                .chain(view.archived.iter())
                .any(|summary| matches!(summary.live_binding, LiveBinding::Observing { .. })),
            _ => false,
        };
        if observing {
            evidence.session_index_while_observing = Some(serde_json::to_value(projection)?);
        }
    }
    if evidence.session_index_while_controlling.is_none() {
        let projection = store.projection(ProjectionName::SessionIndex, Some(session_id))?;
        let controlling = match &projection.payload {
            ProjectionPayload::SessionIndex { view } => view
                .active
                .iter()
                .chain(view.history.iter())
                .chain(view.archived.iter())
                .any(|summary| matches!(summary.live_binding, LiveBinding::Controlling { .. })),
            _ => false,
        };
        if controlling {
            evidence.session_index_while_controlling = Some(serde_json::to_value(projection)?);
            evidence.runtime_capability_while_controlling = Some(serde_json::to_value(
                store.projection(ProjectionName::RuntimeCapability, Some(session_id))?,
            )?);
        }
    }
    if evidence.live_activity_while_streaming.is_none() {
        let projection = store.projection(ProjectionName::LiveActivity, Some(session_id))?;
        let streaming = matches!(
            &projection.payload,
            ProjectionPayload::LiveActivity { view } if !view.streaming_item_ids.is_empty()
        );
        if streaming {
            evidence.live_activity_while_streaming = Some(serde_json::to_value(projection)?);
        }
    }
    let queue = store.projection(ProjectionName::InputQueue, Some(session_id))?;
    if let ProjectionPayload::InputQueue { view } = &queue.payload {
        evidence.steer_delivery_ever_observed |= view
            .entries
            .iter()
            .any(|entry| matches!(entry.state, QueueState::DeliveredAsSteer { .. }));
    }
    Ok(())
}

fn answer_first_approval(
    store: &mut CanonicalStore,
    runtime: &mut dyn ProviderRuntimeSession,
    access: &mut StoreContentAccess,
    decision: ApprovalDecision,
    evidence: &mut LiveEvidence,
) -> Result<bool, HostdError> {
    let entry = store
        .state()
        .attention_entries()
        .into_iter()
        .find(|entry| {
            entry.state == AttentionState::Open
                && matches!(entry.subject, AttentionSubject::Approval { .. })
        })
        .cloned();
    let Some(entry) = entry else {
        return Ok(false);
    };
    let option_id = entry
        .options()
        .iter()
        .find(|option| match decision {
            ApprovalDecision::Accept => matches!(
                option.semantics,
                DecisionSemantics::Allow | DecisionSemantics::AllowAlways
            ),
            ApprovalDecision::Decline => matches!(
                option.semantics,
                DecisionSemantics::Deny | DecisionSemantics::DenyAlways | DecisionSemantics::Cancel
            ),
        })
        .map(|option| option.option_id.clone())
        .ok_or(HostdError::ApprovalOptionUnavailable)?;
    let response = AttentionResponse {
        attention_id: entry.id.clone(),
        session_id: entry.session_id.clone(),
        request_key: entry
            .request_key()
            .ok_or(HostdError::ApprovalOptionUnavailable)?
            .to_owned(),
        expected_expires_at_ms: entry.expires_at_ms,
        option_id: Some(option_id),
        free_form_ref: None,
        question_answers: Vec::new(),
    };
    let command_id = IdentityMint::new("kaleido-host")
        .command_id(&format!("slice-run-attention|{}", system_time_ms()));
    let envelope = CommandEnvelope {
        command_id,
        idempotency_key: "slice-run-attention".to_owned(),
        actor: Actor::Broker,
        issued_at_ms: system_time_ms(),
        expires_at_ms: entry.expires_at_ms,
        body: Command::RespondAttention {
            response: response.clone(),
        },
    };
    reject_if_command_failed(store.submit_command(&envelope, system_time_ms())?)?;
    latch_live_evidence(
        store,
        &entry.session_id.ok_or(HostdError::NoSession)?,
        evidence,
    )?;
    if let Some(session_id) = &response.session_id {
        store.session_snapshot(session_id)?;
    }
    let effects = runtime.respond_attention(&envelope.command_id, &response, access)?;
    apply_live_effects(store, &effects, evidence)?;
    Ok(true)
}

fn reject_if_command_failed(ack: kaleido_proto::command::CommandAck) -> Result<(), HostdError> {
    if ack.outcome.is_rejection() {
        return Err(HostdError::CommandRejected);
    }
    Ok(())
}

fn only_session_id(store: &CanonicalStore) -> Result<SessionId, HostdError> {
    let mut sessions = store.state().sessions();
    match (sessions.next(), sessions.next()) {
        (Some(session), None) => Ok(session.id.clone()),
        _ => Err(HostdError::NoSession),
    }
}

fn turn_is_terminal(store: &CanonicalStore, session_id: &SessionId) -> bool {
    let turns = store.state().turns_of(session_id);
    !turns.is_empty() && turns.iter().all(|turn| turn.status.is_terminal())
}

fn session_is_offline(store: &CanonicalStore, session_id: &SessionId) -> bool {
    store
        .state()
        .session(session_id)
        .is_some_and(|session| session.status == SessionStatus::Offline)
}

fn render_run_outcome(
    store: &CanonicalStore,
    session_id: SessionId,
    evidence: LiveEvidence,
    process_exited: bool,
) -> Result<RunOutcome, HostdError> {
    let mut projections = serde_json::Map::new();
    for name in R3_CODEX_PROJECTIONS {
        projections.insert(
            name.as_str().to_owned(),
            serde_json::to_value(store.projection(name, Some(&session_id))?)?,
        );
    }
    let termination = if process_exited {
        "process_exited"
    } else {
        "turn_terminal"
    };
    let report = serde_json::json!({
        "session_id": session_id,
        "termination": termination,
        "observed": {
            "session_index_while_observing": evidence.session_index_while_observing,
            "session_index_while_controlling": evidence.session_index_while_controlling,
            "runtime_capability_while_controlling": evidence.runtime_capability_while_controlling,
            "live_activity_while_streaming": evidence.live_activity_while_streaming,
            "steer_delivery_ever_observed": evidence.steer_delivery_ever_observed,
        },
        "projections": projections,
    });
    tracing::info!(
        target: "kaleido.slice",
        session = %session_id,
        termination,
        observed_live_binding = evidence.session_index_while_observing.is_some(),
        observed_controlling_binding = evidence.session_index_while_controlling.is_some(),
        observed_streaming_item = evidence.live_activity_while_streaming.is_some(),
        "completed a live diagnostic session"
    );
    Ok(RunOutcome {
        session_id,
        report_json: serde_json::to_string_pretty(&report)?,
    })
}

fn system_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Rebuilds a store from its log and renders one read model.
pub fn show(
    log_dir: &Path,
    projection: ProjectionName,
    session_id: Option<&SessionId>,
) -> Result<String, HostdError> {
    let store = CanonicalStore::load(
        log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS,
        },
    )?;
    let envelope = store.projection(projection, session_id)?;
    Ok(serde_json::to_string_pretty(&envelope)?)
}

/// Rebuilds a store from its log and renders every read model this slice owns.
pub fn show_all(log_dir: &Path, session_id: Option<&SessionId>) -> Result<String, HostdError> {
    let store = CanonicalStore::load(
        log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS,
        },
    )?;
    let mut rendered = serde_json::Map::new();
    for name in R3_CODEX_PROJECTIONS {
        let envelope = store.projection(name, session_id)?;
        rendered.insert(name.as_str().to_owned(), serde_json::to_value(&envelope)?);
    }
    Ok(serde_json::to_string_pretty(&rendered)?)
}

/// Loads a transcript without reducing it, for callers that only need its size.
pub fn read_transcript(fixture: &Path) -> Result<Transcript, HostdError> {
    let raw = std::fs::read_to_string(fixture).map_err(|source| HostdError::io(fixture, source))?;
    Ok(parse_transcript(&raw)?)
}

fn host_platform() -> Result<HostPlatform, HostdError> {
    crate::platform::host_platform().ok_or(HostdError::UnsupportedHostPlatform)
}
