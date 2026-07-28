use std::fmt;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{SecondsFormat, Utc};
use futures_util::StreamExt;
use iroh::{
    endpoint::{presets, Connection, Incoming, PathEvent, PathEventStream, RecvStream, SendStream},
    Endpoint, EndpointAddr, TransportAddr,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};
use tracing::warn;
use uuid::Uuid;

use crate::error::SpikeError;
use crate::record::{ProbeRecord, RECORD_SCHEMA};

pub const ALPN: &[u8] = b"kaleido-spike-iroh/1";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_WINDOW_SECS: u64 = 30;

const PING_INTERVAL: Duration = Duration::from_millis(500);
const STREAM_FINISH_GRACE: Duration = Duration::from_secs(5);
const MAX_FRAME_BYTES: usize = 64 * 1024;
const IROH_VERSION: &str = "1.0.3";

pub type ProbeResult<T> = Result<T, ProbeFailure>;

#[derive(Debug)]
pub struct ProbeFailure {
    pub record: ProbeRecord,
    pub error: SpikeError,
}

impl fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ProbeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

pub fn new_record(role: &str, label: &str, window_secs: u64) -> ProbeRecord {
    ProbeRecord {
        schema: RECORD_SCHEMA,
        run_id: Uuid::new_v4().simple().to_string(),
        started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        role: role.to_owned(),
        label: label.to_owned(),
        iroh_version: IROH_VERSION.to_owned(),
        os: std::env::consts::OS.to_owned(),
        remote_endpoint_id: None,
        connect_ok: false,
        connect_ms: None,
        direct_path_opened_ms: None,
        direct_path_selected_ms: None,
        ended_direct: false,
        selected_path_at_end: None,
        relay_url: None,
        local_direct_addrs: Vec::new(),
        rtt_relay_ms: None,
        rtt_direct_ms: None,
        pings_sent: 0,
        pongs_recv: 0,
        window_secs,
        error: None,
    }
}

pub fn encode_ticket(addr: &EndpointAddr) -> Result<String, SpikeError> {
    let json = serde_json::to_vec(addr).map_err(SpikeError::TicketEncode)?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

pub fn decode_ticket(ticket: &str) -> Result<EndpointAddr, SpikeError> {
    let json = URL_SAFE_NO_PAD
        .decode(ticket)
        .map_err(SpikeError::InvalidTicketBase64)?;
    serde_json::from_slice(&json).map_err(SpikeError::InvalidTicketPayload)
}

pub async fn bind_listener(wait_online: bool) -> Result<Endpoint, SpikeError> {
    let endpoint = Endpoint::builder(presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .map_err(|error| SpikeError::Bind(error.to_string()))?;
    if wait_online {
        endpoint.online().await;
    }
    Ok(endpoint)
}

pub async fn bind_dialer() -> Result<Endpoint, SpikeError> {
    Endpoint::bind(presets::N0)
        .await
        .map_err(|error| SpikeError::Bind(error.to_string()))
}

pub fn loopback_addr(endpoint: &Endpoint) -> Result<EndpointAddr, SpikeError> {
    let addr = endpoint.addr();
    if addr.ip_addrs().next().is_none() && addr.relay_urls().next().is_none() {
        return Err(SpikeError::Bind(
            "bound endpoint has no dialable transport address".to_owned(),
        ));
    }
    Ok(addr)
}

pub async fn dial(ticket: &str, label: &str, window_secs: u64) -> ProbeResult<ProbeRecord> {
    dial_with_timeout(ticket, label, window_secs, CONNECT_TIMEOUT).await
}

pub async fn dial_with_timeout(
    ticket: &str,
    label: &str,
    window_secs: u64,
    connect_timeout: Duration,
) -> ProbeResult<ProbeRecord> {
    let record = new_record("dial", label, window_secs);
    let remote = decode_ticket(ticket).map_err(|error| failed(record.clone(), error))?;
    let endpoint = bind_dialer()
        .await
        .map_err(|error| failed(record.clone(), error))?;
    let result = dial_addr(&endpoint, remote, record, connect_timeout).await;
    endpoint.close().await;
    result
}

pub async fn dial_addr(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    mut record: ProbeRecord,
    connect_timeout: Duration,
) -> ProbeResult<ProbeRecord> {
    let run_started = Instant::now();
    record.remote_endpoint_id = Some(remote.id.to_string());
    record.relay_url = remote.relay_urls().next().map(ToString::to_string);
    record.local_direct_addrs = endpoint
        .addr()
        .ip_addrs()
        .map(|addr| addr.to_string())
        .collect();

    let connection = match time::timeout(connect_timeout, endpoint.connect(remote, ALPN)).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            return Err(failed(record, SpikeError::Connect(error.to_string())));
        }
        Err(_) => {
            return Err(failed(record, SpikeError::ConnectTimeout(connect_timeout)));
        }
    };

    record.connect_ok = true;
    record.connect_ms = Some(elapsed_ms(run_started));

    let window = Duration::from_secs(record.window_secs);
    let deadline = checked_deadline(window).map_err(|error| failed(record.clone(), error))?;
    let path_task = start_path_observer(&connection, run_started, deadline);

    let hello = WireMessage::Hello {
        run_id: record.run_id.clone(),
        label: record.label.clone(),
        window_secs: record.window_secs,
    };
    let stream_setup = async {
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| SpikeError::Stream(error.to_string()))?;
        write_frame(&mut send, &hello).await?;
        Ok::<_, SpikeError>((send, recv))
    };
    let (mut send, recv) = match time::timeout(connect_timeout, stream_setup).await {
        Ok(Ok(streams)) => streams,
        Ok(Err(error)) => {
            path_task.abort();
            return Err(failed(record, error));
        }
        Err(_) => {
            path_task.abort();
            return Err(failed(
                record,
                SpikeError::StreamTimeout {
                    operation: "opening stream and sending hello",
                    duration: connect_timeout,
                },
            ));
        }
    };

    let recv_task = tokio::spawn(receive_pongs(recv));
    let pings_sent = match send_pings(&mut send, run_started, deadline).await {
        Ok(count) => count,
        Err(error) => {
            recv_task.abort();
            path_task.abort();
            return Err(failed(record, error));
        }
    };

    let pongs_recv = match await_receiver(recv_task).await {
        Ok(count) => count,
        Err(error) => {
            path_task.abort();
            return Err(failed(record, error));
        }
    };
    let summary = match path_task.await {
        Ok(summary) => summary,
        Err(error) => {
            return Err(failed(
                record,
                SpikeError::Stream(format!("path observer task failed: {error}")),
            ));
        }
    };

    record.pings_sent = pings_sent;
    record.pongs_recv = pongs_recv;
    apply_path_summary(endpoint, &mut record, summary);
    Ok(record)
}

pub async fn serve_connection(
    endpoint: &Endpoint,
    incoming: Incoming,
    label_override: Option<&str>,
) -> ProbeResult<ProbeRecord> {
    let mut record = new_record(
        "listen",
        label_override.unwrap_or("unknown"),
        DEFAULT_WINDOW_SECS,
    );
    let run_started = Instant::now();
    let connection = incoming
        .await
        .map_err(|error| failed(record.clone(), SpikeError::Accept(error.to_string())))?;

    record.connect_ok = true;
    record.connect_ms = Some(elapsed_ms(run_started));
    record.remote_endpoint_id = Some(connection.remote_id().to_string());

    // Subscribe before reading the hello so path transitions during stream setup are retained.
    let path_events = connection.path_events();
    let initial_paths = PathTracker::from_snapshot(&connection, run_started);

    let stream_setup = async {
        let (send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| SpikeError::Stream(error.to_string()))?;
        let hello = read_frame::<WireMessage>(&mut recv).await?;
        Ok::<_, SpikeError>((send, recv, hello))
    };
    let (mut send, mut recv, hello) = match time::timeout(CONNECT_TIMEOUT, stream_setup).await {
        Ok(Ok(setup)) => setup,
        Ok(Err(error)) => return Err(failed(record, error)),
        Err(_) => {
            return Err(failed(
                record,
                SpikeError::StreamTimeout {
                    operation: "accepting stream and receiving hello",
                    duration: CONNECT_TIMEOUT,
                },
            ));
        }
    };
    let (run_id, peer_label, window_secs) = match hello {
        WireMessage::Hello {
            run_id,
            label,
            window_secs,
        } => (run_id, label, window_secs),
        _ => {
            return Err(failed(
                record,
                SpikeError::Protocol("first frame was not hello".to_owned()),
            ));
        }
    };

    if run_id.is_empty() || peer_label.is_empty() {
        return Err(failed(
            record,
            SpikeError::Protocol("hello contains an empty run_id or label".to_owned()),
        ));
    }
    record.run_id = run_id;
    record.label = label_override.unwrap_or(&peer_label).to_owned();
    record.window_secs = window_secs;

    let deadline = checked_deadline(Duration::from_secs(window_secs))
        .map_err(|error| failed(record.clone(), error))?;
    let (path_stop, path_stop_rx) = oneshot::channel();
    let path_task = start_path_observer_until_signal(
        &connection,
        run_started,
        path_events,
        initial_paths,
        path_stop_rx,
    );
    let (pings_received, pongs_sent) =
        match echo_pings(&mut send, &mut recv, deadline, path_stop).await {
            Ok(counts) => counts,
            Err(error) => {
                path_task.abort();
                return Err(failed(record, error));
            }
        };
    let summary = match path_task.await {
        Ok(summary) => summary,
        Err(error) => {
            return Err(failed(
                record,
                SpikeError::Stream(format!("path observer task failed: {error}")),
            ));
        }
    };

    record.pings_sent = pings_received;
    record.pongs_recv = pongs_sent;
    apply_path_summary(endpoint, &mut record, summary);
    Ok(record)
}

fn failed(mut record: ProbeRecord, error: SpikeError) -> ProbeFailure {
    record.error = Some(error.to_string());
    ProbeFailure { record, error }
}

fn checked_deadline(duration: Duration) -> Result<Instant, SpikeError> {
    Instant::now()
        .checked_add(duration)
        .ok_or_else(|| SpikeError::Protocol("observation window is too large".to_owned()))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireMessage {
    Hello {
        run_id: String,
        label: String,
        window_secs: u64,
    },
    Ping {
        seq: u64,
        monotonic_ns: u64,
    },
    Pong {
        seq: u64,
        monotonic_ns: u64,
    },
    Done,
}

async fn write_frame<T: Serialize + ?Sized>(
    send: &mut SendStream,
    value: &T,
) -> Result<(), SpikeError> {
    let body = serde_json::to_vec(value)
        .map_err(|error| SpikeError::Protocol(format!("frame encode failed: {error}")))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(SpikeError::Protocol(
            "outgoing frame is too large".to_owned(),
        ));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| SpikeError::Protocol("outgoing frame is too large".to_owned()))?;
    send.write_all(&length.to_be_bytes())
        .await
        .map_err(|error| SpikeError::Stream(error.to_string()))?;
    send.write_all(&body)
        .await
        .map_err(|error| SpikeError::Stream(error.to_string()))
}

async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T, SpikeError> {
    let mut header = [0_u8; 4];
    recv.read_exact(&mut header)
        .await
        .map_err(|error| SpikeError::Stream(error.to_string()))?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(SpikeError::Protocol(
            "incoming frame is too large".to_owned(),
        ));
    }
    let mut body = vec![0_u8; length];
    recv.read_exact(&mut body)
        .await
        .map_err(|error| SpikeError::Stream(error.to_string()))?;
    serde_json::from_slice(&body)
        .map_err(|error| SpikeError::Protocol(format!("invalid frame JSON: {error}")))
}

async fn send_pings(
    send: &mut SendStream,
    run_started: Instant,
    deadline: Instant,
) -> Result<u64, SpikeError> {
    let mut ticker = time::interval(PING_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut sent = 0_u64;

    loop {
        tokio::select! {
            _ = time::sleep_until(deadline.into()) => break,
            _ = ticker.tick() => {
                let message = WireMessage::Ping {
                    seq: sent,
                    monotonic_ns: elapsed_ns(run_started),
                };
                write_frame(send, &message).await?;
                sent = sent.checked_add(1).ok_or_else(|| {
                    SpikeError::Protocol("ping sequence overflowed".to_owned())
                })?;
            }
        }
    }

    write_frame(send, &WireMessage::Done).await?;
    send.finish()
        .map_err(|error| SpikeError::Stream(error.to_string()))?;
    Ok(sent)
}

async fn receive_pongs(mut recv: RecvStream) -> Result<u64, SpikeError> {
    let mut expected_seq = 0_u64;
    loop {
        match read_frame::<WireMessage>(&mut recv).await? {
            WireMessage::Pong { seq, .. } if seq == expected_seq => {
                expected_seq = expected_seq
                    .checked_add(1)
                    .ok_or_else(|| SpikeError::Protocol("pong sequence overflowed".to_owned()))?;
            }
            WireMessage::Pong { seq, .. } => {
                return Err(SpikeError::Protocol(format!(
                    "expected pong sequence {expected_seq}, received {seq}"
                )));
            }
            WireMessage::Done => return Ok(expected_seq),
            _ => {
                return Err(SpikeError::Protocol(
                    "dial side received an unexpected frame".to_owned(),
                ));
            }
        }
    }
}

async fn await_receiver(task: JoinHandle<Result<u64, SpikeError>>) -> Result<u64, SpikeError> {
    match time::timeout(STREAM_FINISH_GRACE, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(SpikeError::Stream(format!(
            "pong receiver task failed: {error}"
        ))),
        Err(_) => Err(SpikeError::Stream(
            "timed out waiting for final pong".to_owned(),
        )),
    }
}

async fn echo_pings(
    send: &mut SendStream,
    recv: &mut RecvStream,
    deadline: Instant,
    path_stop: oneshot::Sender<()>,
) -> Result<(u64, u64), SpikeError> {
    let finish_deadline = deadline
        .checked_add(STREAM_FINISH_GRACE)
        .ok_or_else(|| SpikeError::Protocol("observation window is too large".to_owned()))?;
    let mut expected_seq = 0_u64;
    let mut previous_timestamp = None;
    let mut path_stop = Some(path_stop);
    let exchange = async {
        loop {
            match read_frame::<WireMessage>(recv).await? {
                WireMessage::Ping { seq, monotonic_ns } if seq == expected_seq => {
                    if previous_timestamp.is_some_and(|previous| monotonic_ns < previous) {
                        return Err(SpikeError::Protocol(
                            "ping monotonic timestamp moved backwards".to_owned(),
                        ));
                    }
                    previous_timestamp = Some(monotonic_ns);
                    write_frame(send, &WireMessage::Pong { seq, monotonic_ns }).await?;
                    expected_seq = expected_seq.checked_add(1).ok_or_else(|| {
                        SpikeError::Protocol("ping sequence overflowed".to_owned())
                    })?;
                }
                WireMessage::Ping { seq, .. } => {
                    return Err(SpikeError::Protocol(format!(
                        "expected ping sequence {expected_seq}, received {seq}"
                    )));
                }
                WireMessage::Done => {
                    if let Some(stop) = path_stop.take() {
                        let _ = stop.send(());
                    }
                    write_frame(send, &WireMessage::Done).await?;
                    send.finish()
                        .map_err(|error| SpikeError::Stream(error.to_string()))?;
                    match time::timeout(STREAM_FINISH_GRACE, send.stopped()).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            return Err(SpikeError::Stream(error.to_string()));
                        }
                        Err(_) => {
                            return Err(SpikeError::Stream(
                                "timed out waiting for pong delivery acknowledgement".to_owned(),
                            ));
                        }
                    }
                    return Ok((expected_seq, expected_seq));
                }
                _ => {
                    return Err(SpikeError::Protocol(
                        "listener received an unexpected frame".to_owned(),
                    ));
                }
            }
        }
    };

    time::timeout_at(finish_deadline.into(), exchange)
        .await
        .map_err(|_| SpikeError::Stream("timed out waiting for probe completion".to_owned()))?
}

#[derive(Debug, Default)]
struct PathTracker {
    direct_path_opened_ms: Option<u64>,
    direct_path_selected_ms: Option<u64>,
    relay_url: Option<String>,
    rtt_relay_ms: Option<f64>,
    rtt_direct_ms: Option<f64>,
    ended_direct: bool,
    selected_path_at_end: Option<String>,
}

impl PathTracker {
    fn from_snapshot(connection: &Connection, run_started: Instant) -> Self {
        let mut tracker = Self::default();
        tracker.observe_snapshot(connection, run_started, false);
        tracker
    }

    fn observe_snapshot(
        &mut self,
        connection: &Connection,
        run_started: Instant,
        final_snapshot: bool,
    ) {
        if final_snapshot {
            self.ended_direct = false;
            self.selected_path_at_end = None;
        }
        let observed_ms = elapsed_ms(run_started);
        let paths = connection.paths();
        for path in paths.iter() {
            let rtt_ms = duration_ms(path.rtt());
            if path.is_ip() {
                keep_first(&mut self.direct_path_opened_ms, observed_ms);
                keep_lowest(&mut self.rtt_direct_ms, rtt_ms);
                if path.is_selected() {
                    keep_first(&mut self.direct_path_selected_ms, observed_ms);
                }
            } else if path.is_relay() {
                keep_lowest(&mut self.rtt_relay_ms, rtt_ms);
                remember_relay(&mut self.relay_url, path.remote_addr());
            }
            if final_snapshot && path.is_selected() {
                let kind = if path.is_ip() {
                    self.ended_direct = true;
                    "ip"
                } else if path.is_relay() {
                    "relay"
                } else {
                    "other"
                };
                self.selected_path_at_end = Some(kind.to_owned());
            }
        }
    }

    fn observe_event(&mut self, event: PathEvent, connection: &Connection, run_started: Instant) {
        let observed_ms = elapsed_ms(run_started);
        match event {
            PathEvent::Opened { remote_addr, .. } => {
                if remote_addr.is_ip() {
                    keep_first(&mut self.direct_path_opened_ms, observed_ms);
                } else if remote_addr.is_relay() {
                    remember_relay(&mut self.relay_url, &remote_addr);
                }
            }
            PathEvent::Selected { remote_addr, .. } => {
                if remote_addr.is_ip() {
                    keep_first(&mut self.direct_path_selected_ms, observed_ms);
                } else if remote_addr.is_relay() {
                    remember_relay(&mut self.relay_url, &remote_addr);
                }
            }
            PathEvent::Closed {
                remote_addr,
                last_stats,
                ..
            } => {
                let rtt_ms = duration_ms(last_stats.rtt);
                if remote_addr.is_ip() {
                    keep_lowest(&mut self.rtt_direct_ms, rtt_ms);
                } else if remote_addr.is_relay() {
                    keep_lowest(&mut self.rtt_relay_ms, rtt_ms);
                    remember_relay(&mut self.relay_url, &remote_addr);
                }
            }
            PathEvent::Lagged { missed, .. } => {
                warn!(
                    missed,
                    "path event receiver lagged; resynchronizing snapshot"
                );
                self.observe_snapshot(connection, run_started, false);
            }
            _ => {}
        }
    }
}

fn start_path_observer(
    connection: &Connection,
    run_started: Instant,
    deadline: Instant,
) -> JoinHandle<PathTracker> {
    let events = connection.path_events();
    let initial = PathTracker::from_snapshot(connection, run_started);
    start_path_observer_with_state(connection, run_started, deadline, events, initial)
}

fn start_path_observer_with_state(
    connection: &Connection,
    run_started: Instant,
    deadline: Instant,
    mut events: PathEventStream,
    mut tracker: PathTracker,
) -> JoinHandle<PathTracker> {
    let connection = connection.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = time::sleep_until(deadline.into()) => break,
                event = events.next() => {
                    match event {
                        Some(event) => tracker.observe_event(event, &connection, run_started),
                        None => break,
                    }
                }
            }
        }
        tracker.observe_snapshot(&connection, run_started, true);
        tracker
    })
}

fn start_path_observer_until_signal(
    connection: &Connection,
    run_started: Instant,
    mut events: PathEventStream,
    initial: PathTracker,
    mut stop: oneshot::Receiver<()>,
) -> JoinHandle<PathTracker> {
    let connection = connection.clone();
    tokio::spawn(async move {
        let mut tracker = initial;
        loop {
            tokio::select! {
                biased;
                _ = &mut stop => break,
                event = events.next() => {
                    match event {
                        Some(event) => tracker.observe_event(event, &connection, run_started),
                        None => break,
                    }
                }
            }
        }
        tracker.observe_snapshot(&connection, run_started, true);
        tracker
    })
}

fn apply_path_summary(endpoint: &Endpoint, record: &mut ProbeRecord, summary: PathTracker) {
    record.direct_path_opened_ms = summary.direct_path_opened_ms;
    record.direct_path_selected_ms = summary.direct_path_selected_ms;
    record.ended_direct = summary.ended_direct;
    record.selected_path_at_end = summary.selected_path_at_end;
    if summary.relay_url.is_some() {
        record.relay_url = summary.relay_url;
    }
    record.rtt_relay_ms = summary.rtt_relay_ms;
    record.rtt_direct_ms = summary.rtt_direct_ms;
    record.local_direct_addrs = endpoint
        .addr()
        .ip_addrs()
        .map(|addr| addr.to_string())
        .collect();
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn keep_first(slot: &mut Option<u64>, value: u64) {
    if slot.is_none() {
        *slot = Some(value);
    }
}

fn keep_lowest(slot: &mut Option<f64>, value: f64) {
    match slot {
        Some(current) if *current <= value => {}
        _ => *slot = Some(value),
    }
}

fn remember_relay(slot: &mut Option<String>, addr: &TransportAddr) {
    if slot.is_none() {
        if let TransportAddr::Relay(url) = addr {
            *slot = Some(url.to_string());
        }
    }
}
