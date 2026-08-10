#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use std::{collections::BTreeMap, fs, path::PathBuf};

use kaleido_adapter::content::{ContentAccess, ContentAccessError};
use kaleido_adapter_opencode::{OpenCodeReducer, ReducerConfig};
use kaleido_proto::{
    capability::EvidenceSource,
    content::{ContentAvailability, ContentKind, ContentRef, Sensitivity},
    effect::StateEffect,
    host::HostPlatform,
    ids::ContentId,
};
use serde_json::Value;

const BASE_AT_MS: i64 = 1_785_378_397_000;

#[derive(Debug, Default)]
struct MemoryContent {
    next: u64,
    values: BTreeMap<String, Vec<u8>>,
}

impl ContentAccess for MemoryContent {
    fn store(
        &mut self,
        kind: ContentKind,
        sensitivity: Sensitivity,
        bytes: &[u8],
    ) -> Result<ContentRef, ContentAccessError> {
        self.next = self.next.saturating_add(1);
        let id = format!("cnt_{:016x}", self.next);
        self.values.insert(id.clone(), bytes.to_vec());
        Ok(ContentRef {
            content_id: ContentId::new(id),
            kind,
            byte_len: bytes.len() as u64,
            digest: format!("sha256:{}", "0".repeat(64)),
            preview: None,
            sensitivity,
            availability: ContentAvailability::Stored,
        })
    }

    fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, ContentAccessError> {
        self.values
            .get(&reference.content_id.value)
            .cloned()
            .ok_or_else(|| ContentAccessError::Missing {
                content_id: reference.content_id.clone(),
            })
    }
}

fn reducer() -> OpenCodeReducer {
    OpenCodeReducer::new(ReducerConfig {
        host_display_name: "fixture-host".to_owned(),
        host_platform: HostPlatform::Windows,
        project_display_name: "fixture-project".to_owned(),
        project_directory: "<SANDBOX>".to_owned(),
        identity_salt: "test-salt".to_owned(),
        evidence: EvidenceSource::ObservedInTraffic,
        base_at_ms: BASE_AT_MS,
        runtime_version_label: Some("1.18.11".to_owned()),
    })
}

fn fixture() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/opencode/01-simple-turn.jsonl");
    fs::read_to_string(path).expect("committed OpenCode fixture")
}

fn elicitation_fixture() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/opencode/09-elicitation.jsonl");
    fs::read_to_string(path).expect("committed OpenCode elicitation fixture")
}

#[test]
fn real_recorded_sse_prefix_reduces_to_canonical_effects() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let mut seen_session = false;
    let mut effect_count = 0usize;
    for (index, line) in fixture().lines().enumerate().take(11) {
        let envelope: Value = serde_json::from_str(line).expect("fixture JSON");
        if envelope.get("transport").and_then(Value::as_str) != Some("sse") {
            continue;
        }
        let payload_value = envelope.get("payload").expect("fixture payload");
        let payload = serde_json::to_vec(payload_value).expect("payload JSON");
        let expected = if seen_session {
            Some("ses_01504a908ffeMvGUQd5CEjaVK1")
        } else {
            None
        };
        let (_, effects) = reducer
            .reduce_sse_event(
                &payload,
                expected,
                1_785_378_397_000 + index as i64,
                &mut content,
            )
            .expect("recorded structured event should decode");
        seen_session = true;
        effect_count += effects.len();
    }
    assert!(effect_count >= 8);
    assert!(reducer
        .capability_probe()
        .is_proven(kaleido_proto::capability::Capability::LiveObserve));
}

#[test]
fn unknown_event_is_rejected_instead_of_becoming_agent_text() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let result = reducer.reduce_sse_event(
        br#"{"id":"evt_unknown","type":"future.event","properties":{}}"#,
        None,
        1,
        &mut content,
    );
    assert!(result.is_err());
}

#[test]
fn malformed_and_cross_session_events_fail_closed() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let malformed =
        reducer.reduce_sse_event(br#"{"type":"session.status"}"#, None, 1, &mut content);
    assert!(malformed.is_err());
    let cross_scope = reducer.reduce_sse_event(
        br#"{"id":"evt_1","type":"session.status","properties":{"sessionID":"ses_other","status":{"type":"idle"}}}"#,
        Some("ses_selected"),
        1,
        &mut content,
    );
    assert!(cross_scope.is_err());
}

#[test]
fn unsupported_capabilities_stay_unproven() {
    let reducer = reducer();
    let probe = reducer.capability_probe();
    assert!(!probe.is_proven(kaleido_proto::capability::Capability::TurnSteer));
    assert!(!probe.is_proven(kaleido_proto::capability::Capability::LiveMultiSubscriber));
    assert!(!probe.is_proven(kaleido_proto::capability::Capability::TurnRetry));
}

#[test]
fn real_recorded_rest_snapshot_discovers_and_replays_history() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/opencode/08-session-load.jsonl");
    let lines = fs::read_to_string(path).expect("session-load fixture");
    let records = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("fixture JSON"))
        .collect::<Vec<_>>();
    let sessions = records
        .get(1)
        .and_then(|record| record.pointer("/payload/body"))
        .and_then(Value::as_array)
        .expect("session list");
    let session_id = sessions
        .first()
        .and_then(|session| session.get("id"))
        .and_then(Value::as_str)
        .expect("session id")
        .to_owned();
    let messages = records
        .get(5)
        .and_then(|record| record.pointer("/payload/body"))
        .and_then(Value::as_array)
        .expect("message list");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .reduce_snapshot(
            sessions,
            &[(session_id, messages.clone())],
            1_785_304_307_737,
            &mut content,
        )
        .expect("recorded REST snapshot should reduce");
    assert!(effects.iter().any(|effect| matches!(
        effect,
        kaleido_proto::effect::StateEffect::SessionUpserted { .. }
    )));
    assert!(reducer
        .capability_probe()
        .is_proven(kaleido_proto::capability::Capability::HistoryResume));
}

#[test]
fn real_recorded_question_round_trips_every_answer_without_flattening() -> Result<(), String> {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let mut answered = None;
    for line in elicitation_fixture().lines() {
        let envelope: Value = serde_json::from_str(line).expect("fixture JSON");
        if envelope.get("transport").and_then(Value::as_str) != Some("sse") {
            continue;
        }
        let payload_value = envelope.get("payload").expect("fixture payload");
        let event_type = payload_value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<missing>")
            .to_owned();
        let event_shape = payload_value
            .pointer("/properties/part/type")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_owned();
        let payload = serde_json::to_vec(payload_value).expect("SSE payload JSON");
        reducer
            .decode_event(&payload)
            .map_err(|error| format!("generated {event_type} validation failed: {error}"))?;
        let (_, effects) = reducer
            .reduce_sse_event(&payload, None, BASE_AT_MS, &mut content)
            .map_err(|error| format!("recorded {event_type}/{event_shape} must reduce: {error}"))?;
        for effect in effects {
            if let StateEffect::AttentionUpserted { item } = effect {
                if let kaleido_proto::attention::AttentionState::Answered {
                    question_answers, ..
                } = item.state
                {
                    answered = Some(question_answers);
                }
            }
        }
    }
    let answers = answered.expect("recorded question reply becomes canonical answered state");
    assert_eq!(answers.len(), 1);
    let answer = answers.first().expect("one canonical question answer");
    assert_eq!(answer.option_ids.len(), 1);
    assert!(answer.free_form_ref.is_none());
    Ok(())
}
