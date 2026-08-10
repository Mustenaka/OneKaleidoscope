//! The provider-neutral runtime session.
//!
//! A concrete adapter owns one runtime conversation and turns its traffic into
//! canonical state transitions. Every method returns effects rather than
//! mutating anything: the store is the only component allowed to change
//! canonical state (`docs/PROTOCOL.md` section 5.1).

use kaleido_proto::attention::AttentionResponse;
use kaleido_proto::capability::CapabilityUnavailableReason;
use kaleido_proto::content::ContentRef;
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::ConnectionFaultReason;
use kaleido_proto::ids::{
    CommandId, ProjectBindingId, ProjectId, ProviderRuntimeId, SessionId, TurnId,
};
use kaleido_proto::queue::QueueEntry;
use kaleido_proto::ContractViolation;
use thiserror::Error;

use crate::capability::CapabilityProbe;
use crate::content::{ContentAccess, ContentAccessError};

/// What the broker needs in order to own a new runtime session.
#[derive(Debug, Clone)]
pub struct SessionStartRequest {
    pub project_id: ProjectId,
    pub project_binding_id: ProjectBindingId,
    pub runtime_id: ProviderRuntimeId,
    /// Full path of the working directory. Section 10 makes this sensitive, so
    /// it travels as a reference rather than a string.
    pub project_root_ref: ContentRef,
}

/// One runtime conversation the broker owns or is attached to.
pub trait ProviderRuntimeSession {
    /// Discovers provider history before a live session is started.
    ///
    /// The default is deliberately empty: it neither claims history support
    /// nor prevents providers without a public discovery surface from being
    /// started. Adapters that implement this method must keep provider ids
    /// private and return only canonical effects.
    fn discover(
        &mut self,
        _request: &SessionStartRequest,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        Ok(Vec::new())
    }

    /// Establishes the session and reports the effects that follow.
    fn start(
        &mut self,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError>;

    /// Sends a prompt whose body is already stored.
    fn submit_prompt(
        &mut self,
        command_id: &CommandId,
        body: &ContentRef,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError>;

    /// Answers an open approval or question on behalf of a real broker
    /// command. The command identifier lets an adapter distinguish this send
    /// from an answer merely observed on a shared runtime.
    fn respond_attention(
        &mut self,
        command_id: &CommandId,
        response: &AttentionResponse,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError>;

    /// Re-establishes the current provider-owned session after a transport
    /// loss. A provider without a public structured resume path stays
    /// unavailable instead of silently creating a replacement conversation.
    fn reconnect(
        &mut self,
        _request: &SessionStartRequest,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        Err(RuntimeSessionError::CapabilityUnavailable)
    }

    /// Attaches this runtime actor to a session previously returned by
    /// structured discovery. Provider-native ids remain adapter-private.
    fn resume_session(
        &mut self,
        _session_id: &SessionId,
        _request: &SessionStartRequest,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        Err(RuntimeSessionError::CapabilityUnavailable)
    }

    /// Projects connection-scoped capability loss before a reconnect attempt.
    /// Providers that publish connection-scoped support must override this;
    /// the empty default is only correct for runtimes that never proved it.
    fn connection_lost_effects(
        &mut self,
        _reason: CapabilityUnavailableReason,
        _at_ms: i64,
    ) -> Vec<StateEffect> {
        Vec::new()
    }

    /// Interrupts the exact active turn through a structured provider API.
    fn interrupt_turn(
        &mut self,
        _command_id: &CommandId,
        _turn_id: &TurnId,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        Err(RuntimeSessionError::CapabilityUnavailable)
    }

    /// Delivers one broker-owned queue entry. Implementations must return a
    /// terminal queue transition backed by a structured provider receipt.
    fn deliver_queue_entry(
        &mut self,
        _command_id: &CommandId,
        _entry: &QueueEntry,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        Err(RuntimeSessionError::CapabilityUnavailable)
    }

    /// Collects everything the runtime has emitted since the last call.
    ///
    /// This is the receiving end: an adapter never pushes into the store, the
    /// composition root pulls and applies.
    fn drain_effects(
        &mut self,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError>;

    /// Ends the session and reports the resulting transitions.
    fn close(&mut self) -> Result<Vec<StateEffect>, RuntimeSessionError>;

    /// What this connection has actually been observed to support.
    fn capability_probe(&self) -> CapabilityProbe;
}

/// Connection lifecycle failures, kept separate from canonical errors.
///
/// A refused approval is not in here, and cannot be: rule R-P8 makes a decline
/// a normal terminal item state rather than a fault.
#[derive(Debug, Error)]
pub enum RuntimeSessionError {
    #[error("the runtime is not connected")]
    NotConnected,

    #[error("the session has already been started")]
    AlreadyStarted,

    #[error("the runtime connection failed: {reason:?}")]
    ConnectionFault { reason: ConnectionFaultReason },

    #[error("the runtime sent a message that violates its own protocol: {detail}")]
    ProtocolViolation { detail: String },

    #[error("the runtime does not support this operation on this connection")]
    CapabilityUnavailable,

    #[error(transparent)]
    Content(#[from] ContentAccessError),

    #[error("a produced effect violates the canonical contract: {0}")]
    Contract(#[from] ContractViolation),
}

impl RuntimeSessionError {
    /// Whether the caller should mark the runtime connection unusable.
    pub fn ends_the_connection(&self) -> bool {
        matches!(
            self,
            RuntimeSessionError::NotConnected
                | RuntimeSessionError::ConnectionFault { .. }
                | RuntimeSessionError::ProtocolViolation { .. }
        )
    }
}
