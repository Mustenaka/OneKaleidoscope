#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "../src/error.rs"]
pub mod error;
#[path = "../src/record.rs"]
pub mod record;

use record::{summarize, ProbeRecord, RECORD_SCHEMA};

static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

fn probe_record(
    label: &str,
    connect_ok: bool,
    ended_direct: bool,
    direct_path_selected_ms: Option<u64>,
) -> ProbeRecord {
    ProbeRecord {
        schema: RECORD_SCHEMA,
        run_id: format!("run-{}", NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed)),
        started_at: "2026-07-28T09:12:44Z".to_owned(),
        role: "dial".to_owned(),
        label: label.to_owned(),
        iroh_version: "1.0.3".to_owned(),
        os: "windows".to_owned(),
        remote_endpoint_id: Some("test-endpoint".to_owned()),
        connect_ok,
        connect_ms: connect_ok.then_some(25),
        direct_path_opened_ms: direct_path_selected_ms.map(|value| value / 2),
        direct_path_selected_ms,
        ended_direct,
        selected_path_at_end: connect_ok.then(|| {
            if ended_direct {
                "ip".to_owned()
            } else {
                "relay".to_owned()
            }
        }),
        relay_url: Some("https://example.invalid/".to_owned()),
        local_direct_addrs: vec!["127.0.0.1:49152".to_owned()],
        rtt_relay_ms: connect_ok.then_some(20.0),
        rtt_direct_ms: direct_path_selected_ms.map(|_| 10.0),
        pings_sent: if connect_ok { 1 } else { 0 },
        pongs_recv: if connect_ok { 1 } else { 0 },
        window_secs: 30,
        error: (!connect_ok).then(|| "Connect: test failure".to_owned()),
    }
}

#[test]
fn two_of_three_direct_is_66_7_percent_and_optional() {
    let records = vec![
        probe_record("4g", true, true, Some(1_000)),
        probe_record("4g", true, true, Some(3_000)),
        probe_record("4g", true, false, None),
    ];

    assert_eq!(
        summarize(&records),
        concat!(
            "label=4g   runs=3  connect_ok=3  direct=2 (66.7%)  relay_only=1  ",
            "median_time_to_direct=2.00s\n",
            "G0 VERDICT: direct rate 66.7% >= 60.0% -> L2 relay stays OPTIONAL"
        )
    );
}

#[test]
fn two_of_five_direct_is_40_percent_and_failure_stays_in_denominator() {
    let records = vec![
        probe_record("4g", true, true, Some(1_000)),
        probe_record("4g", true, true, Some(2_000)),
        probe_record("4g", true, false, None),
        probe_record("4g", true, false, None),
        probe_record("4g", false, false, None),
    ];

    assert_eq!(
        summarize(&records),
        concat!(
            "label=4g   runs=5  connect_ok=4  direct=2 (40.0%)  relay_only=2  ",
            "median_time_to_direct=1.50s\n",
            "G0 VERDICT: direct rate 40.0% < 60.0% -> L2 relay becomes MANDATORY for v1"
        )
    );
}

#[test]
fn exactly_60_percent_is_optional() {
    let mut records = Vec::new();
    for _ in 0..6 {
        records.push(probe_record("4g", true, true, Some(1_000)));
    }
    for _ in 0..4 {
        records.push(probe_record("4g", true, false, None));
    }

    assert!(
        summarize(&records).ends_with("direct rate 60.0% >= 60.0% -> L2 relay stays OPTIONAL"),
        "the exact threshold must use >= rather than >"
    );
}

#[test]
fn fifty_nine_percent_is_mandatory() {
    let mut records = Vec::new();
    for _ in 0..59 {
        records.push(probe_record("4g", true, true, Some(1_000)));
    }
    for _ in 0..41 {
        records.push(probe_record("4g", true, false, None));
    }

    assert!(
        summarize(&records)
            .ends_with("direct rate 59.0% < 60.0% -> L2 relay becomes MANDATORY for v1"),
        "the first whole-percent value below the threshold must remain mandatory"
    );
}

#[test]
fn g0_aggregates_all_4g_prefix_labels_and_excludes_other_labels() {
    let mut records = vec![
        probe_record("4g", true, true, Some(1_000)),
        probe_record("4g", true, false, None),
        probe_record("4g-tether", true, true, Some(2_000)),
        probe_record("4g-tether", true, true, Some(3_000)),
        probe_record("4g-tether", false, false, None),
    ];
    records.extend((0..10).map(|_| probe_record("lan", false, false, None)));

    let output = summarize(&records);
    assert_eq!(output.matches("G0 VERDICT:").count(), 1);
    assert!(
        output.ends_with("direct rate 60.0% >= 60.0% -> L2 relay stays OPTIONAL"),
        "the verdict must aggregate 3 direct runs out of the five 4g* runs only"
    );
}

#[test]
fn even_median_averages_middle_selected_path_times() {
    let mut first = probe_record("lan", true, true, Some(1_000));
    first.direct_path_opened_ms = Some(50_000);
    let mut second = probe_record("lan", true, true, Some(4_000));
    second.direct_path_opened_ms = Some(90_000);

    let output = summarize(&[first, second]);

    assert_eq!(
        output,
        concat!(
            "label=lan  runs=2  connect_ok=2  direct=2 (100.0%)  relay_only=0  ",
            "median_time_to_direct=2.50s"
        )
    );
    assert!(!output.contains("G0 VERDICT:"));
}

#[test]
fn serialized_record_contains_every_fixed_schema_field() {
    let value = serde_json::to_value(probe_record("4g", false, false, None))
        .expect("the fixture record must serialize");
    let object = value
        .as_object()
        .expect("ProbeRecord must serialize as a JSON object");
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected = BTreeSet::from([
        "schema",
        "run_id",
        "started_at",
        "role",
        "label",
        "iroh_version",
        "os",
        "remote_endpoint_id",
        "connect_ok",
        "connect_ms",
        "direct_path_opened_ms",
        "direct_path_selected_ms",
        "ended_direct",
        "selected_path_at_end",
        "relay_url",
        "local_direct_addrs",
        "rtt_relay_ms",
        "rtt_direct_ms",
        "pings_sent",
        "pongs_recv",
        "window_secs",
        "error",
    ]);

    assert_eq!(actual, expected);
    assert_eq!(
        object.get("schema").and_then(serde_json::Value::as_u64),
        Some(u64::from(RECORD_SCHEMA))
    );
    assert!(object
        .get("connect_ms")
        .is_some_and(serde_json::Value::is_null));
    assert!(object
        .get("direct_path_selected_ms")
        .is_some_and(serde_json::Value::is_null));
}

#[test]
fn mirrored_listener_and_dial_records_count_as_one_run() {
    let mut listener = probe_record("windows-listen", true, false, None);
    listener.role = "listen".to_owned();
    let mut dial = probe_record("4g", true, true, Some(2_000));
    dial.run_id.clone_from(&listener.run_id);
    let failure = probe_record("4g", false, false, None);

    assert_eq!(
        summarize(&[listener, dial, failure]),
        concat!(
            "label=4g   runs=2  connect_ok=1  direct=1 (50.0%)  relay_only=0  ",
            "median_time_to_direct=2.00s\n",
            "G0 VERDICT: direct rate 50.0% < 60.0% -> L2 relay becomes MANDATORY for v1"
        )
    );
}

#[test]
fn connected_probe_error_is_not_reported_as_relay_only() {
    let mut record = probe_record("4g-error", true, false, None);
    record.selected_path_at_end = None;
    record.error = Some("Stream: probe failed after connect".to_owned());

    assert_eq!(
        summarize(&[record]),
        concat!(
            "label=4g-error  runs=1  connect_ok=1  direct=0 (0.0%)  relay_only=0  ",
            "median_time_to_direct=n/a\n",
            "G0 VERDICT: direct rate 0.0% < 60.0% -> L2 relay becomes MANDATORY for v1"
        )
    );
}
