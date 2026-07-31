//! Runtime capability negotiation. See `docs/PROTOCOL.md` section 4.2.
//!
//! Rule R-P6: a capability belongs to one runtime connection and carries
//! evidence. Readers must never branch on a provider name or version label.

use serde::{Deserialize, Serialize};

use crate::content::ContentRef;
use crate::ids::{BlockerId, ProviderRuntimeId};
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RuntimeCapabilities {
    pub runtime_id: ProviderRuntimeId,
    pub negotiated_at_ms: i64,
    pub entries: Vec<CapabilityEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CapabilityEntry {
    pub capability: Capability,
    pub state: CapabilityState,
    pub evidence: CapabilityEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    HistoryList,
    HistoryRead,
    HistoryResume,
    LiveObserve,
    LiveControl,
    LiveMultiSubscriber,
    TurnPrompt,
    TurnSteer,
    TurnInterrupt,
    TurnRetry,
    InteractionApproval,
    InteractionQuestion,
    StatePlan,
    StateTasks,
    StateDiff,
    StateToolLifecycle,
    QueueRead,
    QueueWrite,
    QueueReorder,
    WorkflowParticipate,
}

/// The five states a reader must be able to distinguish (ADR-0009 D-4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapabilityState {
    Supported,
    Unsupported,
    UnavailableOnThisConnection {
        reason: CapabilityUnavailableReason,
    },
    NotVerified,
    /// No public upstream path exists yet. This is never a passing gate.
    UpstreamBlocked {
        blocker_id: BlockerId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum CapabilityUnavailableReason {
    RuntimeDisconnected,
    AuthenticationRequired,
    SubscriptionLost,
    PolicyRestricted,
    TemporarilyUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CapabilityEvidence {
    pub source: EvidenceSource,
    pub observed_at_ms: i64,
    pub note_ref: Option<ContentRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    HandshakeDeclared,
    ObservedInTraffic,
    RecordedFixture,
    ManualAcceptance,
    Absent,
}

impl CapabilityState {
    /// Whether a command guarded by this capability may be sent to the runtime.
    ///
    /// Only `Supported` permits it. `NotVerified` deliberately does not: an
    /// unprobed capability must not be optimistically exercised.
    pub fn permits_use(&self) -> bool {
        matches!(self, CapabilityState::Supported)
    }
}

impl RuntimeCapabilities {
    /// Resolves a capability, defaulting to `NotVerified` when absent.
    ///
    /// Section 4.2 forbids treating an absent entry as either supported or
    /// unsupported.
    pub fn state_of(&self, capability: &Capability) -> CapabilityState {
        self.entries
            .iter()
            .find(|entry| &entry.capability == capability)
            .map_or(CapabilityState::NotVerified, |entry| entry.state.clone())
    }

    pub fn permits(&self, capability: &Capability) -> bool {
        self.state_of(capability).permits_use()
    }

    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.runtime_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "runtime_capabilities.runtime_id",
            });
        }

        let mut seen = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            if seen.contains(&entry.capability) {
                return Err(ContractViolation::DuplicateCapability {
                    capability: entry.capability,
                });
            }
            seen.push(entry.capability);
            entry.evidence.validate()?;
            if let CapabilityState::UpstreamBlocked { blocker_id } = &entry.state {
                if blocker_id.is_empty() {
                    return Err(ContractViolation::EmptyIdentifier {
                        field: "capability.blocker_id",
                    });
                }
            }
        }
        Ok(())
    }
}

impl CapabilityEvidence {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if let Some(note_ref) = &self.note_ref {
            note_ref.ensure_sensitive("capability_evidence.note_ref")?;
        }
        Ok(())
    }
}
