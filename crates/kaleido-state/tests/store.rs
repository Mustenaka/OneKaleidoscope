#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Store behaviour that needs constructed canonical inputs.
//!
//! Reduction from real recordings is exercised where the recordings are, in the
//! adapter and the host daemon. What is left here are the rules a provider
//! cannot demonstrate on its own: command idempotency, and refusing a reply to
//! an approval that is already decided or already expired.

use kaleido_proto::attention::{
    ApprovalRequest, AttentionItem, AttentionResponse, AttentionState, AttentionSubject,
    DecisionOption, DecisionSemantics, JoinState,
};
use kaleido_proto::capability::{
    Capability, CapabilityEntry, CapabilityEvidence, CapabilityState, EvidenceSource,
    RuntimeCapabilities,
};
use kaleido_proto::command::{Actor, Command, CommandEnvelope, CommandOutcome};
use kaleido_proto::content::{ContentKind, ContentRef, Sensitivity};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::error::ErrorCode;
use kaleido_proto::host::{
    ConnectionState, Host, HostPlatform, HostReachability, LaunchSurface, Project, ProjectBinding,
    ProviderFamily, ProviderRuntime, SessionCounts,
};
use kaleido_proto::ids::{
    AttentionId, CommandId, HostId, ItemId, ProjectBindingId, ProjectId, ProviderBindingHandle,
    ProviderBindingId, ProviderBindingKind, ProviderRuntimeId, SessionId, TurnId,
};
use kaleido_proto::queue::{QueueIntent, QueueState};
use kaleido_proto::session::{
    HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason, OwnershipMode, SessionStatus,
};
use kaleido_proto::turn::{Item, ItemBody, ItemStatus, MessagePhase, Turn, TurnOrigin, TurnStatus};
use kaleido_state::{CanonicalStore, ClockSource};

const NOW_MS: i64 = 1_785_378_000_000;

struct Fixture {
    _directory: tempfile::TempDir,
    store: CanonicalStore,
    session_id: SessionId,
    attention_id: AttentionId,
    request_key: String,
    body: ContentRef,
}

fn handle(kind: ProviderBindingKind, suffix: &str) -> ProviderBindingHandle {
    ProviderBindingHandle {
        id: ProviderBindingId::new(format!("bnd_{suffix}")),
        runtime_id: ProviderRuntimeId::new("rtm_0123456789abcdef"),
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
            device_label: "test-device".to_owned(),
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
            device_label: "test-device".to_owned(),
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
