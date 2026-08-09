//! Failures the canonical store can report.

use std::path::PathBuf;

use kaleido_proto::ids::{
    AttentionId, CommandId, ContentId, HostId, ProviderRuntimeId, SessionId, TurnId, WorkflowId,
};
use kaleido_proto::projection::ProjectionKey;
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

    #[error("an existing runtime cannot move to another host")]
    RuntimeHostChanged,

    #[error("effect references unknown turn {turn_id}")]
    UnknownTurn { turn_id: TurnId },

    #[error("the store has no host record yet, so an effect cannot be routed to a stream")]
    UnknownHost,

    #[error("the store has no host record for {host_id}")]
    UnknownHostId { host_id: HostId },

    #[error("the store has no workflow record for {workflow_id}")]
    UnknownWorkflow { workflow_id: WorkflowId },

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

    #[error("content {content_id} is not owned by the authenticated device")]
    ContentUnauthorized { content_id: ContentId },

    #[error("content {content_id} ownership has expired")]
    ContentExpired { content_id: ContentId },

    #[error("content {content_id} metadata does not match the authenticated upload")]
    ContentMetadataMismatch { content_id: ContentId },

    #[error("content identifier {content_id} is not a safe content-store path component")]
    UnsafeContentId { content_id: ContentId },

    #[error("content {content_id} read offset {offset} is outside the stored body")]
    InvalidContentOffset { content_id: ContentId, offset: u64 },

    #[error("the durable log timestamp counter cannot advance further")]
    TimestampOverflow,

    #[error("projection journal retention must contain at least one entry")]
    InvalidProjectionRetention,

    #[error("projection journal diverges from canonical history for {key:?}")]
    ProjectionJournalDiverged { key: ProjectionKey },

    #[error("device command {command_id} was already claimed or is not dispatchable")]
    DispatchNotAvailable { command_id: CommandId },

    #[error("device command {command_id} did not produce a local runtime-dispatch acceptance")]
    DispatchNotAccepted { command_id: CommandId },

    #[error("device command outbox has a conflicting durable record")]
    CommandOutboxDiverged,

    #[error("a durable append committed only partially; reload is required before more writes")]
    RecoveryRequired,

    #[error("device command envelope does not match the authenticated request: {detail}")]
    DeviceCommandMismatch { detail: &'static str },

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
