//! Failures the canonical store can report.

use std::path::PathBuf;

use kaleido_proto::ids::{AttentionId, ContentId, ProviderRuntimeId, SessionId, TurnId};
use kaleido_proto::ContractViolation;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    /// A canonical invariant was violated. This is the contract refusing an
    /// effect, not an operational failure.
    #[error("contract violation: {0}")]
    Contract(#[from] ContractViolation),

    #[error("durable log input/output failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("a durable log record could not be encoded or decoded: {0}")]
    Codec(#[from] serde_json::Error),

    #[error("effect references unknown session {session_id}")]
    UnknownSession { session_id: SessionId },

    #[error("effect references unknown runtime {runtime_id}")]
    UnknownRuntime { runtime_id: ProviderRuntimeId },

    #[error("effect references unknown turn {turn_id}")]
    UnknownTurn { turn_id: TurnId },

    #[error("the store has no host record yet, so an effect cannot be routed to a stream")]
    UnknownHost,

    #[error("projection scope is ambiguous: {detail}")]
    AmbiguousScope { detail: &'static str },

    #[error("effect is outside this build's scope: {detail}")]
    UnsupportedEffect { detail: &'static str },

    #[error("effect references unknown attention entry {attention_id}")]
    UnknownAttention { attention_id: AttentionId },

    #[error("content {content_id} has no stored body")]
    ContentMissing { content_id: ContentId },

    #[error("content {content_id} does not match its recorded digest")]
    ContentDigestMismatch { content_id: ContentId },

    #[error("the durable log timestamp counter cannot advance further")]
    TimestampOverflow,

    #[error("durable log file {path} contains a malformed record on line {line}")]
    MalformedRecord { path: PathBuf, line: usize },
}

impl StateError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        StateError::Io {
            path: path.into(),
            source,
        }
    }
}
