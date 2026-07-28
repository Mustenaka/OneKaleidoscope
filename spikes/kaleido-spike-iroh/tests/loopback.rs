#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

#[path = "../src/error.rs"]
pub mod error;
#[path = "../src/probe.rs"]
pub mod probe;
#[path = "../src/record.rs"]
pub mod record;

use tempfile::tempdir;
use tokio::time;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_probe_exchanges_every_ping_and_persists_parseable_records() {
    let temp = tempdir().expect("temporary result directory must be created");
    let results = temp.path().join("results.jsonl");

    let listener = probe::bind_listener(false)
        .await
        .expect("loopback listener must bind");
    let dialer = probe::bind_dialer()
        .await
        .expect("loopback dialer must bind");
    let remote = probe::loopback_addr(&listener).expect("listener must publish a local address");

    let listener_endpoint = listener.clone();
    let listener_task = tokio::spawn(async move {
        let incoming = time::timeout(Duration::from_secs(10), listener_endpoint.accept())
            .await
            .expect("listener must receive a loopback connection before timeout")
            .expect("listener endpoint must remain open");
        probe::serve_connection(&listener_endpoint, incoming, None)
            .await
            .expect("listener probe must complete")
    });

    let dial_result = probe::dial_addr(
        &dialer,
        remote,
        probe::new_record("dial", "loopback", 2),
        Duration::from_secs(10),
    )
    .await;
    let listen_result = listener_task.await;
    let dial_record = dial_result.expect("dial probe must complete");
    let listen_record = listen_result.expect("listener task must not panic");

    record::append_record(&results, &listen_record).expect("listener record must be appended");
    record::append_record(&results, &dial_record).expect("dial record must be appended");

    dialer.close().await;
    listener.close().await;

    let mut warnings = Vec::new();
    let parsed =
        record::read_records(&results, &mut warnings).expect("JSONL must be readable after append");
    let parsed_dial = parsed
        .iter()
        .find(|entry| entry.role == "dial")
        .expect("dial-side record must be present");

    assert!(parsed_dial.connect_ok);
    assert!(parsed_dial.pings_sent > 0);
    assert_eq!(parsed_dial.pings_sent, parsed_dial.pongs_recv);
    assert!(parsed_dial.ended_direct);
    assert_eq!(parsed_dial.selected_path_at_end.as_deref(), Some("ip"));
    assert!(parsed_dial.direct_path_opened_ms.is_some());
    assert!(parsed_dial.direct_path_selected_ms.is_some());
    assert!(
        parsed_dial.direct_path_opened_ms <= parsed_dial.direct_path_selected_ms,
        "an IP path must open no later than it is selected"
    );
    assert_eq!(parsed.len(), 2);
    assert!(warnings.is_empty());
}
