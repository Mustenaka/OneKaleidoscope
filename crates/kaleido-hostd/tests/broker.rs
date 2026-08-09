#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use kaleido_hostd::broker::{Broker, SubscriptionEvent};
use kaleido_hostd::slice::{self, ReplayRequest, REPLAY_BASE_AT_MS};
use kaleido_proto::effect::{Cursor, StateEffect};
use kaleido_proto::error::ErrorCode;
use kaleido_proto::host::{Host, HostPlatform, HostReachability};
use kaleido_proto::projection::{
    ProjectionKey, ProjectionPayload, ProjectionSubscribe, ProjectionSubscribeOutcome,
};
use kaleido_state::ClockSource;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("crate sits two levels below the repository root")
}

fn replayed_broker() -> (tempfile::TempDir, Broker) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let log_dir = directory.path().join("log");
    let request = ReplayRequest::new(
        repository_root().join("tests/fixtures/codex/01-simple-turn.jsonl"),
        &log_dir,
    );
    slice::replay(&request).expect("replay real Codex fixture");
    let broker = Broker::load(
        &log_dir,
        ClockSource::Fixed {
            at_ms: REPLAY_BASE_AT_MS + 1_000,
        },
        "kaleido-host",
        "kaleido-host",
    )
    .expect("load broker");
    (directory, broker)
}

fn project_index_request(broker: &Broker, since: Option<Cursor>) -> ProjectionSubscribe {
    ProjectionSubscribe {
        key: ProjectionKey::ProjectIndex {
            host_id: broker.host_id(),
        },
        since,
    }
}

#[test]
fn listener_owns_reachability_and_slow_subscription_closes_alone() {
    let (_directory, broker) = replayed_broker();
    let current = broker
        .subscribe(&project_index_request(&broker, None), REPLAY_BASE_AT_MS)
        .expect("current projection");
    let head = match &current.replay().ack.outcome {
        ProjectionSubscribeOutcome::CurrentFollows { current_cursor } => *current_cursor,
        other => panic!("expected current projection, got {other:?}"),
    };
    assert_eq!(current.replay().envelopes.len(), 1);
    drop(current);

    let slow = broker
        .subscribe_with_capacity(
            &project_index_request(&broker, Some(head)),
            REPLAY_BASE_AT_MS,
            1,
        )
        .expect("slow subscriber");
    let healthy = broker
        .subscribe_with_capacity(
            &project_index_request(&broker, Some(head)),
            REPLAY_BASE_AT_MS,
            4,
        )
        .expect("healthy subscriber");
    assert!(slow.replay().envelopes.is_empty());
    assert!(healthy.replay().envelopes.is_empty());

    broker
        .set_lan_ready(true, REPLAY_BASE_AT_MS + 1)
        .expect("listener ready");
    let provider_bootstrap = StateEffect::HostUpserted {
        host: Host {
            id: broker.host_id(),
            display_name: "kaleido-host".to_owned(),
            platform: HostPlatform::Windows,
            reachability: HostReachability::Offline,
            protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
            last_seen_at_ms: REPLAY_BASE_AT_MS + 2,
        },
    };
    assert!(broker
        .apply_effect(&provider_bootstrap, REPLAY_BASE_AT_MS + 2)
        .expect("provider bootstrap cannot own LAN reachability")
        .is_empty());
    broker
        .set_lan_ready(false, REPLAY_BASE_AT_MS + 3)
        .expect("listener closed");

    assert!(matches!(
        slow.recv_timeout(Duration::from_millis(10)),
        Some(SubscriptionEvent::Closed(error))
            if error.code == ErrorCode::CursorGap && error.retriable && error.detail_ref.is_none()
    ));

    let first = healthy
        .recv_timeout(Duration::from_millis(10))
        .expect("first healthy push");
    let second = healthy
        .recv_timeout(Duration::from_millis(10))
        .expect("second healthy push");
    let reaches = [first, second].map(|event| match event {
        SubscriptionEvent::Projection(envelope) => match envelope.payload {
            ProjectionPayload::ProjectIndex { view } => view.reachability,
            other => panic!("unexpected payload {other:?}"),
        },
        SubscriptionEvent::Closed(error) => panic!("healthy subscriber closed: {error:?}"),
    });
    assert_eq!(
        reaches,
        [HostReachability::LanDirect, HostReachability::Offline]
    );
}
