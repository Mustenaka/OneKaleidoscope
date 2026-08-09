#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Store behaviour that needs constructed canonical inputs.
//!
//! Reduction from real recordings is exercised where the recordings are, in the
//! adapter and the host daemon. What is left here are the rules a provider
//! cannot demonstrate on its own: command idempotency, and refusing a reply to
//! an approval that is already decided or already expired.

use kaleido_proto::attention::{
    ApprovalRequest, AttentionAnswerEvidence, AttentionAnswerEvidenceSource, AttentionAnswerSource,
    AttentionItem, AttentionResponse, AttentionState, AttentionSubject, DecisionOption,
    DecisionSemantics, JoinState,
};
use kaleido_proto::capability::{
    Capability, CapabilityEntry, CapabilityEvidence, CapabilityState, EvidenceSource,
    RuntimeCapabilities,
};
use kaleido_proto::command::{Actor, Command, CommandAck, CommandEnvelope, CommandOutcome};
use kaleido_proto::content::{ContentKind, ContentRef, Sensitivity};
use kaleido_proto::effect::{StateEffect, StreamKey};
use kaleido_proto::error::ErrorCode;
use kaleido_proto::host::{
    ConnectionState, Host, HostPlatform, HostReachability, LaunchSurface, Project, ProjectBinding,
    ProviderFamily, ProviderRuntime, SessionCounts,
};
use kaleido_proto::ids::{
    AttentionId, CommandId, HostId, ItemId, ProjectBindingId, ProjectId, ProviderBindingHandle,
    ProviderBindingId, ProviderBindingKind, ProviderRuntimeId, SessionId, TurnId,
};
use kaleido_proto::projection::ProjectionPayload;
use kaleido_proto::queue::{QueueIntent, QueueState};
use kaleido_proto::session::{
    HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason, OwnershipMode, Session,
    SessionStatus,
};
use kaleido_proto::turn::{Item, ItemBody, ItemStatus, MessagePhase, Turn, TurnOrigin, TurnStatus};
use kaleido_proto::ContractViolation;
use kaleido_state::{CanonicalStore, ClockSource, ProjectionName, StateError};

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

fn prompt(fixture: &Fixture, command: &str) -> CommandEnvelope {
    CommandEnvelope {
        command_id: CommandId::new(format!("cmd_{command}")),
        idempotency_key: command.to_owned(),
        actor: Actor::Human {
            device_label: "test-device".to_owned(),
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
