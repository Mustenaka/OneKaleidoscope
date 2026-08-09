#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Store behaviour that needs constructed canonical inputs.
//!
//! Reduction from real recordings is exercised where the recordings are, in the
//! adapter and the host daemon. What is left here are the rules a provider
//! cannot demonstrate on its own: command idempotency, and refusing a reply to
//! an approval that is already decided or already expired.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kaleido_proto::attention::{
    ApprovalRequest, AttentionAnswerEvidence, AttentionAnswerEvidenceSource, AttentionAnswerSource,
    AttentionItem, AttentionResponse, AttentionState, AttentionSubject, DecisionOption,
    DecisionSemantics, JoinState,
};
use kaleido_proto::capability::{
    Capability, CapabilityEntry, CapabilityEvidence, CapabilityState, EvidenceSource,
    RuntimeCapabilities,
};
use kaleido_proto::command::{
    Actor, Command, CommandAck, CommandEnvelope, CommandOutcome, DeviceCommandRequest,
};
use kaleido_proto::content::{
    ContentKind, ContentReadRequest, ContentReadResponse, ContentRef, ContentUnavailableReason,
    ContentWriteRequest, ContentWriteResponse, Sensitivity,
};
use kaleido_proto::effect::{DiagnosticCode, DiagnosticRecord, StateEffect, StreamKey};
use kaleido_proto::error::ErrorCode;
use kaleido_proto::host::{
    ConnectionState, Host, HostPlatform, HostReachability, LaunchSurface, Project, ProjectBinding,
    ProviderFamily, ProviderRuntime, SessionCounts,
};
use kaleido_proto::ids::{
    ArtifactId, AttentionId, CommandId, DeviceId, HostId, ItemId, ProjectBindingId, ProjectId,
    ProviderBindingHandle, ProviderBindingId, ProviderBindingKind, ProviderRuntimeId, QueueEntryId,
    SessionId, StepId, TurnId, WorkflowId,
};
use kaleido_proto::projection::{
    validate_projection_sequence, ProjectionKey, ProjectionPayload, ProjectionSubscribe,
    ProjectionSubscribeOutcome,
};
use kaleido_proto::queue::{QueueEntry, QueueIntent, QueueState};
use kaleido_proto::session::{
    HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason, OwnershipMode, Session,
    SessionStatus,
};
use kaleido_proto::turn::{Item, ItemBody, ItemStatus, MessagePhase, Turn, TurnOrigin, TurnStatus};
use kaleido_proto::workflow::{
    Artifact, ArtifactKind, CompletionCondition, RuntimeSelector, Step, StepAssignment, StepRole,
    StepState, Workflow, WorkflowState,
};
use kaleido_proto::ContractViolation;
use kaleido_state::{CanonicalStore, ClockSource, ProjectionName, StateError};
use sha2::{Digest, Sha256};

const NOW_MS: i64 = 1_785_378_000_000;

struct Fixture {
    _directory: tempfile::TempDir,
    store: CanonicalStore,
    session_id: SessionId,
    attention_id: AttentionId,
    request_key: String,
    body: ContentRef,
}

fn directory_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(directory).expect("read snapshot directory") {
            let path = entry.expect("snapshot entry").path();
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("snapshot path belongs to root")
                    .to_path_buf();
                files.insert(relative, std::fs::read(&path).expect("snapshot file"));
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

fn restore_directory_snapshot(root: &Path, snapshot: &BTreeMap<PathBuf, Vec<u8>>) {
    let current = directory_snapshot(root);
    for relative in current.keys() {
        if !snapshot.contains_key(relative) {
            std::fs::remove_file(root.join(relative)).expect("remove post-snapshot file");
        }
    }
    for (relative, bytes) in snapshot {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("restore parent directory");
        }
        std::fs::write(path, bytes).expect("restore snapshot file");
    }
}

fn handle(kind: ProviderBindingKind, suffix: &str) -> ProviderBindingHandle {
    handle_for_runtime(kind, suffix, ProviderRuntimeId::new("rtm_0123456789abcdef"))
}

fn handle_for_runtime(
    kind: ProviderBindingKind,
    suffix: &str,
    runtime_id: ProviderRuntimeId,
) -> ProviderBindingHandle {
    ProviderBindingHandle {
        id: ProviderBindingId::new(format!("bnd_{suffix}")),
        runtime_id,
        kind,
    }
}

fn capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        negotiated_at_ms: NOW_MS,
        entries: vec![CapabilityEntry {
            capability: Capability::TurnPrompt,
            state: CapabilityState::Supported,
            evidence: CapabilityEvidence {
                source: EvidenceSource::ObservedInTraffic,
                observed_at_ms: NOW_MS,
                note_ref: None,
            },
        }],
    }
}

fn live_capabilities(include_control: bool) -> RuntimeCapabilities {
    let evidence = CapabilityEvidence {
        source: EvidenceSource::ObservedInTraffic,
        observed_at_ms: NOW_MS + 1,
        note_ref: None,
    };
    let mut entries = vec![CapabilityEntry {
        capability: Capability::LiveObserve,
        state: CapabilityState::Supported,
        evidence: evidence.clone(),
    }];
    if include_control {
        entries.push(CapabilityEntry {
            capability: Capability::LiveControl,
            state: CapabilityState::Supported,
            evidence,
        });
    }
    RuntimeCapabilities {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        negotiated_at_ms: NOW_MS + 1,
        entries,
    }
}

fn runtime_ack(command_id: &CommandId) -> CommandAck {
    CommandAck {
        command_id: command_id.clone(),
        outcome: CommandOutcome::AcceptedByRuntime {
            binding_handle: handle(
                ProviderBindingKind::RuntimeAcknowledgement,
                "0000000000000005",
            ),
        },
        acked_at_ms: NOW_MS + 2,
    }
}

fn scaffold(expires_at_ms: Option<i64>) -> Fixture {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut store = CanonicalStore::open(directory.path(), ClockSource::Fixed { at_ms: NOW_MS })
        .expect("open store");

    let sensitive = |store: &CanonicalStore, kind: ContentKind, text: &str| {
        store
            .store_content(kind, Sensitivity::Sensitive, text.as_bytes())
            .expect("store body")
    };
    let root_ref = sensitive(&store, ContentKind::FilePath, "/projects/slice");
    let summary_ref = sensitive(&store, ContentKind::StructuredSummary, "file edit");
    let message_ref = sensitive(&store, ContentKind::Markdown, "hello");
    let body = sensitive(&store, ContentKind::PlainText, "please also mention DONE");

    let host_id = HostId::new("hst_0123456789abcdef");
    let runtime_id = ProviderRuntimeId::new("rtm_0123456789abcdef");
    let project_id = ProjectId::new("prj_0123456789abcdef");
    let project_binding_id = ProjectBindingId::new("pbd_0123456789abcdef");
    let session_id = SessionId::new("ses_0123456789abcdef");
    let turn_id = TurnId::new("trn_0123456789abcdef");
    let item_id = ItemId::new("itm_0123456789abcdef");
    let attention_id = AttentionId::new("atn_0123456789abcdef");
    let request_key = "req_0123456789abcdef".to_owned();

    let effects = vec![
        StateEffect::HostUpserted {
            host: Host {
                id: host_id.clone(),
                display_name: "host".to_owned(),
                platform: HostPlatform::Windows,
                reachability: HostReachability::LanDirect,
                protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
                last_seen_at_ms: NOW_MS,
            },
        },
        StateEffect::RuntimeUpserted {
            runtime: ProviderRuntime {
                id: runtime_id.clone(),
                host_id: host_id.clone(),
                family: ProviderFamily::Codex,
                version_label: None,
                launch_surface: LaunchSurface::BrokerLaunched,
                connection: ConnectionState::Connected {
                    since_at_ms: NOW_MS,
                },
                capabilities: capabilities(),
                binding_handle: None,
            },
        },
        StateEffect::ProjectUpserted {
            project: Project {
                id: project_id.clone(),
                display_name: "slice".to_owned(),
                bindings: vec![ProjectBinding {
                    id: project_binding_id.clone(),
                    project_id: project_id.clone(),
                    runtime_id: runtime_id.clone(),
                    root_ref,
                }],
                session_counts: SessionCounts::default(),
                workflow_count: 0,
                attention_count: 0,
                last_activity_at_ms: NOW_MS,
            },
        },
        StateEffect::SessionUpserted {
            session: kaleido_proto::session::Session {
                id: session_id.clone(),
                project_id: project_id.clone(),
                project_binding_id,
                ownership: OwnershipMode::BrokerManaged,
                history_source: HistorySource {
                    kind: HistorySourceKind::BrokerLog,
                    runtime_id: Some(runtime_id.clone()),
                    evidence: CapabilityEvidence {
                        source: EvidenceSource::RecordedFixture,
                        observed_at_ms: NOW_MS,
                        note_ref: None,
                    },
                },
                live_binding: LiveBinding::NotBound {
                    reason: LiveUnboundReason::NeverStarted,
                },
                status: SessionStatus::Idle,
                title: None,
                created_at_ms: NOW_MS,
                updated_at_ms: NOW_MS,
                last_activity_at_ms: NOW_MS,
                active_turn_id: None,
                queue_depth: 0,
                open_attention_count: 0,
                archived: false,
                binding_handle: Some(handle(ProviderBindingKind::Session, "0000000000000001")),
            },
        },
        StateEffect::TurnUpserted {
            turn: Turn {
                id: turn_id.clone(),
                session_id: session_id.clone(),
                status: TurnStatus::Completed,
                origin: TurnOrigin::LocalSurface,
                started_at_ms: Some(NOW_MS),
                completed_at_ms: Some(NOW_MS),
                item_ids: Vec::new(),
                error: None,
                binding_handle: Some(handle(ProviderBindingKind::Turn, "0000000000000002")),
            },
        },
        StateEffect::ItemUpserted {
            item: Item {
                id: item_id.clone(),
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                sequence: 0,
                status: ItemStatus::Completed,
                body: ItemBody::AgentMessage {
                    content: message_ref,
                    phase: MessagePhase::FinalAnswer,
                },
                created_at_ms: NOW_MS,
                updated_at_ms: NOW_MS,
                binding_handle: Some(handle(ProviderBindingKind::Item, "0000000000000003")),
            },
        },
        StateEffect::AttentionUpserted {
            item: AttentionItem {
                id: attention_id.clone(),
                host_id,
                project_id,
                session_id: Some(session_id.clone()),
                turn_id: Some(turn_id),
                workflow_id: None,
                subject: AttentionSubject::Approval {
                    request: ApprovalRequest {
                        request_key: request_key.clone(),
                        target_item_id: item_id.clone(),
                        join: JoinState::Joined { item_id },
                        options: vec![
                            DecisionOption {
                                option_id: "accept".to_owned(),
                                label: "accept".to_owned(),
                                semantics: DecisionSemantics::Allow,
                            },
                            DecisionOption {
                                option_id: "decline".to_owned(),
                                label: "decline".to_owned(),
                                semantics: DecisionSemantics::Deny,
                            },
                        ],
                        summary_ref,
                        detail_ref: None,
                        binding_handle: handle(
                            ProviderBindingKind::InteractionRequest,
                            "0000000000000004",
                        ),
                    },
                },
                state: AttentionState::Open,
                created_at_ms: NOW_MS,
                expires_at_ms,
            },
        },
    ];
    store.apply_all(&effects).expect("apply scaffold");

    Fixture {
        _directory: directory,
        store,
        session_id,
        attention_id,
        request_key,
        body,
    }
}

fn reply(
    fixture: &Fixture,
    command: &str,
    option: &str,
    expires_at_ms: Option<i64>,
) -> CommandEnvelope {
    CommandEnvelope {
        command_id: CommandId::new(format!("cmd_{command}")),
        idempotency_key: command.to_owned(),
        actor: Actor::Human {
            device_id: DeviceId::new("device-test"),
        },
        issued_at_ms: NOW_MS,
        expires_at_ms: None,
        body: Command::RespondAttention {
            response: AttentionResponse {
                attention_id: fixture.attention_id.clone(),
                session_id: Some(fixture.session_id.clone()),
                request_key: fixture.request_key.clone(),
                expected_expires_at_ms: expires_at_ms,
                option_id: Some(option.to_owned()),
                free_form_ref: None,
            },
        },
    }
}

fn prompt(fixture: &Fixture, command: &str) -> CommandEnvelope {
    CommandEnvelope {
        command_id: CommandId::new(format!("cmd_{command}")),
        idempotency_key: command.to_owned(),
        actor: Actor::Human {
            device_id: DeviceId::new("device-test"),
        },
        issued_at_ms: NOW_MS,
        expires_at_ms: None,
        body: Command::SubmitPrompt {
            session_id: fixture.session_id.clone(),
            body: fixture.body.clone(),
        },
    }
}

fn remote_turn(session_id: &SessionId, command_id: &CommandId, suffix: &str) -> Turn {
    Turn {
        id: TurnId::new(format!("trn_remote_{suffix}")),
        session_id: session_id.clone(),
        status: TurnStatus::Running,
        origin: TurnOrigin::RemoteCommand {
            command_id: command_id.clone(),
        },
        started_at_ms: Some(NOW_MS + 1),
        completed_at_ms: None,
        item_ids: Vec::new(),
        error: None,
        binding_handle: Some(handle(
            ProviderBindingKind::Turn,
            &format!("remote_{suffix}"),
        )),
    }
}

fn controlling(session: &Session) -> Session {
    let mut controlling = session.clone();
    controlling.live_binding = LiveBinding::Controlling {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        since_at_ms: NOW_MS,
        evidence: CapabilityEvidence {
            source: EvidenceSource::ObservedInTraffic,
            observed_at_ms: NOW_MS + 2,
            note_ref: None,
        },
    };
    controlling
}

fn second_session(fixture: &Fixture) -> Session {
    let mut session = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("first session exists")
        .clone();
    session.id = SessionId::new("ses_second_session");
    session.binding_handle = Some(handle(ProviderBindingKind::Session, "0000000000000006"));
    session
}

#[test]
fn a_repeated_command_is_reported_as_duplicate_and_appends_nothing() {
    let mut fixture = scaffold(None);
    let envelope = reply(&fixture, "first", "accept", None);
    let first = fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("first submission");
    assert!(matches!(
        first.outcome,
        CommandOutcome::AcceptedLocally { .. }
    ));

    let records_before = fixture.store.log().read_all().expect("read log").len();
    let repeat = CommandEnvelope {
        command_id: CommandId::new("cmd_second"),
        ..envelope
    };
    let second = fixture
        .store
        .submit_command(&repeat, NOW_MS)
        .expect("repeat submission");
    match second.outcome {
        CommandOutcome::Duplicate {
            original_command_id,
        } => assert_eq!(original_command_id, CommandId::new("cmd_first")),
        other => panic!("expected a duplicate, found {other:?}"),
    }
    let records_after = fixture.store.log().read_all().expect("read log").len();
    assert_eq!(
        records_before, records_after,
        "a duplicate must not append a record"
    );
}

#[test]
fn idempotency_survives_a_reload() {
    let fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let envelope = reply(&fixture, "first", "accept", None);
    let mut store = fixture.store;
    store
        .submit_command(&envelope, NOW_MS)
        .expect("first submission");
    drop(store);

    let mut reloaded =
        CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS }).expect("reload store");
    let repeat = CommandEnvelope {
        command_id: CommandId::new("cmd_second"),
        ..envelope
    };
    assert!(matches!(
        reloaded
            .submit_command(&repeat, NOW_MS)
            .expect("repeat after reload")
            .outcome,
        CommandOutcome::Duplicate { .. }
    ));
}

#[test]
fn zero_two_idempotency_table_fails_loud_instead_of_reexecuting() {
    let fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let envelope = reply(&fixture, "legacy-idempotency", "accept", None);
    let mut store = fixture.store;
    store
        .submit_command(&envelope, NOW_MS)
        .expect("first submission");
    drop(store);

    let path = root.join("idempotency.jsonl");
    let current = std::fs::read_to_string(&path).expect("read versioned idempotency table");
    let record: serde_json::Value =
        serde_json::from_str(current.trim()).expect("parse current idempotency record");
    let key_digest = record
        .get("key_digest")
        .and_then(serde_json::Value::as_str)
        .expect("key digest");
    let command_id = record
        .get("command_id")
        .and_then(|value| value.get("value"))
        .and_then(serde_json::Value::as_str)
        .expect("command identifier");
    let legacy = format!("{} {}\n", key_digest, command_id);
    std::fs::write(&path, legacy).expect("write v0.2 idempotency shape");

    assert!(matches!(
        CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS }),
        Err(StateError::MalformedRecord { line: 1, .. })
    ));
}

#[test]
fn answering_an_already_answered_approval_is_refused() {
    let mut fixture = scaffold(None);
    let first = reply(&fixture, "first", "accept", None);
    assert!(matches!(
        fixture
            .store
            .submit_command(&first, NOW_MS)
            .expect("first decision")
            .outcome,
        CommandOutcome::AcceptedLocally { .. }
    ));
    // A different command, so idempotency does not mask the refusal.
    let second = reply(&fixture, "second", "decline", None);
    match fixture
        .store
        .submit_command(&second, NOW_MS)
        .expect("second decision")
        .outcome
    {
        CommandOutcome::Rejected { error } => {
            assert_eq!(error.code, ErrorCode::ApprovalAlreadyAnswered);
        }
        other => panic!("expected a rejection, found {other:?}"),
    }
}

#[test]
fn a_local_answer_references_the_real_envelope_command() {
    let mut fixture = scaffold(None);
    let envelope = reply(&fixture, "real-envelope", "accept", None);
    fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("submit local answer");

    let answered = fixture
        .store
        .state()
        .attention(&fixture.attention_id)
        .expect("answered attention");
    match &answered.state {
        AttentionState::Answered {
            answer_source: AttentionAnswerSource::LocalCommand { command_id },
            ..
        } => assert_eq!(command_id, &envelope.command_id),
        other => panic!("expected a local-command answer, found {other:?}"),
    }
}

#[test]
fn a_local_reply_after_an_external_answer_is_already_answered() {
    let mut fixture = scaffold(None);
    let mut externally_answered = fixture
        .store
        .state()
        .attention(&fixture.attention_id)
        .cloned()
        .expect("open attention");
    externally_answered.state = AttentionState::Answered {
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW_MS,
        answer_source: AttentionAnswerSource::ObservedExternal {
            evidence: AttentionAnswerEvidence {
                observer_host_id: externally_answered.host_id.clone(),
                observed_at_ms: NOW_MS,
                source: AttentionAnswerEvidenceSource::ObservedInTraffic,
            },
        },
    };
    fixture
        .store
        .apply(&StateEffect::AttentionUpserted {
            item: externally_answered,
        })
        .expect("apply external observation");

    let envelope = reply(&fixture, "after-external", "decline", None);
    match fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("submit after external answer")
        .outcome
    {
        CommandOutcome::Rejected { error } => {
            assert_eq!(error.code, ErrorCode::ApprovalAlreadyAnswered);
        }
        other => panic!("expected an already-answered rejection, found {other:?}"),
    }
    assert!(matches!(
        fixture
            .store
            .state()
            .attention(&fixture.attention_id)
            .map(|item| &item.state),
        Some(AttentionState::Answered {
            answer_source: AttentionAnswerSource::ObservedExternal { .. },
            ..
        })
    ));
}

#[test]
fn a_zero_one_answered_log_fails_loud_without_migration() {
    let mut fixture = scaffold(None);
    let envelope = reply(&fixture, "legacy-shape", "accept", None);
    fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("write a current answer");

    let stream = StreamKey::Session {
        session_id: fixture.session_id.clone(),
    };
    let path = fixture.store.log().path_for(&stream);
    let contents = std::fs::read_to_string(&path).expect("read session log");
    let mut replaced = false;
    let mut legacy_lines = Vec::new();
    for line in contents.lines() {
        let mut value: serde_json::Value =
            serde_json::from_str(line).expect("parse current record");
        if value
            .pointer("/effect/kind")
            .and_then(serde_json::Value::as_str)
            == Some("attention_upserted")
            && value
                .pointer("/effect/item/state/kind")
                .and_then(serde_json::Value::as_str)
                == Some("answered")
        {
            let state = value
                .pointer_mut("/effect/item/state")
                .and_then(serde_json::Value::as_object_mut)
                .expect("answered state object");
            let answer_source = state
                .remove("answer_source")
                .expect("current answer source");
            let command_id = answer_source
                .get("command_id")
                .cloned()
                .expect("local command id");
            state.insert("command_id".to_owned(), command_id);
            replaced = true;
        }
        legacy_lines.push(serde_json::to_string(&value).expect("encode legacy record"));
    }
    assert!(replaced, "the test must rewrite an Answered record");
    let mut legacy_log = legacy_lines.join("\n");
    legacy_log.push('\n');
    std::fs::write(&path, legacy_log).expect("write legacy-shaped log");

    let root = fixture.store.root().to_path_buf();
    drop(fixture.store);
    assert!(matches!(
        CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS }),
        Err(StateError::MalformedRecord { .. })
    ));
}

#[test]
fn answering_an_expired_approval_is_refused() {
    let expiry = NOW_MS + 1_000;
    let mut fixture = scaffold(Some(expiry));
    let envelope = reply(&fixture, "late", "accept", Some(expiry));
    match fixture
        .store
        .submit_command(&envelope, expiry + 1)
        .expect("late decision")
        .outcome
    {
        CommandOutcome::Rejected { error } => assert_eq!(error.code, ErrorCode::ApprovalExpired),
        other => panic!("expected a rejection, found {other:?}"),
    }
}

#[test]
fn a_steering_intent_is_queued_rather_than_injected() {
    // Rule R-P9: the runtime here has never proved steering, so the entry has
    // to stay pending. There is no way to express anything else — a delivered
    // steer needs an acknowledgement the contract will not let us fabricate.
    let mut fixture = scaffold(None);
    let envelope = CommandEnvelope {
        command_id: CommandId::new("cmd_steer"),
        idempotency_key: "steer".to_owned(),
        actor: Actor::Human {
            device_id: DeviceId::new("device-test"),
        },
        issued_at_ms: NOW_MS,
        expires_at_ms: None,
        body: Command::EnqueueInput {
            session_id: fixture.session_id.clone(),
            body: fixture.body.clone(),
            intent: QueueIntent::SteerActiveTurn,
        },
    };
    let ack = fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("enqueue");
    assert!(matches!(ack.outcome, CommandOutcome::Enqueued { .. }));
    assert!(!ack.outcome.reached_runtime());

    let entries = fixture.store.state().queue_of(&fixture.session_id);
    let entry = entries.first().expect("the queue holds the entry");
    assert_eq!(entry.state, QueueState::Pending);
    assert!(entry.editable);
    assert!(!entry.state.reached_runtime());

    let view = fixture
        .store
        .projection(
            kaleido_state::ProjectionName::InputQueue,
            Some(&fixture.session_id),
        )
        .expect("input queue projection");
    let encoded = serde_json::to_string(&view).expect("render projection");
    assert!(encoded.contains("\"steer_supported\":false"));
    assert!(!encoded.contains("delivered_as_steer"));
}

#[test]
fn an_expired_command_is_rejected_rather_than_executed() {
    let mut fixture = scaffold(None);
    let mut envelope = reply(&fixture, "stale", "accept", None);
    envelope.expires_at_ms = Some(NOW_MS);
    match fixture
        .store
        .submit_command(&envelope, NOW_MS + 1)
        .expect("stale command")
        .outcome
    {
        CommandOutcome::Rejected { error } => assert_eq!(error.code, ErrorCode::CommandExpired),
        other => panic!("expected a rejection, found {other:?}"),
    }
    assert!(!fixture
        .store
        .state()
        .attention_is_answered(&fixture.attention_id));
}

#[test]
fn runtime_acceptance_requires_local_correlation_and_survives_reload() {
    let mut fixture = scaffold(None);
    let envelope = prompt(&fixture, "control");
    let local_ack = fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("record local acceptance");
    assert!(matches!(
        local_ack.outcome,
        CommandOutcome::AcceptedLocally { .. }
    ));
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: remote_turn(&fixture.session_id, &envelope.command_id, "control"),
        })
        .expect("record the correlated remote-command turn");

    fixture
        .store
        .apply(&StateEffect::CommandAcknowledged {
            ack: runtime_ack(&envelope.command_id),
        })
        .expect("record correlated runtime acceptance");
    fixture
        .store
        .apply(&StateEffect::CapabilitiesUpdated {
            capabilities: live_capabilities(true),
        })
        .expect("publish live capabilities before controlling binding");

    let mut session = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("session exists")
        .clone();
    session.live_binding = LiveBinding::Controlling {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        since_at_ms: NOW_MS,
        evidence: CapabilityEvidence {
            source: EvidenceSource::ObservedInTraffic,
            observed_at_ms: NOW_MS + 2,
            note_ref: None,
        },
    };
    fixture
        .store
        .apply(&StateEffect::SessionUpserted { session })
        .expect("write controlling binding with both live capabilities");

    let root = fixture.store.root().to_path_buf();
    drop(fixture.store);
    let reloaded =
        CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS }).expect("reload store");
    assert_eq!(reloaded.state().acknowledgements().len(), 2);
    assert!(matches!(
        reloaded
            .state()
            .acknowledgements()
            .get(1)
            .expect("runtime acknowledgement follows local acceptance")
            .outcome,
        CommandOutcome::AcceptedByRuntime { .. }
    ));

    let projection = reloaded
        .projection(ProjectionName::SessionIndex, Some(&fixture.session_id))
        .expect("session index projection");
    let ProjectionPayload::SessionIndex { view } = projection.payload else {
        panic!("expected session index projection");
    };
    let summary = view
        .active
        .iter()
        .chain(&view.history)
        .chain(&view.archived)
        .find(|summary| summary.session_id == fixture.session_id)
        .expect("session summary");
    assert!(matches!(
        summary.live_binding,
        LiveBinding::Controlling { .. }
    ));

    let capabilities = reloaded
        .projection(ProjectionName::RuntimeCapability, Some(&fixture.session_id))
        .expect("runtime capability projection");
    let ProjectionPayload::RuntimeCapability { view } = capabilities.payload else {
        panic!("expected runtime capability projection");
    };
    assert!(view
        .entries
        .iter()
        .any(|entry| entry.capability == Capability::LiveControl
            && entry.state == CapabilityState::Supported));
}

#[test]
fn uncorrelated_runtime_acceptance_is_rejected_without_append() {
    let mut fixture = scaffold(None);
    let records_before = fixture.store.log().read_all().expect("read log").len();
    let acknowledgements_before = fixture.store.state().acknowledgements().len();

    let error = fixture
        .store
        .apply(&StateEffect::CommandAcknowledged {
            ack: runtime_ack(&CommandId::new("cmd_without_local_acceptance")),
        })
        .expect_err("uncorrelated runtime acceptance must fail");
    assert!(matches!(
        error,
        StateError::UncorrelatedRuntimeAcknowledgement
    ));
    assert_eq!(
        fixture.store.state().acknowledgements().len(),
        acknowledgements_before
    );
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before,
        "a refused runtime acknowledgement must not be durable"
    );
}

#[test]
fn public_effect_ingestion_cannot_forge_local_acceptance() {
    let mut fixture = scaffold(None);
    let fake = StateEffect::CommandAcknowledged {
        ack: CommandAck {
            command_id: CommandId::new("cmd_forged_local"),
            outcome: CommandOutcome::AcceptedLocally { note_ref: None },
            acked_at_ms: NOW_MS + 1,
        },
    };
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let direct = fixture
        .store
        .apply(&fake)
        .expect_err("public apply must reject forged local acceptance");
    assert!(matches!(direct, StateError::UntrustedLocalAcknowledgement));
    let batched = fixture
        .store
        .apply_all(std::slice::from_ref(&fake))
        .expect_err("public apply_all must reject forged local acceptance");
    assert!(matches!(batched, StateError::UntrustedLocalAcknowledgement));
    assert!(fixture.store.state().acknowledgements().is_empty());
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before,
        "neither public ingestion path may append a local acceptance"
    );
}

#[test]
fn runtime_acceptance_without_a_remote_command_turn_is_rejected() {
    let mut fixture = scaffold(None);
    let envelope = prompt(&fixture, "missing_turn");
    fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("record local acceptance");
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let error = fixture
        .store
        .apply(&StateEffect::CommandAcknowledged {
            ack: runtime_ack(&envelope.command_id),
        })
        .expect_err("runtime acceptance needs an observed remote-command turn");
    assert!(matches!(
        error,
        StateError::RuntimeAcknowledgementWithoutRemoteTurn
    ));
    assert_eq!(fixture.store.state().acknowledgements().len(), 1);
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before
    );
}

#[test]
fn a_remote_command_id_cannot_be_reused_by_another_turn() {
    let mut fixture = scaffold(None);
    let envelope = prompt(&fixture, "ambiguous_turn");
    fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("record local acceptance");
    let second_session = second_session(&fixture);
    fixture
        .store
        .apply(&StateEffect::SessionUpserted {
            session: second_session.clone(),
        })
        .expect("record second session");
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: remote_turn(&fixture.session_id, &envelope.command_id, "unique"),
        })
        .expect("record the first remote-command turn");
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let error = fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: remote_turn(&second_session.id, &envelope.command_id, "conflict"),
        })
        .expect_err("one command id cannot be rebound to another turn");
    assert!(matches!(error, StateError::RemoteCommandTurnConflict));
    assert_eq!(fixture.store.state().acknowledgements().len(), 1);
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before
    );

    let root = fixture.store.root().to_path_buf();
    drop(fixture.store);
    let mut reloaded =
        CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS }).expect("reload store");
    let remote_turns = reloaded
        .state()
        .turns_of(&fixture.session_id)
        .into_iter()
        .filter(|turn| {
            matches!(
                &turn.origin,
                TurnOrigin::RemoteCommand { command_id }
                    if command_id == &envelope.command_id
            )
        })
        .count();
    assert_eq!(remote_turns, 1);
    reloaded
        .apply(&StateEffect::CommandAcknowledged {
            ack: runtime_ack(&envelope.command_id),
        })
        .expect("the surviving unique turn still correlates after reload");
}

#[test]
fn an_existing_turn_cannot_rewrite_its_identity() {
    let mut fixture = scaffold(None);
    let second_session = second_session(&fixture);
    fixture
        .store
        .apply(&StateEffect::SessionUpserted {
            session: second_session.clone(),
        })
        .expect("record second session");
    let command_id = CommandId::new("cmd_stable_turn_identity");
    let original = remote_turn(&fixture.session_id, &command_id, "stable_identity");
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: original.clone(),
        })
        .expect("record original turn identity");
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let mut changed_origin = original.clone();
    changed_origin.origin = TurnOrigin::LocalSurface;
    let origin_error = fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: changed_origin,
        })
        .expect_err("turn origin must be stable");
    assert!(matches!(origin_error, StateError::TurnOriginChanged));

    let mut changed_session = original.clone();
    changed_session.session_id = second_session.id;
    let session_error = fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: changed_session,
        })
        .expect_err("turn session must be stable");
    assert!(matches!(session_error, StateError::TurnSessionChanged));

    let mut changed_binding = original.clone();
    changed_binding.binding_handle =
        Some(handle(ProviderBindingKind::Turn, "different_turn_binding"));
    let binding_error = fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: changed_binding,
        })
        .expect_err("turn binding must not cross provider identities");
    assert!(matches!(binding_error, StateError::TurnBindingChanged));
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before,
        "identity rewrites must not be durable"
    );

    let mut omitted_binding = original.clone();
    omitted_binding.binding_handle = None;
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: omitted_binding,
        })
        .expect("an update may omit an already known binding");
    let stored = fixture
        .store
        .state()
        .turns_of(&fixture.session_id)
        .into_iter()
        .find(|turn| turn.id == original.id)
        .expect("stable turn remains present");
    assert_eq!(stored.binding_handle, original.binding_handle);
}

#[test]
fn live_session_candidate_must_resolve_its_own_runtime() {
    let mut fixture = scaffold(None);
    fixture
        .store
        .apply(&StateEffect::CapabilitiesUpdated {
            capabilities: live_capabilities(false),
        })
        .expect("publish live-observe capability");

    let mut missing_runtime = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("session exists")
        .clone();
    missing_runtime.binding_handle = None;
    missing_runtime.history_source.runtime_id = None;
    missing_runtime.live_binding = LiveBinding::Observing {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        since_at_ms: NOW_MS,
        evidence: CapabilityEvidence {
            source: EvidenceSource::ObservedInTraffic,
            observed_at_ms: NOW_MS + 1,
            note_ref: None,
        },
    };
    let records_before_missing = fixture.store.log().read_all().expect("read log").len();
    let missing_error = fixture
        .store
        .apply(&StateEffect::SessionUpserted {
            session: missing_runtime,
        })
        .expect_err("live candidate cannot borrow the stored session runtime");
    assert!(matches!(
        missing_error,
        StateError::LiveSessionWithoutRuntimeReference
    ));
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before_missing
    );

    let second_runtime_id = ProviderRuntimeId::new("rtm_second_runtime");
    let mut second_runtime = fixture
        .store
        .state()
        .runtime(&ProviderRuntimeId::new("rtm_0123456789abcdef"))
        .expect("first runtime exists")
        .clone();
    second_runtime.id = second_runtime_id.clone();
    second_runtime.capabilities.runtime_id = second_runtime_id.clone();
    second_runtime.binding_handle = None;
    fixture
        .store
        .apply(&StateEffect::RuntimeUpserted {
            runtime: second_runtime,
        })
        .expect("record second observing runtime");

    let base_candidate = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("stored session remains unchanged")
        .clone();
    let records_before_rewrites = fixture.store.log().read_all().expect("read log").len();

    let mut rewritten_binding = base_candidate.clone();
    rewritten_binding.binding_handle = Some(handle_for_runtime(
        ProviderBindingKind::Session,
        "rewritten_session_runtime",
        second_runtime_id.clone(),
    ));
    rewritten_binding.live_binding = LiveBinding::Observing {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        since_at_ms: NOW_MS,
        evidence: CapabilityEvidence {
            source: EvidenceSource::ObservedInTraffic,
            observed_at_ms: NOW_MS + 1,
            note_ref: None,
        },
    };
    let binding_error = fixture
        .store
        .apply(&StateEffect::SessionUpserted {
            session: rewritten_binding,
        })
        .expect_err("candidate binding runtime must match its live binding");
    assert!(matches!(
        binding_error,
        StateError::Contract(ContractViolation::LiveBindingRuntimeMismatch)
    ));

    let mut rewritten_history = base_candidate;
    rewritten_history.binding_handle = None;
    rewritten_history.history_source.runtime_id = Some(second_runtime_id);
    rewritten_history.live_binding = LiveBinding::Observing {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        since_at_ms: NOW_MS,
        evidence: CapabilityEvidence {
            source: EvidenceSource::ObservedInTraffic,
            observed_at_ms: NOW_MS + 1,
            note_ref: None,
        },
    };
    let history_error = fixture
        .store
        .apply(&StateEffect::SessionUpserted {
            session: rewritten_history,
        })
        .expect_err("candidate history runtime must match its live binding");
    assert!(matches!(
        history_error,
        StateError::Contract(ContractViolation::LiveBindingRuntimeMismatch)
    ));
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before_rewrites,
        "candidate runtime rewrites must not be durable"
    );
}

#[test]
fn runtime_identity_cannot_move_between_hosts() {
    let mut fixture = scaffold(None);
    let runtime_id = ProviderRuntimeId::new("rtm_0123456789abcdef");
    let original_host = HostId::new("hst_0123456789abcdef");
    let second_host = HostId::new("hst_second_runtime_host");
    fixture
        .store
        .apply(&StateEffect::HostUpserted {
            host: Host {
                id: second_host.clone(),
                display_name: "second host".to_owned(),
                platform: HostPlatform::Linux,
                reachability: HostReachability::LanDirect,
                protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
                last_seen_at_ms: NOW_MS,
            },
        })
        .expect("record second host");
    let old_key = ProjectionKey::RuntimeCapability {
        host_id: original_host.clone(),
        runtime_id: runtime_id.clone(),
    };
    let old_head = fixture
        .store
        .projection_journal()
        .head(&old_key)
        .expect("old runtime capability");
    let records_before = fixture.store.log().read_all().expect("read log").len();
    let mut moved = fixture
        .store
        .state()
        .runtime(&runtime_id)
        .expect("runtime")
        .clone();
    moved.host_id = second_host.clone();

    assert!(matches!(
        fixture
            .store
            .apply(&StateEffect::RuntimeUpserted { runtime: moved }),
        Err(StateError::RuntimeHostChanged)
    ));
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before
    );
    assert_eq!(
        fixture
            .store
            .state()
            .runtime(&runtime_id)
            .expect("runtime remains")
            .host_id,
        original_host
    );
    assert_eq!(
        fixture.store.projection_journal().head(&old_key),
        Some(old_head)
    );
    assert!(fixture
        .store
        .projection_journal()
        .head(&ProjectionKey::RuntimeCapability {
            host_id: second_host,
            runtime_id,
        })
        .is_none());
}

#[test]
fn runtime_acceptance_cannot_cross_runtime_boundaries() {
    let mut fixture = scaffold(None);
    let envelope = prompt(&fixture, "wrong_runtime");
    fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("record local acceptance");
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: remote_turn(&fixture.session_id, &envelope.command_id, "wrong_runtime"),
        })
        .expect("record remote-command turn");
    let mut ack = runtime_ack(&envelope.command_id);
    let CommandOutcome::AcceptedByRuntime { binding_handle } = &mut ack.outcome else {
        panic!("runtime_ack helper returned the wrong outcome");
    };
    binding_handle.runtime_id = ProviderRuntimeId::new("rtm_other_runtime");
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let error = fixture
        .store
        .apply(&StateEffect::CommandAcknowledged { ack })
        .expect_err("runtime acknowledgement cannot cross runtime boundaries");
    assert!(matches!(
        error,
        StateError::RuntimeAcknowledgementRuntimeMismatch
    ));
    assert_eq!(fixture.store.state().acknowledgements().len(), 1);
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before
    );
}

#[test]
fn duplicate_runtime_acceptance_is_rejected_without_append() {
    let mut fixture = scaffold(None);
    let envelope = prompt(&fixture, "control");
    fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("record local acceptance");
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: remote_turn(&fixture.session_id, &envelope.command_id, "control"),
        })
        .expect("record the correlated remote-command turn");
    let effect = StateEffect::CommandAcknowledged {
        ack: runtime_ack(&envelope.command_id),
    };
    fixture
        .store
        .apply(&effect)
        .expect("record first runtime acceptance");
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let error = fixture
        .store
        .apply(&effect)
        .expect_err("duplicate runtime acceptance must fail");
    assert!(matches!(error, StateError::DuplicateRuntimeAcknowledgement));
    assert_eq!(fixture.store.state().acknowledgements().len(), 2);
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before,
        "a duplicate runtime acknowledgement must not be durable"
    );
}

#[test]
fn controlling_binding_is_rejected_when_live_control_is_not_supported() {
    let mut fixture = scaffold(None);
    fixture
        .store
        .apply(&StateEffect::CapabilitiesUpdated {
            capabilities: live_capabilities(false),
        })
        .expect("publish live-observe-only capabilities");
    let mut session = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("session exists")
        .clone();
    session.live_binding = LiveBinding::Controlling {
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
        since_at_ms: NOW_MS,
        evidence: CapabilityEvidence {
            source: EvidenceSource::ObservedInTraffic,
            observed_at_ms: NOW_MS + 2,
            note_ref: None,
        },
    };
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let error = fixture
        .store
        .apply(&StateEffect::SessionUpserted { session })
        .expect_err("controlling without live-control capability must fail");
    assert!(matches!(
        error,
        StateError::Contract(ContractViolation::LiveBindingUnsupported {
            missing: "live_control"
        })
    ));
    assert!(matches!(
        fixture
            .store
            .state()
            .session(&fixture.session_id)
            .expect("session remains present")
            .live_binding,
        LiveBinding::NotBound { .. }
    ));
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before,
        "a refused controlling binding must not be durable"
    );
}

#[test]
fn live_control_capability_without_runtime_acceptance_is_rejected() {
    let mut fixture = scaffold(None);
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let update_error = fixture
        .store
        .apply(&StateEffect::CapabilitiesUpdated {
            capabilities: live_capabilities(true),
        })
        .expect_err("runtime acceptance must precede live-control capability");
    assert!(matches!(
        update_error,
        StateError::LiveControlCapabilityWithoutRuntimeAcceptance
    ));

    let mut runtime = fixture
        .store
        .state()
        .runtime(&ProviderRuntimeId::new("rtm_0123456789abcdef"))
        .expect("runtime exists")
        .clone();
    runtime.capabilities = live_capabilities(true);
    let upsert_error = fixture
        .store
        .apply(&StateEffect::RuntimeUpserted { runtime })
        .expect_err("runtime upsert cannot bypass the live-control evidence gate");
    assert!(matches!(
        upsert_error,
        StateError::LiveControlCapabilityWithoutRuntimeAcceptance
    ));
    let projection = fixture
        .store
        .projection(ProjectionName::RuntimeCapability, Some(&fixture.session_id))
        .expect("runtime capability projection");
    let ProjectionPayload::RuntimeCapability { view } = projection.payload else {
        panic!("expected runtime capability projection");
    };
    assert!(!view
        .entries
        .iter()
        .any(|entry| entry.capability == Capability::LiveControl));
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before,
        "a refused capability promotion must not be durable"
    );
}

#[test]
fn one_sessions_runtime_acceptance_cannot_control_another_session() {
    let mut fixture = scaffold(None);
    let second_session = second_session(&fixture);
    fixture
        .store
        .apply(&StateEffect::SessionUpserted {
            session: second_session.clone(),
        })
        .expect("record second session");

    let envelope = prompt(&fixture, "first_session_only");
    fixture
        .store
        .submit_command(&envelope, NOW_MS)
        .expect("record local acceptance for first session");
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: remote_turn(
                &fixture.session_id,
                &envelope.command_id,
                "first_session_only",
            ),
        })
        .expect("record first session's remote-command turn");
    fixture
        .store
        .apply(&StateEffect::CommandAcknowledged {
            ack: runtime_ack(&envelope.command_id),
        })
        .expect("record first session's runtime acceptance");
    fixture
        .store
        .apply(&StateEffect::CapabilitiesUpdated {
            capabilities: live_capabilities(true),
        })
        .expect("publish both live capabilities");
    let records_before = fixture.store.log().read_all().expect("read log").len();

    let error = fixture
        .store
        .apply(&StateEffect::SessionUpserted {
            session: controlling(&second_session),
        })
        .expect_err("first session's evidence cannot control the second session");
    assert!(matches!(
        error,
        StateError::ControllingBindingWithoutRuntimeAcceptance
    ));
    assert!(matches!(
        fixture
            .store
            .state()
            .session(&second_session.id)
            .expect("second session remains present")
            .live_binding,
        LiveBinding::NotBound { .. }
    ));
    assert_eq!(
        fixture.store.log().read_all().expect("read log").len(),
        records_before
    );
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hex = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

fn upload(store: &mut CanonicalStore, device_id: &DeviceId, bytes: &[u8]) -> ContentRef {
    let request = ContentWriteRequest {
        content_kind: ContentKind::PlainText,
        byte_len: u64::try_from(bytes.len()).expect("test upload length"),
        digest: digest(bytes),
    };
    let ContentWriteResponse::Stored { content_ref } = store
        .write_content_for_device(device_id, &request, bytes, NOW_MS)
        .expect("owned content write")
    else {
        panic!("expected stored content");
    };
    content_ref
}

fn device_prompt(
    fixture: &mut Fixture,
    device_id: &DeviceId,
    key: &str,
    body_bytes: &[u8],
) -> (DeviceCommandRequest, CommandEnvelope) {
    let body = upload(&mut fixture.store, device_id, body_bytes);
    let request = DeviceCommandRequest {
        idempotency_key: key.to_owned(),
        ttl_ms: None,
        body: Command::SubmitPrompt {
            session_id: fixture.session_id.clone(),
            body,
        },
    };
    let envelope = CommandEnvelope {
        command_id: CommandId::new(format!("cmd_{key}")),
        idempotency_key: key.to_owned(),
        actor: Actor::Human {
            device_id: device_id.clone(),
        },
        issued_at_ms: NOW_MS,
        expires_at_ms: None,
        body: request.body.clone(),
    };
    (request, envelope)
}

fn device_enqueue(
    fixture: &mut Fixture,
    device_id: &DeviceId,
    key: &str,
    body_bytes: &[u8],
) -> (DeviceCommandRequest, CommandEnvelope) {
    let body = upload(&mut fixture.store, device_id, body_bytes);
    let request = DeviceCommandRequest {
        idempotency_key: key.to_owned(),
        ttl_ms: None,
        body: Command::EnqueueInput {
            session_id: fixture.session_id.clone(),
            body,
            intent: QueueIntent::SteerActiveTurn,
        },
    };
    let envelope = CommandEnvelope {
        command_id: CommandId::new(format!("cmd_{key}")),
        idempotency_key: key.to_owned(),
        actor: Actor::Human {
            device_id: device_id.clone(),
        },
        issued_at_ms: NOW_MS,
        expires_at_ms: None,
        body: request.body.clone(),
    };
    (request, envelope)
}

#[test]
fn project_index_and_projection_journal_track_only_visible_changes() {
    let mut fixture = scaffold(None);
    let host_id = HostId::new("hst_0123456789abcdef");
    let project_key = ProjectionKey::ProjectIndex {
        host_id: host_id.clone(),
    };
    let project = fixture
        .store
        .projection_journal()
        .current(&project_key)
        .expect("project index exists");
    let ProjectionPayload::ProjectIndex { view } = &project.payload else {
        panic!("expected project index");
    };
    assert_eq!(view.host_id, host_id);
    assert_eq!(view.groups.len(), 1);
    let group = view.groups.first().expect("Codex provider group");
    assert_eq!(group.family, ProviderFamily::Codex);
    assert_eq!(group.projects.len(), 1);
    let summary = group.projects.first().expect("project summary");
    assert_eq!(summary.session_counts.total, 1);
    assert_eq!(summary.attention_count, 1);

    let unchanged = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("session")
        .clone();
    let commit = fixture
        .store
        .apply_commit(&StateEffect::SessionUpserted { session: unchanged })
        .expect("no-op session upsert");
    assert!(
        commit.projections.is_empty(),
        "fanout may recompute, but an unchanged full view must not consume a cursor"
    );

    let mut titled = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("session")
        .clone();
    titled.title = Some("visible title".to_owned());
    let commit = fixture
        .store
        .apply_commit(&StateEffect::SessionUpserted { session: titled })
        .expect("visible session change");
    assert_eq!(commit.projections.len(), 1);
    assert!(matches!(
        commit
            .projections
            .first()
            .expect("session index update")
            .key,
        ProjectionKey::SessionIndex { .. }
    ));
}

#[test]
fn every_state_effect_has_an_explicit_projection_fanout_case() {
    let fixture = scaffold(None);
    let state = fixture.store.state();
    let host = state.hosts().next().expect("host").clone();
    let runtime = state.runtimes().next().expect("runtime").clone();
    let project = state.projects().next().expect("project").clone();
    let session = state.session(&fixture.session_id).expect("session").clone();
    let turn = (*state.turns_of(&fixture.session_id).first().expect("turn")).clone();
    let item = state
        .item(&ItemId::new("itm_0123456789abcdef"))
        .expect("item")
        .clone();
    let attention = state
        .attention(&fixture.attention_id)
        .expect("attention")
        .clone();
    let queue_entry = QueueEntry {
        id: QueueEntryId::new("que_matrix"),
        session_id: fixture.session_id.clone(),
        position: 0,
        intent: QueueIntent::NewTurn,
        body: fixture.body.clone(),
        state: QueueState::Pending,
        editable: true,
        created_at_ms: NOW_MS,
        updated_at_ms: NOW_MS,
    };
    let workflow_id = WorkflowId::new("wfl_matrix");
    let step_id = StepId::new("stp_matrix");
    let workflow = Workflow {
        id: workflow_id.clone(),
        project_id: project.id.clone(),
        title: "matrix".to_owned(),
        state: WorkflowState::Ready,
        step_ids: vec![step_id.clone()],
        created_at_ms: NOW_MS,
        updated_at_ms: NOW_MS,
    };
    let step = Step {
        id: step_id.clone(),
        workflow_id: workflow_id.clone(),
        title: "matrix".to_owned(),
        role: StepRole::Implement,
        assignment: StepAssignment {
            selector: RuntimeSelector {
                family: ProviderFamily::Codex,
                required: vec![Capability::TurnPrompt],
                runtime_id: Some(runtime.id.clone()),
            },
            project_binding_id: session.project_binding_id.clone(),
            worktree_ref: fixture.body.clone(),
        },
        depends_on: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        completion: CompletionCondition::AgentTurnCompleted,
        human_gate: None,
        session_id: Some(session.id.clone()),
        state: StepState::Ready,
        attempt: 0,
        audit: Vec::new(),
    };
    let artifact = Artifact {
        id: ArtifactId::new("art_matrix"),
        workflow_id: workflow_id.clone(),
        produced_by: Some(step_id),
        kind: ArtifactKind::TestReport,
        content: fixture.body.clone(),
        created_at_ms: NOW_MS,
    };
    let rejected_ack = CommandAck {
        command_id: CommandId::new("cmd_matrix"),
        outcome: CommandOutcome::Rejected {
            error: kaleido_proto::error::CanonicalError {
                code: ErrorCode::InvalidCommand,
                retriable: false,
                detail_ref: None,
                at_ms: NOW_MS,
            },
        },
        acked_at_ms: NOW_MS,
    };
    let cases = vec![
        (
            "host",
            StateEffect::HostUpserted { host },
            vec!["attention", "project"],
        ),
        (
            "runtime",
            StateEffect::RuntimeUpserted {
                runtime: runtime.clone(),
            },
            vec![
                "input",
                "live",
                "project",
                "runtime",
                "session",
                "transcript",
            ],
        ),
        (
            "capabilities",
            StateEffect::CapabilitiesUpdated {
                capabilities: runtime.capabilities.clone(),
            },
            vec![
                "input",
                "live",
                "project",
                "runtime",
                "session",
                "transcript",
            ],
        ),
        (
            "project",
            StateEffect::ProjectUpserted {
                project: project.clone(),
            },
            vec!["project", "session"],
        ),
        (
            "session",
            StateEffect::SessionUpserted {
                session: session.clone(),
            },
            vec!["input", "live", "project", "session", "transcript"],
        ),
        (
            "session-status",
            StateEffect::SessionStatusChanged {
                session_id: session.id.clone(),
                status: SessionStatus::Idle,
            },
            vec!["input", "live", "project", "session", "transcript"],
        ),
        (
            "turn",
            StateEffect::TurnUpserted { turn },
            vec!["input", "live", "project", "session", "transcript"],
        ),
        (
            "item",
            StateEffect::ItemUpserted { item },
            vec!["input", "live", "project", "session", "transcript"],
        ),
        (
            "queue-entry",
            StateEffect::QueueEntryUpserted { entry: queue_entry },
            vec!["input", "live", "project", "session", "transcript"],
        ),
        (
            "queue-order",
            StateEffect::QueueReordered {
                session_id: session.id.clone(),
                order: Vec::new(),
            },
            vec!["input", "live", "project", "session", "transcript"],
        ),
        (
            "attention",
            StateEffect::AttentionUpserted { item: attention },
            vec![
                "attention",
                "input",
                "live",
                "project",
                "session",
                "transcript",
            ],
        ),
        (
            "workflow",
            StateEffect::WorkflowUpserted { workflow },
            vec!["project", "workflow"],
        ),
        ("step", StateEffect::StepUpserted { step }, vec!["workflow"]),
        (
            "artifact",
            StateEffect::ArtifactUpserted { artifact },
            vec!["workflow"],
        ),
        (
            "ack",
            StateEffect::CommandAcknowledged { ack: rejected_ack },
            Vec::new(),
        ),
        (
            "diagnostic",
            StateEffect::DiagnosticRecorded {
                diagnostic: DiagnosticRecord {
                    runtime_id: Some(runtime.id),
                    session_id: Some(session.id),
                    code: DiagnosticCode::UnknownUpstreamMessage,
                    count: 1,
                    first_at_ms: NOW_MS,
                    last_at_ms: NOW_MS,
                    detail_ref: None,
                },
            },
            Vec::new(),
        ),
    ];

    for (name, effect, expected) in cases {
        let keys = kaleido_state::projection::affected_keys(state, state, &effect);
        let mut actual = keys.iter().map(projection_kind).collect::<Vec<_>>();
        actual.sort_unstable();
        actual.dedup();
        assert_eq!(actual, expected, "wrong explicit fanout for {name}");
        assert_eq!(
            keys.len(),
            keys.iter().collect::<std::collections::HashSet<_>>().len(),
            "fanout for {name} contained duplicate keys"
        );
    }
}

#[test]
fn project_and_runtime_applies_refresh_the_related_workflow_board() {
    let mut fixture = scaffold(None);
    let project_id = ProjectId::new("prj_0123456789abcdef");
    let first_runtime_id = ProviderRuntimeId::new("rtm_0123456789abcdef");
    let second_runtime_id = ProviderRuntimeId::new("rtm_workflow_second");
    let mut second_runtime = fixture
        .store
        .state()
        .runtime(&first_runtime_id)
        .expect("first runtime")
        .clone();
    second_runtime.id = second_runtime_id.clone();
    second_runtime.capabilities.runtime_id = second_runtime_id.clone();
    second_runtime.capabilities.entries = vec![CapabilityEntry {
        capability: Capability::TurnPrompt,
        state: CapabilityState::Unsupported,
        evidence: CapabilityEvidence {
            source: EvidenceSource::Absent,
            observed_at_ms: NOW_MS,
            note_ref: None,
        },
    }];
    fixture
        .store
        .apply(&StateEffect::RuntimeUpserted {
            runtime: second_runtime.clone(),
        })
        .expect("second runtime");

    let workflow_id = WorkflowId::new("wfl_fanout_apply");
    let step_id = StepId::new("stp_fanout_apply");
    let project_binding_id = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("session")
        .project_binding_id
        .clone();
    fixture
        .store
        .apply(&StateEffect::WorkflowUpserted {
            workflow: Workflow {
                id: workflow_id.clone(),
                project_id: project_id.clone(),
                title: "fanout".to_owned(),
                state: WorkflowState::Ready,
                step_ids: vec![step_id.clone()],
                created_at_ms: NOW_MS,
                updated_at_ms: NOW_MS,
            },
        })
        .expect("workflow");
    fixture
        .store
        .apply(&StateEffect::StepUpserted {
            step: Step {
                id: step_id,
                workflow_id: workflow_id.clone(),
                title: "fanout".to_owned(),
                role: StepRole::Implement,
                assignment: StepAssignment {
                    selector: RuntimeSelector {
                        family: ProviderFamily::Codex,
                        required: vec![Capability::TurnPrompt],
                        runtime_id: None,
                    },
                    project_binding_id,
                    worktree_ref: fixture.body.clone(),
                },
                depends_on: Vec::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                completion: CompletionCondition::AgentTurnCompleted,
                human_gate: None,
                session_id: Some(fixture.session_id.clone()),
                state: StepState::Ready,
                attempt: 0,
                audit: Vec::new(),
            },
        })
        .expect("workflow step");
    let key = ProjectionKey::WorkflowBoard {
        workflow_id: workflow_id.clone(),
    };
    let before_project = fixture
        .store
        .projection_journal()
        .head(&key)
        .expect("initial workflow board");

    let mut project = fixture
        .store
        .state()
        .project(&project_id)
        .expect("project")
        .clone();
    project
        .bindings
        .first_mut()
        .expect("project binding")
        .runtime_id = second_runtime_id.clone();
    let project_commit = fixture
        .store
        .apply_commit(&StateEffect::ProjectUpserted { project })
        .expect("project binding moves to unsupported runtime");
    assert!(project_commit
        .projections
        .iter()
        .any(|projection| projection.key == key));
    let after_project = fixture
        .store
        .projection_journal()
        .head(&key)
        .expect("project-refreshed workflow board");
    assert!(after_project.seq > before_project.seq);
    let ProjectionPayload::WorkflowBoard { view } = &fixture
        .store
        .projection_journal()
        .current(&key)
        .expect("workflow board")
        .payload
    else {
        panic!("expected workflow board");
    };
    assert!(view.steps.iter().any(|step| {
        step.blockers.iter().any(|blocker| {
            matches!(
                blocker,
                kaleido_proto::workflow::StepBlocker::CapabilityNotSupported {
                    capability: Capability::TurnPrompt
                }
            )
        })
    }));

    second_runtime
        .capabilities
        .entries
        .first_mut()
        .expect("turn prompt capability")
        .state = CapabilityState::Supported;
    second_runtime.capabilities.negotiated_at_ms = NOW_MS + 1;
    let runtime_commit = fixture
        .store
        .apply_commit(&StateEffect::RuntimeUpserted {
            runtime: second_runtime,
        })
        .expect("runtime capability refresh");
    assert!(runtime_commit
        .projections
        .iter()
        .any(|projection| projection.key == key));
    let after_runtime = fixture
        .store
        .projection_journal()
        .head(&key)
        .expect("runtime-refreshed workflow board");
    assert!(after_runtime.seq > after_project.seq);
}

fn projection_kind(key: &ProjectionKey) -> &'static str {
    match key {
        ProjectionKey::ProjectIndex { .. } => "project",
        ProjectionKey::SessionIndex { .. } => "session",
        ProjectionKey::Transcript { .. } => "transcript",
        ProjectionKey::LiveActivity { .. } => "live",
        ProjectionKey::InputQueue { .. } => "input",
        ProjectionKey::AttentionInbox { .. } => "attention",
        ProjectionKey::WorkflowBoard { .. } => "workflow",
        ProjectionKey::RuntimeCapability { .. } => "runtime",
    }
}

#[test]
fn disconnected_multi_key_changes_resume_exactly_after_reload() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let keys = [
        ProjectionKey::ProjectIndex {
            host_id: HostId::new("hst_0123456789abcdef"),
        },
        ProjectionKey::SessionIndex {
            project_id: ProjectId::new("prj_0123456789abcdef"),
        },
        ProjectionKey::Transcript {
            session_id: fixture.session_id.clone(),
        },
        ProjectionKey::AttentionInbox {
            host_id: HostId::new("hst_0123456789abcdef"),
        },
    ];
    let heads = keys
        .iter()
        .map(|key| {
            fixture
                .store
                .projection_journal()
                .head(key)
                .expect("projection head")
        })
        .collect::<Vec<_>>();

    let mut session = fixture
        .store
        .state()
        .session(&fixture.session_id)
        .expect("session")
        .clone();
    session.title = Some("changed while disconnected".to_owned());
    fixture
        .store
        .apply(&StateEffect::SessionUpserted { session })
        .expect("session change");
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: Turn {
                id: TurnId::new("trn_disconnected"),
                session_id: fixture.session_id.clone(),
                status: TurnStatus::Running,
                origin: TurnOrigin::LocalSurface,
                started_at_ms: Some(NOW_MS + 10),
                completed_at_ms: None,
                item_ids: Vec::new(),
                error: None,
                binding_handle: None,
            },
        })
        .expect("turn change");
    let mut attention = fixture
        .store
        .state()
        .attention(&fixture.attention_id)
        .expect("attention")
        .clone();
    attention.state = AttentionState::Cancelled { at_ms: NOW_MS + 11 };
    fixture
        .store
        .apply(&StateEffect::AttentionUpserted { item: attention })
        .expect("attention change");
    drop(fixture.store);

    let reloaded = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("reload projection history");
    for (key, old_head) in keys.iter().zip(heads) {
        let replay = reloaded
            .projection_replay(
                &ProjectionSubscribe {
                    key: key.clone(),
                    since: Some(old_head),
                },
                NOW_MS + 20,
            )
            .expect("projection replay");
        let ProjectionSubscribeOutcome::Resumed { from_cursor } = replay.ack.outcome else {
            panic!("expected resumed replay for {key:?}");
        };
        assert_eq!(from_cursor, old_head.next().expect("next cursor"));
        assert!(!replay.envelopes.is_empty());
        validate_projection_sequence(key, old_head, &replay.envelopes)
            .expect("strict per-key replay");
    }
}

#[test]
fn reload_repairs_only_a_missing_projection_tail_from_canonical_history() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let key = ProjectionKey::Transcript {
        session_id: fixture.session_id.clone(),
    };
    let before = fixture
        .store
        .projection_journal()
        .head(&key)
        .expect("transcript head");
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: Turn {
                id: TurnId::new("trn_projection_crash"),
                session_id: fixture.session_id.clone(),
                status: TurnStatus::Running,
                origin: TurnOrigin::LocalSurface,
                started_at_ms: Some(NOW_MS + 30),
                completed_at_ms: None,
                item_ids: Vec::new(),
                error: None,
                binding_handle: None,
            },
        })
        .expect("append canonical and projections");
    let expected_head = fixture
        .store
        .projection_journal()
        .head(&key)
        .expect("advanced transcript head");
    drop(fixture.store);

    let projection_root = root.join(kaleido_state::projection_journal::PROJECTION_DIRECTORY);
    let mut target = None;
    for entry in std::fs::read_dir(&projection_root).expect("projection directory") {
        let path = entry.expect("projection entry").path();
        let contents = std::fs::read_to_string(&path).expect("projection contents");
        let Some(first) = contents.lines().next() else {
            continue;
        };
        let envelope: kaleido_proto::projection::ProjectionEnvelope =
            serde_json::from_str(first).expect("projection envelope");
        if envelope.key == key {
            target = Some((path, contents));
            break;
        }
    }
    let (path, contents) = target.expect("transcript journal file");
    let mut lines = contents.lines().collect::<Vec<_>>();
    lines.pop().expect("projection tail");
    let truncated = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    std::fs::write(&path, truncated).expect("simulate missing projection append");

    let repaired = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("canonical history repairs an exact missing tail");
    assert_eq!(
        repaired.projection_journal().head(&key),
        Some(expected_head)
    );
    let replay = repaired
        .projection_replay(
            &ProjectionSubscribe {
                key: key.clone(),
                since: Some(before),
            },
            NOW_MS + 40,
        )
        .expect("repaired replay");
    validate_projection_sequence(&key, before, &replay.envelopes)
        .expect("repaired tail is still contiguous");
}

#[test]
fn device_command_digest_conflict_and_claim_recovery_are_closed() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let device_id = DeviceId::new("device-owned-command");
    let (request, envelope) = device_prompt(&mut fixture, &device_id, "mobile-same", b"first");
    let admission = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("first admission");
    assert!(matches!(
        admission.ack.outcome,
        CommandOutcome::AcceptedLocally { .. }
    ));
    let ticket = admission.dispatch_ticket.expect("prompt dispatch ticket");

    let mut retry_envelope = envelope.clone();
    retry_envelope.command_id = CommandId::new("cmd_mobile_retry");
    assert!(matches!(
        fixture
            .store
            .admit_device_command(&device_id, &retry_envelope, &request, NOW_MS)
            .expect("exact retry")
            .ack
            .outcome,
        CommandOutcome::Duplicate { .. }
    ));

    let (conflict_request, mut conflict_envelope) =
        device_prompt(&mut fixture, &device_id, "different-temp-key", b"second");
    let conflict_request = DeviceCommandRequest {
        idempotency_key: request.idempotency_key.clone(),
        ..conflict_request
    };
    conflict_envelope.idempotency_key = request.idempotency_key.clone();
    conflict_envelope.body = conflict_request.body.clone();
    assert!(matches!(
        fixture
            .store
            .admit_device_command(&device_id, &conflict_envelope, &conflict_request, NOW_MS)
            .expect("conflicting retry")
            .ack
            .outcome,
        CommandOutcome::Rejected {
            error: kaleido_proto::error::CanonicalError {
                code: ErrorCode::IdempotencyConflict,
                ..
            }
        }
    ));

    drop(fixture.store);
    let reloaded = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("reload pending outbox");
    let pending = reloaded.pending_dispatches();
    assert_eq!(pending.len(), 1);
    let pending_command = pending.first().expect("pending command");
    assert_eq!(pending_command.ticket, ticket);
    assert_eq!(pending_command.envelope, envelope);
    // A broker may inspect this while its target runtime is still absent. The
    // read-only inspection must not consume or claim the recoverable command.
    drop(reloaded);
    let mut reloaded = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("pending survives runtime-not-ready restart");
    assert_eq!(reloaded.pending_dispatches().len(), 1);
    let claim = reloaded.claim_dispatch(&ticket).expect("durable claim");
    assert_eq!(claim.envelope, envelope);
    drop(reloaded);

    let reloaded = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("reload claimed outbox");
    assert!(reloaded.pending_dispatches().is_empty());
    assert_eq!(reloaded.uncertain_dispatches(), vec![envelope.command_id]);
}

#[test]
fn outbox_reload_recomputes_key_and_request_digests() {
    for field in ["key_digest", "request_digest"] {
        let mut fixture = scaffold(None);
        let root = fixture.store.root().to_path_buf();
        let device_id = DeviceId::new(format!("device-outbox-tamper-{field}"));
        let (request, envelope) = device_prompt(&mut fixture, &device_id, field, field.as_bytes());
        fixture
            .store
            .admit_device_command(&device_id, &envelope, &request, NOW_MS)
            .expect("durable prompt admission");
        drop(fixture.store);

        let path = root.join("device-command-outbox.jsonl");
        let mut record: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&path)
                .expect("outbox")
                .lines()
                .next()
                .expect("outbox record"),
        )
        .expect("json outbox record");
        *record
            .as_object_mut()
            .and_then(|object| object.get_mut(field))
            .expect("digest field") = serde_json::Value::String("0".repeat(64));
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string(&record).expect("tampered record")
            ),
        )
        .expect("tamper durable digest");
        assert!(matches!(
            CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS }),
            Err(StateError::MalformedRecord { .. })
        ));
    }
}

#[test]
fn attention_admission_recovery_closes_a_partial_canonical_decision_idempotently() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let device_id = DeviceId::new("device-test");
    let envelope = reply(&fixture, "attention-admission-recovery", "accept", None);
    let request = DeviceCommandRequest {
        idempotency_key: envelope.idempotency_key.clone(),
        ttl_ms: None,
        body: envelope.body.clone(),
    };
    let before_admission = directory_snapshot(&root);
    let admission = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("produce ready admission");
    let ticket = admission
        .dispatch_ticket
        .clone()
        .expect("attention dispatch ticket");
    let outbox_path = root.join("device-command-outbox.jsonl");
    let ready_record = std::fs::read(&outbox_path).expect("ready outbox record");
    restore_directory_snapshot(&root, &before_admission);
    drop(fixture.store);

    let mut partial = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("open pre-admission state");
    let mut attention = partial
        .state()
        .attention(&fixture.attention_id)
        .expect("open attention")
        .clone();
    attention.state = AttentionState::Answered {
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW_MS,
        answer_source: AttentionAnswerSource::LocalCommand {
            command_id: envelope.command_id.clone(),
        },
    };
    partial
        .apply(&StateEffect::AttentionUpserted { item: attention })
        .expect("persist only the attention decision");
    drop(partial);
    std::fs::write(&outbox_path, ready_record).expect("restore ready write-ahead record");

    let recovered = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("repair missing local acknowledgement");
    assert_eq!(recovered.pending_dispatches().len(), 1);
    assert_eq!(
        recovered
            .pending_dispatches()
            .first()
            .expect("pending recovered attention")
            .ticket,
        ticket
    );
    assert_eq!(
        recovered
            .state()
            .acknowledgements()
            .iter()
            .filter(|ack| ack.command_id == envelope.command_id)
            .count(),
        1
    );
    let records_after_recovery = recovered.log().read_all().expect("recovered log").len();
    drop(recovered);

    let recovered_again = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("second recovery is idempotent");
    assert_eq!(
        recovered_again
            .log()
            .read_all()
            .expect("idempotent log")
            .len(),
        records_after_recovery
    );
    assert_eq!(recovered_again.pending_dispatches().len(), 1);
}

#[test]
fn a_claimed_attention_crash_never_redispatches_after_durable_local_decision() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let device_id = DeviceId::new("device-test");
    let envelope = reply(&fixture, "outbox-original", "accept", None);
    let request = DeviceCommandRequest {
        idempotency_key: envelope.idempotency_key.clone(),
        ttl_ms: None,
        body: envelope.body.clone(),
    };
    let admission = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("valid attention admission");
    let ticket = admission.dispatch_ticket.expect("attention ticket");

    assert!(matches!(
        fixture
            .store
            .state()
            .attention(&fixture.attention_id)
            .expect("attention decided during admission")
            .state,
        AttentionState::Answered { .. }
    ));
    assert!(fixture
        .store
        .state()
        .acknowledgements()
        .iter()
        .any(|ack| ack == &admission.ack));

    let competing = reply(&fixture, "competing-answer", "decline", None);
    let competing_ack = fixture
        .store
        .submit_command(&competing, NOW_MS)
        .expect("competing local answer");
    assert!(matches!(
        competing_ack.outcome,
        CommandOutcome::Rejected {
            error: kaleido_proto::error::CanonicalError {
                code: ErrorCode::ApprovalAlreadyAnswered,
                ..
            }
        }
    ));
    fixture
        .store
        .claim_dispatch(&ticket)
        .expect("claim only transitions the durable outbox");
    drop(fixture.store);

    let reloaded = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("reload uncertain claim");
    assert!(reloaded.pending_dispatches().is_empty());
    assert_eq!(reloaded.uncertain_dispatches(), vec![envelope.command_id]);
}

#[test]
fn approval_dispatch_completes_without_fabricating_runtime_acceptance() {
    let mut fixture = scaffold(None);
    let device_id = DeviceId::new("device-test");
    let envelope = reply(&fixture, "approval-complete", "accept", None);
    let request = DeviceCommandRequest {
        idempotency_key: envelope.idempotency_key.clone(),
        ttl_ms: None,
        body: envelope.body.clone(),
    };
    let admission = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("approval admission");
    assert!(!admission.projections.is_empty());
    assert!(matches!(
        fixture
            .store
            .state()
            .attention(&fixture.attention_id)
            .expect("admission durably decides attention")
            .state,
        AttentionState::Answered { .. }
    ));
    assert!(fixture
        .store
        .state()
        .acknowledgements()
        .iter()
        .any(|ack| ack == &admission.ack));
    let ticket = admission.dispatch_ticket.expect("approval ticket");
    fixture
        .store
        .claim_dispatch(&ticket)
        .expect("approval claim");
    fixture
        .store
        .finish_dispatch(&ticket, &[])
        .expect("structured approval success completes without runtime ack");
    let matching = fixture
        .store
        .state()
        .acknowledgements()
        .iter()
        .filter(|ack| ack.command_id == envelope.command_id)
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1);
    assert!(matches!(
        matching.first().expect("local acknowledgement").outcome,
        CommandOutcome::AcceptedLocally { .. }
    ));
    assert!(fixture.store.uncertain_dispatches().is_empty());
}

#[test]
fn prompt_dispatch_cannot_complete_without_terminal_runtime_evidence() {
    let mut fixture = scaffold(None);
    let device_id = DeviceId::new("device-prompt-terminal");
    let (request, envelope) = device_prompt(&mut fixture, &device_id, "prompt-terminal", b"go");
    let ticket = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("prompt admission")
        .dispatch_ticket
        .expect("prompt ticket");
    fixture.store.claim_dispatch(&ticket).expect("prompt claim");
    assert!(matches!(
        fixture.store.finish_dispatch(&ticket, &[]),
        Err(StateError::DeviceCommandMismatch { .. })
    ));
    assert_eq!(
        fixture.store.uncertain_dispatches(),
        vec![envelope.command_id.clone()]
    );
    for outcome in [
        CommandOutcome::Duplicate {
            original_command_id: envelope.command_id.clone(),
        },
        CommandOutcome::Enqueued {
            entry_id: QueueEntryId::new("que_not_a_runtime_terminal"),
        },
    ] {
        assert!(matches!(
            fixture.store.finish_dispatch(
                &ticket,
                &[StateEffect::CommandAcknowledged {
                    ack: CommandAck {
                        command_id: envelope.command_id.clone(),
                        outcome,
                        acked_at_ms: NOW_MS + 2,
                    },
                }],
            ),
            Err(StateError::DeviceCommandMismatch { .. })
        ));
    }
    fixture
        .store
        .finish_dispatch(
            &ticket,
            &[
                StateEffect::TurnUpserted {
                    turn: remote_turn(&fixture.session_id, &envelope.command_id, "terminal"),
                },
                StateEffect::CommandAcknowledged {
                    ack: runtime_ack(&envelope.command_id),
                },
            ],
        )
        .expect("real structured runtime evidence completes prompt");
    assert!(fixture.store.uncertain_dispatches().is_empty());
}

#[test]
fn a_terminal_runtime_ack_closes_a_claimed_outbox_after_a_finish_crash() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let device_id = DeviceId::new("device-prompt-finish-recovery");
    let (request, envelope) = device_prompt(
        &mut fixture,
        &device_id,
        "prompt-finish-recovery",
        b"finish recovery",
    );
    let ticket = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("prompt admission")
        .dispatch_ticket
        .expect("prompt ticket");
    fixture.store.claim_dispatch(&ticket).expect("prompt claim");

    // This is the exact durable state after `finish_dispatch` has applied the
    // provider effects but before it appends the Completed outbox record.
    fixture
        .store
        .apply(&StateEffect::TurnUpserted {
            turn: remote_turn(&fixture.session_id, &envelope.command_id, "finish-recovery"),
        })
        .expect("durable remote turn");
    fixture
        .store
        .apply(&StateEffect::CommandAcknowledged {
            ack: runtime_ack(&envelope.command_id),
        })
        .expect("durable terminal acknowledgement");
    drop(fixture.store);

    let mut recovered = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
        .expect("complete the claimed outbox without redispatch");
    assert!(recovered.pending_dispatches().is_empty());
    assert!(recovered.uncertain_dispatches().is_empty());
    assert_eq!(
        recovered
            .state()
            .acknowledgements()
            .iter()
            .filter(|ack| ack.command_id == envelope.command_id)
            .count(),
        2
    );

    let duplicate = recovered
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("same request remains idempotent");
    assert!(matches!(
        duplicate.ack.outcome,
        CommandOutcome::Duplicate { .. }
    ));
    assert!(duplicate.dispatch_ticket.is_none());
}

#[test]
fn an_unrecoverable_ready_runtime_route_is_rejected_without_provider_dispatch() {
    let mut fixture = scaffold(None);
    let device_id = DeviceId::new("device-stale-runtime-route");
    let (request, envelope) = device_prompt(
        &mut fixture,
        &device_id,
        "stale-runtime-route",
        b"never sent",
    );
    let ticket = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("prompt admission")
        .dispatch_ticket
        .expect("ready route");

    fixture
        .store
        .reject_ready_dispatch(&ticket, NOW_MS + 1)
        .expect("durable runtime-unavailable rejection");

    assert!(fixture.store.pending_dispatches().is_empty());
    assert!(fixture.store.uncertain_dispatches().is_empty());
    let matching = fixture
        .store
        .state()
        .acknowledgements()
        .iter()
        .filter(|ack| ack.command_id == envelope.command_id)
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 2);
    assert!(matches!(
        matching.get(1).expect("terminal rejection").outcome,
        CommandOutcome::Rejected {
            error: kaleido_proto::error::CanonicalError {
                code: ErrorCode::RuntimeUnavailable,
                retriable: true,
                ..
            }
        }
    ));
}

#[test]
fn device_enqueue_stays_pending_without_a_runtime_dispatch_ticket() {
    let mut fixture = scaffold(None);
    let device_id = DeviceId::new("device-enqueue");
    let body = upload(&mut fixture.store, &device_id, b"queued steer");
    let request = DeviceCommandRequest {
        idempotency_key: "device-enqueue".to_owned(),
        ttl_ms: None,
        body: Command::EnqueueInput {
            session_id: fixture.session_id.clone(),
            body,
            intent: QueueIntent::SteerActiveTurn,
        },
    };
    let envelope = CommandEnvelope {
        command_id: CommandId::new("cmd_device_enqueue"),
        idempotency_key: request.idempotency_key.clone(),
        actor: Actor::Human {
            device_id: device_id.clone(),
        },
        issued_at_ms: NOW_MS,
        expires_at_ms: None,
        body: request.body.clone(),
    };
    let admission = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("device enqueue");
    assert!(admission.dispatch_ticket.is_none());
    assert!(matches!(
        admission.ack.outcome,
        CommandOutcome::Enqueued { .. }
    ));
    let queue = fixture
        .store
        .projection(ProjectionName::InputQueue, Some(&fixture.session_id))
        .expect("input queue");
    let ProjectionPayload::InputQueue { view } = queue.payload else {
        panic!("expected input queue");
    };
    assert_eq!(view.entries.len(), 1);
    assert_eq!(
        view.entries.first().expect("pending entry").state,
        QueueState::Pending
    );
}

#[test]
fn device_prompt_for_an_unknown_session_is_durably_rejected_without_dispatch() {
    let mut fixture = scaffold(None);
    let device_id = DeviceId::new("device-unknown-session");
    let (mut request, mut envelope) =
        device_prompt(&mut fixture, &device_id, "unknown-session", b"hello");
    let Command::SubmitPrompt { session_id, .. } = &mut request.body else {
        panic!("expected prompt");
    };
    *session_id = SessionId::new("ses_missing_device_target");
    envelope.body = request.body.clone();
    let admission = fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("canonical not-found admission");
    assert!(admission.dispatch_ticket.is_none());
    assert!(matches!(
        admission.ack.outcome,
        CommandOutcome::Rejected {
            error: kaleido_proto::error::CanonicalError {
                code: ErrorCode::NotFound,
                ..
            }
        }
    ));
    assert!(fixture
        .store
        .state()
        .acknowledgements()
        .iter()
        .any(|ack| ack == &admission.ack));
}

#[test]
fn local_admission_recovers_each_outbox_to_canonical_crash_window() {
    // Window 1: the write-ahead outbox record is durable but no canonical
    // effect is. Startup must finish the local enqueue exactly once.
    {
        let mut fixture = scaffold(None);
        let root = fixture.store.root().to_path_buf();
        let device_id = DeviceId::new("device-local-recovery-before-canonical");
        let (request, envelope) = device_enqueue(
            &mut fixture,
            &device_id,
            "local-recovery-before-canonical",
            b"before canonical",
        );
        let before_admission = directory_snapshot(&root);
        let admission = fixture
            .store
            .admit_device_command(&device_id, &envelope, &request, NOW_MS)
            .expect("produce durable local transaction");
        let CommandOutcome::Enqueued { entry_id } = admission.ack.outcome else {
            panic!("expected enqueue admission");
        };
        let outbox_path = root.join("device-command-outbox.jsonl");
        let outbox = std::fs::read_to_string(&outbox_path).expect("outbox transaction");
        let claimed = format!(
            "{}\n",
            outbox.lines().next().expect("claimed write-ahead record")
        );
        restore_directory_snapshot(&root, &before_admission);
        std::fs::write(&outbox_path, claimed).expect("restore claimed-only outbox");
        drop(fixture.store);

        let mut recovered = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
            .expect("recover before-canonical window");
        assert!(recovered.state().queue_entry(&entry_id).is_some());
        assert!(recovered.uncertain_dispatches().is_empty());
        assert!(matches!(
            recovered
                .admit_device_command(&device_id, &envelope, &request, NOW_MS)
                .expect("retry recovered admission")
                .ack
                .outcome,
            CommandOutcome::Duplicate { .. }
        ));
    }

    // Window 2: the queue effect is durable, but its acknowledgement and
    // idempotency side-table append are not. Recovery must not enqueue twice.
    {
        let mut fixture = scaffold(None);
        let root = fixture.store.root().to_path_buf();
        let device_id = DeviceId::new("device-local-recovery-after-effect");
        let (request, envelope) = device_enqueue(
            &mut fixture,
            &device_id,
            "local-recovery-after-effect",
            b"after queue effect",
        );
        let before_admission = directory_snapshot(&root);
        let admission = fixture
            .store
            .admit_device_command(&device_id, &envelope, &request, NOW_MS)
            .expect("produce durable local transaction");
        let CommandOutcome::Enqueued { entry_id } = admission.ack.outcome.clone() else {
            panic!("expected enqueue admission");
        };
        let outbox_path = root.join("device-command-outbox.jsonl");
        let outbox = std::fs::read_to_string(&outbox_path).expect("outbox transaction");
        let claimed = format!(
            "{}\n",
            outbox.lines().next().expect("claimed write-ahead record")
        );
        restore_directory_snapshot(&root, &before_admission);
        drop(fixture.store);

        let Command::EnqueueInput {
            session_id,
            body,
            intent,
        } = &request.body
        else {
            panic!("expected enqueue request");
        };
        let mut partial = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
            .expect("open partial canonical store");
        partial
            .apply(&StateEffect::QueueEntryUpserted {
                entry: QueueEntry {
                    id: entry_id.clone(),
                    session_id: session_id.clone(),
                    position: 0,
                    intent: *intent,
                    body: body.clone(),
                    state: QueueState::Pending,
                    editable: true,
                    created_at_ms: NOW_MS,
                    updated_at_ms: NOW_MS,
                },
            })
            .expect("persist only the queue portion");
        drop(partial);
        std::fs::write(&outbox_path, claimed).expect("restore claimed-only outbox");

        let recovered = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
            .expect("recover after-effect window");
        assert_eq!(recovered.state().queue_of(session_id).len(), 1);
        assert_eq!(
            recovered
                .state()
                .acknowledgements()
                .iter()
                .filter(|ack| ack.command_id == envelope.command_id)
                .count(),
            1
        );
        assert!(recovered.uncertain_dispatches().is_empty());
    }

    // Window 3: canonical effect, acknowledgement and idempotency are durable,
    // while the final outbox completion is missing. Startup only completes the
    // outbox record and does not append canonical state again.
    {
        let mut fixture = scaffold(None);
        let root = fixture.store.root().to_path_buf();
        let device_id = DeviceId::new("device-local-recovery-after-ack");
        let (request, envelope) = device_enqueue(
            &mut fixture,
            &device_id,
            "local-recovery-after-ack",
            b"after canonical ack",
        );
        fixture
            .store
            .admit_device_command(&device_id, &envelope, &request, NOW_MS)
            .expect("complete local admission");
        let outbox_path = root.join("device-command-outbox.jsonl");
        let outbox = std::fs::read_to_string(&outbox_path).expect("outbox transaction");
        let claimed = format!(
            "{}\n",
            outbox.lines().next().expect("claimed write-ahead record")
        );
        std::fs::write(&outbox_path, claimed).expect("remove completion append");
        drop(fixture.store);

        let recovered = CanonicalStore::load(&root, ClockSource::Fixed { at_ms: NOW_MS })
            .expect("recover after-ack window");
        assert_eq!(
            recovered
                .state()
                .acknowledgements()
                .iter()
                .filter(|ack| ack.command_id == envelope.command_id)
                .count(),
            1
        );
        assert!(recovered.uncertain_dispatches().is_empty());
        assert_eq!(
            std::fs::read_to_string(outbox_path)
                .expect("completed recovery outbox")
                .lines()
                .count(),
            2
        );
    }
}

#[test]
fn device_content_cannot_cross_authentication_boundaries_in_commands() {
    let mut fixture = scaffold(None);
    let owner = DeviceId::new("device-owner");
    let attacker = DeviceId::new("device-attacker");
    let (request, mut envelope) = device_prompt(&mut fixture, &owner, "owned", b"private");
    envelope.actor = Actor::Human {
        device_id: attacker.clone(),
    };
    assert!(matches!(
        fixture
            .store
            .admit_device_command(&attacker, &envelope, &request, NOW_MS),
        Err(StateError::ContentUnauthorized { .. })
    ));
}

#[test]
fn authenticated_device_reads_canonical_agent_content_but_not_random_ids() {
    let fixture = scaffold(None);
    let transcript = fixture
        .store
        .projection(ProjectionName::Transcript, Some(&fixture.session_id))
        .expect("transcript projection");
    let ProjectionPayload::Transcript { view } = transcript.payload else {
        panic!("expected transcript");
    };
    let item = view
        .turns
        .first()
        .and_then(|turn| turn.items.first())
        .expect("recorded agent message");
    let ItemBody::AgentMessage { content, .. } = &item.body else {
        panic!("expected agent message");
    };
    let device_id = DeviceId::new("any-paired-device");
    let ContentReadResponse::Chunk { chunk } = fixture
        .store
        .read_content_for_device(
            &device_id,
            &ContentReadRequest {
                content_id: content.content_id.clone(),
                offset: 0,
                max_bytes: 65_536,
            },
            NOW_MS,
        )
        .expect("canonical content read")
    else {
        panic!("canonical content should be readable");
    };
    assert_eq!(chunk.bytes, b"hello");

    assert!(matches!(
        fixture
            .store
            .read_content_for_device(
                &device_id,
                &ContentReadRequest {
                    content_id: kaleido_proto::ids::ContentId::new("random-content-id"),
                    offset: 0,
                    max_bytes: 1,
                },
                NOW_MS,
            )
            .expect("unknown content response"),
        ContentReadResponse::Unavailable {
            reason: ContentUnavailableReason::Unauthorized,
            ..
        }
    ));
}

#[test]
fn startup_cleanup_removes_expired_and_orphan_bodies_but_preserves_canonical_content() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let canonical = fixture
        .store
        .state()
        .item(&ItemId::new("itm_0123456789abcdef"))
        .and_then(|item| match &item.body {
            ItemBody::AgentMessage { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("canonical agent content");
    let orphan = fixture
        .store
        .store_content(
            ContentKind::PlainText,
            Sensitivity::Sensitive,
            b"orphan provider body",
        )
        .expect("unreferenced provider body");
    let device_id = DeviceId::new("device-expired-cleanup");
    let expired_upload = upload(&mut fixture.store, &device_id, b"expired mobile body");
    let temporary = fixture
        .store
        .content()
        .root()
        .join(".crashed-content-write.tmp");
    std::fs::write(&temporary, b"partial body").expect("residual temporary body");
    drop(fixture.store);

    let reloaded = CanonicalStore::load(
        &root,
        ClockSource::Fixed {
            at_ms: NOW_MS + kaleido_state::content::DEVICE_CONTENT_TTL_MS,
        },
    )
    .expect("cleanup after canonical replay");
    assert_eq!(
        reloaded.content().load(&canonical).expect("canonical body"),
        b"hello"
    );
    assert!(!reloaded.content().contains(&orphan.content_id));
    assert!(!reloaded.content().contains(&expired_upload.content_id));
    assert!(!temporary.exists());
    let ownership =
        std::fs::read_to_string(reloaded.content().root().join("device-ownership.jsonl"))
            .expect("compacted ownership journal");
    assert!(ownership.is_empty());
}

#[test]
fn pending_outbox_content_survives_ownership_expiry_cleanup() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let device_id = DeviceId::new("device-pending-content-cleanup");
    let (request, envelope) = device_prompt(
        &mut fixture,
        &device_id,
        "pending-content-cleanup",
        b"pending prompt body",
    );
    fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("pending prompt");
    drop(fixture.store);

    let reloaded = CanonicalStore::load(
        &root,
        ClockSource::Fixed {
            at_ms: NOW_MS + kaleido_state::content::DEVICE_CONTENT_TTL_MS,
        },
    )
    .expect("pending outbox protects content");
    let pending = reloaded
        .pending_dispatches()
        .into_iter()
        .next()
        .expect("pending dispatch");
    let Command::SubmitPrompt { body, .. } = pending.envelope.body else {
        panic!("expected prompt");
    };
    assert_eq!(
        reloaded
            .content()
            .load(&body)
            .expect("protected prompt body"),
        b"pending prompt body"
    );
}

#[test]
fn sensitive_fixture_and_full_user_path_never_enter_side_files() {
    let mut fixture = scaffold(None);
    let root = fixture.store.root().to_path_buf();
    let device_id = DeviceId::new("device-privacy-regression");
    let fixture_bytes = include_bytes!("../../../tests/fixtures/codex/01-simple-turn.jsonl");
    let user_path = br"C:\Users\privacy-canary-user\private-project";
    let mut sensitive = fixture_bytes.to_vec();
    sensitive.extend_from_slice(b"\n");
    sensitive.extend_from_slice(user_path);
    let (request, envelope) =
        device_enqueue(&mut fixture, &device_id, "privacy-regression", &sensitive);
    fixture
        .store
        .admit_device_command(&device_id, &envelope, &request, NOW_MS)
        .expect("persist reference-only command");

    for (relative, bytes) in directory_snapshot(&root) {
        let is_body = relative
            .parent()
            .is_some_and(|parent| parent == Path::new("content"))
            && relative
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.len() == 64
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                });
        if is_body {
            continue;
        }
        assert!(
            !contains_bytes(&bytes, &sensitive),
            "sensitive fixture leaked into {}",
            relative.display()
        );
        assert!(
            !contains_bytes(&bytes, user_path),
            "full user path leaked into {}",
            relative.display()
        );
        assert!(
            !contains_bytes(&bytes, b"privacy-canary-user"),
            "username leaked into {}",
            relative.display()
        );
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
