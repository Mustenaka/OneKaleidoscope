use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

const BRIDGE_RELATIVE_PATH: &str = "crates/kaleido-adapter-claude/bridge";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostFamily {
    Windows,
    Unix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessStep {
    name: &'static str,
    executable: &'static str,
    arguments: &'static [&'static str],
}

#[derive(Debug)]
pub enum SidecarCheckError {
    MissingInput {
        path: PathBuf,
    },
    Spawn {
        step: &'static str,
        source: io::Error,
    },
    StepFailed {
        step: &'static str,
        status: ExitStatus,
    },
}

impl fmt::Display for SidecarCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingInput { path } => {
                write!(
                    formatter,
                    "Claude sidecar input is missing: {}",
                    path.display()
                )
            }
            Self::Spawn { step, .. } => {
                write!(formatter, "could not start Claude sidecar step `{step}`")
            }
            Self::StepFailed { step, status } => {
                write!(
                    formatter,
                    "Claude sidecar step `{step}` failed with status {status}"
                )
            }
        }
    }
}

impl Error for SidecarCheckError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::MissingInput { .. } | Self::StepFailed { .. } => None,
        }
    }
}

pub fn check_workspace(workspace: &Path) -> Result<(), SidecarCheckError> {
    let bridge = workspace.join(BRIDGE_RELATIVE_PATH);
    for required in ["package.json", "package-lock.json", "index.ts"] {
        let path = bridge.join(required);
        if !path.is_file() {
            return Err(SidecarCheckError::MissingInput { path });
        }
    }

    for step in process_plan(host_family()) {
        let status = Command::new(step.executable)
            .args(step.arguments)
            .current_dir(&bridge)
            .status()
            .map_err(|source| SidecarCheckError::Spawn {
                step: step.name,
                source,
            })?;
        if !status.success() {
            return Err(SidecarCheckError::StepFailed {
                step: step.name,
                status,
            });
        }
    }
    Ok(())
}

const fn host_family() -> HostFamily {
    if cfg!(windows) {
        HostFamily::Windows
    } else {
        HostFamily::Unix
    }
}

const fn process_plan(host: HostFamily) -> [ProcessStep; 2] {
    let executable = match host {
        HostFamily::Windows => "npm.cmd",
        HostFamily::Unix => "npm",
    };
    [
        ProcessStep {
            name: "claude-sidecar-npm-ci",
            executable,
            arguments: &["ci", "--ignore-scripts"],
        },
        ProcessStep {
            name: "claude-sidecar-typecheck",
            executable,
            arguments: &["run", "typecheck"],
        },
    ]
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{check_workspace, process_plan, HostFamily, SidecarCheckError};

    #[test]
    fn npm_plan_uses_the_platform_entrypoint_and_locked_install() {
        let windows = process_plan(HostFamily::Windows);
        let unix = process_plan(HostFamily::Unix);

        assert_eq!(windows[0].executable, "npm.cmd");
        assert_eq!(unix[0].executable, "npm");
        for plan in [windows, unix] {
            assert_eq!(plan[0].arguments, ["ci", "--ignore-scripts"]);
            assert_eq!(plan[1].arguments, ["run", "typecheck"]);
        }
    }

    #[test]
    fn missing_bridge_inputs_fail_before_spawning_npm() {
        let root = tempfile::tempdir().expect("temporary workspace is created");
        let error = check_workspace(root.path()).expect_err("missing package inputs must fail");

        assert!(matches!(error, SidecarCheckError::MissingInput { .. }));
    }
}
