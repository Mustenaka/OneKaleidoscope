#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Contract tests are driven by the real SDK capture in `fixtures/sandbox`.
//! Additional frames below exercise refusal and malformed-input gates using
//! the same closed sidecar envelope (they are not upstream DTO fixtures).

mod support;

use std::fs;

use kaleido_adapter_claude::error::ClaudeAdapterError;
use kaleido_adapter_claude::transcript::{Direction, TranscriptFrame, SIDECAR_PROTOCOL};
use kaleido_adapter_claude::{parse_transcript, ClaudeReducer};
use kaleido_proto::attention::{AttentionState, AttentionSubject};
use kaleido_proto::capability::{Capability, CapabilityState, CapabilityUnavailableReason};
use kaleido_proto::effect::{DiagnosticCode, StateEffect};
use kaleido_proto::turn::TurnStatus;
use serde_json::{json, Value};

use support::{fixture_path, reducer, MemoryContent, BASE_AT_MS};

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
                    "resume": false,
                    "cwd": "<sandbox/toy-project>"
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
fn real_sdk_capture_reduces_to_session_agent_item_and_failed_turn() {
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
    // The fixture is intentionally an authentication failure, but the typed
    // assistant message is still projected as an agent item rather than
    // discarded or replaced by a fabricated success string.
    assert!(effects.iter().any(|effect| matches!(
        effect,
        StateEffect::ItemUpserted { item }
            if matches!(item.body, kaleido_proto::turn::ItemBody::AgentMessage { .. })
    )));
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
    assert!(effects
        .iter()
        .all(|effect| effect.validate_for_log().is_ok()));
    assert!(!reducer.capability_probe().is_proven(Capability::TurnSteer));
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
                "input": { "command": "true" }
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

    let mut reducer: ClaudeReducer = reducer();
    let mut content = MemoryContent::default();
    let session = frame(
        "session_started",
        json!({ "session_id": "session-unknown", "cwd": "<sandbox/toy-project>" }),
        BASE_AT_MS,
    );
    reducer
        .ingest_frame(&session, &mut content)
        .expect("session frame reduces");
    let unknown = frame(
        "sdk_message",
        json!({
            "session_id": "session-unknown",
            "message": { "type": "future_sdk_message" }
        }),
        BASE_AT_MS + 1,
    );
    let effects = reducer
        .ingest_frame(&unknown, &mut content)
        .expect("unknown message is diagnosed, not guessed");
    assert!(effects.iter().any(|effect| matches!(
        effect,
        StateEffect::DiagnosticRecorded { diagnostic }
            if diagnostic.code == DiagnosticCode::UnknownUpstreamMessage
    )));
    assert!(!effects
        .iter()
        .any(|effect| matches!(effect, StateEffect::ItemUpserted { .. })));

    let malformed = frame(
        "sdk_message",
        json!({ "session_id": "session-unknown" }),
        BASE_AT_MS + 2,
    );
    assert!(matches!(
        reducer.ingest_frame(&malformed, &mut content),
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
                    "multiSelect": false
                },
                {
                    "question": "Enable which features?",
                    "header": "Features",
                    "options": [
                        {"label": "Fast", "description": "Speed"},
                        {"label": "Safe", "description": "Checks"}
                    ],
                    "multiSelect": true
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
