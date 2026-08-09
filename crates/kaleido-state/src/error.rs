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

    #[error("only command submission may record local command acceptance")]
    UntrustedLocalAcknowledgement,

    #[error("runtime acknowledgement has no earlier local acceptance for the same command")]
    UncorrelatedRuntimeAcknowledgement,

    #[error("runtime acknowledgement has no matching remote-command turn")]
    RuntimeAcknowledgementWithoutRemoteTurn,

    #[error("runtime acknowledgement matches more than one remote-command turn")]
    AmbiguousRuntimeAcknowledgement,

    #[error("runtime acknowledgement runtime does not match the command turn's session runtime")]
    RuntimeAcknowledgementRuntimeMismatch,

    #[error("runtime acknowledgement repeats an earlier runtime acceptance for the same command")]
    DuplicateRuntimeAcknowledgement,

    #[error("live-control capability has no prior legal runtime acceptance for this runtime")]
    LiveControlCapabilityWithoutRuntimeAcceptance,

    #[error("controlling binding has no prior runtime acceptance for this session and runtime")]
    ControllingBindingWithoutRuntimeAcceptance,

    #[error("a remote command identifier is already bound to another turn")]
    RemoteCommandTurnConflict,

    #[error("an existing turn cannot move to another session")]
    TurnSessionChanged,

    #[error("an existing turn cannot change its origin")]
    TurnOriginChanged,

    #[error("an existing turn cannot change its provider binding identity")]
    TurnBindingChanged,

    #[error("a live session candidate has no runtime reference in its binding or history")]
    LiveSessionWithoutRuntimeReference,

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
