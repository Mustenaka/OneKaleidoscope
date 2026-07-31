//! User input queue. See `docs/PROTOCOL.md` section 4.6.
//!
//! This is the third of the four independent state families in ADR-0010 D-3.
//! Rule R-P9: a queued input may only be reported as injected into the active
//! turn once the runtime has acknowledged that injection in observed traffic.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, EvidenceSource, RuntimeCapabilities};
use crate::content::{ContentRef, Sensitivity};
use crate::error::CanonicalError;
use crate::ids::{
    CommandId, ProviderBindingHandle, ProviderBindingKind, ProviderRuntimeId, QueueEntryId,
    SessionId, TurnId,
};
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QueueEntry {
    pub id: QueueEntryId,
    pub session_id: SessionId,
    /// Zero-based, contiguous across pending entries.
    pub position: u32,
    pub intent: QueueIntent,
    pub body: ContentRef,
    pub state: QueueState,
    pub editable: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum QueueIntent {
    /// Start a new turn when the session is free.
    NewTurn,
    /// Reach the turn that is already running.
    SteerActiveTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueueState {
    /// Held by the broker. Readers must show this as queued, never as sent.
    Pending,
    Submitting {
        command_id: CommandId,
    },
    DeliveredAsNewTurn {
        turn_id: TurnId,
        delivered_at_ms: i64,
    },
    /// Reached the running turn, proven by a runtime acknowledgement.
    DeliveredAsSteer {
        runtime_id: ProviderRuntimeId,
        turn_id: TurnId,
        binding_handle: ProviderBindingHandle,
        injected_at_ms: i64,
        ack: SteerAcknowledgement,
    },
    Rejected {
        error: CanonicalError,
    },
    Cancelled {
        at_ms: i64,
    },
}

/// Proof that the runtime injected an input into the running turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SteerAcknowledgement {
    /// Must be [`EvidenceSource::ObservedInTraffic`]; nothing else counts.
    pub source: EvidenceSource,
    pub runtime_id: ProviderRuntimeId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub binding_handle: ProviderBindingHandle,
    pub observed_at_ms: i64,
}

impl QueueState {
    /// Whether the input has actually reached the runtime.
    pub fn reached_runtime(&self) -> bool {
        matches!(
            self,
            QueueState::DeliveredAsNewTurn { .. } | QueueState::DeliveredAsSteer { .. }
        )
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, QueueState::Pending)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            QueueState::DeliveredAsNewTurn { .. }
                | QueueState::DeliveredAsSteer { .. }
                | QueueState::Rejected { .. }
                | QueueState::Cancelled { .. }
        )
    }
}

impl QueueEntry {
    /// Enforces the section 4.6 invariants.
    pub fn validate(&self) -> Result<(), ContractViolation> {
        self.body.validate()?;
        if self.body.sensitivity != Sensitivity::Sensitive {
            return Err(ContractViolation::SensitiveContentRequired {
                field: "queue_entry.body",
            });
        }
        if self.editable && !self.state.is_pending() {
            return Err(ContractViolation::EditableNonPendingQueueEntry);
        }
        if let QueueState::DeliveredAsSteer {
            runtime_id,
            turn_id,
            binding_handle,
            ack,
            ..
        } = &self.state
        {
            if ack.source != EvidenceSource::ObservedInTraffic {
                return Err(ContractViolation::UnprovenSteerDelivery {
                    evidence_source: ack.source,
                });
            }
            if ack.session_id != self.session_id {
                return Err(ContractViolation::SteerSessionMismatch);
            }
            if ack.turn_id != *turn_id {
                return Err(ContractViolation::SteerTurnMismatch);
            }
            if ack.runtime_id != *runtime_id {
                return Err(ContractViolation::SteerRuntimeMismatch);
            }
            if ack.binding_handle != *binding_handle {
                return Err(ContractViolation::SteerBindingMismatch);
            }
            binding_handle.validate_for(ProviderBindingKind::RuntimeAcknowledgement)?;
            if &binding_handle.runtime_id != runtime_id {
                return Err(ContractViolation::SteerBindingMismatch);
            }
        }
        if let QueueState::Rejected { error } = &self.state {
            error.validate()?;
        }
        Ok(())
    }

    /// Validates the additional facts needed to display a delivered steer
    /// against a runtime's current live turn.
    pub fn validate_for_active_turn(
        &self,
        active_turn_id: &TurnId,
        capabilities: &RuntimeCapabilities,
    ) -> Result<(), ContractViolation> {
        self.validate()?;
        if let QueueState::DeliveredAsSteer {
            runtime_id,
            turn_id,
            ..
        } = &self.state
        {
            if turn_id != active_turn_id {
                return Err(ContractViolation::SteerNotActiveTurn);
            }
            if &capabilities.runtime_id != runtime_id {
                return Err(ContractViolation::SteerRuntimeMismatch);
            }
            if !capabilities.permits(&Capability::TurnSteer) {
                return Err(ContractViolation::SteerCapabilityUnsupported);
            }
        }
        Ok(())
    }

    /// Whether this entry may be promoted out of `Pending` for the given
    /// runtime capabilities.
    ///
    /// A steering intent against a runtime that does not support steering must
    /// stay queued. This is the check that prevents a queued message from being
    /// presented as an injected one.
    pub fn may_submit(&self, capabilities: &RuntimeCapabilities) -> bool {
        match self.intent {
            QueueIntent::NewTurn => capabilities.permits(&Capability::TurnPrompt),
            QueueIntent::SteerActiveTurn => capabilities.permits(&Capability::TurnSteer),
        }
    }
}

/// Recomputes contiguous positions over the pending entries of one session.
///
/// Returns the identifiers in their new order. Non-pending entries keep their
/// recorded position and are excluded, because section 4.6 only permits
/// reordering pending entries.
pub fn reorderable_entries(entries: &[QueueEntry]) -> Vec<QueueEntryId> {
    let mut pending: Vec<&QueueEntry> = entries
        .iter()
        .filter(|entry| entry.state.is_pending())
        .collect();
    pending.sort_by_key(|entry| entry.position);
    pending.iter().map(|entry| entry.id.clone()).collect()
}

/// Validates a queue reorder command without mutating canonical state.
///
/// The supplied order must contain every pending entry for `session_id`
/// exactly once and no other entry.
pub fn validate_queue_reorder(
    session_id: &SessionId,
    order: &[QueueEntryId],
    entries: &[QueueEntry],
) -> Result<(), ContractViolation> {
    let mut seen = HashSet::new();
    for entry_id in order {
        if !seen.insert(entry_id.clone()) {
            return Err(ContractViolation::QueueReorderDuplicate);
        }
        let entry = entries
            .iter()
            .find(|candidate| candidate.id == *entry_id)
            .ok_or(ContractViolation::QueueReorderUnknownEntry)?;
        if &entry.session_id != session_id {
            return Err(ContractViolation::QueueReorderCrossSession);
        }
        if !entry.state.is_pending() {
            return Err(ContractViolation::QueueReorderNonPending);
        }
    }

    if entries.iter().any(|entry| {
        &entry.session_id == session_id && entry.state.is_pending() && !seen.contains(&entry.id)
    }) {
        return Err(ContractViolation::QueueReorderMissingPending);
    }
    Ok(())
}
