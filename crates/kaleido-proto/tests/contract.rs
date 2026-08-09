#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Executable contract checks for UACP v0.3.
//!
//! These tests intentionally exercise both success and rejection paths. The
//! Codex evidence checks read the committed recorder fixtures; they do not
//! invent provider events or decode them into canonical provider shadow types.

use std::collections::HashSet;
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;

use kaleido_proto::attention::{
    ApprovalRequest, AttentionAnswerEvidence, AttentionAnswerEvidenceSource, AttentionAnswerSource,
    AttentionItem, AttentionResponse, AttentionState, AttentionSubject, DecisionOption,
    DecisionSemantics, JoinFailureReason, JoinState, ReplyRejection, WorkflowGateRequest,
};
use kaleido_proto::capability::{
    Capability, CapabilityEntry, CapabilityEvidence, CapabilityState, CapabilityUnavailableReason,
    EvidenceSource, RuntimeCapabilities,
};
use kaleido_proto::command::{
    Actor, Command, CommandAck, CommandEnvelope, CommandOutcome, DeviceCommandRequest,
    MAX_DEVICE_COMMAND_TTL_MS, MAX_IDEMPOTENCY_KEY_BYTES,
};
use kaleido_proto::content::{
    ContentAvailability, ContentKind, ContentReadChunk, ContentReadRequest, ContentReadResponse,
    ContentRef, ContentUnavailableReason, ContentWriteRequest, ContentWriteResponse, Sensitivity,
    MAX_CONTENT_READ_BYTES, MAX_CONTENT_WRITE_BYTES,
};
use kaleido_proto::effect::{
    validate_replay_window, verify_contiguous, Cursor, DiagnosticCode, DiagnosticRecord,
    HostSnapshot, LogRecord, ProjectSnapshot, SessionSnapshot, SnapshotEnvelope, SnapshotPayload,
    StateEffect, StreamKey, WorkflowSnapshot,
};
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::host::{
    ConnectionState, Host, HostPlatform, HostReachability, LaunchSurface, Project, ProjectBinding,
    ProviderFamily, ProviderRuntime, SessionCounts,
};
use kaleido_proto::ids::{
    AgentTaskId, ArtifactId, AttentionId, BlockerId, CommandId, ContentId, DeviceId, HostId,
    ItemId, ProjectBindingId, ProjectId, ProviderBindingHandle, ProviderBindingId,
    ProviderBindingKind, ProviderRuntimeId, QueueEntryId, SessionId, StepId, TurnId, WorkflowId,
};
use kaleido_proto::projection::{
    decide_projection_subscription, validate_projection_sequence, AttentionInboxView,
    InputQueueView, LiveActivityView, ProjectBindingSummary, ProjectIndexView, ProjectSummary,
    ProjectionEnvelope, ProjectionKey, ProjectionPayload, ProjectionSubscribe,
    ProjectionSubscribeAck, ProjectionSubscribeOutcome, ProviderGroup, RuntimeCapabilityView,
    SessionIndexView, SessionSummary, TranscriptTurn, TranscriptView, WorkflowBoardStep,
    WorkflowBoardView, PROJECTION_VERSION,
};
use kaleido_proto::queue::{
    validate_queue_reorder, QueueEntry, QueueIntent, QueueState, SteerAcknowledgement,
};
use kaleido_proto::session::{
    derive_session_status, HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason,
    OwnershipMode, Session, SessionStatus, StatusInputs,
};
use kaleido_proto::turn::{
    AgentTask, Item, ItemBody, ItemStatus, MessagePhase, PlanEntry, PlanEntryState, Turn,
    TurnOrigin, TurnStatus,
};
use kaleido_proto::workflow::{
    validate_transition, Artifact, ArtifactKind, CompletionCondition, RuntimeSelector, Step,
    StepAssignment, StepBlocker, StepRole, StepState, Workflow, WorkflowAction, WorkflowState,
};
use kaleido_proto::{version_is_compatible, ContractViolation, PROTOCOL_VERSION};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

const NOW: i64 = 1_785_400_000_000;

// --- Real Codex evidence --------------------------------------------------

#[test]
fn real_permission_fixtures_prove_approval_join_and_decline_semantics() {
    assert_permission_fixture("03-permission-approve.jsonl", "accept", "completed");
    assert_permission_fixture("04-permission-deny.jsonl", "decline", "declined");

    let deny = fixture_rows("04-permission-deny.jsonl");
    let declined = deny.iter().any(|row| {
        method(row) == Some("item/completed")
            && row
                .pointer("/payload/params/item/status")
                .and_then(Value::as_str)
                == Some("declined")
    });
    let completed_turn = deny
        .iter()
        .find(|row| method(row) == Some("turn/completed"));

    assert!(
        declined,
        "the recorded deny fixture must contain a declined item"
    );
    assert_eq!(
        completed_turn
            .and_then(|row| row.pointer("/payload/params/turn/status"))
            .and_then(Value::as_str),
        Some("completed")
    );
    assert!(
        completed_turn
            .and_then(|row| row.pointer("/payload/params/turn/error"))
            .is_some_and(Value::is_null),
        "decline must not be represented as a turn error"
    );
    assert!(
        deny.iter().all(|row| method(row) != Some("turn/failed")),
        "the recorded Codex surface has no independent turn/failed notification"
    );
}

#[test]
fn all_three_real_fixtures_prove_completion_summaries_are_not_transcripts() {
    for fixture in [
        "01-simple-turn.jsonl",
        "03-permission-approve.jsonl",
        "04-permission-deny.jsonl",
    ] {
        let rows = fixture_rows(fixture);
        let observed: HashSet<String> = rows
            .iter()
            .filter(|row| matches!(method(row), Some("item/started") | Some("item/completed")))
            .filter_map(|row| {
                row.pointer("/payload/params/item/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let completion = rows
            .iter()
            .find(|row| method(row) == Some("turn/completed"))
            .expect("fixture must contain turn/completed");
        assert_eq!(
            completion
                .pointer("/payload/params/turn/itemsView")
                .and_then(Value::as_str),
            Some("summary"),
            "{fixture} must retain the recorded summary marker"
        );
        let summary: HashSet<String> = completion
            .pointer("/payload/params/turn/items")
            .and_then(Value::as_array)
            .expect("completion summary must be an array")
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        assert!(
            !summary.is_empty(),
            "{fixture} summary must contain an item"
        );
        assert!(
            summary.is_subset(&observed),
            "{fixture} summary items must have been observed individually"
        );
        assert!(
            observed.len() > summary.len(),
            "{fixture} proves replacing accumulated items with the completion summary loses data"
        );
    }
}

fn assert_permission_fixture(fixture: &str, decision: &str, final_item_status: &str) {
    let rows = fixture_rows(fixture);
    let request = rows
        .iter()
        .find(|row| method(row) == Some("item/fileChange/requestApproval"))
        .expect("permission fixture must contain an approval request");
    let request_id = request
        .pointer("/payload/id")
        .expect("approval request must carry a request id");
    let target_item_id = request
        .pointer("/payload/params/itemId")
        .and_then(Value::as_str)
        .expect("approval request must carry an item id");
    let session_id = request
        .pointer("/payload/params/threadId")
        .and_then(Value::as_str)
        .expect("approval request must carry a thread id");
    let turn_id = request
        .pointer("/payload/params/turnId")
        .and_then(Value::as_str)
        .expect("approval request must carry a turn id");

    assert!(rows.iter().any(|row| {
        method(row) == Some("item/started")
            && row
                .pointer("/payload/params/item/id")
                .and_then(Value::as_str)
                == Some(target_item_id)
            && row
                .pointer("/payload/params/threadId")
                .and_then(Value::as_str)
                == Some(session_id)
            && row
                .pointer("/payload/params/turnId")
                .and_then(Value::as_str)
                == Some(turn_id)
    }));
    assert!(rows.iter().any(|row| {
        row.pointer("/payload/id") == Some(request_id)
            && row
                .pointer("/payload/result/decision")
                .and_then(Value::as_str)
                == Some(decision)
    }));
    assert!(rows.iter().any(|row| {
        method(row) == Some("item/completed")
            && row
                .pointer("/payload/params/item/id")
                .and_then(Value::as_str)
                == Some(target_item_id)
            && row
                .pointer("/payload/params/item/status")
                .and_then(Value::as_str)
                == Some(final_item_status)
    }));
}

// --- Decline, turn status and item accumulation --------------------------

#[test]
fn declined_item_is_terminal_but_not_a_failure_and_turn_can_complete() {
    assert!(ItemStatus::Declined.is_terminal());
    assert!(!ItemStatus::Declined.is_failure());
    assert!(ItemStatus::Failed.is_failure());

    let turn = completed_turn();
    assert_eq!(turn.status_after_decline(), TurnStatus::Completed);
    assert!(turn.error.is_none());
    assert!(turn.validate().is_ok());
}

#[test]
fn turn_error_and_terminal_invariants_reject_illegal_states() {
    let completed_with_error = Turn {
        error: Some(canonical_error()),
        ..completed_turn()
    };
    assert_eq!(
        completed_with_error.validate(),
        Err(ContractViolation::TurnErrorWithoutFailure {
            status: TurnStatus::Completed
        })
    );

    let failed_without_error = Turn {
        status: TurnStatus::Failed,
        error: None,
        ..completed_turn()
    };
    assert_eq!(
        failed_without_error.validate(),
        Err(ContractViolation::FailedTurnWithoutError)
    );

    let terminal_without_timestamp = Turn {
        completed_at_ms: None,
        ..completed_turn()
    };
    assert_eq!(
        terminal_without_timestamp.validate(),
        Err(ContractViolation::TerminalTurnWithoutTimestamp)
    );

    let duplicate_item = Turn {
        item_ids: vec![item_id(), item_id()],
        ..completed_turn()
    };
    assert_eq!(
        duplicate_item.validate(),
        Err(ContractViolation::DuplicateItemReference)
    );
}

// --- Approval join, structured gates and reply binding -------------------

#[test]
fn approval_join_handles_immediate_delayed_unknown_ambiguous_and_scope_mismatch() {
    let matching = canonical_item();
    let mut immediate = approval(JoinState::Unjoined {
        reason: JoinFailureReason::ItemNotYetSeen,
    });
    immediate.refresh_approval_join(std::slice::from_ref(&matching), false);
    assert!(matches!(
        approval_join(&immediate).expect("helper approval must expose its join"),
        JoinState::Joined { item_id: joined } if joined == &matching.id
    ));

    let mut delayed = approval(JoinState::Unjoined {
        reason: JoinFailureReason::ItemNotYetSeen,
    });
    delayed.refresh_approval_join(&[], false);
    assert_eq!(
        approval_join(&delayed).expect("helper approval must expose its join"),
        &JoinState::Unjoined {
            reason: JoinFailureReason::ItemNotYetSeen
        }
    );
    delayed.refresh_approval_join(std::slice::from_ref(&matching), false);
    assert!(matches!(
        approval_join(&delayed).expect("helper approval must expose its join"),
        JoinState::Joined { .. }
    ));

    let mut unknown = approval(JoinState::Unjoined {
        reason: JoinFailureReason::ItemNotYetSeen,
    });
    unknown.refresh_approval_join(&[], true);
    assert_eq!(
        approval_join(&unknown).expect("helper approval must expose its join"),
        &JoinState::Unjoined {
            reason: JoinFailureReason::ItemUnknown
        }
    );

    let mut other_scope = matching.clone();
    other_scope.session_id = SessionId::new("session-other");
    let mut mismatched = approval(JoinState::Unjoined {
        reason: JoinFailureReason::ItemNotYetSeen,
    });
    mismatched.refresh_approval_join(std::slice::from_ref(&other_scope), false);
    assert_eq!(
        approval_join(&mismatched).expect("helper approval must expose its join"),
        &JoinState::Unjoined {
            reason: JoinFailureReason::ScopeMismatch
        }
    );

    let mut ambiguous = approval(JoinState::Unjoined {
        reason: JoinFailureReason::ItemNotYetSeen,
    });
    ambiguous.refresh_approval_join(&[matching.clone(), matching], false);
    assert_eq!(
        approval_join(&ambiguous).expect("helper approval must expose its join"),
        &JoinState::Unjoined {
            reason: JoinFailureReason::AmbiguousTarget
        }
    );
}

#[test]
fn attention_reply_binds_target_session_key_state_expiry_and_offered_option() {
    let item = approval(JoinState::Joined { item_id: item_id() });
    let response = approval_response();
    assert!(response.validate().is_ok());
    assert!(item.check_reply(&response, NOW).is_ok());

    let wrong_attention = AttentionResponse {
        attention_id: AttentionId::new("attention-other"),
        ..response.clone()
    };
    assert_eq!(
        item.check_reply(&wrong_attention, NOW),
        Err(ReplyRejection::AttentionMismatch)
    );
    let wrong_session = AttentionResponse {
        session_id: Some(SessionId::new("session-other")),
        ..response.clone()
    };
    assert_eq!(
        item.check_reply(&wrong_session, NOW),
        Err(ReplyRejection::SessionMismatch)
    );
    let wrong_key = AttentionResponse {
        request_key: "request-other".to_owned(),
        ..response.clone()
    };
    assert_eq!(
        item.check_reply(&wrong_key, NOW),
        Err(ReplyRejection::RequestKeyMismatch)
    );
    let wrong_expiry = AttentionResponse {
        expected_expires_at_ms: Some(NOW + 9_999),
        ..response.clone()
    };
    assert_eq!(
        item.check_reply(&wrong_expiry, NOW),
        Err(ReplyRejection::ExpiryMismatch)
    );
    let unknown_option = AttentionResponse {
        option_id: Some("invented".to_owned()),
        ..response.clone()
    };
    assert_eq!(
        item.check_reply(&unknown_option, NOW),
        Err(ReplyRejection::UnknownOption)
    );
    assert_eq!(
        item.check_reply(&response, NOW + 1_000),
        Err(ReplyRejection::Expired)
    );

    let mut answered = item;
    answered.state = AttentionState::Answered {
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW,
        answer_source: AttentionAnswerSource::LocalCommand {
            command_id: CommandId::new("command-answered"),
        },
    };
    assert_eq!(
        answered.check_reply(&response, NOW),
        Err(ReplyRejection::NotOpen)
    );
}

#[test]
fn answered_attention_distinguishes_local_commands_from_external_observations() {
    let response = approval_response();

    let mut local = approval(JoinState::Joined { item_id: item_id() });
    local.state = AttentionState::Answered {
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW,
        answer_source: AttentionAnswerSource::LocalCommand {
            command_id: CommandId::new("command-local"),
        },
    };
    assert!(local.validate().is_ok());
    assert_eq!(
        local.check_reply(&response, NOW),
        Err(ReplyRejection::NotOpen)
    );
    let local_json = serde_json::to_value(&local).expect("serialize local answer");
    assert_eq!(
        local_json.pointer("/state/answer_source/kind"),
        Some(&Value::from("local_command"))
    );
    assert_eq!(
        local_json.pointer("/state/answer_source/command_id/value"),
        Some(&Value::from("command-local"))
    );

    let mut external = approval(JoinState::Joined { item_id: item_id() });
    external.state = AttentionState::Answered {
        option_id: Some("decline".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW,
        answer_source: AttentionAnswerSource::ObservedExternal {
            evidence: AttentionAnswerEvidence {
                observer_host_id: host_id(),
                observed_at_ms: NOW,
                source: AttentionAnswerEvidenceSource::RecordedFixture,
            },
        },
    };
    assert!(external.validate().is_ok());
    assert_eq!(
        external.check_reply(&response, NOW),
        Err(ReplyRejection::NotOpen)
    );
    let external_json = serde_json::to_value(&external).expect("serialize external answer");
    assert_eq!(
        external_json.pointer("/state/answer_source/kind"),
        Some(&Value::from("observed_external"))
    );
    assert_eq!(
        external_json.pointer("/state/answer_source/evidence/source/kind"),
        Some(&Value::from("recorded_fixture"))
    );
    assert!(external_json.pointer("/state/command_id").is_none());
    assert!(external_json
        .pointer("/state/answer_source/command_id")
        .is_none());
}

#[test]
fn attention_answer_source_rejects_empty_or_cross_host_evidence_and_old_wire_shape() {
    let mut empty_command = approval(JoinState::Joined { item_id: item_id() });
    empty_command.state = AttentionState::Answered {
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW,
        answer_source: AttentionAnswerSource::LocalCommand {
            command_id: CommandId::new(""),
        },
    };
    assert_eq!(
        empty_command.validate(),
        Err(ContractViolation::EmptyIdentifier {
            field: "attention_state.answer_source.command_id"
        })
    );

    let mut empty_observer = approval(JoinState::Joined { item_id: item_id() });
    empty_observer.state = AttentionState::Answered {
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW,
        answer_source: AttentionAnswerSource::ObservedExternal {
            evidence: AttentionAnswerEvidence {
                observer_host_id: HostId::new(""),
                observed_at_ms: NOW,
                source: AttentionAnswerEvidenceSource::ObservedInTraffic,
            },
        },
    };
    assert_eq!(
        empty_observer.validate(),
        Err(ContractViolation::EmptyIdentifier {
            field: "attention_state.answer_source.evidence.observer_host_id"
        })
    );

    let mut wrong_observer = approval(JoinState::Joined { item_id: item_id() });
    wrong_observer.state = AttentionState::Answered {
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
        decided_at_ms: NOW,
        answer_source: AttentionAnswerSource::ObservedExternal {
            evidence: AttentionAnswerEvidence {
                observer_host_id: HostId::new("host-other"),
                observed_at_ms: NOW,
                source: AttentionAnswerEvidenceSource::ObservedInTraffic,
            },
        },
    };
    assert_eq!(
        wrong_observer.validate(),
        Err(ContractViolation::AttentionAnswerObserverHostMismatch)
    );

    let old_answered = serde_json::json!({
        "kind": "answered",
        "option_id": "accept",
        "free_form_ref": null,
        "decided_at_ms": NOW,
        "command_id": "command-old-wire"
    });
    assert!(serde_json::from_value::<AttentionState>(old_answered).is_err());
    assert!(
        serde_json::from_value::<AttentionAnswerEvidenceSource>(serde_json::json!({
            "kind": "future_source"
        }))
        .is_err()
    );
}

#[test]
fn workflow_gate_is_structured_and_answerable() {
    let gate = workflow_gate_attention();
    let response = AttentionResponse {
        attention_id: gate.id.clone(),
        session_id: None,
        request_key: "workflow-gate-request".to_owned(),
        expected_expires_at_ms: gate.expires_at_ms,
        option_id: Some("accept".to_owned()),
        free_form_ref: None,
    };
    assert!(gate.validate().is_ok());
    assert!(gate.check_reply(&response, NOW).is_ok());
}

// --- History/live/capability separation ---------------------------------

#[test]
fn history_capabilities_cannot_be_promoted_to_live_binding() {
    let history_only = runtime_capabilities(vec![
        capability_entry(Capability::HistoryList, CapabilityState::Supported),
        capability_entry(Capability::HistoryRead, CapabilityState::Supported),
        capability_entry(Capability::HistoryResume, CapabilityState::Supported),
    ]);
    let binding = LiveBinding::Observing {
        runtime_id: runtime_id(),
        since_at_ms: NOW,
        evidence: capability_evidence(EvidenceSource::ObservedInTraffic),
    };
    assert_eq!(
        binding.validate_against(&history_only),
        Err(ContractViolation::LiveBindingUnsupported {
            missing: "live_observe"
        })
    );

    let history = HistorySource {
        kind: HistorySourceKind::ProviderApi,
        runtime_id: Some(runtime_id()),
        evidence: capability_evidence(EvidenceSource::RecordedFixture),
    };
    assert_eq!(history.kind, HistorySourceKind::ProviderApi);
    assert!(!LiveBinding::NotBound {
        reason: LiveUnboundReason::NoPublicAttachPath
    }
    .is_live());
}

#[test]
fn controlling_requires_observe_control_and_observed_traffic() {
    let controlling = LiveBinding::Controlling {
        runtime_id: runtime_id(),
        since_at_ms: NOW,
        evidence: capability_evidence(EvidenceSource::ObservedInTraffic),
    };
    let control_only = runtime_capabilities(vec![capability_entry(
        Capability::LiveControl,
        CapabilityState::Supported,
    )]);
    assert_eq!(
        controlling.validate_against(&control_only),
        Err(ContractViolation::LiveBindingUnsupported {
            missing: "live_observe"
        })
    );

    let observe_only = runtime_capabilities(vec![capability_entry(
        Capability::LiveObserve,
        CapabilityState::Supported,
    )]);
    assert_eq!(
        controlling.validate_against(&observe_only),
        Err(ContractViolation::LiveBindingUnsupported {
            missing: "live_control"
        })
    );

    let both = runtime_capabilities(vec![
        capability_entry(Capability::LiveObserve, CapabilityState::Supported),
        capability_entry(Capability::LiveControl, CapabilityState::Supported),
    ]);
    assert!(controlling.validate_against(&both).is_ok());
    assert!(controlling.accepts_control());

    let unobserved = LiveBinding::Controlling {
        runtime_id: runtime_id(),
        since_at_ms: NOW,
        evidence: capability_evidence(EvidenceSource::HandshakeDeclared),
    };
    assert_eq!(
        unobserved.validate_against(&both),
        Err(ContractViolation::LiveBindingEvidenceNotObserved)
    );
}

#[test]
fn missing_and_non_supported_capabilities_never_permit_use() {
    let negotiated = runtime_capabilities(vec![capability_entry(
        Capability::TurnPrompt,
        CapabilityState::Supported,
    )]);
    assert_eq!(
        negotiated.state_of(&Capability::TurnSteer),
        CapabilityState::NotVerified
    );
    assert!(!negotiated.permits(&Capability::TurnSteer));
    assert!(!CapabilityState::Unsupported.permits_use());
    assert!(!CapabilityState::UnavailableOnThisConnection {
        reason: CapabilityUnavailableReason::RuntimeDisconnected
    }
    .permits_use());
    assert!(!CapabilityState::UpstreamBlocked {
        blocker_id: BlockerId::new("blocker-public-attach")
    }
    .permits_use());
}

#[test]
fn duplicate_capability_entries_are_rejected() {
    let duplicate = runtime_capabilities(vec![
        capability_entry(Capability::TurnPrompt, CapabilityState::Supported),
        capability_entry(Capability::TurnPrompt, CapabilityState::Unsupported),
    ]);
    assert_eq!(
        duplicate.validate(),
        Err(ContractViolation::DuplicateCapability {
            capability: Capability::TurnPrompt
        })
    );
}

// --- Queue, steer proof and command acknowledgement ----------------------

#[test]
fn steer_stays_queued_without_runtime_observation_or_capability() {
    let queued = pending_queue_entry("queue-1", "session-1", 0);
    assert!(!queued.state.reached_runtime());
    assert!(!queued.may_submit(&runtime_capabilities(Vec::new())));

    let handshake_only = delivered_steer(EvidenceSource::HandshakeDeclared);
    assert_eq!(
        handshake_only.validate(),
        Err(ContractViolation::UnprovenSteerDelivery {
            evidence_source: EvidenceSource::HandshakeDeclared
        })
    );
}

#[test]
fn delivered_steer_requires_matching_runtime_session_turn_binding_and_active_turn() {
    let delivered = delivered_steer(EvidenceSource::ObservedInTraffic);
    let steer_capable = runtime_capabilities(vec![capability_entry(
        Capability::TurnSteer,
        CapabilityState::Supported,
    )]);
    assert!(delivered.validate().is_ok());
    assert!(delivered
        .validate_for_active_turn(&turn_id(), &steer_capable)
        .is_ok());
    assert!(delivered.state.reached_runtime());

    assert_eq!(
        delivered.validate_for_active_turn(&TurnId::new("turn-other"), &steer_capable),
        Err(ContractViolation::SteerNotActiveTurn)
    );
    assert_eq!(
        delivered.validate_for_active_turn(&turn_id(), &runtime_capabilities(Vec::new())),
        Err(ContractViolation::SteerCapabilityUnsupported)
    );

    let mut wrong_session = delivered.clone();
    if let QueueState::DeliveredAsSteer { ack, .. } = &mut wrong_session.state {
        ack.session_id = SessionId::new("session-other");
    }
    assert_eq!(
        wrong_session.validate(),
        Err(ContractViolation::SteerSessionMismatch)
    );

    let mut wrong_turn = delivered.clone();
    if let QueueState::DeliveredAsSteer { ack, .. } = &mut wrong_turn.state {
        ack.turn_id = TurnId::new("turn-other");
    }
    assert_eq!(
        wrong_turn.validate(),
        Err(ContractViolation::SteerTurnMismatch)
    );

    let mut wrong_runtime = delivered.clone();
    if let QueueState::DeliveredAsSteer { ack, .. } = &mut wrong_runtime.state {
        ack.runtime_id = ProviderRuntimeId::new("runtime-other");
    }
    assert_eq!(
        wrong_runtime.validate(),
        Err(ContractViolation::SteerRuntimeMismatch)
    );

    let mut wrong_binding = delivered;
    if let QueueState::DeliveredAsSteer { ack, .. } = &mut wrong_binding.state {
        ack.binding_handle.id = ProviderBindingId::new("bnd_other1234");
    }
    assert_eq!(
        wrong_binding.validate(),
        Err(ContractViolation::SteerBindingMismatch)
    );
}

#[test]
fn local_acceptance_and_runtime_acceptance_are_distinct() {
    let local = CommandOutcome::AcceptedLocally { note_ref: None };
    let runtime = CommandOutcome::AcceptedByRuntime {
        binding_handle: binding_handle(ProviderBindingKind::RuntimeAcknowledgement),
    };
    let queued = CommandOutcome::Enqueued {
        entry_id: QueueEntryId::new("queue-1"),
    };
    assert!(!local.reached_runtime());
    assert!(runtime.reached_runtime());
    assert!(!queued.reached_runtime());
    assert!(local.validate().is_ok());
    assert!(runtime.validate().is_ok());
}

#[test]
fn device_command_request_cannot_claim_trusted_envelope_fields() {
    let request = DeviceCommandRequest {
        idempotency_key: "mobile-submit-1".to_owned(),
        ttl_ms: Some(30_000),
        body: Command::SubmitPrompt {
            session_id: session_id(),
            body: sensitive_content("content-mobile-submit", ContentKind::PlainText),
        },
    };
    request.validate().expect("valid device command request");
    let encoded = serde_json::to_value(&request).expect("serialize device command request");
    for forbidden in ["actor", "command_id", "issued_at_ms", "expires_at_ms"] {
        assert!(
            encoded.get(forbidden).is_none(),
            "remote shape exposed {forbidden}"
        );
    }
    assert!(
        serde_json::from_value::<DeviceCommandRequest>(serde_json::json!({
            "idempotency_key": "forged",
            "ttl_ms": 1,
            "body": request.body,
            "actor": { "kind": "broker" },
            "command_id": { "value": "forged-command" },
            "issued_at_ms": NOW
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<Actor>(serde_json::json!({
        "kind": "human",
        "device_label": "legacy-phone"
    }))
    .is_err());

    assert_eq!(
        DeviceCommandRequest {
            idempotency_key: String::new(),
            ttl_ms: None,
            body: Command::CloseSession {
                session_id: session_id(),
            },
        }
        .validate(),
        Err(ContractViolation::EmptyIdentifier {
            field: "device_command.idempotency_key"
        })
    );
    let oversized_key = "界".repeat((MAX_IDEMPOTENCY_KEY_BYTES / 3) + 1);
    assert_eq!(
        DeviceCommandRequest {
            idempotency_key: oversized_key.clone(),
            ttl_ms: None,
            body: Command::CloseSession {
                session_id: session_id(),
            },
        }
        .validate(),
        Err(ContractViolation::IdempotencyKeyTooLong {
            byte_len: oversized_key.len()
        })
    );
    for ttl_ms in [0, MAX_DEVICE_COMMAND_TTL_MS + 1] {
        assert_eq!(
            DeviceCommandRequest {
                idempotency_key: "ttl-boundary".to_owned(),
                ttl_ms: Some(ttl_ms),
                body: Command::CloseSession {
                    session_id: session_id(),
                },
            }
            .validate(),
            Err(ContractViolation::InvalidDeviceCommandTtl { ttl_ms })
        );
    }

    let first = CommandEnvelope {
        command_id: CommandId::new("command-first"),
        idempotency_key: "same-key".to_owned(),
        actor: Actor::Human {
            device_id: DeviceId::new("device-first"),
        },
        issued_at_ms: NOW,
        expires_at_ms: None,
        body: Command::CloseSession {
            session_id: session_id(),
        },
    };
    let second = CommandEnvelope {
        actor: Actor::Human {
            device_id: DeviceId::new("device-second"),
        },
        ..first.clone()
    };
    assert_ne!(first.dedupe_key(), second.dedupe_key());
    let separator_collision_left = CommandEnvelope {
        actor: Actor::Human {
            device_id: DeviceId::new("a"),
        },
        idempotency_key: "b|c".to_owned(),
        ..first.clone()
    };
    let separator_collision_right = CommandEnvelope {
        actor: Actor::Human {
            device_id: DeviceId::new("a|b"),
        },
        idempotency_key: "c".to_owned(),
        ..first.clone()
    };
    assert_ne!(
        separator_collision_left.dedupe_key(),
        separator_collision_right.dedupe_key(),
        "typed length prefixes must keep arbitrary UTF-8 actor IDs and keys injective"
    );
    assert_eq!(
        CommandEnvelope {
            actor: Actor::Human {
                device_id: DeviceId::new("")
            },
            ..first
        }
        .validate(),
        Err(ContractViolation::EmptyIdentifier {
            field: "actor.device_id"
        })
    );
}

#[test]
fn queue_reorder_requires_the_exact_pending_set_of_one_session() {
    let first = pending_queue_entry("queue-1", "session-1", 0);
    let second = pending_queue_entry("queue-2", "session-1", 1);
    let other_session = pending_queue_entry("queue-3", "session-2", 0);
    let non_pending = QueueEntry {
        id: QueueEntryId::new("queue-4"),
        state: QueueState::Cancelled { at_ms: NOW },
        editable: false,
        ..pending_queue_entry("queue-4", "session-1", 2)
    };
    let entries = vec![
        first.clone(),
        second.clone(),
        other_session.clone(),
        non_pending.clone(),
    ];
    let session = session_id();

    assert!(
        validate_queue_reorder(&session, &[second.id.clone(), first.id.clone()], &entries).is_ok()
    );
    assert_eq!(
        validate_queue_reorder(&session, std::slice::from_ref(&first.id), &entries),
        Err(ContractViolation::QueueReorderMissingPending)
    );
    assert_eq!(
        validate_queue_reorder(&session, &[first.id.clone(), first.id.clone()], &entries),
        Err(ContractViolation::QueueReorderDuplicate)
    );
    assert_eq!(
        validate_queue_reorder(
            &session,
            &[first.id.clone(), second.id.clone(), other_session.id],
            &entries
        ),
        Err(ContractViolation::QueueReorderCrossSession)
    );
    assert_eq!(
        validate_queue_reorder(
            &session,
            &[first.id.clone(), second.id.clone(), non_pending.id],
            &entries
        ),
        Err(ContractViolation::QueueReorderNonPending)
    );
    assert_eq!(
        validate_queue_reorder(
            &session,
            &[first.id, second.id, QueueEntryId::new("queue-not-known")],
            &entries
        ),
        Err(ContractViolation::QueueReorderUnknownEntry)
    );
}

// --- Cursor, four streams, snapshot and replay ---------------------------

#[test]
fn cursor_sequence_rejects_repeat_gap_cross_stream_and_overflow() {
    assert!(verify_contiguous(&[
        log_record(session_stream(), 1),
        log_record(session_stream(), 2),
        log_record(session_stream(), 3),
    ])
    .is_ok());
    assert_eq!(
        verify_contiguous(&[
            log_record(session_stream(), 1),
            log_record(session_stream(), 1)
        ]),
        Err(ContractViolation::CursorRepeated { cursor: 1 })
    );
    assert_eq!(
        verify_contiguous(&[
            log_record(session_stream(), 1),
            log_record(session_stream(), 3)
        ]),
        Err(ContractViolation::CursorGap {
            expected: 2,
            found: 3
        })
    );
    assert_eq!(
        verify_contiguous(&[
            log_record(session_stream(), 1),
            log_record(
                StreamKey::Project {
                    project_id: project_id()
                },
                2
            )
        ]),
        Err(ContractViolation::MixedStreams)
    );
    assert_eq!(
        Cursor { seq: u64::MAX }.next(),
        Err(ContractViolation::CursorOverflow)
    );
    assert!(Cursor { seq: 2 }.follows(Cursor { seq: 1 }));
    assert!(!Cursor { seq: 3 }.follows(Cursor { seq: 1 }));
}

#[test]
fn host_project_session_and_workflow_snapshots_converge_with_replay() {
    for snapshot in all_snapshots() {
        snapshot
            .validate()
            .expect("snapshot must be internally valid");
        let replay = vec![
            log_record(snapshot.stream.clone(), 11),
            log_record(snapshot.stream.clone(), 12),
        ];
        validate_replay_window(&snapshot, &replay)
            .expect("the immediate contiguous replay must converge");
        assert_json_roundtrip(&snapshot);
        for record in replay {
            assert_json_roundtrip(&record);
        }
    }
}

#[test]
fn snapshot_replay_rejects_repeat_gap_cross_stream_and_overflow() {
    let snapshot = session_snapshot_envelope(10);
    assert_eq!(
        validate_replay_window(
            &snapshot,
            &[log_record(snapshot.stream.clone(), snapshot.cursor.seq)]
        ),
        Err(ContractViolation::CursorRepeated { cursor: 10 })
    );
    assert_eq!(
        validate_replay_window(&snapshot, &[log_record(snapshot.stream.clone(), 12)]),
        Err(ContractViolation::CursorGap {
            expected: 11,
            found: 12
        })
    );
    assert_eq!(
        validate_replay_window(
            &snapshot,
            &[log_record(StreamKey::Host { host_id: host_id() }, 11)]
        ),
        Err(ContractViolation::MixedStreams)
    );

    let overflow = session_snapshot_envelope(u64::MAX);
    assert_eq!(
        validate_replay_window(&overflow, &[log_record(overflow.stream.clone(), u64::MAX)]),
        Err(ContractViolation::CursorOverflow)
    );
}

#[test]
fn snapshot_payload_must_match_its_stream() {
    let mut envelope = session_snapshot_envelope(10);
    envelope.stream = StreamKey::Project {
        project_id: project_id(),
    };
    assert_eq!(
        envelope.validate(),
        Err(ContractViolation::SnapshotStreamMismatch)
    );
}

// --- Sensitive content, paths and provider-private identifiers -----------

#[test]
fn sensitive_content_and_unsafe_preview_cannot_enter_log_or_projection() {
    let sensitive_preview = ContentRef {
        preview: Some("secret".to_owned()),
        ..sensitive_content("content-sensitive-preview", ContentKind::PlainText)
    };
    assert_eq!(
        sensitive_preview.validate(),
        Err(ContractViolation::SensitivePreview)
    );

    let path_preview = ContentRef {
        preview: Some(r"C:\Users\Alice\private\worktree".to_owned()),
        ..business_content("content-path-preview", "safe")
    };
    assert_eq!(
        path_preview.validate(),
        Err(ContractViolation::UnsafePreview)
    );

    let unsafe_entry = QueueEntry {
        body: path_preview,
        ..pending_queue_entry("queue-unsafe", "session-1", 0)
    };
    let unsafe_effect = StateEffect::QueueEntryUpserted {
        entry: unsafe_entry.clone(),
    };
    assert_eq!(
        unsafe_effect.validate_for_log(),
        Err(ContractViolation::UnsafePreview)
    );
    let unsafe_projection = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::InputQueue {
            session_id: session_id(),
        },
        cursor: Cursor { seq: 1 },
        payload: ProjectionPayload::InputQueue {
            view: InputQueueView {
                session_id: session_id(),
                entries: vec![unsafe_entry],
                writable: true,
                steer_supported: false,
            },
        },
    };
    assert_eq!(
        unsafe_projection.validate_for_transport(),
        Err(ContractViolation::UnsafePreview)
    );
}

#[test]
fn raw_upstream_ids_are_rejected_in_logs_and_projections() {
    let raw_session_handle = ProviderBindingHandle {
        id: ProviderBindingId::new("019fb1e8-2927-7cd1-a740-fbbef7e40608"),
        runtime_id: runtime_id(),
        kind: ProviderBindingKind::Session,
    };
    assert_eq!(
        raw_session_handle.validate(),
        Err(ContractViolation::InvalidProviderBindingId)
    );

    let unsafe_session = Session {
        binding_handle: Some(raw_session_handle),
        ..canonical_session()
    };
    assert_eq!(
        StateEffect::SessionUpserted {
            session: unsafe_session
        }
        .validate_for_log(),
        Err(ContractViolation::InvalidProviderBindingId)
    );

    let raw_ack_handle = ProviderBindingHandle {
        id: ProviderBindingId::new("call_VPQYSRFTv9gqoAPa1eKIzBIv"),
        runtime_id: runtime_id(),
        kind: ProviderBindingKind::RuntimeAcknowledgement,
    };
    let mut unsafe_queue_entry = delivered_steer(EvidenceSource::ObservedInTraffic);
    if let QueueState::DeliveredAsSteer {
        binding_handle,
        ack,
        ..
    } = &mut unsafe_queue_entry.state
    {
        *binding_handle = raw_ack_handle.clone();
        ack.binding_handle = raw_ack_handle;
    }
    let unsafe_projection = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::InputQueue {
            session_id: session_id(),
        },
        cursor: Cursor { seq: 1 },
        payload: ProjectionPayload::InputQueue {
            view: InputQueueView {
                session_id: session_id(),
                entries: vec![unsafe_queue_entry],
                writable: true,
                steer_supported: true,
            },
        },
    };
    assert_eq!(
        unsafe_projection.validate_for_transport(),
        Err(ContractViolation::InvalidProviderBindingId)
    );

    let safe_json = serde_json::to_string(&StateEffect::SessionUpserted {
        session: canonical_session(),
    })
    .expect("safe effect must serialize");
    assert!(!safe_json.contains("external_ref"));
    assert!(!safe_json.contains("019fb1e8-2927-7cd1-a740-fbbef7e40608"));
    assert!(!safe_json.contains(r"C:\Users\Alice"));
}

#[test]
fn binding_handles_cannot_cross_canonical_entity_kinds() {
    let wrong_session = Session {
        binding_handle: Some(binding_handle(ProviderBindingKind::Item)),
        ..canonical_session()
    };
    assert_eq!(
        wrong_session.validate_shape(),
        Err(ContractViolation::ProviderBindingKindMismatch {
            expected: ProviderBindingKind::Session,
            actual: ProviderBindingKind::Item
        })
    );

    let mut wrong_request = approval(JoinState::Joined { item_id: item_id() });
    if let AttentionSubject::Approval { request } = &mut wrong_request.subject {
        request.binding_handle.kind = ProviderBindingKind::Session;
    }
    assert_eq!(
        wrong_request.validate(),
        Err(ContractViolation::ProviderBindingKindMismatch {
            expected: ProviderBindingKind::InteractionRequest,
            actual: ProviderBindingKind::Session
        })
    );

    let wrong_runtime_ack = CommandOutcome::AcceptedByRuntime {
        binding_handle: binding_handle(ProviderBindingKind::Turn),
    };
    assert_eq!(
        wrong_runtime_ack.validate(),
        Err(ContractViolation::ProviderBindingKindMismatch {
            expected: ProviderBindingKind::RuntimeAcknowledgement,
            actual: ProviderBindingKind::Turn
        })
    );

    let mut wrong_steer_ack = delivered_steer(EvidenceSource::ObservedInTraffic);
    if let QueueState::DeliveredAsSteer {
        binding_handle,
        ack,
        ..
    } = &mut wrong_steer_ack.state
    {
        binding_handle.kind = ProviderBindingKind::Item;
        ack.binding_handle.kind = ProviderBindingKind::Item;
    }
    assert_eq!(
        wrong_steer_ack.validate(),
        Err(ContractViolation::ProviderBindingKindMismatch {
            expected: ProviderBindingKind::RuntimeAcknowledgement,
            actual: ProviderBindingKind::Item
        })
    );
}

#[test]
fn project_root_worktree_reasons_and_diagnostics_require_sensitive_content_refs() {
    let unsafe_root = Project {
        bindings: vec![ProjectBinding {
            root_ref: business_content("content-root", "repository"),
            ..project_binding()
        }],
        ..project()
    };
    assert_eq!(
        unsafe_root.validate(),
        Err(ContractViolation::SensitiveContentRequired {
            field: "project_binding.root_ref"
        })
    );

    let unsafe_assignment = StepAssignment {
        worktree_ref: business_content("content-worktree", "worktree"),
        ..step_assignment()
    };
    assert_eq!(
        unsafe_assignment.validate(),
        Err(ContractViolation::SensitiveContentRequired {
            field: "step_assignment.worktree_ref"
        })
    );

    let diagnostic = DiagnosticRecord {
        runtime_id: Some(runtime_id()),
        session_id: Some(session_id()),
        code: DiagnosticCode::MalformedProviderMessage,
        count: 1,
        first_at_ms: NOW,
        last_at_ms: NOW,
        detail_ref: Some(business_content("content-diagnostic", "detail")),
    };
    assert_eq!(
        diagnostic.validate(),
        Err(ContractViolation::SensitiveContentRequired {
            field: "diagnostic.detail_ref"
        })
    );
}

#[test]
fn content_ref_has_a_bounded_read_path_and_explicit_unavailable_state() {
    let request = ContentReadRequest {
        content_id: ContentId::new("content-message"),
        offset: 0,
        max_bytes: 4096,
    };
    assert!(request.validate().is_ok());
    assert_eq!(
        ContentReadRequest {
            max_bytes: 0,
            ..request.clone()
        }
        .validate(),
        Err(ContractViolation::InvalidContentReadSize { max_bytes: 0 })
    );
    assert_eq!(
        ContentReadRequest {
            max_bytes: MAX_CONTENT_READ_BYTES + 1,
            ..request.clone()
        }
        .validate(),
        Err(ContractViolation::InvalidContentReadSize {
            max_bytes: MAX_CONTENT_READ_BYTES + 1
        })
    );

    let response = ContentReadResponse::Chunk {
        chunk: ContentReadChunk {
            content_id: request.content_id.clone(),
            offset: 0,
            bytes: vec![1, 2, 3],
            next_offset: None,
            eof: true,
            digest: digest(),
        },
    };
    assert!(response.validate().is_ok());
    assert_json_roundtrip(&response);

    let bad_continuation = ContentReadChunk {
        content_id: request.content_id.clone(),
        offset: 7,
        bytes: vec![1, 2, 3],
        next_offset: Some(11),
        eof: false,
        digest: digest(),
    };
    assert_eq!(
        bad_continuation.validate(),
        Err(ContractViolation::ContentReadOffsetMismatch {
            expected: 10,
            found: Some(11)
        })
    );
    assert_eq!(
        ContentReadChunk {
            offset: u64::MAX,
            bytes: vec![1],
            next_offset: None,
            eof: false,
            ..bad_continuation.clone()
        }
        .validate(),
        Err(ContractViolation::ContentReadOffsetOverflow)
    );
    assert_eq!(
        ContentReadChunk {
            next_offset: Some(10),
            eof: true,
            ..bad_continuation
        }
        .validate(),
        Err(ContractViolation::ContentReadEofHasNext)
    );
    assert_eq!(
        ContentReadChunk {
            content_id: request.content_id.clone(),
            offset: 0,
            bytes: vec![0; MAX_CONTENT_READ_BYTES as usize + 1],
            next_offset: None,
            eof: true,
            digest: digest(),
        }
        .validate(),
        Err(ContractViolation::ContentReadChunkTooLarge {
            byte_len: MAX_CONTENT_READ_BYTES as usize + 1
        })
    );

    let unavailable = ContentReadResponse::Unavailable {
        content_id: request.content_id,
        reason: ContentUnavailableReason::Evicted,
    };
    assert!(unavailable.validate().is_ok());
    assert_json_roundtrip(&unavailable);
}

#[test]
fn content_write_metadata_is_bounded_and_stored_sensitive_without_preview() {
    let request = ContentWriteRequest {
        content_kind: ContentKind::Markdown,
        byte_len: 32,
        digest: digest(),
    };
    request.validate().expect("valid content write metadata");
    assert_json_roundtrip(&request);

    let stored_ref = ContentRef {
        content_id: ContentId::new("content-uploaded"),
        kind: request.content_kind.clone(),
        byte_len: request.byte_len,
        digest: request.digest.clone(),
        preview: None,
        sensitivity: Sensitivity::Sensitive,
        availability: ContentAvailability::Stored,
    };
    let response = ContentWriteResponse::Stored {
        content_ref: stored_ref.clone(),
    };
    response
        .validate_for(&request)
        .expect("stored response must bind request metadata");
    assert_json_roundtrip(&response);

    for byte_len in [0, MAX_CONTENT_WRITE_BYTES + 1] {
        assert_eq!(
            ContentWriteRequest {
                byte_len,
                ..request.clone()
            }
            .validate(),
            Err(ContractViolation::InvalidContentWriteSize { byte_len })
        );
    }
    assert_eq!(
        ContentWriteRequest {
            content_kind: ContentKind::ToolArguments,
            ..request.clone()
        }
        .validate(),
        Err(ContractViolation::UnsupportedContentWriteKind {
            content_kind: ContentKind::ToolArguments
        })
    );
    assert!(matches!(
        ContentWriteRequest {
            digest: "sha256:ABC".to_owned(),
            ..request.clone()
        }
        .validate(),
        Err(ContractViolation::MalformedDigest { .. })
    ));
    assert!(
        serde_json::from_value::<ContentWriteRequest>(serde_json::json!({
            "content_kind": "markdown",
            "byte_len": 32,
            "digest": digest(),
            "bytes": [1, 2, 3],
            "preview": "must not be accepted",
            "sensitivity": "business"
        }))
        .is_err()
    );

    assert_eq!(
        ContentWriteResponse::Stored {
            content_ref: ContentRef {
                sensitivity: Sensitivity::Business,
                ..stored_ref.clone()
            }
        }
        .validate_for(&request),
        Err(ContractViolation::SensitiveContentRequired {
            field: "content_write.content_ref"
        })
    );
    assert_eq!(
        ContentWriteResponse::Stored {
            content_ref: ContentRef {
                preview: Some("secret".to_owned()),
                ..stored_ref.clone()
            }
        }
        .validate_for(&request),
        Err(ContractViolation::SensitivePreview)
    );
    assert_eq!(
        ContentWriteResponse::Stored {
            content_ref: ContentRef {
                availability: ContentAvailability::Evicted,
                ..stored_ref.clone()
            }
        }
        .validate_for(&request),
        Err(ContractViolation::InvalidContentWriteAvailability {
            availability: ContentAvailability::Evicted
        })
    );
    for mismatched in [
        ContentRef {
            kind: ContentKind::PlainText,
            ..stored_ref.clone()
        },
        ContentRef {
            byte_len: request.byte_len + 1,
            ..stored_ref.clone()
        },
        ContentRef {
            digest: format!("sha256:{}", "1".repeat(64)),
            ..stored_ref
        },
    ] {
        assert_eq!(
            ContentWriteResponse::Stored {
                content_ref: mismatched
            }
            .validate_for(&request),
            Err(ContractViolation::ContentWriteResponseMismatch)
        );
    }

    let rejected = ContentWriteResponse::Rejected {
        error: canonical_error(),
    };
    rejected
        .validate_for(&request)
        .expect("structured rejection must validate");
    assert_json_roundtrip(&rejected);
}

// --- Workflow dependencies, gates and every manual action ----------------

#[test]
fn workflow_step_reports_dependency_capability_gate_and_state_blockers() {
    let mut blocked_step = step();
    blocked_step.id = StepId::new("step-2");
    blocked_step.depends_on = vec![StepId::new("step-1")];
    blocked_step.assignment.selector.required =
        vec![Capability::TurnPrompt, Capability::LiveObserve];
    blocked_step.human_gate = Some(AttentionId::new("attention-gate"));
    let negotiated = runtime_capabilities(vec![capability_entry(
        Capability::TurnPrompt,
        CapabilityState::Supported,
    )]);

    let blockers = blocked_step.blockers(
        &[(StepId::new("step-1"), StepState::Running)],
        &negotiated,
        true,
    );
    assert!(blockers.contains(&StepBlocker::DependencyIncomplete {
        step_id: StepId::new("step-1")
    }));
    assert!(blockers.contains(&StepBlocker::CapabilityNotSupported {
        capability: Capability::LiveObserve
    }));
    assert!(blockers.contains(&StepBlocker::HumanGateOpen {
        attention_id: AttentionId::new("attention-gate")
    }));

    blocked_step.state = StepState::Blocked;
    assert!(blocked_step.blockers(&[], &negotiated, false).contains(
        &StepBlocker::NotSchedulable {
            state: StepState::Blocked
        }
    ));

    blocked_step.state = StepState::Ready;
    let ready = runtime_capabilities(vec![
        capability_entry(Capability::TurnPrompt, CapabilityState::Supported),
        capability_entry(Capability::LiveObserve, CapabilityState::Supported),
    ]);
    assert!(blocked_step
        .blockers(
            &[(StepId::new("step-1"), StepState::Completed)],
            &ready,
            false
        )
        .is_empty());
}

#[test]
fn workflow_manual_advance_retry_rework_skip_cancel_and_reassign_are_closed() {
    let valid = [
        (StepState::Draft, WorkflowAction::Advance, StepState::Ready),
        (StepState::Failed, WorkflowAction::Retry, StepState::Ready),
        (StepState::Review, WorkflowAction::Rework, StepState::Rework),
        (StepState::Blocked, WorkflowAction::Skip, StepState::Skipped),
        (
            StepState::Running,
            WorkflowAction::Cancel,
            StepState::Cancelled,
        ),
        (StepState::Ready, WorkflowAction::Reassign, StepState::Ready),
    ];
    for (from, action, to) in valid {
        assert!(
            validate_transition(from, action, to).is_ok(),
            "{action:?} must permit the documented transition"
        );
    }

    let invalid = [
        (
            StepState::Completed,
            WorkflowAction::Retry,
            StepState::Ready,
        ),
        (
            StepState::Running,
            WorkflowAction::Rework,
            StepState::Rework,
        ),
        (StepState::Running, WorkflowAction::Skip, StepState::Skipped),
        (
            StepState::Completed,
            WorkflowAction::Cancel,
            StepState::Cancelled,
        ),
        (
            StepState::Running,
            WorkflowAction::Reassign,
            StepState::Running,
        ),
        (StepState::Review, WorkflowAction::Advance, StepState::Ready),
    ];
    for (from, action, to) in invalid {
        assert_eq!(
            validate_transition(from, action, to),
            Err(ContractViolation::WorkflowTransitionNotAllowed)
        );
    }
}

#[test]
fn workflow_retry_attempt_cannot_wrap() {
    let mut exhausted = Step {
        state: StepState::Failed,
        attempt: u32::MAX,
        ..step()
    };
    assert_eq!(
        exhausted.validate_transition(WorkflowAction::Retry, StepState::Ready),
        Err(ContractViolation::WorkflowAttemptOverflow)
    );
    assert_eq!(
        exhausted.increment_attempt(),
        Err(ContractViolation::WorkflowAttemptOverflow)
    );
}

// --- Unknown upstream traffic and closed protocol enums ------------------

#[test]
fn unknown_provider_message_becomes_diagnostic_without_fabricating_support() {
    let unknown: Value = serde_json::from_str(
        r#"{"method":"future/provider/message","params":{"opaque":"not interpreted"}}"#,
    )
    .expect("unknown upstream traffic is still valid JSON");
    let code = match unknown.get("method").and_then(Value::as_str) {
        Some("future/provider/message") => DiagnosticCode::UnknownUpstreamMessage,
        _ => DiagnosticCode::MalformedProviderMessage,
    };
    let effect = StateEffect::DiagnosticRecorded {
        diagnostic: DiagnosticRecord {
            runtime_id: Some(runtime_id()),
            session_id: None,
            code,
            count: 1,
            first_at_ms: NOW,
            last_at_ms: NOW,
            detail_ref: None,
        },
    };
    assert!(effect.validate_for_log().is_ok());

    let capabilities = runtime_capabilities(Vec::new());
    assert_eq!(
        capabilities.state_of(&Capability::TurnSteer),
        CapabilityState::NotVerified
    );
    assert!(serde_json::from_str::<StateEffect>(r#"{"kind":"future_effect"}"#).is_err());
    assert!(serde_json::from_str::<MessagePhase>(r#""future_phase""#).is_err());
}

// --- Version boundary and projection refresh -----------------------------

#[test]
fn pre_one_compatibility_is_limited_to_the_zero_three_line() {
    assert_eq!(PROTOCOL_VERSION, "0.3.0");
    assert!(version_is_compatible("0.3.0"));
    assert!(version_is_compatible("0.3.999"));
    assert!(!version_is_compatible("0.0.9"));
    assert!(!version_is_compatible("0.1.999"));
    assert!(!version_is_compatible("0.2.999"));
    assert!(!version_is_compatible("1.0.0"));
    assert!(!version_is_compatible("0.2"));
    assert!(!version_is_compatible("0.2.0.1"));
    assert!(!version_is_compatible("not-a-version"));
}

#[test]
fn stale_projection_version_requires_full_refresh() {
    let mut projection = all_projections()
        .into_iter()
        .next()
        .expect("projection set must not be empty");
    assert!(!projection.requires_full_refresh());
    projection.projection_version += 1;
    assert!(projection.requires_full_refresh());
}

#[test]
fn every_projection_key_matches_only_its_payload_and_exact_scope() {
    let projections = all_projections();
    let keys = projections
        .iter()
        .map(|projection| projection.key.clone())
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 8);

    for (payload_index, projection) in projections.iter().enumerate() {
        for (key_index, key) in keys.iter().enumerate() {
            let candidate = ProjectionEnvelope {
                key: key.clone(),
                ..projection.clone()
            };
            if payload_index == key_index {
                candidate
                    .validate_for_transport()
                    .expect("matching key and payload must validate");
            } else {
                assert_eq!(
                    candidate.validate_for_transport(),
                    Err(ContractViolation::ProjectionKeyPayloadMismatch)
                );
            }
        }
    }

    let runtime = projections
        .last()
        .expect("runtime projection fixture")
        .clone();
    for key in [
        ProjectionKey::RuntimeCapability {
            host_id: HostId::new("host-other"),
            runtime_id: runtime_id(),
        },
        ProjectionKey::RuntimeCapability {
            host_id: host_id(),
            runtime_id: ProviderRuntimeId::new("runtime-other"),
        },
    ] {
        assert_eq!(
            ProjectionEnvelope {
                key,
                ..runtime.clone()
            }
            .validate_for_transport(),
            Err(ContractViolation::ProjectionKeyPayloadMismatch)
        );
    }
    assert!(serde_json::from_str::<ProjectionKey>(r#"{"kind":"future_projection"}"#).is_err());
}

#[test]
fn projection_subscribe_decision_covers_resume_current_ahead_floor_and_overflow() {
    let key = ProjectionKey::Transcript {
        session_id: session_id(),
    };
    let request = |since| ProjectionSubscribe {
        key: key.clone(),
        since,
    };
    let floor = Cursor { seq: 5 };
    let head = Cursor { seq: 10 };

    assert_eq!(
        decide_projection_subscription(&request(None), floor, head, NOW)
            .expect("initial subscription"),
        ProjectionSubscribeAck {
            key: key.clone(),
            outcome: ProjectionSubscribeOutcome::CurrentFollows {
                current_cursor: head
            }
        }
    );
    for (since, from_cursor) in [(10, 11), (5, 6), (4, 5)] {
        assert_eq!(
            decide_projection_subscription(&request(Some(Cursor { seq: since })), floor, head, NOW)
                .expect("retained resume"),
            ProjectionSubscribeAck {
                key: key.clone(),
                outcome: ProjectionSubscribeOutcome::Resumed {
                    from_cursor: Cursor { seq: from_cursor }
                }
            }
        );
    }
    assert_eq!(
        decide_projection_subscription(&request(Some(Cursor { seq: 3 })), floor, head, NOW)
            .expect("cursor before retained predecessor needs current"),
        ProjectionSubscribeAck {
            key: key.clone(),
            outcome: ProjectionSubscribeOutcome::CurrentFollows {
                current_cursor: head
            }
        }
    );
    let ahead =
        decide_projection_subscription(&request(Some(Cursor { seq: 11 })), floor, head, NOW)
            .expect("ahead is a structured rejection");
    assert!(matches!(
        ahead.outcome,
        ProjectionSubscribeOutcome::Rejected {
            error: CanonicalError {
                code: ErrorCode::CursorGap,
                retriable: true,
                ..
            }
        }
    ));
    assert_eq!(
        decide_projection_subscription(&request(None), Cursor { seq: 11 }, head, NOW),
        Err(ContractViolation::InvalidProjectionCursorWindow {
            floor: 11,
            head: 10
        })
    );
    let overflow = decide_projection_subscription(
        &request(Some(Cursor { seq: u64::MAX })),
        Cursor { seq: u64::MAX },
        Cursor { seq: u64::MAX },
        NOW,
    )
    .expect("cursor overflow is a structured wire rejection");
    assert!(matches!(
        overflow.outcome,
        ProjectionSubscribeOutcome::Rejected {
            error: CanonicalError {
                code: ErrorCode::CursorGap,
                retriable: true,
                ..
            }
        }
    ));

    let mut cross_key = decide_projection_subscription(&request(None), floor, head, NOW)
        .expect("valid acknowledgement");
    cross_key.key = ProjectionKey::LiveActivity {
        session_id: session_id(),
    };
    assert_eq!(
        cross_key.validate_for(&request(None)),
        Err(ContractViolation::ProjectionSubscribeKeyMismatch)
    );

    let resumed_request = request(Some(Cursor { seq: 5 }));
    let resumed = decide_projection_subscription(&resumed_request, floor, head, NOW)
        .expect("resumed acknowledgement");
    resumed
        .validate_for(&resumed_request)
        .expect("decision helper produces a bound resume cursor");
    assert_eq!(
        ProjectionSubscribeAck {
            key: key.clone(),
            outcome: ProjectionSubscribeOutcome::Resumed {
                from_cursor: Cursor { seq: 7 }
            }
        }
        .validate_for(&resumed_request),
        Err(ContractViolation::ProjectionResumeCursorMismatch {
            expected: 6,
            found: 7
        })
    );
    assert_eq!(
        ProjectionSubscribeAck {
            key: key.clone(),
            outcome: ProjectionSubscribeOutcome::Resumed {
                from_cursor: Cursor { seq: 1 }
            }
        }
        .validate_for(&request(None)),
        Err(ContractViolation::ProjectionResumeWithoutCursor)
    );
    assert_eq!(
        ProjectionSubscribeAck {
            key: key.clone(),
            outcome: ProjectionSubscribeOutcome::CurrentFollows {
                current_cursor: Cursor { seq: 5 }
            }
        }
        .validate_for(&request(Some(Cursor { seq: 10 }))),
        Err(ContractViolation::ProjectionCurrentCursorNotAhead {
            since: 10,
            current: 5
        })
    );
}

#[test]
fn projection_sequence_rejects_repeat_gap_mixed_key_and_overflow() {
    let mut first = all_projections()
        .get(2)
        .expect("transcript projection fixture")
        .clone();
    first.cursor = Cursor { seq: 6 };
    let mut second = first.clone();
    second.cursor = Cursor { seq: 7 };
    assert!(
        validate_projection_sequence(&first.key, Cursor { seq: 5 }, &[first.clone(), second])
            .is_ok()
    );

    let mut repeated = first.clone();
    repeated.cursor = Cursor { seq: 5 };
    assert_eq!(
        validate_projection_sequence(&first.key, Cursor { seq: 5 }, &[repeated]),
        Err(ContractViolation::CursorRepeated { cursor: 5 })
    );
    let mut gap = first.clone();
    gap.cursor = Cursor { seq: 7 };
    assert_eq!(
        validate_projection_sequence(&first.key, Cursor { seq: 5 }, &[gap]),
        Err(ContractViolation::CursorGap {
            expected: 6,
            found: 7
        })
    );
    let mut other = all_projections()
        .get(3)
        .expect("live activity projection fixture")
        .clone();
    other.cursor = Cursor { seq: 6 };
    assert_eq!(
        validate_projection_sequence(&first.key, Cursor { seq: 5 }, &[other]),
        Err(ContractViolation::MixedProjectionKeys)
    );
    first.cursor = Cursor::START;
    let key = first.key.clone();
    assert_eq!(
        validate_projection_sequence(&key, Cursor { seq: u64::MAX }, &[first]),
        Err(ContractViolation::CursorOverflow)
    );
}

// --- Exhaustive JSON round trips ----------------------------------------

#[test]
fn every_state_effect_variant_round_trips_and_validates_for_log() {
    let effects = all_state_effects();
    assert_eq!(
        effects.len(),
        16,
        "update this test when StateEffect changes"
    );
    for effect in effects {
        effect
            .validate_for_log()
            .expect("round-trip fixture effect must be valid for the durable log");
        assert_json_roundtrip(&effect);
    }
}

#[test]
fn every_command_variant_round_trips_in_an_envelope() {
    let commands = all_commands();
    assert_eq!(commands.len(), 18, "update this test when Command changes");
    for (index, command) in commands.into_iter().enumerate() {
        let envelope = CommandEnvelope {
            command_id: CommandId::new(format!("command-{index}")),
            idempotency_key: format!("idempotency-{index}"),
            actor: Actor::Human {
                device_id: DeviceId::new("device-phone"),
            },
            issued_at_ms: NOW,
            expires_at_ms: Some(NOW + 60_000),
            body: command,
        };
        envelope.validate().expect("command fixture must be valid");
        assert_json_roundtrip(&envelope);
    }
}

#[test]
fn every_projection_variant_round_trips_and_is_transport_safe() {
    let projections = all_projections();
    assert_eq!(
        projections.len(),
        8,
        "update this test when ProjectionPayload changes"
    );
    for projection in projections {
        projection
            .validate_for_transport()
            .expect("projection fixture must be transport safe");
        assert_json_roundtrip(&projection);
    }
}

#[test]
fn every_snapshot_variant_round_trips_and_validates() {
    let snapshots = all_snapshots();
    assert_eq!(
        snapshots.len(),
        4,
        "update this test when SnapshotPayload changes"
    );
    for snapshot in snapshots {
        snapshot.validate().expect("snapshot fixture must be valid");
        assert_json_roundtrip(&snapshot);
    }
}

// --- Other product-semantic validators ----------------------------------

#[test]
fn session_status_precedence_keeps_queue_and_attention_distinct() {
    let base = StatusInputs {
        runtime_usable: true,
        has_active_turn: false,
        open_approval: false,
        open_question: false,
        pending_queue_entries: 0,
        terminal: None,
    };
    assert_eq!(
        derive_session_status(StatusInputs {
            runtime_usable: false,
            has_active_turn: true,
            open_approval: true,
            ..base
        }),
        SessionStatus::Offline
    );
    assert_eq!(
        derive_session_status(StatusInputs {
            open_approval: true,
            open_question: true,
            has_active_turn: true,
            ..base
        }),
        SessionStatus::WaitingApproval
    );
    assert_eq!(
        derive_session_status(StatusInputs {
            open_question: true,
            has_active_turn: true,
            ..base
        }),
        SessionStatus::WaitingUser
    );
    assert_eq!(
        derive_session_status(StatusInputs {
            has_active_turn: true,
            pending_queue_entries: 1,
            ..base
        }),
        SessionStatus::Running
    );
    assert_eq!(
        derive_session_status(StatusInputs {
            pending_queue_entries: 1,
            ..base
        }),
        SessionStatus::Queued
    );
    assert!(!SessionStatus::Queued.waits_for_human());
    assert!(SessionStatus::WaitingApproval.waits_for_human());
}

#[test]
fn upstream_blocked_error_carries_a_nonempty_blocker_id() {
    let valid = CanonicalError {
        code: ErrorCode::UpstreamBlocked {
            blocker_id: BlockerId::new("blocker-no-public-attach"),
        },
        retriable: false,
        detail_ref: None,
        at_ms: NOW,
    };
    assert!(valid.validate().is_ok());
    assert!(valid.code.needs_human());
    assert_json_roundtrip(&valid);
}

// --- Fixtures and object builders ---------------------------------------

fn fixture_rows(name: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/codex")
        .join(name);
    fs::read_to_string(path)
        .expect("committed fixture must be readable")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("every JSONL line must be valid JSON"))
        .collect()
}

fn method(row: &Value) -> Option<&str> {
    row.pointer("/payload/method").and_then(Value::as_str)
}

fn digest() -> String {
    format!("sha256:{}", "0".repeat(64))
}

fn sensitive_content(id: &str, kind: ContentKind) -> ContentRef {
    ContentRef {
        content_id: ContentId::new(id),
        kind,
        byte_len: 32,
        digest: digest(),
        preview: None,
        sensitivity: Sensitivity::Sensitive,
        availability: ContentAvailability::Stored,
    }
}

fn business_content(id: &str, preview: &str) -> ContentRef {
    ContentRef {
        content_id: ContentId::new(id),
        kind: ContentKind::StructuredSummary,
        byte_len: preview.len() as u64,
        digest: digest(),
        preview: Some(preview.to_owned()),
        sensitivity: Sensitivity::Business,
        availability: ContentAvailability::Stored,
    }
}

fn host_id() -> HostId {
    HostId::new("host-1")
}

fn runtime_id() -> ProviderRuntimeId {
    ProviderRuntimeId::new("runtime-1")
}

fn project_id() -> ProjectId {
    ProjectId::new("project-1")
}

fn project_binding_id() -> ProjectBindingId {
    ProjectBindingId::new("project-binding-1")
}

fn session_id() -> SessionId {
    SessionId::new("session-1")
}

fn turn_id() -> TurnId {
    TurnId::new("turn-1")
}

fn item_id() -> ItemId {
    ItemId::new("item-1")
}

fn workflow_id() -> WorkflowId {
    WorkflowId::new("workflow-1")
}

fn step_id() -> StepId {
    StepId::new("step-1")
}

fn artifact_id() -> ArtifactId {
    ArtifactId::new("artifact-1")
}

fn binding_handle(kind: ProviderBindingKind) -> ProviderBindingHandle {
    ProviderBindingHandle {
        id: ProviderBindingId::new("bnd_12345678"),
        runtime_id: runtime_id(),
        kind,
    }
}

fn capability_evidence(source: EvidenceSource) -> CapabilityEvidence {
    CapabilityEvidence {
        source,
        observed_at_ms: NOW,
        note_ref: None,
    }
}

fn capability_entry(capability: Capability, state: CapabilityState) -> CapabilityEntry {
    CapabilityEntry {
        capability,
        state,
        evidence: capability_evidence(EvidenceSource::ObservedInTraffic),
    }
}

fn runtime_capabilities(entries: Vec<CapabilityEntry>) -> RuntimeCapabilities {
    RuntimeCapabilities {
        runtime_id: runtime_id(),
        negotiated_at_ms: NOW,
        entries,
    }
}

fn canonical_error() -> CanonicalError {
    CanonicalError {
        code: ErrorCode::UpstreamTimeout,
        retriable: true,
        detail_ref: Some(sensitive_content(
            "content-error-detail",
            ContentKind::PlainText,
        )),
        at_ms: NOW,
    }
}

fn host() -> Host {
    Host {
        id: host_id(),
        display_name: "Development host".to_owned(),
        platform: HostPlatform::Windows,
        reachability: HostReachability::LanDirect,
        protocol_version: PROTOCOL_VERSION.to_owned(),
        last_seen_at_ms: NOW,
    }
}

fn provider_runtime() -> ProviderRuntime {
    ProviderRuntime {
        id: runtime_id(),
        host_id: host_id(),
        family: ProviderFamily::Codex,
        version_label: Some("recorded-runtime".to_owned()),
        launch_surface: LaunchSurface::SharedServer,
        connection: ConnectionState::Connected { since_at_ms: NOW },
        capabilities: runtime_capabilities(vec![
            capability_entry(Capability::TurnPrompt, CapabilityState::Supported),
            capability_entry(Capability::TurnSteer, CapabilityState::Supported),
        ]),
        binding_handle: None,
    }
}

fn project_binding() -> ProjectBinding {
    ProjectBinding {
        id: project_binding_id(),
        project_id: project_id(),
        runtime_id: runtime_id(),
        root_ref: sensitive_content("content-project-root", ContentKind::FilePath),
    }
}

fn project() -> Project {
    Project {
        id: project_id(),
        display_name: "OneKaleidoscope".to_owned(),
        bindings: vec![project_binding()],
        session_counts: SessionCounts {
            total: 1,
            running: 0,
            waiting_human: 1,
            failed: 0,
            archived: 0,
        },
        workflow_count: 1,
        attention_count: 1,
        last_activity_at_ms: NOW,
    }
}

fn canonical_session() -> Session {
    Session {
        id: session_id(),
        project_id: project_id(),
        project_binding_id: project_binding_id(),
        ownership: OwnershipMode::BrokerManaged,
        history_source: HistorySource {
            kind: HistorySourceKind::BrokerLog,
            runtime_id: Some(runtime_id()),
            evidence: capability_evidence(EvidenceSource::RecordedFixture),
        },
        live_binding: LiveBinding::NotBound {
            reason: LiveUnboundReason::NeverStarted,
        },
        status: SessionStatus::Completed,
        title: Some("Recorded contract session".to_owned()),
        created_at_ms: NOW,
        updated_at_ms: NOW,
        last_activity_at_ms: NOW,
        active_turn_id: None,
        queue_depth: 1,
        open_attention_count: 1,
        archived: false,
        binding_handle: Some(binding_handle(ProviderBindingKind::Session)),
    }
}

fn completed_turn() -> Turn {
    Turn {
        id: turn_id(),
        session_id: session_id(),
        status: TurnStatus::Completed,
        origin: TurnOrigin::LocalSurface,
        started_at_ms: Some(NOW),
        completed_at_ms: Some(NOW + 1),
        item_ids: vec![item_id()],
        error: None,
        binding_handle: Some(binding_handle(ProviderBindingKind::Turn)),
    }
}

fn canonical_item() -> Item {
    Item {
        id: item_id(),
        session_id: session_id(),
        turn_id: turn_id(),
        sequence: 0,
        status: ItemStatus::Declined,
        body: ItemBody::AgentMessage {
            content: sensitive_content("content-agent-message", ContentKind::Markdown),
            phase: MessagePhase::FinalAnswer,
        },
        created_at_ms: NOW,
        updated_at_ms: NOW,
        binding_handle: Some(binding_handle(ProviderBindingKind::Item)),
    }
}

fn pending_queue_entry(id: &str, session: &str, position: u32) -> QueueEntry {
    QueueEntry {
        id: QueueEntryId::new(id),
        session_id: SessionId::new(session),
        position,
        intent: QueueIntent::SteerActiveTurn,
        body: sensitive_content(&format!("content-{id}"), ContentKind::PlainText),
        state: QueueState::Pending,
        editable: true,
        created_at_ms: NOW,
        updated_at_ms: NOW,
    }
}

fn delivered_steer(source: EvidenceSource) -> QueueEntry {
    let handle = binding_handle(ProviderBindingKind::RuntimeAcknowledgement);
    QueueEntry {
        state: QueueState::DeliveredAsSteer {
            runtime_id: runtime_id(),
            turn_id: turn_id(),
            binding_handle: handle.clone(),
            injected_at_ms: NOW,
            ack: SteerAcknowledgement {
                source,
                runtime_id: runtime_id(),
                session_id: session_id(),
                turn_id: turn_id(),
                binding_handle: handle,
                observed_at_ms: NOW,
            },
        },
        editable: false,
        ..pending_queue_entry("queue-steer", "session-1", 0)
    }
}

fn decision_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            option_id: "accept".to_owned(),
            label: "Allow".to_owned(),
            semantics: DecisionSemantics::Allow,
        },
        DecisionOption {
            option_id: "decline".to_owned(),
            label: "Deny".to_owned(),
            semantics: DecisionSemantics::Deny,
        },
    ]
}

fn approval(join: JoinState) -> AttentionItem {
    AttentionItem {
        id: AttentionId::new("attention-approval"),
        host_id: host_id(),
        project_id: project_id(),
        session_id: Some(session_id()),
        turn_id: Some(turn_id()),
        workflow_id: None,
        subject: AttentionSubject::Approval {
            request: ApprovalRequest {
                request_key: "approval-request".to_owned(),
                target_item_id: item_id(),
                join,
                options: decision_options(),
                summary_ref: sensitive_content(
                    "content-approval-summary",
                    ContentKind::StructuredSummary,
                ),
                detail_ref: Some(sensitive_content(
                    "content-approval-detail",
                    ContentKind::ToolArguments,
                )),
                binding_handle: binding_handle(ProviderBindingKind::InteractionRequest),
            },
        },
        state: AttentionState::Open,
        created_at_ms: NOW,
        expires_at_ms: Some(NOW + 1_000),
    }
}

fn approval_join(item: &AttentionItem) -> Option<&JoinState> {
    match &item.subject {
        AttentionSubject::Approval { request } => Some(&request.join),
        AttentionSubject::Question { .. }
        | AttentionSubject::WorkflowGate { .. }
        | AttentionSubject::ConnectionFault { .. } => None,
    }
}

fn approval_response() -> AttentionResponse {
    AttentionResponse {
        attention_id: AttentionId::new("attention-approval"),
        session_id: Some(session_id()),
        request_key: "approval-request".to_owned(),
        expected_expires_at_ms: Some(NOW + 1_000),
        option_id: Some("decline".to_owned()),
        free_form_ref: None,
    }
}

fn workflow_gate_attention() -> AttentionItem {
    AttentionItem {
        id: AttentionId::new("attention-gate"),
        host_id: host_id(),
        project_id: project_id(),
        session_id: None,
        turn_id: None,
        workflow_id: Some(workflow_id()),
        subject: AttentionSubject::WorkflowGate {
            request: WorkflowGateRequest {
                request_key: "workflow-gate-request".to_owned(),
                step_id: step_id(),
                prompt_ref: sensitive_content("content-workflow-gate", ContentKind::PlainText),
                options: decision_options(),
                free_form_allowed: false,
            },
        },
        state: AttentionState::Open,
        created_at_ms: NOW,
        expires_at_ms: Some(NOW + 1_000),
    }
}

fn step_assignment() -> StepAssignment {
    StepAssignment {
        selector: RuntimeSelector {
            family: ProviderFamily::Codex,
            required: vec![Capability::TurnPrompt],
            runtime_id: Some(runtime_id()),
        },
        project_binding_id: project_binding_id(),
        worktree_ref: sensitive_content("content-worktree-path", ContentKind::FilePath),
    }
}

fn step() -> Step {
    Step {
        id: step_id(),
        workflow_id: workflow_id(),
        title: "Implement".to_owned(),
        role: StepRole::Implement,
        assignment: step_assignment(),
        depends_on: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        completion: CompletionCondition::AgentTurnCompleted,
        human_gate: None,
        session_id: Some(session_id()),
        state: StepState::Ready,
        attempt: 0,
        audit: Vec::new(),
    }
}

fn workflow() -> Workflow {
    Workflow {
        id: workflow_id(),
        project_id: project_id(),
        title: "R1 workflow".to_owned(),
        state: WorkflowState::Running,
        step_ids: vec![step_id()],
        created_at_ms: NOW,
        updated_at_ms: NOW,
    }
}

fn artifact() -> Artifact {
    Artifact {
        id: artifact_id(),
        workflow_id: workflow_id(),
        produced_by: Some(step_id()),
        kind: ArtifactKind::TestReport,
        content: sensitive_content("content-test-report", ContentKind::Markdown),
        created_at_ms: NOW,
    }
}

fn diagnostic_effect() -> StateEffect {
    StateEffect::DiagnosticRecorded {
        diagnostic: DiagnosticRecord {
            runtime_id: Some(runtime_id()),
            session_id: None,
            code: DiagnosticCode::UnknownUpstreamMessage,
            count: 1,
            first_at_ms: NOW,
            last_at_ms: NOW,
            detail_ref: None,
        },
    }
}

fn session_stream() -> StreamKey {
    StreamKey::Session {
        session_id: session_id(),
    }
}

fn log_record(stream: StreamKey, seq: u64) -> LogRecord {
    LogRecord {
        cursor: Cursor { seq },
        stream,
        appended_at_ms: NOW,
        effect: diagnostic_effect(),
    }
}

fn all_snapshots() -> Vec<SnapshotEnvelope> {
    let host_snapshot = SnapshotEnvelope {
        stream: StreamKey::Host { host_id: host_id() },
        cursor: Cursor { seq: 10 },
        payload: SnapshotPayload::Host {
            snapshot: HostSnapshot {
                host: host(),
                runtimes: vec![provider_runtime()],
                projects: vec![project()],
            },
        },
    };
    let project_snapshot = SnapshotEnvelope {
        stream: StreamKey::Project {
            project_id: project_id(),
        },
        cursor: Cursor { seq: 10 },
        payload: SnapshotPayload::Project {
            snapshot: ProjectSnapshot {
                project: project(),
                sessions: vec![canonical_session()],
                workflows: vec![workflow()],
                attention: Vec::new(),
            },
        },
    };
    let session_snapshot = session_snapshot_envelope(10);

    let mut workflow_step = step();
    workflow_step.inputs = vec![artifact_id()];
    workflow_step.outputs = vec![artifact_id()];
    workflow_step.human_gate = Some(AttentionId::new("attention-gate"));
    let workflow_snapshot = SnapshotEnvelope {
        stream: StreamKey::Workflow {
            workflow_id: workflow_id(),
        },
        cursor: Cursor { seq: 10 },
        payload: SnapshotPayload::Workflow {
            snapshot: WorkflowSnapshot {
                workflow: workflow(),
                steps: vec![workflow_step],
                artifacts: vec![artifact()],
                attention: vec![workflow_gate_attention()],
            },
        },
    };
    vec![
        host_snapshot,
        project_snapshot,
        session_snapshot,
        workflow_snapshot,
    ]
}

fn session_snapshot_envelope(cursor: u64) -> SnapshotEnvelope {
    SnapshotEnvelope {
        stream: session_stream(),
        cursor: Cursor { seq: cursor },
        payload: SnapshotPayload::Session {
            snapshot: SessionSnapshot {
                session: canonical_session(),
                turns: vec![completed_turn()],
                items: vec![canonical_item()],
                queue: vec![pending_queue_entry("queue-1", "session-1", 0)],
                attention: vec![approval(JoinState::Joined { item_id: item_id() })],
                capabilities: runtime_capabilities(Vec::new()),
            },
        },
    }
}

fn all_state_effects() -> Vec<StateEffect> {
    vec![
        StateEffect::HostUpserted { host: host() },
        StateEffect::RuntimeUpserted {
            runtime: provider_runtime(),
        },
        StateEffect::CapabilitiesUpdated {
            capabilities: runtime_capabilities(vec![capability_entry(
                Capability::TurnPrompt,
                CapabilityState::Supported,
            )]),
        },
        StateEffect::ProjectUpserted { project: project() },
        StateEffect::SessionUpserted {
            session: canonical_session(),
        },
        StateEffect::SessionStatusChanged {
            session_id: session_id(),
            status: SessionStatus::WaitingApproval,
        },
        StateEffect::TurnUpserted {
            turn: completed_turn(),
        },
        StateEffect::ItemUpserted {
            item: canonical_item(),
        },
        StateEffect::QueueEntryUpserted {
            entry: pending_queue_entry("queue-1", "session-1", 0),
        },
        StateEffect::QueueReordered {
            session_id: session_id(),
            order: vec![QueueEntryId::new("queue-1")],
        },
        StateEffect::AttentionUpserted {
            item: approval(JoinState::Joined { item_id: item_id() }),
        },
        StateEffect::WorkflowUpserted {
            workflow: workflow(),
        },
        StateEffect::StepUpserted { step: step() },
        StateEffect::ArtifactUpserted {
            artifact: artifact(),
        },
        StateEffect::CommandAcknowledged {
            ack: CommandAck {
                command_id: CommandId::new("command-ack"),
                outcome: CommandOutcome::AcceptedLocally { note_ref: None },
                acked_at_ms: NOW,
            },
        },
        diagnostic_effect(),
    ]
}

fn all_commands() -> Vec<Command> {
    vec![
        Command::SubmitPrompt {
            session_id: session_id(),
            body: sensitive_content("content-submit", ContentKind::PlainText),
        },
        Command::EnqueueInput {
            session_id: session_id(),
            body: sensitive_content("content-enqueue", ContentKind::PlainText),
            intent: QueueIntent::SteerActiveTurn,
        },
        Command::EditQueueEntry {
            entry_id: QueueEntryId::new("queue-1"),
            body: sensitive_content("content-edit", ContentKind::PlainText),
        },
        Command::ReorderQueue {
            session_id: session_id(),
            order: vec![QueueEntryId::new("queue-1")],
        },
        Command::CancelQueueEntry {
            entry_id: QueueEntryId::new("queue-1"),
        },
        Command::InterruptTurn {
            session_id: session_id(),
            turn_id: turn_id(),
        },
        Command::RetryTurn {
            session_id: session_id(),
            turn_id: turn_id(),
        },
        Command::RespondAttention {
            response: approval_response(),
        },
        Command::OpenSession {
            project_id: project_id(),
            runtime_id: runtime_id(),
        },
        Command::ResumeSession {
            session_id: session_id(),
        },
        Command::CloseSession {
            session_id: session_id(),
        },
        Command::AdvanceStep { step_id: step_id() },
        Command::RetryStep { step_id: step_id() },
        Command::ReworkStep {
            step_id: step_id(),
            reason_ref: Some(sensitive_content(
                "content-rework-reason",
                ContentKind::PlainText,
            )),
        },
        Command::SkipStep {
            step_id: step_id(),
            reason_ref: Some(sensitive_content(
                "content-skip-reason",
                ContentKind::PlainText,
            )),
        },
        Command::CancelStep { step_id: step_id() },
        Command::ReassignStep {
            step_id: step_id(),
            assignment: step_assignment(),
        },
        Command::CancelWorkflow {
            workflow_id: workflow_id(),
        },
    ]
}

fn all_projections() -> Vec<ProjectionEnvelope> {
    let project_index = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::ProjectIndex { host_id: host_id() },
        cursor: Cursor { seq: 1 },
        payload: ProjectionPayload::ProjectIndex {
            view: ProjectIndexView {
                host_id: host_id(),
                reachability: HostReachability::LanDirect,
                groups: vec![ProviderGroup {
                    family: ProviderFamily::Codex,
                    runtime_ids: vec![runtime_id()],
                    projects: vec![ProjectSummary {
                        project_id: project_id(),
                        display_name: "OneKaleidoscope".to_owned(),
                        bindings: vec![ProjectBindingSummary {
                            binding_id: project_binding_id(),
                            runtime_id: runtime_id(),
                        }],
                        session_counts: SessionCounts {
                            total: 1,
                            running: 0,
                            waiting_human: 1,
                            failed: 0,
                            archived: 0,
                        },
                        attention_count: 1,
                        workflow_count: 1,
                        last_activity_at_ms: NOW,
                    }],
                }],
            },
        },
    };
    let session_index = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::SessionIndex {
            project_id: project_id(),
        },
        cursor: Cursor { seq: 2 },
        payload: ProjectionPayload::SessionIndex {
            view: SessionIndexView {
                project_id: project_id(),
                active: vec![SessionSummary {
                    session_id: session_id(),
                    project_binding_id: project_binding_id(),
                    title: Some("Session".to_owned()),
                    status: SessionStatus::WaitingApproval,
                    ownership: OwnershipMode::BrokerManaged,
                    live_binding: LiveBinding::NotBound {
                        reason: LiveUnboundReason::NeverStarted,
                    },
                    queue_depth: 1,
                    open_attention_count: 1,
                    last_activity_at_ms: NOW,
                }],
                history: Vec::new(),
                archived: Vec::new(),
            },
        },
    };
    let transcript = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::Transcript {
            session_id: session_id(),
        },
        cursor: Cursor { seq: 3 },
        payload: ProjectionPayload::Transcript {
            view: TranscriptView {
                session_id: session_id(),
                turns: vec![TranscriptTurn {
                    turn: completed_turn(),
                    items: vec![canonical_item()],
                }],
                has_earlier: false,
            },
        },
    };
    let live_activity = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::LiveActivity {
            session_id: session_id(),
        },
        cursor: Cursor { seq: 4 },
        payload: ProjectionPayload::LiveActivity {
            view: LiveActivityView {
                session_id: session_id(),
                active_turn_id: Some(turn_id()),
                streaming_item_ids: vec![item_id()],
                plan: vec![PlanEntry {
                    title_ref: sensitive_content("content-plan-title", ContentKind::PlainText),
                    state: PlanEntryState::InProgress,
                }],
                tasks: vec![AgentTask {
                    id: AgentTaskId::new("agent-task-1"),
                    title_ref: sensitive_content("content-task-title", ContentKind::PlainText),
                    state: PlanEntryState::Pending,
                }],
                updated_at_ms: NOW,
            },
        },
    };
    let input_queue = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::InputQueue {
            session_id: session_id(),
        },
        cursor: Cursor { seq: 5 },
        payload: ProjectionPayload::InputQueue {
            view: InputQueueView {
                session_id: session_id(),
                entries: vec![pending_queue_entry("queue-1", "session-1", 0)],
                writable: true,
                steer_supported: true,
            },
        },
    };
    let attention_inbox = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::AttentionInbox { host_id: host_id() },
        cursor: Cursor { seq: 6 },
        payload: ProjectionPayload::AttentionInbox {
            view: AttentionInboxView {
                entries: vec![approval(JoinState::Joined { item_id: item_id() })],
            },
        },
    };
    let workflow_board = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::WorkflowBoard {
            workflow_id: workflow_id(),
        },
        cursor: Cursor { seq: 7 },
        payload: ProjectionPayload::WorkflowBoard {
            view: WorkflowBoardView {
                workflow_id: workflow_id(),
                state: WorkflowState::Running,
                steps: vec![WorkflowBoardStep {
                    step_id: step_id(),
                    title: "Implement".to_owned(),
                    state: StepState::Ready,
                    depends_on: Vec::new(),
                    assignment: step_assignment(),
                    session_id: Some(session_id()),
                    blockers: Vec::new(),
                }],
                artifacts: vec![artifact()],
            },
        },
    };
    let runtime_capability = ProjectionEnvelope {
        projection_version: PROJECTION_VERSION,
        key: ProjectionKey::RuntimeCapability {
            host_id: host_id(),
            runtime_id: runtime_id(),
        },
        cursor: Cursor { seq: 8 },
        payload: ProjectionPayload::RuntimeCapability {
            view: RuntimeCapabilityView::from_capabilities(
                host_id(),
                &runtime_capabilities(vec![capability_entry(
                    Capability::TurnPrompt,
                    CapabilityState::Supported,
                )]),
            ),
        },
    };
    vec![
        project_index,
        session_index,
        transcript,
        live_activity,
        input_queue,
        attention_inbox,
        workflow_board,
        runtime_capability,
    ]
}

fn assert_json_roundtrip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let encoded = serde_json::to_string(value).expect("wire value must serialize");
    let decoded: T = serde_json::from_str(&encoded).expect("wire value must deserialize");
    assert_eq!(&decoded, value);
}
