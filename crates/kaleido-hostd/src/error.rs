//! Failures the host daemon reports to its operator.

use std::path::PathBuf;

use kaleido_adapter::session::RuntimeSessionError;
use kaleido_adapter_codex::CodexAdapterError;
use kaleido_state::StateError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostdError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Adapter(#[from] CodexAdapterError),

    #[error(transparent)]
    Runtime(#[from] RuntimeSessionError),

    #[error(transparent)]
    State(#[from] StateError),

    #[error("could not render output as JSON: {0}")]
    Render(#[from] serde_json::Error),

    #[error("usage: {detail}")]
    Usage { detail: String },

    #[error("the replay produced no session, so there is nothing to observe")]
    NoSession,

    #[error("the live turn did not finish before its configured deadline")]
    LiveTimeout,

    #[error("the runtime exposed no matching option for the requested approval decision")]
    ApprovalOptionUnavailable,

    #[error("the canonical store rejected a locally issued command")]
    CommandRejected,

    #[error("the configured project root could not be resolved")]
    ProjectRootUnavailable,

    #[error("live state persistence failed")]
    LiveStatePersistence,
}

impl HostdError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        HostdError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn usage(detail: impl Into<String>) -> Self {
        HostdError::Usage {
            detail: detail.into(),
        }
    }

    /// Removes filesystem paths from failures that can reach `slice run`
    /// operator output. Replay keeps its existing diagnostic behaviour.
    pub fn redact_live_path(self) -> Self {
        match self {
            HostdError::Io { .. }
            | HostdError::State(StateError::Io { .. } | StateError::MalformedRecord { .. }) => {
                HostdError::LiveStatePersistence
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostdError;

    #[test]
    fn live_path_errors_are_redacted_before_display() {
        let error = HostdError::State(kaleido_state::StateError::io(
            r"C:\Users\secret\live-log",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        ))
        .redact_live_path();
        let rendered = error.to_string();
        assert_eq!(rendered, "live state persistence failed");
        assert!(!rendered.contains("secret"));
    }
}
