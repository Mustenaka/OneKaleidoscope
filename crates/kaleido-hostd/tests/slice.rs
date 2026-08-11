#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! End-to-end checks for the offline slice.
//!
//! Everything here runs the same decoder, reducer and store a live process
//! would, driven by recordings already in the repository, so no assertion
//! depends on a login, a network or the version of Codex installed locally.

use std::io::Write;
mod support;

use kaleido_adapter::IdentityMint;
use kaleido_hostd::slice::{
    self, ApprovalDecision, ReplayRequest, RunRequest, R3_CODEX_PROJECTIONS, REPLAY_BASE_AT_MS,
};
use kaleido_proto::attention::{AttentionAnswerSource, AttentionState, AttentionSubject};
use kaleido_proto::capability::Capability;
use kaleido_proto::command::CommandOutcome;
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::{ConnectionFaultReason, ConnectionState};
use kaleido_proto::ids::ProviderBindingKind;
use kaleido_proto::projection::ProjectionKey;
use kaleido_proto::session::{LiveBinding, LiveUnboundReason, SessionStatus};
use kaleido_proto::ContractViolation;
use kaleido_state::{CanonicalStore, ClockSource, ProjectionName, StateError};

use support::{fixture, forbidden_strings, replay, FixtureRuntime, FIXTURES};

fn r3_keys(
    state: &kaleido_state::CanonicalState,
    session_id: &kaleido_proto::ids::SessionId,
) -> Vec<ProjectionKey> {
    let session = state.session(session_id).expect("fixture session");
    let host_id = state.hosts().next().expect("fixture host").id.clone();
    let runtime_id = session
        .history_source
        .runtime_id
        .clone()
        .expect("fixture runtime binding");
    vec![
        ProjectionKey::ProjectIndex {
            host_id: host_id.clone(),
        },
        ProjectionKey::SessionIndex {
            project_id: session.project_id.clone(),
        },
        ProjectionKey::Transcript {
            session_id: session_id.clone(),
        },
        ProjectionKey::LiveActivity {
            session_id: session_id.clone(),
        },
        ProjectionKey::InputQueue {
            session_id: session_id.clone(),
        },
        ProjectionKey::AttentionInbox {
            host_id: host_id.clone(),
        },
        ProjectionKey::RuntimeCapability {
            host_id,
            runtime_id,
        },
    ]
}

#[test]
fn a_replayed_session_rebuilds_field_for_field_after_a_reload() {
    // Section 5.4 sets the criterion: the same inputs must converge to the same
    // canonical state, which is a structural comparison and deliberately not a
    // byte comparison of the log.
    for name in FIXTURES {
        let replayed = replay(name);
        let reloaded = CanonicalStore::load(
            &replayed.log_dir,
            ClockSource::Fixed {
                at_ms: slice::REPLAY_BASE_AT_MS,
            },
        )
        .expect("reload the store");
        assert_eq!(
            &replayed.state,
            reloaded.state(),
            "{name} did not converge after a reload"
        );
    }
}

#[test]
fn every_projection_is_identical_before_and_after_a_reload() {
    for fixture_name in FIXTURES {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log_dir = directory.path().join("log");
        let request = ReplayRequest::new(fixture(fixture_name), &log_dir);
        let (outcome, store) =
            slice::replay_into_store(&request).expect("replay the recorded fixture");
        let keys = r3_keys(store.state(), &outcome.session_id);
        assert_eq!(keys.len(), R3_CODEX_PROJECTIONS.len());
        let before = keys
            .iter()
            .map(|key| {
                store
                    .projection_journal()
                    .current(key)
                    .cloned()
                    .unwrap_or_else(|| panic!("{fixture_name}: missing current {key:?}"))
            })
            .collect::<Vec<_>>();
        drop(store);

        let reloaded = CanonicalStore::load(
            &log_dir,
            ClockSource::Fixed {
                at_ms: slice::REPLAY_BASE_AT_MS,
            },
        )
        .expect("reload projection journal");
        let after = keys
            .iter()
            .map(|key| {
                reloaded
                    .projection_journal()
                    .current(key)
                    .cloned()
                    .unwrap_or_else(|| panic!("{fixture_name}: missing reloaded {key:?}"))
            })
            .collect::<Vec<_>>();
        assert_eq!(before, after, "{fixture_name}: journal reload diverged");

        let all = slice::show_all(&log_dir, Some(&outcome.session_id))
            .expect("show all seven Codex projections");
        assert!(!all.contains("workflow-board"));
    }
}

#[test]
fn a_skipped_cursor_in_the_durable_log_fails_the_reload() {
    let replayed = replay("01-simple-turn.jsonl");
    let streams = replayed.log_dir.join("streams");
    let mut session_log = None;
    for entry in std::fs::read_dir(&streams).expect("stream directory") {
        let path = entry.expect("stream entry").path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-"))
        {
            session_log = Some(path);
        }
    }
    let session_log = session_log.expect("the session stream exists");
    let contents = std::fs::read_to_string(&session_log).expect("read the stream");
    let mut lines = contents.lines().collect::<Vec<_>>();
    // Remove one record from the middle: the cursors either side no longer
    // step by one, which section 5.2 treats as corruption rather than a hint.
    lines.remove(2);
    let mut file = std::fs::File::create(&session_log).expect("rewrite the stream");
    for line in lines {
        writeln!(file, "{line}").expect("write a record");
    }
    drop(file);

    let error = CanonicalStore::load(
        &replayed.log_dir,
        ClockSource::Fixed {
            at_ms: slice::REPLAY_BASE_AT_MS,
        },
    )
    .expect_err("a gapped log must not load");
    assert!(
        matches!(
            error,
            StateError::Contract(ContractViolation::CursorGap { .. })
        ),
        "expected a cursor gap, found {error:?}"
    );
}

#[test]
fn the_durable_log_never_carries_a_body_a_path_or_an_upstream_identifier() {
    for name in FIXTURES {
        let replayed = replay(name);
        let streams = replayed.log_dir.join("streams");
        for entry in std::fs::read_dir(&streams).expect("stream directory") {
            let path = entry.expect("stream entry").path();
            let contents = std::fs::read_to_string(&path).expect("read the stream");
            for forbidden in forbidden_strings() {
                assert!(
                    !contents.contains(forbidden),
                    "{name}: `{forbidden}` appears in {}",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn bodies_exist_only_in_the_content_directory() {
    let replayed = replay("03-permission-approve.jsonl");
    let content_dir = replayed.log_dir.join("content");
    let mut found_body = false;
    for entry in std::fs::read_dir(&content_dir).expect("content directory") {
        let path = entry.expect("content entry").path();
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        if contents.contains("KALEIDO PERMISSION PROBE") {
            found_body = true;
        }
    }
    assert!(
        found_body,
        "the diff body must be retrievable from the content store"
    );

    // And the log only ever names it by digest.
    let streams = replayed.log_dir.join("streams");
    let mut saw_digest = false;
    for entry in std::fs::read_dir(&streams).expect("stream directory") {
        let contents =
            std::fs::read_to_string(entry.expect("stream entry").path()).expect("read stream");
        if contents.contains("sha256:") {
            saw_digest = true;
        }
    }
    assert!(saw_digest, "the log must reference bodies by digest");
}

#[test]
fn approving_and_declining_differ_only_in_the_operation_state() {
    let approved = replay("03-permission-approve.jsonl");
    let declined = replay("04-permission-deny.jsonl");

    let approved_transcript = slice::show(
        &approved.log_dir,
        ProjectionName::Transcript,
        Some(&approved.session_id),
    )
    .expect("render the approved transcript");
    let declined_transcript = slice::show(
        &declined.log_dir,
        ProjectionName::Transcript,
        Some(&declined.session_id),
    )
    .expect("render the declined transcript");

    assert!(approved_transcript.contains("\"status\": \"completed\""));
    assert!(
        declined_transcript.contains("\"status\": \"declined\""),
        "a refusal must reach the read model as a declined operation"
    );
    assert!(
        !approved_transcript.contains("\"status\": \"declined\""),
        "the approved run must not contain a refusal"
    );
    // Both turns completed, and neither carries an error.
    for rendered in [&approved_transcript, &declined_transcript] {
        assert!(rendered.contains("\"error\": null"));
    }
    for (state, name) in [(&approved.state, "approved"), (&declined.state, "declined")] {
        let session = state
            .session(if name == "approved" {
                &approved.session_id
            } else {
                &declined.session_id
            })
            .expect("the session exists");
        assert_eq!(
            session.status,
            kaleido_proto::session::SessionStatus::Idle,
            "{name} run should settle idle"
        );
        assert_eq!(session.open_attention_count, 0);
    }
}

#[test]
fn the_inbox_shows_an_open_approval_while_it_is_undecided() {
    // Truncating the recording before the reply leaves the approval open, which
    // is what a phone would be looking at while the human decides.
    let directory = tempfile::tempdir().expect("temporary directory");
    let log_dir = directory.path().join("log");
    let raw =
        std::fs::read_to_string(fixture("03-permission-approve.jsonl")).expect("read fixture");
    let truncated = raw.lines().take(50).collect::<Vec<_>>().join("\n");
    let partial = directory.path().join("partial.jsonl");
    std::fs::write(&partial, truncated).expect("write the truncated recording");

    let request = ReplayRequest::new(&partial, &log_dir);
    let (outcome, store) = slice::replay_into_store(&request).expect("replay the prefix");
    let rendered = slice::show(
        &log_dir,
        ProjectionName::AttentionInbox,
        Some(&outcome.session_id),
    )
    .expect("render the inbox");
    assert!(
        rendered.contains("\"kind\": \"approval\""),
        "an undecided approval must be visible: {rendered}"
    );
    assert!(rendered.contains("\"kind\": \"open\""));
    let session = store
        .state()
        .session(&outcome.session_id)
        .expect("the session exists");
    assert_eq!(
        session.status,
        kaleido_proto::session::SessionStatus::WaitingApproval
    );
    assert_eq!(session.open_attention_count, 1);
}

#[test]
fn fixture_replay_never_claims_live_control_or_a_controlling_binding() {
    for name in FIXTURES {
        let directory = tempfile::tempdir().expect("temporary directory");
        let request = ReplayRequest::new(fixture(name), directory.path().join("log"));
        let (outcome, store) =
            slice::replay_into_store(&request).expect("replay the recorded fixture");
        assert!(
            !outcome.probe.is_proven(Capability::LiveControl),
            "{name}: recorded traffic is not current runtime acceptance"
        );
        assert!(
            !matches!(
                store
                    .state()
                    .session(&outcome.session_id)
                    .expect("session")
                    .live_binding,
                LiveBinding::Controlling { .. }
            ),
            "{name}: replay must not become controlling"
        );
    }
}

#[test]
fn live_orchestration_latches_streaming_and_keeps_steer_pending() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let command_id = IdentityMint::new("hostd-live-test").command_id("submit");
    let submitted_command_id = command_id.clone();
    let mut runtime = FixtureRuntime::new("03-permission-approve.jsonl", command_id.clone());
    let identity = runtime.identity();
    let runtime_id = identity.runtime_id.clone();
    let mut request = RunRequest::new(
        "unused-by-fixture-runtime",
        directory.path(),
        directory.path().join("log"),
        "sensitive test prompt",
    );
    request.decide_first_approval = Some(ApprovalDecision::Accept);
    request.enqueue_steer = Some("sensitive queued input".to_owned());
    let outcome = slice::run_with_session(&request, &mut runtime, identity, command_id)
        .expect("run the recorded live session");
    let report: serde_json::Value =
        serde_json::from_str(&outcome.report_json).expect("parse report");

    let observing = report
        .pointer("/observed/session_index_while_observing/payload/view/active/0/live_binding/kind")
        .or_else(|| {
            report.pointer(
                "/observed/session_index_while_observing/payload/view/history/0/live_binding/kind",
            )
        })
        .and_then(serde_json::Value::as_str);
    assert_eq!(observing, Some("observing"));
    let controlling = report
        .pointer("/observed/session_index_while_controlling/payload/view/active/0/live_binding/kind")
        .or_else(|| {
            report.pointer(
                "/observed/session_index_while_controlling/payload/view/history/0/live_binding/kind",
            )
        })
        .and_then(serde_json::Value::as_str);
    assert_eq!(
        controlling,
        Some("controlling"),
        "the matched turn/start response must promote only this live session"
    );
    let controlling_capabilities = report
        .pointer("/observed/runtime_capability_while_controlling/payload/view/entries")
        .and_then(serde_json::Value::as_array)
        .expect("runtime capabilities are sampled with the controlling binding");
    let live_control = controlling_capabilities
        .iter()
        .find(|entry| {
            entry.get("capability").and_then(serde_json::Value::as_str) == Some("live_control")
        })
        .expect("live_control remains explicit");
    assert_eq!(
        live_control
            .pointer("/state/kind")
            .and_then(serde_json::Value::as_str),
        Some("supported")
    );
    assert_eq!(
        live_control
            .pointer("/evidence/source")
            .and_then(serde_json::Value::as_str),
        Some("observed_in_traffic")
    );
    let controlling_turn_steer = controlling_capabilities
        .iter()
        .find(|entry| {
            entry.get("capability").and_then(serde_json::Value::as_str) == Some("turn_steer")
        })
        .expect("turn_steer remains explicit");
    assert_eq!(
        controlling_turn_steer
            .pointer("/state/kind")
            .and_then(serde_json::Value::as_str),
        Some("not_verified")
    );
    assert_eq!(
        controlling_turn_steer
            .pointer("/evidence/source")
            .and_then(serde_json::Value::as_str),
        Some("absent")
    );
    let streaming = report
        .pointer("/observed/live_activity_while_streaming/payload/view/streaming_item_ids")
        .and_then(serde_json::Value::as_array)
        .expect("streaming evidence is an array");
    assert!(
        !streaming.is_empty(),
        "a real recorded in-progress item must be sampled before completion"
    );
    assert_eq!(
        report
            .pointer("/projections/input-queue/payload/view/steer_supported")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        report
            .pointer("/projections/input-queue/payload/view/entries/0/state/kind")
            .and_then(serde_json::Value::as_str),
        Some("pending"),
        "local enqueueing must never fabricate a delivered steer"
    );
    let capability_entries = report
        .pointer("/projections/runtime-capability/payload/view/entries")
        .and_then(serde_json::Value::as_array)
        .expect("capability entries");
    let turn_steer = capability_entries
        .iter()
        .find(|entry| {
            entry.get("capability").and_then(serde_json::Value::as_str) == Some("turn_steer")
        })
        .expect("turn_steer remains explicit");
    assert_eq!(
        turn_steer
            .pointer("/state/kind")
            .and_then(serde_json::Value::as_str),
        Some("not_verified")
    );
    assert_eq!(
        turn_steer
            .pointer("/evidence/source")
            .and_then(serde_json::Value::as_str),
        Some("absent")
    );
    assert!(!outcome.report_json.contains("delivered_as_steer"));
    assert_eq!(
        report
            .pointer("/observed/steer_delivery_ever_observed")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert!(
        !outcome.report_json.contains("sensitive test prompt")
            && !outcome.report_json.contains("sensitive queued input"),
        "the machine report must only contain content references"
    );

    let canonical_root = std::fs::canonicalize(directory.path())
        .expect("canonical test root")
        .to_string_lossy()
        .into_owned();
    for entry in
        std::fs::read_dir(directory.path().join("log").join("streams")).expect("stream directory")
    {
        let contents =
            std::fs::read_to_string(entry.expect("stream entry").path()).expect("read stream");
        assert!(
            !contents.contains("delivered_as_steer"),
            "durable history must never claim the queued input was injected"
        );
        for forbidden in forbidden_strings().into_iter().chain([
            "sensitive test prompt",
            "sensitive queued input",
            canonical_root.as_str(),
        ]) {
            assert!(
                !contents.contains(forbidden),
                "`{forbidden}` leaked into a live durable stream"
            );
        }
    }
    let mut stored_content = String::new();
    for entry in
        std::fs::read_dir(directory.path().join("log").join("content")).expect("content directory")
    {
        stored_content.push_str(
            &std::fs::read_to_string(entry.expect("content entry").path()).unwrap_or_default(),
        );
    }
    assert!(stored_content.contains("sensitive test prompt"));
    assert!(stored_content.contains("sensitive queued input"));
    assert!(stored_content.contains(&canonical_root));

    let reloaded = CanonicalStore::load(
        directory.path().join("log"),
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS,
        },
    )
    .expect("reload live log");
    reloaded
        .session_snapshot(&outcome.session_id)
        .expect("the live session remains contract-valid after reload");
    let submitted_acks = reloaded
        .state()
        .acknowledgements()
        .iter()
        .filter(|ack| ack.command_id == submitted_command_id)
        .collect::<Vec<_>>();
    assert_eq!(
        submitted_acks.len(),
        2,
        "one local acceptance must be followed by one correlated runtime acceptance"
    );
    assert!(matches!(
        submitted_acks.first().map(|ack| &ack.outcome),
        Some(CommandOutcome::AcceptedLocally { .. })
    ));
    assert!(matches!(
        submitted_acks.get(1).map(|ack| &ack.outcome),
        Some(CommandOutcome::AcceptedByRuntime { binding_handle, .. })
            if binding_handle.kind == ProviderBindingKind::RuntimeAcknowledgement
    ));
    assert!(matches!(
        reloaded
            .state()
            .runtime(&runtime_id)
            .expect("runtime")
            .connection,
        ConnectionState::Disconnected
    ));
    let session = reloaded
        .state()
        .session(&outcome.session_id)
        .expect("session");
    assert_eq!(session.status, SessionStatus::Offline);
    assert!(matches!(
        session.live_binding,
        LiveBinding::NotBound {
            reason: LiveUnboundReason::RuntimeExited
        }
    ));
    assert!(
        !reloaded
            .state()
            .attention_entries()
            .into_iter()
            .any(|entry| matches!(entry.subject, AttentionSubject::ConnectionFault { .. })),
        "an intentional close must not fabricate a connection fault"
    );
    let local_command_id = reloaded
        .state()
        .attention_entries()
        .into_iter()
        .find_map(|entry| match &entry.state {
            AttentionState::Answered {
                answer_source: AttentionAnswerSource::LocalCommand { command_id },
                ..
            } => Some(command_id.clone()),
            _ => None,
        })
        .expect("the broker-owned approval keeps its real local command ID");
    assert!(
        reloaded
            .log()
            .read_all()
            .expect("read live log")
            .iter()
            .any(|record| matches!(
                &record.effect,
                StateEffect::CommandAcknowledged { ack } if ack.command_id == local_command_id
            )),
        "the answer source must reference an actual command acknowledgement"
    );
}

#[test]
fn live_decline_keeps_the_turn_completed_and_declines_only_the_file_change() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let command_id = IdentityMint::new("hostd-live-decline").command_id("submit");
    let mut runtime = FixtureRuntime::new("04-permission-deny.jsonl", command_id.clone());
    let identity = runtime.identity();
    let mut request = RunRequest::new(
        "unused-by-fixture-runtime",
        directory.path(),
        directory.path().join("log"),
        "sensitive test prompt",
    );
    request.decide_first_approval = Some(ApprovalDecision::Decline);
    let outcome = slice::run_with_session(&request, &mut runtime, identity, command_id)
        .expect("run the recorded decline");
    let report: serde_json::Value =
        serde_json::from_str(&outcome.report_json).expect("parse report");
    let turn = report
        .pointer("/projections/transcript/payload/view/turns/0/turn")
        .expect("completed turn");
    assert_eq!(
        turn.get("status").and_then(serde_json::Value::as_str),
        Some("completed")
    );
    assert!(turn.get("error").is_some_and(serde_json::Value::is_null));
    let items = report
        .pointer("/projections/transcript/payload/view/turns/0/items")
        .and_then(serde_json::Value::as_array)
        .expect("turn items");
    assert!(items.iter().any(|item| {
        item.pointer("/body/kind")
            .and_then(serde_json::Value::as_str)
            == Some("file_edit")
            && item.get("status").and_then(serde_json::Value::as_str) == Some("declined")
    }));
}

#[test]
fn live_process_exit_is_durable_offline_state_and_one_connection_fault() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let command_id = IdentityMint::new("hostd-live-exit").command_id("submit");
    let mut runtime = FixtureRuntime::exiting("01-simple-turn.jsonl", command_id.clone());
    let identity = runtime.identity();
    let runtime_id = identity.runtime_id.clone();
    let request = RunRequest::new(
        "unused-by-fixture-runtime",
        directory.path(),
        directory.path().join("log"),
        "sensitive test prompt",
    );
    let outcome = slice::run_with_session(&request, &mut runtime, identity, command_id)
        .expect("preserve an early exit as canonical state");
    let reloaded = CanonicalStore::load(
        directory.path().join("log"),
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS,
        },
    )
    .expect("reload exit log");
    let runtime = reloaded.state().runtime(&runtime_id).expect("runtime");
    assert!(matches!(
        runtime.connection,
        ConnectionState::Unavailable {
            reason: ConnectionFaultReason::ProcessExited {
                exit_code: Some(23)
            },
            ..
        }
    ));
    let session = reloaded
        .state()
        .session(&outcome.session_id)
        .expect("session");
    assert_eq!(session.status, SessionStatus::Offline);
    assert!(matches!(
        session.live_binding,
        LiveBinding::NotBound {
            reason: LiveUnboundReason::RuntimeExited
        }
    ));
    let faults = reloaded
        .state()
        .attention_entries()
        .into_iter()
        .filter(|entry| matches!(entry.subject, AttentionSubject::ConnectionFault { .. }))
        .count();
    assert_eq!(faults, 1);
    reloaded
        .session_snapshot(&outcome.session_id)
        .expect("exit snapshot remains valid");
}
