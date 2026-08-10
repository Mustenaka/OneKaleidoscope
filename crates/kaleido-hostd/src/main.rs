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
//! kaleido-hostd lan run      --executable <codex.exe> --project-root <dir>
//!                            --data-dir <dir> --bind <lan-ip:port>
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use kaleido_adapter_claude::{
    ClaudeRuntimeConfig, ClaudeRuntimeSession, ReducerConfig as ClaudeReducerConfig,
};
use kaleido_adapter_codex::{
    CodexRuntimeConfig, CodexRuntimeSession, CodexSandboxMode, ReducerConfig as CodexReducerConfig,
};
use kaleido_adapter_opencode::{
    OpenCodeClientConfig, OpenCodeRuntimeConfig, OpenCodeRuntimeSession,
    ReducerConfig as OpenCodeReducerConfig,
};
use kaleido_hostd::error::HostdError;
use kaleido_hostd::slice::{self, ApprovalDecision, ReplayRequest, RunRequest};
use kaleido_hostd::{
    RuntimeBootstrap, RuntimeBootstrapFactory, StructuredLanConfig, StructuredLanHost,
};
use kaleido_proto::capability::EvidenceSource;
use kaleido_proto::host::LaunchSurface;
use kaleido_proto::ids::SessionId;
use kaleido_proto::turn::TurnOrigin;
use kaleido_state::ProjectionName;

const USAGE: &str = "\
kaleido-hostd slice replay --fixture <path.jsonl> --log-dir <dir> [--base-at-ms <int>]
kaleido-hostd slice run    --executable <codex.exe> --project-root <dir>
                           --log-dir <dir> --prompt <text>
                           [--decide-first-approval accept|decline]
                           [--enqueue-steer <text>] [--timeout-secs 120]
kaleido-hostd slice show   --log-dir <dir> --projection <name> [--session <id>]
kaleido-hostd lan run      --providers <codex,opencode,claude> --project-root <dir>
                           --data-dir <dir> --bind <lan-ip:port>
                           [--executable <codex.exe>] [--opencode-url <url>]
                           [--node-executable <node>] [--claude-bridge <index.ts>]
                           [--no-print-pairing]
                           [--serve-secs <positive>] [--timeout-secs 30]

projections: project-index, session-index, transcript, live-activity, input-queue,
             attention-inbox, runtime-capability, all";

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match run().await {
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

async fn run() -> Result<String, HostdError> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mut positional = arguments.iter();
    match (
        positional.next().map(String::as_str),
        positional.next().map(String::as_str),
    ) {
        (Some("slice"), Some("replay")) => run_replay(&arguments),
        (Some("slice"), Some("run")) => run_live(&arguments),
        (Some("slice"), Some("show")) => run_show(&arguments),
        (Some("lan"), Some("run")) => run_lan(&arguments).await,
        _ => Err(HostdError::usage(
            "expected `slice replay`, `slice run`, `slice show` or `lan run`",
        )),
    }
}

async fn run_lan(arguments: &[String]) -> Result<String, HostdError> {
    let bind_address = parse_bind(&required(arguments, "--bind")?)?;
    let serve_seconds = optional_positive_seconds(arguments, "--serve-secs")?;
    let timeout_seconds = positive_seconds(arguments, "--timeout-secs", 30)?;
    let providers = optional(arguments, "--providers").unwrap_or_else(|| "codex".to_owned());
    let provider_names = providers
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if provider_names.is_empty() {
        return Err(HostdError::usage("--providers must not be empty"));
    }
    let mut runtimes = Vec::<RuntimeBootstrapFactory>::new();
    for provider in provider_names {
        // #[allow(kaleido::agent_name_branch)] reason: this CLI composition root parses an explicit operator provider selection; product behavior remains capability-driven
        match provider {
            "codex" => {
                let executable = PathBuf::from(required(arguments, "--executable")?);
                runtimes.push(Box::new(move |context| {
                    let reducer = CodexReducerConfig {
                        host_display_name: "kaleido-host".to_owned(),
                        host_platform: context.host_platform,
                        project_display_name: "kaleido-project".to_owned(),
                        identity_salt: context.identity_salt,
                        evidence: EvidenceSource::ObservedInTraffic,
                        launch_surface: LaunchSurface::BrokerLaunched,
                        turn_origin: TurnOrigin::LocalSurface,
                        base_at_ms: now_ms(),
                        runtime_version_label: None,
                    };
                    let runtime = CodexRuntimeSession::new(CodexRuntimeConfig {
                        executable,
                        reducer,
                        sandbox: CodexSandboxMode::WorkspaceWrite,
                        request_timeout: Duration::from_secs(timeout_seconds),
                    });
                    Ok(bootstrap(runtime))
                }));
            }
            "opencode" => {
                let base_url = optional(arguments, "--opencode-url")
                    .unwrap_or_else(|| "http://127.0.0.1:4096".to_owned());
                runtimes.push(Box::new(move |context| {
                    let directory = context.project_root.to_string_lossy().into_owned();
                    let runtime = OpenCodeRuntimeSession::new(OpenCodeRuntimeConfig {
                        client: OpenCodeClientConfig {
                            base_url,
                            project_directory: Some(directory.clone()),
                            request_timeout: Duration::from_secs(timeout_seconds),
                        },
                        reducer: OpenCodeReducerConfig {
                            host_display_name: "kaleido-host".to_owned(),
                            host_platform: context.host_platform,
                            project_display_name: "kaleido-project".to_owned(),
                            project_directory: directory,
                            identity_salt: context.identity_salt,
                            evidence: EvidenceSource::ObservedInTraffic,
                            base_at_ms: now_ms(),
                            runtime_version_label: None,
                        },
                    })
                    .map_err(|_| kaleido_hostd::CodexLanError::Runtime)?;
                    Ok(bootstrap(runtime))
                }));
            }
            "claude" => {
                let node = PathBuf::from(required(arguments, "--node-executable")?);
                let bridge = PathBuf::from(required(arguments, "--claude-bridge")?);
                runtimes.push(Box::new(move |context| {
                    let runtime = ClaudeRuntimeSession::new(ClaudeRuntimeConfig {
                        node_executable: node,
                        bridge_script: bridge,
                        reducer: ClaudeReducerConfig {
                            host_display_name: "kaleido-host".to_owned(),
                            host_platform: context.host_platform,
                            project_display_name: "kaleido-project".to_owned(),
                            identity_salt: context.identity_salt,
                            evidence: EvidenceSource::ObservedInTraffic,
                            launch_surface: LaunchSurface::BrokerLaunched,
                            turn_origin: TurnOrigin::LocalSurface,
                            base_at_ms: now_ms(),
                            runtime_version_label: None,
                        },
                        request_timeout: Duration::from_secs(timeout_seconds),
                        resume_session: None,
                    });
                    Ok(bootstrap(runtime))
                }));
            }
            _ => {
                return Err(HostdError::usage(
                    "--providers accepts only codex, opencode and claude",
                ));
            }
        }
    }
    let config = StructuredLanConfig {
        project_root: PathBuf::from(required(arguments, "--project-root")?),
        data_directory: PathBuf::from(required(arguments, "--data-dir")?),
        bind_address,
        runtimes,
    };
    // Blocking provider clients (notably reqwest's OpenCode client) own a
    // private Tokio runtime and must be constructed outside this async task.
    // Keeping the whole bootstrap in `spawn_blocking` also makes rollback drop
    // adapters on the same valid blocking boundary.
    let host = tokio::task::spawn_blocking(move || StructuredLanHost::start(config))
        .await
        .map_err(|_| HostdError::Lan)?
        .map_err(|_| HostdError::Lan)?;
    // This is an operator-requested one-time credential. It deliberately
    // bypasses tracing and is never retained in the returned summary.
    if !arguments
        .iter()
        .any(|argument| argument == "--no-print-pairing")
    {
        println!("{}", host.pairing_uri());
    }
    let session_ids = host.session_ids().to_vec();
    if let Some(serve_seconds) = serve_seconds {
        tokio::task::block_in_place(|| host.run_for(Duration::from_secs(serve_seconds)));
    } else {
        let stop = tokio::signal::ctrl_c();
        tokio::pin!(stop);
        loop {
            tokio::select! {
                signal = &mut stop => {
                    signal.map_err(|_| HostdError::Lan)?;
                    break;
                }
                () = tokio::time::sleep(Duration::from_millis(50)) => {
                    tokio::task::block_in_place(|| host.run_for(Duration::from_millis(1)));
                }
            }
        }
    }
    tokio::task::block_in_place(move || host.shutdown()).map_err(|_| HostdError::Lan)?;
    Ok(format!(
        "LAN sessions {} stopped cleanly",
        session_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    ))
}

fn bootstrap<T>(runtime: T) -> RuntimeBootstrap
where
    T: kaleido_adapter::ProviderRuntimeSession + Send + 'static,
    T: RuntimeIdentity,
{
    RuntimeBootstrap {
        project_id: runtime.project_id().clone(),
        project_binding_id: runtime.project_binding_id().clone(),
        runtime_id: runtime.runtime_id().clone(),
        runtime: Box::new(runtime),
    }
}

trait RuntimeIdentity {
    fn project_id(&self) -> &kaleido_proto::ids::ProjectId;
    fn project_binding_id(&self) -> &kaleido_proto::ids::ProjectBindingId;
    fn runtime_id(&self) -> &kaleido_proto::ids::ProviderRuntimeId;
}

macro_rules! runtime_identity {
    ($runtime:ty) => {
        impl RuntimeIdentity for $runtime {
            fn project_id(&self) -> &kaleido_proto::ids::ProjectId {
                self.project_id()
            }
            fn project_binding_id(&self) -> &kaleido_proto::ids::ProjectBindingId {
                self.project_binding_id()
            }
            fn runtime_id(&self) -> &kaleido_proto::ids::ProviderRuntimeId {
                self.runtime_id()
            }
        }
    };
}

runtime_identity!(CodexRuntimeSession);
runtime_identity!(OpenCodeRuntimeSession);
runtime_identity!(ClaudeRuntimeSession);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn parse_bind(value: &str) -> Result<std::net::SocketAddr, HostdError> {
    let address = value
        .parse::<std::net::SocketAddr>()
        .map_err(|_| HostdError::usage("--bind expects an explicit LAN IP and port"))?;
    if address.ip().is_unspecified() {
        return Err(HostdError::usage(
            "--bind must name a reachable LAN or explicit loopback IP",
        ));
    }
    Ok(address)
}

fn optional_positive_seconds(arguments: &[String], flag: &str) -> Result<Option<u64>, HostdError> {
    optional(arguments, flag)
        .map(|value| {
            let seconds = value
                .parse::<u64>()
                .map_err(|_| HostdError::usage(format!("{flag} expects a positive integer")))?;
            if seconds == 0 {
                return Err(HostdError::usage(format!(
                    "{flag} expects a positive integer"
                )));
            }
            Ok(seconds)
        })
        .transpose()
}

fn positive_seconds(arguments: &[String], flag: &str, default: u64) -> Result<u64, HostdError> {
    let value = optional(arguments, flag).unwrap_or_else(|| default.to_string());
    let seconds = value
        .parse::<u64>()
        .map_err(|_| HostdError::usage(format!("{flag} expects a positive integer")))?;
    if seconds == 0 {
        return Err(HostdError::usage(format!(
            "{flag} expects a positive integer"
        )));
    }
    Ok(seconds)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{optional_positive_seconds, parse_bind};

    #[test]
    fn lan_bind_rejects_unspecified_addresses_but_allows_explicit_diagnostics() {
        assert!(parse_bind("0.0.0.0:1234").is_err());
        assert!(parse_bind("[::]:1234").is_err());
        assert_eq!(
            parse_bind("127.0.0.1:0").expect("explicit loopback"),
            "127.0.0.1:0".parse().expect("socket address")
        );
    }

    #[test]
    fn missing_serve_seconds_selects_the_persistent_signal_lifecycle() {
        let arguments = Vec::<String>::new();
        assert_eq!(
            optional_positive_seconds(&arguments, "--serve-secs").expect("optional duration"),
            None
        );
        assert!(optional_positive_seconds(
            &["--serve-secs".to_owned(), "0".to_owned()],
            "--serve-secs"
        )
        .is_err());
    }
}
