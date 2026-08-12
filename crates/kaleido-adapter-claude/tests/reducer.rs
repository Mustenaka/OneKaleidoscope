#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Contract tests are driven by the real SDK capture in `fixtures/sandbox`.
//! Additional frames below exercise refusal and malformed-input gates using
//! the same closed sidecar envelope (they are not upstream DTO fixtures).

mod support;

use std::fs;

use kaleido_adapter_claude::error::ClaudeAdapterError;
use kaleido_adapter_claude::parse_transcript;
use kaleido_adapter_claude::transcript::{Direction, TranscriptFrame, SIDECAR_PROTOCOL};
use kaleido_proto::attention::{AttentionState, AttentionSubject};
use kaleido_proto::capability::{Capability, CapabilityState, CapabilityUnavailableReason};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::turn::TurnStatus;
use serde_json::{json, Value};

use support::{auth_failure_fixture_path, fixture_path, reducer, MemoryContent, BASE_AT_MS};

fn frame(kind: &str, payload: Value, at_ms: i64) -> TranscriptFrame {
    TranscriptFrame::from_value(
        Direction::BridgeToHost,
        at_ms,
        json!({
            "v": 1,
            "protocol": SIDECAR_PROTOCOL,
            "kind": kind,
            "payload": payload,
        }),
    )
    .expect("closed sidecar frame is valid")
}

#[test]
fn ready_projects_an_unbound_broker_session_until_the_sdk_assigns_its_id() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest_frame(
            &frame(
                "ready",
                json!({
                    "sdk_version": "0.3.226",
                    "cwd": "<sandbox/toy-project>",
                    "resume_session_id": null
                }),
                BASE_AT_MS,
            ),
            &mut content,
        )
        .expect("ready reduces");
    let provisional = effects.iter().find_map(|effect| match effect {
        StateEffect::SessionUpserted { session } => Some(session.clone()),
        _ => None,
    });
    let provisional = provisional.expect("ready creates a broker session");
    assert!(provisional.binding_handle.is_none());
    assert!(matches!(
        provisional.live_binding,
        kaleido_proto::session::LiveBinding::NotBound { .. }
    ));

    let bound = reducer
        .ingest_frame(
            &frame(
                "session_started",
                json!({
                    "session_id": "real-sdk-session",
                    "cwd": "<sandbox/toy-project>"
                }),
                BASE_AT_MS + 1,
            ),
            &mut content,
        )
        .expect("session assignment reduces")
        .into_iter()
        .find_map(|effect| match effect {
            StateEffect::SessionUpserted { session } => Some(session),
            _ => None,
        })
        .expect("session assignment updates the projection");
    assert_eq!(bound.id, provisional.id);
    assert!(bound.binding_handle.is_some());
}

#[test]
fn real_sdk_capture_reduces_to_session_agent_item_and_completed_turn() {
    let raw = fs::read_to_string(fixture_path()).expect("real SDK fixture exists");
    let transcript = parse_transcript(&raw).expect("real SDK fixture parses");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("capture reduces");

    assert!(effects
        .iter()
        .any(|effect| matches!(effect, StateEffect::SessionUpserted { .. })));
    // The typed assistant message is projected from the real SDK capture,
    // rather than replaced by a fabricated success string.
    assert!(effects.iter().any(|effect| matches!(
        effect,
        StateEffect::ItemUpserted { item }
            if matches!(item.body, kaleido_proto::turn::ItemBody::AgentMessage { .. })
    )));
    let turn = effects.iter().rev().find_map(|effect| match effect {
        StateEffect::TurnUpserted { turn } => Some(turn),
        _ => None,
    });
    assert_eq!(turn.map(|value| value.status), Some(TurnStatus::Completed));
    assert!(turn.is_some_and(|value| value.error.is_none()));
    assert!(effects
        .iter()
        .all(|effect| effect.validate_for_log().is_ok()));
    assert!(!reducer.capability_probe().is_proven(Capability::TurnSteer));
    assert!(reducer.capability_probe().is_proven(Capability::TurnPrompt));
}

#[test]
fn real_sdk_authentication_failure_projects_auth_required_without_prompt_capability() {
    let raw = fs::read_to_string(auth_failure_fixture_path())
        .expect("real SDK authentication-failure fixture exists");
    let transcript =
        parse_transcript(&raw).expect("real SDK authentication-failure fixture parses");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("authentication-failure capture reduces");

    let turn = effects.iter().rev().find_map(|effect| match effect {
        StateEffect::TurnUpserted { turn } => Some(turn),
        _ => None,
    });
    assert_eq!(turn.map(|value| value.status), Some(TurnStatus::Failed));
    assert!(turn.is_some_and(|value| {
        value
            .error
            .as_ref()
            .is_some_and(|error| error.code == kaleido_proto::error::ErrorCode::AuthRequired)
    }));
    assert_eq!(
        reducer
            .capability_probe()
            .to_capabilities()
            .state_of(&Capability::TurnPrompt),
        CapabilityState::UnavailableOnThisConnection {
            reason: CapabilityUnavailableReason::AuthenticationRequired,
        }
    );
}

#[test]
fn permission_allow_and_deny_are_structured_and_decline_is_not_a_turn_error() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let frames = [
        frame(
            "session_started",
            json!({
                "session_id": "real-session",
                "cwd": "<sandbox/toy-project>"
            }),
            BASE_AT_MS,
        ),
        frame(
            "prompt_accepted",
            json!({ "turn_id": "turn-permission" }),
            BASE_AT_MS + 1,
        ),
        frame(
            "permission_request",
            json!({
                "request_id": "permission-1",
                "tool_name": "Bash",
                "tool_use_id": "tool-1",
                "title": "Run a command",
                "input_json": "{\"command\":\"true\"}"
            }),
            BASE_AT_MS + 2,
        ),
        frame(
            "permission_result",
            json!({ "request_id": "permission-1", "decision": "deny" }),
            BASE_AT_MS + 3,
        ),
    ];
    let mut effects = Vec::new();
    for item in &frames {
        effects.extend(
            reducer
                .ingest_frame(item, &mut content)
                .expect("frame reduces"),
        );
    }
    let attention = effects.iter().rev().find_map(|effect| match effect {
        StateEffect::AttentionUpserted { item } => Some(item),
        _ => None,
    });
    assert!(matches!(
        attention.map(|item| &item.state),
        Some(AttentionState::Answered { option_id: Some(option), .. }) if option == "deny"
    ));
    assert!(matches!(
        attention.map(|item| &item.subject),
        Some(AttentionSubject::Approval { .. })
    ));
    assert!(effects
        .iter()
        .all(|effect| effect.validate_for_log().is_ok()));
    assert!(reducer
        .capability_probe()
        .is_proven(Capability::InteractionApproval));
    assert_eq!(
        reducer
            .capability_probe()
            .to_capabilities()
            .state_of(&Capability::TurnSteer),
        CapabilityState::NotVerified
    );
    let unknown_decision = frame(
        "permission_result",
        json!({ "request_id": "permission-1", "decision": "maybe" }),
        BASE_AT_MS + 4,
    );
    assert!(matches!(
        reducer.ingest_frame(&unknown_decision, &mut content),
        Err(ClaudeAdapterError::ProtocolViolation { .. })
    ));
}

#[test]
fn malformed_and_unknown_frames_fail_closed_without_business_projection() {
    let error = parse_transcript("not-json\n").expect_err("malformed recording is rejected");
    assert!(matches!(
        error,
        ClaudeAdapterError::MalformedTranscriptLine { .. }
    ));

    assert!(matches!(
        TranscriptFrame::from_value(
            Direction::BridgeToHost,
            BASE_AT_MS,
            json!({
                "v": 1,
                "protocol": SIDECAR_PROTOCOL,
                "kind": "sdk_event",
                "payload": {
                    "session_id": "session-unknown",
                    "turn_id": null,
                    "event": { "event": "future_sdk_event" }
                }
            }),
        ),
        Err(ClaudeAdapterError::UnknownFrameKind)
    ));
    assert!(matches!(
        TranscriptFrame::from_value(
            Direction::BridgeToHost,
            BASE_AT_MS,
            json!({
                "v": 1,
                "protocol": SIDECAR_PROTOCOL,
                "kind": "ready",
                "payload": {
                    "sdk_version": "0.3.226",
                    "cwd": "<sandbox/toy-project>",
                    "resume_session_id": null,
                    "unexpected": true
                }
            }),
        ),
        Err(ClaudeAdapterError::MalformedFrame)
    ));
}

#[test]
fn ask_user_question_is_not_flattened_into_a_single_question() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let session = frame(
        "session_started",
        json!({ "session_id": "session-question", "cwd": "<sandbox/toy-project>" }),
        BASE_AT_MS,
    );
    reducer
        .ingest_frame(&session, &mut content)
        .expect("session frame reduces");
    let request = frame(
        "question_request",
        json!({
            "request_id": "q-1",
            "tool_name": "AskUserQuestion",
            "questions": [
                {
                    "question": "Choose a library?",
                    "header": "Library",
                    "options": [
                        {"label": "A", "description": "First"},
                        {"label": "B", "description": "Second"}
                    ],
                    "multi_select": false
                },
                {
                    "question": "Enable which features?",
                    "header": "Features",
                    "options": [
                        {"label": "Fast", "description": "Speed"},
                        {"label": "Safe", "description": "Checks"}
                    ],
                    "multi_select": true
                }
            ]
        }),
        BASE_AT_MS + 1,
    );
    let effects = reducer
        .ingest_frame(&request, &mut content)
        .expect("structured question set reduces");
    assert!(effects.iter().any(|effect| matches!(
        effect,
        StateEffect::AttentionUpserted { item }
            if matches!(
                &item.subject,
                kaleido_proto::attention::AttentionSubject::Question { request }
                    if request.questions.len() == 2
                        && request
                            .questions
                            .first()
                            .is_some_and(|question| !question.multi_select)
                        && request
                            .questions
                            .get(1)
                            .is_some_and(|question| question.multi_select)
            )
    )));
}

#[test]
fn host_direction_and_changed_session_identity_are_rejected_before_projection() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let host_frame = TranscriptFrame::from_value(
        Direction::HostToBridge,
        BASE_AT_MS,
        json!({
            "v": 1,
            "protocol": SIDECAR_PROTOCOL,
            "kind": "prompt_accepted",
            "payload": { "turn_id": "host-command" }
        }),
    )
    .expect("closed frame parses independently from its recorded direction");
    assert!(matches!(
        reducer.ingest_frame(&host_frame, &mut content),
        Err(ClaudeAdapterError::ProtocolViolation { .. })
    ));

    reducer
        .ingest_frame(
            &frame(
                "session_started",
                json!({ "session_id": "session-one", "cwd": "<sandbox/toy-project>" }),
                BASE_AT_MS + 1,
            ),
            &mut content,
        )
        .expect("first provider identity binds");
    assert!(matches!(
        reducer.ingest_frame(
            &frame(
                "session_started",
                json!({ "session_id": "session-two", "cwd": "<sandbox/toy-project>" }),
                BASE_AT_MS + 2,
            ),
            &mut content,
        ),
        Err(ClaudeAdapterError::ProtocolViolation { .. })
    ));
}

#[test]
fn bounded_official_history_page_proves_read_only_after_real_messages() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let list = frame(
        "session_list",
        json!({
            "cwd": "<sandbox/toy-project>",
            "sessions": [{
                "session_id": "history-session",
                "summary": "History session",
                "last_modified": BASE_AT_MS
            }]
        }),
        BASE_AT_MS,
    );
    reducer
        .ingest_frame(&list, &mut content)
        .expect("official list response reduces");
    let page = frame(
        "session_messages",
        json!({
            "cwd": "<sandbox/toy-project>",
            "session_id": "history-session",
            "offset": 0,
            "limit": 2,
            "next_offset": null,
            "messages": [{
                "role": "assistant",
                "message_id": "message-1",
                "session_id": "history-session",
                "parent_tool_use_id": null,
                "parent_agent_id": null,
                "message_json": "{\"role\":\"assistant\",\"content\":\"hello\"}"
            }]
        }),
        BASE_AT_MS + 1,
    );
    let effects = reducer
        .ingest_frame(&page, &mut content)
        .expect("bounded official history response reduces");
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, StateEffect::ItemUpserted { .. })));
    assert!(reducer
        .capability_probe()
        .is_proven(Capability::HistoryRead));

    let wrong_scope = frame(
        "session_messages",
        json!({
            "cwd": "<sandbox/toy-project>",
            "session_id": "not-discovered",
            "offset": 0,
            "limit": 1,
            "next_offset": null,
            "messages": []
        }),
        BASE_AT_MS + 2,
    );
    assert!(matches!(
        reducer.ingest_frame(&wrong_scope, &mut content),
        Err(ClaudeAdapterError::ProtocolViolation { .. })
    ));
}
