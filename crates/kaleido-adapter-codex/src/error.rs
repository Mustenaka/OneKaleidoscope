//! Failures this adapter reports.
//!
//! ADR-0012 D-3 draws the line: an *unregistered* message is a diagnostic and
//! is safely ignored, while a *registered* message whose pinned path no longer
//! resolves is a canonical protocol violation. Silently downgrading the second
//! case to success is the failure mode a generated client would not have
//! caught either, because shape drift happens at run time.

use kaleido_adapter::ContentAccessError;
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::ContractViolation;
use thiserror::Error;

use crate::surface::SurfacePurpose;

#[derive(Debug, Error)]
pub enum CodexAdapterError {
    #[error("transcript line {line} is not valid JSON")]
    MalformedTranscriptLine { line: usize },

    #[error("transcript line {line} is missing the `{field}` envelope field")]
    MalformedTranscriptEnvelope { line: usize, field: &'static str },

    #[error("an upstream frame is not valid JSON")]
    MalformedFrame,

    /// A registered path stopped resolving. This is the drift alarm.
    #[error("pinned pointer `{pointer}` for {purpose:?} did not resolve")]
    PointerUnresolved {
        purpose: SurfacePurpose,
        pointer: &'static str,
    },

    /// A registered path resolved to a value of the wrong shape.
    #[error("pinned pointer `{pointer}` for {purpose:?} resolved to the wrong value type")]
    PointerTypeMismatch {
        purpose: SurfacePurpose,
        pointer: &'static str,
    },

    /// A closed upstream enumeration produced a value this build does not
    /// model. Guessing a neighbouring value is forbidden.
    #[error("{purpose:?} carried a value this build does not model")]
    UnmodelledEnumeration { purpose: SurfacePurpose },

    #[error("upstream referenced a {scope} this adapter never bound")]
    UnknownBinding { scope: &'static str },

    #[error("a local attention answer did not match its registered command response")]
    LocalAttentionAnswerMismatch,

    #[error("the reducer evidence source cannot prove an external attention answer")]
    InvalidExternalAnswerEvidence,

    #[error(transparent)]
    Content(#[from] ContentAccessError),

    #[error("a produced effect violates the canonical contract: {0}")]
    Contract(#[from] ContractViolation),
}

impl CodexAdapterError {
    /// The canonical error a reader would be shown.
    ///
    /// Everything the adapter can fail on above maps to
    /// [`ErrorCode::RuntimeProtocolViolation`]: the runtime broke the contract
    /// its own committed schema declares. Note what is absent — there is no
    /// mapping for a refused approval, because rule R-P8 makes a decline a
    /// decision rather than a fault.
    pub fn canonical_error(&self, at_ms: i64) -> CanonicalError {
        CanonicalError {
            code: ErrorCode::RuntimeProtocolViolation,
            retriable: false,
            // Section 7 keeps upstream text out of canonical errors entirely.
            detail_ref: None,
            at_ms,
        }
    }

    /// Whether this failure means the pinned surface has drifted.
    pub fn is_surface_drift(&self) -> bool {
        matches!(
            self,
            CodexAdapterError::PointerUnresolved { .. }
                | CodexAdapterError::PointerTypeMismatch { .. }
                | CodexAdapterError::UnmodelledEnumeration { .. }
        )
    }
}
