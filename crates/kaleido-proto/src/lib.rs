//! UACP contract types.
//!
//! This crate is the machine-readable form of `docs/PROTOCOL.md`. Every type
//! here has a definition in that document, and every version 0.5 type in that
//! document exists here. Changing this crate requires changing the document and
//! recording an ADR first (`AGENTS.md` section 2.1).
//!
//! # What the contract enforces
//!
//! The rules that previous iterations of this project kept losing are encoded as
//! validators rather than prose, so an implementation cannot quietly bypass
//! them:
//!
//! - [`turn::ItemStatus::Declined`] is a normal terminal state. It is not a
//!   failure and it does not fail the enclosing turn ([`turn::Turn::validate`]).
//! - A queued input only becomes a delivered steer with runtime-observed proof
//!   ([`queue::QueueEntry::validate`]).
//! - History provenance never implies a live attachment
//!   ([`session::LiveBinding::validate_against`]).
//! - Local command acceptance is not runtime acceptance
//!   ([`command::CommandOutcome::reached_runtime`]).
//! - Sensitive bodies never travel inline or as previews
//!   ([`content::ContentRef::validate`]).
//! - A capability absent from negotiation resolves to `NotVerified`, never to
//!   supported or unsupported ([`capability::RuntimeCapabilities::state_of`]).
//!
//! # Shape restrictions
//!
//! Rule R-P1 limits this crate to constructs a foreign-function binding
//! generator can express: records with named fields, enums whose variants are
//! either fieldless or have named fields, and the scalar, `Vec` and `Option`
//! types built from them. There are deliberately no generics, no tuple structs,
//! no tuples, no trait objects, no untyped JSON values and no map types. All
//! instants are `i64` Unix epoch milliseconds.

pub mod attention;
pub mod capability;
pub mod command;
pub mod content;
pub mod effect;
pub mod error;
pub mod host;
pub mod ids;
pub mod projection;
pub mod queue;
pub mod session;
pub mod turn;
pub mod workflow;

use thiserror::Error;

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

/// The protocol version this crate implements.
///
/// Peers must refuse a different major version rather than guess
/// (`docs/PROTOCOL.md` section 1).
pub const PROTOCOL_VERSION: &str = "0.5.0";

/// Whether a peer's protocol version is compatible with this build.
///
/// Pre-1.0 minor versions are compatibility boundaries. This build therefore
/// accepts `0.5.x`, but rejects every other `0.x` line. Once the protocol
/// reaches 1.0, normal same-major compatibility applies.
pub fn version_is_compatible(peer_version: &str) -> bool {
    fn parse(version: &str) -> Option<(u64, u64, u64)> {
        let mut parts = version.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some((major, minor, patch))
    }

    // #[allow(kaleido::version_branch)] reason: protocol handshake compatibility is not a runtime capability decision
    match (parse(PROTOCOL_VERSION), parse(peer_version)) {
        (Some((0, our_minor, _)), Some((0, peer_minor, _))) => our_minor == peer_minor,
        (Some((our_major, _, _)), Some((peer_major, _, _))) => our_major == peer_major,
        _ => false,
    }
}

/// A canonical invariant was violated.
///
/// This is a programming-error signal for reducers and stores, distinct from
/// [`error::CanonicalError`], which is the wire-visible error shown to a user.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContractViolation {
    #[error("identifier `{field}` must not be empty")]
    EmptyIdentifier { field: &'static str },

    #[error("provider binding identifier is not broker-shaped")]
    InvalidProviderBindingId,

    #[error("provider binding kind mismatch: expected {expected:?}, found {actual:?}")]
    ProviderBindingKindMismatch {
        expected: ids::ProviderBindingKind,
        actual: ids::ProviderBindingKind,
    },

    #[error("digest `{digest}` is not a canonical SHA-256 digest")]
    MalformedDigest { digest: String },

    #[error("idempotency key of {byte_len} bytes exceeds the contract limit")]
    IdempotencyKeyTooLong { byte_len: usize },

    #[error("device command TTL {ttl_ms}ms is outside the allowed range")]
    InvalidDeviceCommandTtl { ttl_ms: u64 },

    #[error("a projection preview contains unsafe text")]
    UnsafePreview,

    #[error("sensitive content must not carry a preview")]
    SensitivePreview,

    #[error("preview of {byte_len} bytes exceeds the projection limit")]
    PreviewTooLong { byte_len: usize },

    #[error("inline content of {byte_len} bytes exceeds the inline limit")]
    InlineTooLarge { byte_len: u64 },

    #[error("sensitive content must not be stored inline")]
    SensitiveInline,

    #[error("content in `{field}` must be marked sensitive")]
    SensitiveContentRequired { field: &'static str },

    #[error("content read size {max_bytes} is outside the allowed range")]
    InvalidContentReadSize { max_bytes: u32 },

    #[error("content read chunk of {byte_len} bytes exceeds the allowed range")]
    ContentReadChunkTooLarge { byte_len: usize },

    #[error("content read offset overflow")]
    ContentReadOffsetOverflow,

    #[error("content read continuation offset mismatch: expected {expected}, found {found:?}")]
    ContentReadOffsetMismatch { expected: u64, found: Option<u64> },

    #[error("a terminal content read chunk must not carry a continuation offset")]
    ContentReadEofHasNext,

    #[error("content kind {content_kind:?} cannot be written by a mobile device")]
    UnsupportedContentWriteKind { content_kind: content::ContentKind },

    #[error("content write size {byte_len} is outside the allowed range")]
    InvalidContentWriteSize { byte_len: u64 },

    #[error("stored content write response does not match its request metadata")]
    ContentWriteResponseMismatch,

    #[error("content write response has invalid availability {availability:?}")]
    InvalidContentWriteAvailability {
        availability: content::ContentAvailability,
    },

    #[error("a turn carries an error while its status is {status:?}")]
    TurnErrorWithoutFailure { status: turn::TurnStatus },

    #[error("a failed turn must carry an error")]
    FailedTurnWithoutError,

    #[error("a terminal turn must carry a completion timestamp")]
    TerminalTurnWithoutTimestamp,

    #[error("a turn references the same item more than once")]
    DuplicateItemReference,

    #[error("only a pending queue entry may be editable")]
    EditableNonPendingQueueEntry,

    #[error("a delivered steer requires observed runtime proof, got {evidence_source:?}")]
    UnprovenSteerDelivery {
        evidence_source: capability::EvidenceSource,
    },

    #[error("a delivered steer acknowledgement names another session")]
    SteerSessionMismatch,

    #[error("a delivered steer acknowledgement names another turn")]
    SteerTurnMismatch,

    #[error("a delivered steer acknowledgement names another runtime")]
    SteerRuntimeMismatch,

    #[error("a delivered steer acknowledgement names another binding handle")]
    SteerBindingMismatch,

    #[error("a delivered steer does not target the current active turn")]
    SteerNotActiveTurn,

    #[error("the runtime has not proved steering support")]
    SteerCapabilityUnsupported,

    #[error("queue reorder contains an identifier more than once")]
    QueueReorderDuplicate,

    #[error("queue reorder omits a pending entry")]
    QueueReorderMissingPending,

    #[error("queue reorder includes an entry from another session")]
    QueueReorderCrossSession,

    #[error("queue reorder includes a non-pending entry")]
    QueueReorderNonPending,

    #[error("queue reorder includes an unknown entry")]
    QueueReorderUnknownEntry,

    #[error("an interactive request must offer at least one decision option")]
    DecisionOptionsMissing,

    #[error("an interactive request repeats decision option `{option_id}`")]
    DuplicateDecisionOption { option_id: String },

    #[error("a question request must contain at least one question")]
    QuestionSetEmpty,

    #[error("a question request repeats question key `{question_key}`")]
    DuplicateQuestionKey { question_key: String },

    #[error("a question response must contain one answer for every question")]
    QuestionAnswersRequired,

    #[error("question answers are not valid for this attention subject")]
    QuestionAnswersUnexpected,

    #[error("question answers cannot use top-level option or free-form fields")]
    QuestionTopLevelDecision,

    #[error("a question answer contains no option or free-form content")]
    QuestionAnswerEmpty,

    #[error("a question response repeats an answer key")]
    QuestionAnswerDuplicateKey,

    #[error("a question answer repeats an option id")]
    QuestionAnswerDuplicateOption,

    #[error("a question answer names an unknown question key")]
    QuestionAnswerUnknownKey,

    #[error("a question answer names an unknown option")]
    QuestionAnswerUnknownOption,

    #[error("a single-select question has more than one option")]
    QuestionAnswerTooManyOptions,

    #[error("a question response omits an answer")]
    QuestionAnswerMissing,

    #[error("free-form content is not allowed for this question")]
    FreeFormNotAllowed,

    #[error("question free-form content is invalid")]
    InvalidFreeForm,

    #[error("a response names an unknown option")]
    UnknownOption,

    #[error("an attention response contains neither an option nor free-form content")]
    AttentionDecisionMissing,

    #[error("an approval or question must be scoped to a session")]
    AttentionSessionRequired,

    #[error("a workflow gate must be scoped to a workflow")]
    AttentionWorkflowRequired,

    #[error("approval join target belongs to another session or turn")]
    ApprovalJoinTargetMismatch,

    #[error("externally observed attention answer names another observer host")]
    AttentionAnswerObserverHostMismatch,

    #[error("a live binding requires the `{missing}` capability")]
    LiveBindingUnsupported { missing: &'static str },

    #[error("live binding runtime does not match negotiated capabilities")]
    LiveBindingRuntimeMismatch,

    #[error("live binding lacks observed-traffic evidence")]
    LiveBindingEvidenceNotObserved,

    #[error("capability appears more than once: {capability:?}")]
    DuplicateCapability { capability: capability::Capability },

    #[error("project binding appears more than once: {binding_id}")]
    DuplicateProjectBinding { binding_id: ids::ProjectBindingId },

    #[error("project binding belongs to another project: {binding_id}")]
    ProjectBindingMismatch { binding_id: ids::ProjectBindingId },

    #[error("cursor gap: expected {expected}, found {found}")]
    CursorGap { expected: u64, found: u64 },

    #[error("cursor {cursor} was applied more than once")]
    CursorRepeated { cursor: u64 },

    #[error("cursor cannot advance beyond u64::MAX")]
    CursorOverflow,

    #[error("records from more than one stream were verified together")]
    MixedStreams,

    #[error("projections from more than one key were verified together")]
    MixedProjectionKeys,

    #[error("projection payload does not match its key")]
    ProjectionKeyPayloadMismatch,

    #[error("projection subscription acknowledgement names another key")]
    ProjectionSubscribeKeyMismatch,

    #[error("a resumed projection subscription requires a prior cursor")]
    ProjectionResumeWithoutCursor,

    #[error("projection resume cursor mismatch: expected {expected}, found {found}")]
    ProjectionResumeCursorMismatch { expected: u64, found: u64 },

    #[error(
        "projection current cursor must advance the requested cursor: since {since}, current {current}"
    )]
    ProjectionCurrentCursorNotAhead { since: u64, current: u64 },

    #[error("projection retention window is inverted: floor {floor}, head {head}")]
    InvalidProjectionCursorWindow { floor: u64, head: u64 },

    #[error("snapshot payload does not match its stream")]
    SnapshotStreamMismatch,

    #[error("reference in `{field}` does not resolve inside the snapshot")]
    DanglingReference { field: &'static str },

    #[error("workflow step transition is not allowed")]
    WorkflowTransitionNotAllowed,

    #[error("workflow attempt counter overflow")]
    WorkflowAttemptOverflow,
}
