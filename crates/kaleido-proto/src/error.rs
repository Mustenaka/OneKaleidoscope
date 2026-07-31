//! Canonical error model. See `docs/PROTOCOL.md` section 7.

use serde::{Deserialize, Serialize};

use crate::content::{ContentRef, Sensitivity};
use crate::ids::BlockerId;
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CanonicalError {
    pub code: ErrorCode,
    pub retriable: bool,
    /// Anything that could be sensitive, including upstream error text.
    pub detail_ref: Option<ContentRef>,
    pub at_ms: i64,
}

/// Note what is absent: there is no code for a refused approval. A human
/// refusal is [`crate::turn::ItemStatus::Declined`] plus an answered attention
/// entry, never an error (rule R-P8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidCommand,
    CommandExpired,
    IdempotencyConflict,
    /// The runtime does not implement this capability at all.
    CapabilityUnsupported,
    /// The runtime implements it, but this connection cannot use it now.
    CapabilityUnavailable,
    /// No public upstream path exists. Never a passing gate.
    UpstreamBlocked {
        blocker_id: BlockerId,
    },
    ApprovalExpired,
    ApprovalAlreadyAnswered,
    JoinFailed,
    RuntimeUnavailable,
    RuntimeProtocolViolation,
    UpstreamRejected,
    UpstreamTimeout,
    AuthRequired,
    CursorGap,
    ContentEvicted,
    BackpressureDropped,
    Internal,
}

impl CanonicalError {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if let Some(detail_ref) = &self.detail_ref {
            detail_ref.validate()?;
            if detail_ref.sensitivity != Sensitivity::Sensitive {
                return Err(ContractViolation::SensitiveContentRequired {
                    field: "canonical_error.detail_ref",
                });
            }
        }
        Ok(())
    }
}

impl ErrorCode {
    /// Whether the condition may resolve on its own.
    pub fn transient(&self) -> bool {
        matches!(
            self,
            ErrorCode::RuntimeUnavailable | ErrorCode::UpstreamTimeout | ErrorCode::CursorGap
        )
    }

    /// Whether the condition requires a human decision before retrying.
    pub fn needs_human(&self) -> bool {
        matches!(
            self,
            ErrorCode::AuthRequired
                | ErrorCode::UpstreamBlocked { .. }
                | ErrorCode::ApprovalExpired
        )
    }
}
