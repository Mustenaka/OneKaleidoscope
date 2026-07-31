//! Session ownership, history provenance and live binding.
//! See `docs/PROTOCOL.md` section 4.3.

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, CapabilityEvidence, EvidenceSource, RuntimeCapabilities};
use crate::ids::{
    BlockerId, ProjectBindingId, ProjectId, ProviderBindingHandle, ProviderBindingKind,
    ProviderRuntimeId, SessionId, TurnId,
};
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Session {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub project_binding_id: ProjectBindingId,
    pub ownership: OwnershipMode,
    pub history_source: HistorySource,
    pub live_binding: LiveBinding,
    pub status: SessionStatus,
    pub title: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_activity_at_ms: i64,
    pub active_turn_id: Option<TurnId>,
    pub queue_depth: u32,
    pub open_attention_count: u32,
    pub archived: bool,
    pub binding_handle: Option<ProviderBindingHandle>,
}

/// The three ownership modes of ADR-0009 D-2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum OwnershipMode {
    /// The broker created and owns the runtime.
    BrokerManaged,
    /// A native surface and the broker are both clients of one public server.
    SharedRuntime,
    /// A native surface created the session independently.
    ExternalNative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Offline,
    Idle,
    Running,
    WaitingUser,
    WaitingApproval,
    /// No active turn, and at least one pending queue entry awaits submission.
    Queued,
    Failed,
    Completed,
    Cancelled,
}

/// Where history comes from. Rule R-P7 keeps this independent of
/// [`LiveBinding`]: being able to list or resume history never proves that
/// another process's current turn can be observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct HistorySource {
    pub kind: HistorySourceKind,
    pub runtime_id: Option<ProviderRuntimeId>,
    pub evidence: CapabilityEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum HistorySourceKind {
    None,
    ProviderApi,
    ProviderLocalStore,
    BrokerLog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveBinding {
    NotBound {
        reason: LiveUnboundReason,
    },
    /// Real-time traffic for this session has actually been received.
    Observing {
        runtime_id: ProviderRuntimeId,
        since_at_ms: i64,
        evidence: CapabilityEvidence,
    },
    /// Observing, plus the runtime accepts control for this session.
    Controlling {
        runtime_id: ProviderRuntimeId,
        since_at_ms: i64,
        evidence: CapabilityEvidence,
    },
    Blocked {
        blocker_id: BlockerId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum LiveUnboundReason {
    NeverStarted,
    RuntimeExited,
    SubscriptionLost,
    /// No public attach path exists for this surface.
    NoPublicAttachPath,
}

impl LiveBinding {
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            LiveBinding::Observing { .. } | LiveBinding::Controlling { .. }
        )
    }

    pub fn accepts_control(&self) -> bool {
        matches!(self, LiveBinding::Controlling { .. })
    }

    pub fn validate_shape(&self) -> Result<(), ContractViolation> {
        match self {
            LiveBinding::NotBound { .. } => Ok(()),
            LiveBinding::Blocked { blocker_id } => {
                if blocker_id.is_empty() {
                    Err(ContractViolation::EmptyIdentifier {
                        field: "live_binding.blocker_id",
                    })
                } else {
                    Ok(())
                }
            }
            LiveBinding::Observing {
                runtime_id,
                evidence,
                ..
            }
            | LiveBinding::Controlling {
                runtime_id,
                evidence,
                ..
            } => validate_observed_evidence(runtime_id, evidence),
        }
    }

    /// Rejects a binding the negotiated capabilities do not support.
    ///
    /// This is the check that stops history access, disk polling or process
    /// discovery from being promoted into a live attachment.
    pub fn validate_against(
        &self,
        capabilities: &RuntimeCapabilities,
    ) -> Result<(), ContractViolation> {
        match self {
            LiveBinding::NotBound { .. } | LiveBinding::Blocked { .. } => Ok(()),
            LiveBinding::Observing {
                runtime_id,
                evidence,
                ..
            } => {
                validate_live_evidence(runtime_id, evidence, capabilities)?;
                if capabilities.permits(&Capability::LiveObserve) {
                    Ok(())
                } else {
                    Err(ContractViolation::LiveBindingUnsupported {
                        missing: "live_observe",
                    })
                }
            }
            LiveBinding::Controlling {
                runtime_id,
                evidence,
                ..
            } => {
                validate_live_evidence(runtime_id, evidence, capabilities)?;
                if !capabilities.permits(&Capability::LiveObserve) {
                    Err(ContractViolation::LiveBindingUnsupported {
                        missing: "live_observe",
                    })
                } else if !capabilities.permits(&Capability::LiveControl) {
                    Err(ContractViolation::LiveBindingUnsupported {
                        missing: "live_control",
                    })
                } else {
                    Ok(())
                }
            }
        }
    }
}

fn validate_live_evidence(
    runtime_id: &ProviderRuntimeId,
    evidence: &CapabilityEvidence,
    capabilities: &RuntimeCapabilities,
) -> Result<(), ContractViolation> {
    validate_observed_evidence(runtime_id, evidence)?;
    if runtime_id != &capabilities.runtime_id {
        return Err(ContractViolation::LiveBindingRuntimeMismatch);
    }
    Ok(())
}

fn validate_observed_evidence(
    runtime_id: &ProviderRuntimeId,
    evidence: &CapabilityEvidence,
) -> Result<(), ContractViolation> {
    if runtime_id.is_empty() {
        return Err(ContractViolation::EmptyIdentifier {
            field: "live_binding.runtime_id",
        });
    }
    if evidence.source != EvidenceSource::ObservedInTraffic {
        return Err(ContractViolation::LiveBindingEvidenceNotObserved);
    }
    evidence.validate()
}

impl SessionStatus {
    /// Whether a human is expected to act before the session can continue.
    pub fn waits_for_human(&self) -> bool {
        matches!(
            self,
            SessionStatus::WaitingUser | SessionStatus::WaitingApproval
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(self, SessionStatus::Running | SessionStatus::Queued)
    }
}

impl Session {
    pub fn validate_shape(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "session.id",
            });
        }
        if self.project_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "session.project_id",
            });
        }
        if self.project_binding_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "session.project_binding_id",
            });
        }
        self.history_source.evidence.validate()?;
        if let Some(runtime_id) = &self.history_source.runtime_id {
            if runtime_id.is_empty() {
                return Err(ContractViolation::EmptyIdentifier {
                    field: "session.history_source.runtime_id",
                });
            }
        }
        if let Some(binding_handle) = &self.binding_handle {
            binding_handle.validate_for(ProviderBindingKind::Session)?;
        }
        self.live_binding.validate_shape()
    }

    pub fn validate(&self, capabilities: &RuntimeCapabilities) -> Result<(), ContractViolation> {
        self.validate_shape()?;
        capabilities.validate()?;
        self.live_binding.validate_against(capabilities)
    }
}

/// Derives the runtime status from the four independent state families.
///
/// Section 4.3 fixes the precedence so that two implementations cannot disagree
/// about what a session shows while both an approval and a question are open,
/// or while the queue is non-empty but no turn is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusInputs {
    pub runtime_usable: bool,
    pub has_active_turn: bool,
    pub open_approval: bool,
    pub open_question: bool,
    pub pending_queue_entries: u32,
    pub terminal: Option<SessionStatus>,
}

pub fn derive_session_status(inputs: StatusInputs) -> SessionStatus {
    if !inputs.runtime_usable {
        return SessionStatus::Offline;
    }
    if inputs.open_approval {
        return SessionStatus::WaitingApproval;
    }
    if inputs.open_question {
        return SessionStatus::WaitingUser;
    }
    if inputs.has_active_turn {
        return SessionStatus::Running;
    }
    if inputs.pending_queue_entries > 0 {
        return SessionStatus::Queued;
    }
    match inputs.terminal {
        Some(
            terminal
            @ (SessionStatus::Failed | SessionStatus::Completed | SessionStatus::Cancelled),
        ) => terminal,
        _ => SessionStatus::Idle,
    }
}
