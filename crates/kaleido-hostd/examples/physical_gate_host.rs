//! Local-only operator harness for the external Android physical-device gate.
//!
//! Pairing URIs are written directly to stdout because they contain one-time
//! credentials. They must never be redirected into ordinary logs or tracing.

use std::env;
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use kaleido_adapter_codex::CodexSandboxMode;
use kaleido_hostd::{CodexLanConfig, CodexLanHost};
use kaleido_proto::ids::DeviceId;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

fn main() -> Result<(), Box<dyn Error>> {
    let config = parse_config(env::args().skip(1))?;
    let host = CodexLanHost::start(&config)?;
    write_secret_line(host.pairing_uri())?;

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let command = match line {
                Ok(line) => parse_operator_command(&line),
                Err(_) => OperatorCommand::Stop,
            };
            if sender.send(command).is_err() {
                return;
            }
        }
        let _ = sender.send(OperatorCommand::Stop);
    });

    let mut stopping = false;
    while !stopping {
        host.run_for(POLL_INTERVAL);
        loop {
            match receiver.try_recv() {
                Ok(OperatorCommand::Pair) => match host.issue_pairing_uri() {
                    Ok(uri) => write_secret_line(&uri)?,
                    Err(_) => write_status("ERROR pair")?,
                },
                Ok(OperatorCommand::Revoke(device_id)) => {
                    if host.revoke_device(&device_id).is_ok() {
                        write_status("REVOKED")?;
                    } else {
                        write_status("ERROR revoke")?;
                    }
                }
                Ok(OperatorCommand::Stop) => {
                    stopping = true;
                    break;
                }
                Ok(OperatorCommand::Invalid) => write_status("ERROR invalid-command")?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stopping = true;
                    break;
                }
            }
        }
    }
    host.shutdown()?;
    Ok(())
}

fn write_secret_line(value: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{value}")?;
    stdout.flush()
}

fn write_status(value: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{value}")?;
    stdout.flush()
}

struct GateConfig {
    executable: PathBuf,
    project_root: PathBuf,
    data_directory: PathBuf,
    bind_address: SocketAddr,
    sandbox: CodexSandboxMode,
}

fn parse_config(arguments: impl Iterator<Item = String>) -> Result<CodexLanConfig, String> {
    let arguments = arguments.collect::<Vec<_>>();
    let gate = GateConfig {
        executable: PathBuf::from(required(&arguments, "--executable")?),
        project_root: PathBuf::from(required(&arguments, "--project-root")?),
        data_directory: PathBuf::from(required(&arguments, "--data-dir")?),
        bind_address: required(&arguments, "--bind")?
            .parse::<SocketAddr>()
            .map_err(|_| "--bind expects an explicit LAN IP and port".to_owned())?,
        sandbox: match required(&arguments, "--sandbox")?.as_str() {
            "workspace-write" => CodexSandboxMode::WorkspaceWrite,
            "read-only" => CodexSandboxMode::ReadOnly,
            _ => return Err("--sandbox expects workspace-write or read-only".to_owned()),
        },
    };
    if gate.bind_address.ip().is_unspecified() || gate.bind_address.ip().is_loopback() {
        return Err("--bind must name a reachable LAN IP".to_owned());
    }
    Ok(CodexLanConfig {
        executable: gate.executable,
        project_root: gate.project_root,
        data_directory: gate.data_directory,
        bind_address: gate.bind_address,
        sandbox: gate.sandbox,
        request_timeout: REQUEST_TIMEOUT,
    })
}

fn required(arguments: &[String], flag: &str) -> Result<String, String> {
    arguments
        .iter()
        .position(|argument| argument == flag)
        .and_then(|index| index.checked_add(1))
        .and_then(|index| arguments.get(index))
        .cloned()
        .ok_or_else(|| format!("missing {flag}"))
}

enum OperatorCommand {
    Pair,
    Revoke(DeviceId),
    Stop,
    Invalid,
}

fn parse_operator_command(line: &str) -> OperatorCommand {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["pair"] => OperatorCommand::Pair,
        ["revoke", device_id] if !device_id.is_empty() => {
            OperatorCommand::Revoke(DeviceId::new((*device_id).to_owned()))
        }
        ["stop"] => OperatorCommand::Stop,
        _ => OperatorCommand::Invalid,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{parse_config, parse_operator_command, CodexSandboxMode, OperatorCommand};

    #[test]
    fn operator_commands_are_closed_and_revoke_keeps_the_exact_device_id() {
        assert!(matches!(
            parse_operator_command("pair"),
            OperatorCommand::Pair
        ));
        assert!(matches!(
            parse_operator_command("stop"),
            OperatorCommand::Stop
        ));
        match parse_operator_command("revoke device-42") {
            OperatorCommand::Revoke(device_id) => assert_eq!(device_id.as_str(), "device-42"),
            _ => panic!("expected revoke"),
        }
        assert!(matches!(
            parse_operator_command("revoke"),
            OperatorCommand::Invalid
        ));
        assert!(matches!(
            parse_operator_command("pair extra"),
            OperatorCommand::Invalid
        ));
    }

    #[test]
    fn gate_config_rejects_unreachable_bind_addresses() {
        let common = [
            "--executable",
            "codex",
            "--project-root",
            ".",
            "--data-dir",
            "gate-data",
            "--bind",
        ];
        for address in ["0.0.0.0:0", "127.0.0.1:4567", "not-an-address"] {
            let args = common
                .into_iter()
                .chain([address, "--sandbox", "read-only"])
                .map(str::to_owned);
            assert!(parse_config(args).is_err());
        }
        let valid = common
            .into_iter()
            .chain(["192.168.1.2:4567", "--sandbox", "read-only"])
            .map(str::to_owned);
        let config = parse_config(valid).unwrap();
        assert_eq!(config.bind_address.to_string(), "192.168.1.2:4567");
        assert_eq!(config.sandbox, CodexSandboxMode::ReadOnly);
    }
}
