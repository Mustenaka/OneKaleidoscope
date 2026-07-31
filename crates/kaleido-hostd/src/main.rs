//! The diagnostic client.
//!
//! Diagnostic commands for replaying recordings, running a real process and
//! reading the resulting projections:
//!
//! ```text
//! kaleido-hostd slice replay --fixture <path.jsonl> --log-dir <dir>
//! kaleido-hostd slice run    --executable <codex.exe> --project-root <dir>
//!                            --log-dir <dir> --prompt <text>
//! kaleido-hostd slice show   --log-dir <dir> --projection <name> [--session <id>]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use kaleido_hostd::error::HostdError;
use kaleido_hostd::slice::{self, ApprovalDecision, ReplayRequest, RunRequest};
use kaleido_proto::ids::SessionId;
use kaleido_state::ProjectionName;

const USAGE: &str = "\
kaleido-hostd slice replay --fixture <path.jsonl> --log-dir <dir> [--base-at-ms <int>]
kaleido-hostd slice run    --executable <codex.exe> --project-root <dir>
                           --log-dir <dir> --prompt <text>
                           [--decide-first-approval accept|decline]
                           [--enqueue-steer <text>] [--timeout-secs 120]
kaleido-hostd slice show   --log-dir <dir> --projection <name> [--session <id>]

projections: session-index, transcript, live-activity, input-queue,
             attention-inbox, runtime-capability, all";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match run() {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("kaleido-hostd: {error}");
            if matches!(error, HostdError::Usage { .. }) {
                eprintln!("{USAGE}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, HostdError> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mut positional = arguments.iter();
    match (
        positional.next().map(String::as_str),
        positional.next().map(String::as_str),
    ) {
        (Some("slice"), Some("replay")) => run_replay(&arguments),
        (Some("slice"), Some("run")) => run_live(&arguments),
        (Some("slice"), Some("show")) => run_show(&arguments),
        _ => Err(HostdError::usage(
            "expected `slice replay`, `slice run` or `slice show`",
        )),
    }
}

fn run_replay(arguments: &[String]) -> Result<String, HostdError> {
    let fixture = required(arguments, "--fixture")?;
    let log_dir = required(arguments, "--log-dir")?;
    let mut request = ReplayRequest::new(PathBuf::from(fixture), PathBuf::from(log_dir));
    if let Some(base) = optional(arguments, "--base-at-ms") {
        request.base_at_ms = base
            .parse()
            .map_err(|_| HostdError::usage("--base-at-ms expects an integer"))?;
    }
    let outcome = slice::replay(&request)?;
    Ok(format!(
        "replayed {} frame(s) into {} effect(s) and {} log record(s); session {}",
        outcome.frames, outcome.effects, outcome.records, outcome.session_id
    ))
}

fn run_live(arguments: &[String]) -> Result<String, HostdError> {
    let mut request = RunRequest::new(
        PathBuf::from(required(arguments, "--executable")?),
        PathBuf::from(required(arguments, "--project-root")?),
        PathBuf::from(required(arguments, "--log-dir")?),
        required(arguments, "--prompt")?,
    );
    request.decide_first_approval = optional(arguments, "--decide-first-approval")
        .map(|decision| match decision.as_str() {
            "accept" => Ok(ApprovalDecision::Accept),
            "decline" => Ok(ApprovalDecision::Decline),
            _ => Err(HostdError::usage(
                "--decide-first-approval expects `accept` or `decline`",
            )),
        })
        .transpose()?;
    request.enqueue_steer = optional(arguments, "--enqueue-steer");
    if let Some(timeout) = optional(arguments, "--timeout-secs") {
        let seconds = timeout
            .parse::<u64>()
            .map_err(|_| HostdError::usage("--timeout-secs expects a positive integer"))?;
        if seconds == 0 {
            return Err(HostdError::usage(
                "--timeout-secs expects a positive integer",
            ));
        }
        request.timeout = Duration::from_secs(seconds);
    }
    Ok(slice::run(&request)?.report_json)
}

fn run_show(arguments: &[String]) -> Result<String, HostdError> {
    let log_dir = PathBuf::from(required(arguments, "--log-dir")?);
    let projection = required(arguments, "--projection")?;
    let session = optional(arguments, "--session").map(SessionId::new);
    if projection == "all" {
        return slice::show_all(&log_dir, session.as_ref());
    }
    let name = ProjectionName::parse(&projection)
        .ok_or_else(|| HostdError::usage(format!("unknown projection `{projection}`")))?;
    slice::show(&log_dir, name, session.as_ref())
}

fn optional(arguments: &[String], flag: &str) -> Option<String> {
    arguments
        .iter()
        .position(|argument| argument == flag)
        .and_then(|index| index.checked_add(1))
        .and_then(|index| arguments.get(index))
        .cloned()
}

fn required(arguments: &[String], flag: &str) -> Result<String, HostdError> {
    optional(arguments, flag).ok_or_else(|| HostdError::usage(format!("missing {flag}")))
}
