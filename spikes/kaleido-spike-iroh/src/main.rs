mod error;
mod probe;
mod record;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use error::SpikeError;
use probe::{ProbeFailure, ProbeResult};
use record::ProbeRecord;
use tokio::sync::Mutex;
use tokio::task::{JoinError, JoinSet};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

pub use error::SpikeError as PublicSpikeError;
pub use probe::{
    bind_dialer, bind_listener, decode_ticket, dial_addr, dial_with_timeout, encode_ticket,
    loopback_addr, new_record, serve_connection, ALPN, CONNECT_TIMEOUT,
};
pub use record::{
    append_record, read_records, read_records_from, summarize, summarize_file, RECORD_SCHEMA,
};

const EXIT_DIRECT: u8 = 0;
const EXIT_RELAY: u8 = 10;
const EXIT_ERROR: u8 = 20;
const DEFAULT_RESULTS_PATH: &str = "results.jsonl";

#[derive(Debug, Parser)]
#[command(name = "spike-iroh", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Listen {
        #[arg(long, default_value = DEFAULT_RESULTS_PATH)]
        out: PathBuf,
        #[arg(long)]
        label: Option<String>,
    },
    Dial {
        ticket: String,
        #[arg(long)]
        label: String,
        #[arg(long, default_value_t = probe::DEFAULT_WINDOW_SECS)]
        window_secs: u64,
        #[arg(long, default_value = DEFAULT_RESULTS_PATH)]
        out: PathBuf,
    },
    Summarize {
        results: PathBuf,
    },
}

#[derive(Debug)]
pub struct DialExecution {
    pub record: ProbeRecord,
    pub exit_code: u8,
    pub error: Option<SpikeError>,
}

#[tokio::main]
async fn main() -> ExitCode {
    initialize_tracing();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let is_display = matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            );
            if let Err(print_error) = error.print() {
                eprintln!("ERROR CliOutput: {print_error}");
            }
            return ExitCode::from(if is_display { EXIT_DIRECT } else { EXIT_ERROR });
        }
    };

    let code = match run(cli.command).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ERROR {error}");
            EXIT_ERROR
        }
    };
    ExitCode::from(code)
}

fn initialize_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("spike_iroh=info"));
    if let Err(error) = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .try_init()
    {
        eprintln!("ERROR TracingInit: {error}");
    }
}

async fn run(command: Command) -> Result<u8, SpikeError> {
    match command {
        Command::Listen { out, label } => run_listener(out, label).await,
        Command::Dial {
            ticket,
            label,
            window_secs,
            out,
        } => {
            let execution = execute_dial(&ticket, &label, window_secs, &out).await?;
            print_dial_summary(&execution.record);
            if let Some(error) = execution.error {
                eprintln!("ERROR {error}");
            }
            Ok(execution.exit_code)
        }
        Command::Summarize { results } => {
            let mut warnings = io::stderr().lock();
            let output = summarize_file(results, &mut warnings)?;
            if !output.is_empty() {
                println!("{output}");
            }
            Ok(EXIT_DIRECT)
        }
    }
}

async fn run_listener(out: PathBuf, label: Option<String>) -> Result<u8, SpikeError> {
    let endpoint = bind_listener(true).await?;
    let ticket = encode_ticket(&endpoint.addr())?;
    println!("TICKET {ticket}");
    io::stdout().flush().map_err(SpikeError::RecordIo)?;

    let write_lock = Arc::new(Mutex::new(()));
    let mut handlers = JoinSet::new();
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal.map_err(|error| SpikeError::Stream(error.to_string()))?;
                break;
            }
            incoming = endpoint.accept() => {
                let incoming = incoming.ok_or_else(|| {
                    SpikeError::Accept("endpoint closed while listening".to_owned())
                })?;
                let endpoint = endpoint.clone();
                let out = out.clone();
                let label = label.clone();
                let write_lock = Arc::clone(&write_lock);
                handlers.spawn(async move {
                    handle_listener_connection(
                        &endpoint,
                        incoming,
                        label.as_deref(),
                        &out,
                        &write_lock,
                    )
                    .await
                });
            }
            completed = handlers.join_next(), if !handlers.is_empty() => {
                handle_joined(completed)?;
            }
        }
    }

    while let Some(completed) = handlers.join_next().await {
        completed.map_err(join_error)??;
    }
    endpoint.close().await;
    Ok(EXIT_DIRECT)
}

async fn handle_listener_connection(
    endpoint: &iroh::Endpoint,
    incoming: iroh::endpoint::Incoming,
    label: Option<&str>,
    out: &PathBuf,
    write_lock: &Mutex<()>,
) -> Result<(), SpikeError> {
    let (record, probe_error) =
        split_probe_result(serve_connection(endpoint, incoming, label).await);
    {
        let _guard = write_lock.lock().await;
        append_record(out, &record)?;
    }

    if let Some(error) = probe_error {
        warn!(
            error_variant = error.variant_name(),
            connect_ok = record.connect_ok,
            "listener probe failed; failure record persisted"
        );
    } else {
        info!(
            connect_ok = record.connect_ok,
            ended_direct = record.ended_direct,
            "listener probe record persisted"
        );
    }
    Ok(())
}

fn handle_joined(
    completed: Option<Result<Result<(), SpikeError>, JoinError>>,
) -> Result<(), SpikeError> {
    if let Some(result) = completed {
        result.map_err(join_error)??;
    }
    Ok(())
}

fn join_error(error: JoinError) -> SpikeError {
    SpikeError::Stream(format!("listener task failed: {error}"))
}

pub async fn execute_dial_with_timeout(
    ticket: &str,
    label: &str,
    window_secs: u64,
    out: &PathBuf,
    connect_timeout: Duration,
) -> Result<DialExecution, SpikeError> {
    persist_dial_result(
        out,
        probe::dial_with_timeout(ticket, label, window_secs, connect_timeout).await,
    )
}

pub async fn execute_dial(
    ticket: &str,
    label: &str,
    window_secs: u64,
    out: &PathBuf,
) -> Result<DialExecution, SpikeError> {
    persist_dial_result(out, probe::dial(ticket, label, window_secs).await)
}

fn persist_dial_result(
    out: &PathBuf,
    result: ProbeResult<ProbeRecord>,
) -> Result<DialExecution, SpikeError> {
    let (record, error) = split_probe_result(result);
    append_record(out, &record)?;

    let exit_code = if error.is_some() {
        EXIT_ERROR
    } else if record.connect_ok && record.ended_direct {
        EXIT_DIRECT
    } else if record.connect_ok && record.selected_path_at_end.as_deref() == Some("relay") {
        EXIT_RELAY
    } else {
        EXIT_ERROR
    };

    Ok(DialExecution {
        record,
        exit_code,
        error,
    })
}

fn split_probe_result(result: ProbeResult<ProbeRecord>) -> (ProbeRecord, Option<SpikeError>) {
    match result {
        Ok(record) => (record, None),
        Err(ProbeFailure { record, error }) => (record, Some(error)),
    }
}

fn print_dial_summary(record: &ProbeRecord) {
    let selected = record.selected_path_at_end.as_deref().unwrap_or("none");
    let opened = display_optional_ms(record.direct_path_opened_ms);
    let selected_ms = display_optional_ms(record.direct_path_selected_ms);
    println!(
        "RESULT role=dial label={} connect_ok={} ended_direct={} \
         selected_path_at_end={} pings_sent={} pongs_recv={} \
         direct_path_opened_ms={} direct_path_selected_ms={}",
        record.label,
        record.connect_ok,
        record.ended_direct,
        selected,
        record.pings_sent,
        record.pongs_recv,
        opened,
        selected_ms
    );
}

fn display_optional_ms(value: Option<u64>) -> String {
    value
        .map(|milliseconds| milliseconds.to_string())
        .unwrap_or_else(|| "null".to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn connected_relay_outcome_persists_and_maps_to_exit_10() {
        let directory = tempfile::tempdir().expect("temporary result directory must be created");
        let output = directory.path().join("relay.jsonl");
        let mut record = new_record("dial", "4g", probe::DEFAULT_WINDOW_SECS);
        record.connect_ok = true;
        record.selected_path_at_end = Some("relay".to_owned());

        let execution =
            persist_dial_result(&output, Ok(record)).expect("relay-only result must be persisted");
        assert_eq!(execution.exit_code, 10);
        assert!(!execution.record.ended_direct);

        let mut warnings = Vec::new();
        let persisted =
            read_records(&output, &mut warnings).expect("persisted result must be readable");
        assert_eq!(persisted, vec![execution.record]);
        assert!(warnings.is_empty());
    }
}
