#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::{Duration, Instant};

use kaleido_adapter::{IdentityMint, SessionStartRequest};
use kaleido_hostd::slice::REPLAY_BASE_AT_MS;
use kaleido_hostd::{Broker, ReadyRecoveryOutcome, RuntimeSupervisor};
use kaleido_proto::command::{Command, CommandOutcome, DeviceCommandRequest};
use kaleido_proto::content::{ContentKind, ContentWriteRequest, ContentWriteResponse, Sensitivity};
use kaleido_proto::error::ErrorCode;
use kaleido_proto::ids::DeviceId;
use kaleido_proto::queue::QueueIntent;
use kaleido_state::{CanonicalStore, ClockSource};
use sha2::{Digest, Sha256};

mod support;

use support::FixtureRuntime;

#[test]
fn runtime_is_resolved_before_claim_and_only_structured_submit_response_completes_it() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let log_dir = directory.path().join("canonical");
    let broker = Broker::open(
        &log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS,
        },
        "kaleido-host",
        "kaleido-host",
    )
    .expect("open canonical broker");
    let device_id = DeviceId::new("device-runtime");
    let idempotency_key = "submit-one";
    let seed = format!(
        "{}:{}|{}:{}",
        device_id.as_str().len(),
        device_id,
        idempotency_key.len(),
        idempotency_key
    );
    let command_id = IdentityMint::new("kaleido-host").command_id(&seed);
    let runtime = FixtureRuntime::new("01-simple-turn.jsonl", command_id.clone());
    let identity = runtime.identity();
    let root_ref = broker
        .content_store()
        .store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            b"fixture-project-root",
        )
        .expect("store canonical project root");
    let start_request = SessionStartRequest {
        project_id: identity.project_id,
        project_binding_id: identity.project_binding_id,
        runtime_id: identity.runtime_id,
        project_root_ref: root_ref,
    };
    let supervisor = RuntimeSupervisor::new(broker.clone());
    let session_id = supervisor
        .start_runtime(start_request.clone(), Box::new(runtime))
        .expect("runtime becomes ready after bootstrap effects");

    let prompt = b"mobile prompt body";
    let response = broker
        .write_content(
            &device_id,
            &write_request(prompt),
            prompt,
            REPLAY_BASE_AT_MS + 10,
        )
        .expect("write prompt");
    let prompt_ref = match response {
        ContentWriteResponse::Stored { content_ref } => Some(content_ref),
        ContentWriteResponse::Rejected { .. } => None,
    }
    .expect("prompt is stored");
    let admission = broker
        .admit_device_command(
            &device_id,
            &DeviceCommandRequest {
                idempotency_key: idempotency_key.to_owned(),
                ttl_ms: Some(30_000),
                body: Command::SubmitPrompt {
                    session_id: session_id.clone(),
                    body: prompt_ref,
                },
            },
            REPLAY_BASE_AT_MS + 11,
        )
        .expect("admit submit");
    assert!(matches!(
        admission.ack.outcome,
        CommandOutcome::AcceptedLocally { .. }
    ));
    let ticket = admission.dispatch_ticket.expect("submit has runtime route");
    assert_eq!(ticket.command_id(), &command_id);

    supervisor
        .dispatch_ticket(&ticket)
        .expect("ready runtime is resolved before durable claim");
    let report = wait_for_report(&supervisor);
    assert_eq!(report.command_id, command_id);
    assert_eq!(report.result, Ok(()));
    assert!(broker.pending_dispatches().is_empty());

    let queued = b"queued body";
    let queued_ref = match broker
        .write_content(
            &device_id,
            &write_request(queued),
            queued,
            REPLAY_BASE_AT_MS + 20,
        )
        .expect("write queued input")
    {
        ContentWriteResponse::Stored { content_ref } => Some(content_ref),
        ContentWriteResponse::Rejected { .. } => None,
    }
    .expect("queued input is stored");
    let enqueue = broker
        .admit_device_command(
            &device_id,
            &DeviceCommandRequest {
                idempotency_key: "enqueue-one".to_owned(),
                ttl_ms: None,
                body: Command::EnqueueInput {
                    session_id: session_id.clone(),
                    body: queued_ref,
                    intent: QueueIntent::NewTurn,
                },
            },
            REPLAY_BASE_AT_MS + 21,
        )
        .expect("admit enqueue");
    assert!(matches!(
        enqueue.ack.outcome,
        CommandOutcome::Enqueued { .. }
    ));
    assert!(
        enqueue.dispatch_ticket.is_none(),
        "enqueue has no runtime route"
    );

    supervisor.stop_session(&session_id).expect("stop runtime");
    drop(supervisor);
    drop(broker);
    let reloaded = CanonicalStore::load(
        &log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS + 100,
        },
    )
    .expect("reload canonical command history");
    let correlated = reloaded
        .state()
        .acknowledgements()
        .iter()
        .filter(|ack| ack.command_id == command_id)
        .collect::<Vec<_>>();
    assert_eq!(correlated.len(), 2);
    assert!(matches!(
        correlated.first().map(|ack| &ack.outcome),
        Some(CommandOutcome::AcceptedLocally { .. })
    ));
    assert!(matches!(
        correlated.get(1).map(|ack| &ack.outcome),
        Some(CommandOutcome::AcceptedByRuntime { .. })
    ));
}

#[test]
fn a_claimed_command_is_uncertain_after_restart_and_is_never_redispatched() {
    let replayed = support::replay("01-simple-turn.jsonl");
    let broker = Broker::load(
        &replayed.log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS + 1,
        },
        "kaleido-host",
        "kaleido-host",
    )
    .expect("load fixture broker");
    let device_id = DeviceId::new("claimed-device");
    let body = b"claimed prompt";
    let body_ref = match broker
        .write_content(
            &device_id,
            &write_request(body),
            body,
            REPLAY_BASE_AT_MS + 2,
        )
        .expect("write prompt")
    {
        ContentWriteResponse::Stored { content_ref } => Some(content_ref),
        ContentWriteResponse::Rejected { .. } => None,
    }
    .expect("stored prompt");
    let admission = broker
        .admit_device_command(
            &device_id,
            &DeviceCommandRequest {
                idempotency_key: "claimed-before-crash".to_owned(),
                ttl_ms: Some(30_000),
                body: Command::SubmitPrompt {
                    session_id: replayed.session_id,
                    body: body_ref,
                },
            },
            REPLAY_BASE_AT_MS + 3,
        )
        .expect("admit prompt");
    let ticket = admission.dispatch_ticket.expect("ready ticket");
    broker
        .claim_dispatch(&ticket, REPLAY_BASE_AT_MS + 4)
        .expect("durable claim");
    drop(broker);

    let reloaded = Broker::load(
        &replayed.log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS + 5,
        },
        "kaleido-host",
        "kaleido-host",
    )
    .expect("reload claimed outbox");
    assert!(reloaded.pending_dispatches().is_empty());
    assert!(RuntimeSupervisor::new(reloaded)
        .dispatch_all_ready()
        .is_empty());
}

#[test]
fn an_old_ready_session_is_rejected_once_without_claiming_or_sending() {
    let replayed = support::replay("01-simple-turn.jsonl");
    let broker = Broker::load(
        &replayed.log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS + 1,
        },
        "kaleido-host",
        "kaleido-host",
    )
    .expect("load old session");
    let device_id = DeviceId::new("old-ready-device");
    let body = b"old ready prompt";
    let body_ref = match broker
        .write_content(
            &device_id,
            &write_request(body),
            body,
            REPLAY_BASE_AT_MS + 2,
        )
        .expect("write prompt")
    {
        ContentWriteResponse::Stored { content_ref } => Some(content_ref),
        ContentWriteResponse::Rejected { .. } => None,
    }
    .expect("stored prompt");
    let admission = broker
        .admit_device_command(
            &device_id,
            &DeviceCommandRequest {
                idempotency_key: "old-ready-before-restart".to_owned(),
                ttl_ms: Some(30_000),
                body: Command::SubmitPrompt {
                    session_id: replayed.session_id,
                    body: body_ref,
                },
            },
            REPLAY_BASE_AT_MS + 3,
        )
        .expect("admit ready prompt");
    let command_id = admission.ack.command_id.clone();
    assert!(admission.dispatch_ticket.is_some());

    let supervisor = RuntimeSupervisor::new(broker.clone());
    let recovered = supervisor.recover_all_ready();
    assert!(matches!(
        recovered.as_slice(),
        [(recovered_id, Ok(ReadyRecoveryOutcome::RejectedRuntimeUnavailable))]
            if recovered_id == &command_id
    ));
    assert!(broker.pending_dispatches().is_empty());
    assert!(supervisor.recover_all_ready().is_empty());
    drop(supervisor);
    drop(broker);

    let reloaded = CanonicalStore::load(
        &replayed.log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS + 4,
        },
    )
    .expect("reload completed rejection");
    let acks = reloaded
        .state()
        .acknowledgements()
        .iter()
        .filter(|ack| ack.command_id == command_id)
        .collect::<Vec<_>>();
    assert_eq!(acks.len(), 2);
    assert!(matches!(
        acks.get(1).map(|ack| &ack.outcome),
        Some(CommandOutcome::Rejected { error })
            if error.code == ErrorCode::RuntimeUnavailable && error.retriable
    ));
}

fn write_request(bytes: &[u8]) -> ContentWriteRequest {
    ContentWriteRequest {
        content_kind: ContentKind::PlainText,
        byte_len: u64::try_from(bytes.len()).expect("body length"),
        digest: format!("sha256:{:x}", Sha256::digest(bytes)),
    }
}

fn wait_for_report(supervisor: &RuntimeSupervisor) -> kaleido_hostd::RuntimeDispatchReport {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(report) = supervisor.try_report() {
            return report;
        }
        assert!(Instant::now() < deadline, "runtime report timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
