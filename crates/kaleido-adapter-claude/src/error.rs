//! Fail-closed errors for the Claude Agent SDK adapter.
//!
//! The SDK side of the bridge is deliberately kept in TypeScript.  Rust sees
//! only the versioned sidecar envelopes and turns malformed or unsupported
//! traffic into diagnostics instead of guessing at a neighbouring message
//! shape.

use kaleido_adapter::ContentAccessError;
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::ContractViolation;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClaudeAdapterError {
    #[error("sidecar frame {line} is not valid JSON")]
    MalformedTranscriptLine { line: usize },

    #[error("sidecar frame {line} is missing the `{field}` envelope field")]
    MalformedTranscriptEnvelope { line: usize, field: &'static str },

    #[error("sidecar frame is not valid JSON")]
    MalformedFrame,

    #[error("sidecar protocol version {found} is not supported (expected {expected})")]
    ProtocolVersion { found: u64, expected: u64 },

    #[error("sidecar frame has an unknown kind")]
    UnknownFrameKind,

    #[error("the SDK message is missing a session identifier")]
    MissingSessionId,

    #[error("the SDK message has an unsupported or unknown shape: {kind}")]
    UnknownSdkMessage { kind: String },

    #[error(
        "the SDK emitted an AskUserQuestion question set which this contract cannot represent"
    )]
    UnsupportedQuestionSet,

    #[error("the sidecar stopped before the session was established")]
    SidecarCrashed,

    #[error("the sidecar transport failed")]
    Transport,

    #[error("the runtime sent a protocol-invalid value: {detail}")]
    ProtocolViolation { detail: &'static str },

    #[error(transparent)]
    Content(#[from] ContentAccessError),

    #[error("a produced effect violates the canonical contract: {0}")]
    Contract(#[from] ContractViolation),
}

impl ClaudeAdapterError {
    /// Canonical errors never carry upstream text or provider identifiers.
    pub fn canonical_error(&self, at_ms: i64) -> CanonicalError {
        CanonicalError {
            code: ErrorCode::RuntimeProtocolViolation,
            retriable: false,
            detail_ref: None,
            at_ms,
        }
    }

    /// Whether a malformed sidecar envelope should end the connection.
    pub fn ends_connection(&self) -> bool {
        matches!(
            self,
            Self::ProtocolVersion { .. }
                | Self::MalformedFrame
                | Self::SidecarCrashed
                | Self::Transport
                | Self::ProtocolViolation { .. }
        )
    }
}
