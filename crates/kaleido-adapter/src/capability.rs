//! What an adapter concluded a runtime connection can actually do.
//!
//! Rule R-P6: a capability belongs to one runtime connection and carries
//! evidence. A probe therefore reports what it *observed*, and anything it did
//! not observe stays [`CapabilityState::NotVerified`] rather than being
//! optimistically promoted.

use kaleido_proto::capability::{
    Capability, CapabilityEntry, CapabilityEvidence, CapabilityState, EvidenceSource,
    RuntimeCapabilities,
};
use kaleido_proto::command::CommandOutcome;
use kaleido_proto::ids::{ProviderBindingKind, ProviderRuntimeId};

/// Every capability the contract defines, in a fixed order.
///
/// A probe emits an explicit entry for each one so a reader can distinguish
/// "unsupported" from "never checked" without consulting a provider name.
pub const ALL_CAPABILITIES: [Capability; 20] = [
    Capability::HistoryList,
    Capability::HistoryRead,
    Capability::HistoryResume,
    Capability::LiveObserve,
    Capability::LiveControl,
    Capability::LiveMultiSubscriber,
    Capability::TurnPrompt,
    Capability::TurnSteer,
    Capability::TurnInterrupt,
    Capability::TurnRetry,
    Capability::InteractionApproval,
    Capability::InteractionQuestion,
    Capability::StatePlan,
    Capability::StateTasks,
    Capability::StateDiff,
    Capability::StateToolLifecycle,
    Capability::QueueRead,
    Capability::QueueWrite,
    Capability::QueueReorder,
    Capability::WorkflowParticipate,
];

/// The result of probing one runtime connection.
#[derive(Debug, Clone)]
pub struct CapabilityProbe {
    runtime_id: ProviderRuntimeId,
    observed_at_ms: i64,
    proven: Vec<Capability>,
    evidence_source: EvidenceSource,
}

impl CapabilityProbe {
    /// Starts a probe in which nothing has been proven yet.
    pub fn new(
        runtime_id: ProviderRuntimeId,
        observed_at_ms: i64,
        evidence_source: EvidenceSource,
    ) -> Self {
        Self {
            runtime_id,
            observed_at_ms,
            proven: Vec::new(),
            evidence_source,
        }
    }

    /// Records that traffic proved this capability on this connection.
    pub fn prove(&mut self, capability: Capability) {
        if !self.proven.contains(&capability) {
            self.proven.push(capability);
        }
    }

    pub fn is_proven(&self, capability: Capability) -> bool {
        self.proven.contains(&capability)
    }

    /// Records session control only when live structured traffic proves that
    /// the runtime accepted a real broker command.
    ///
    /// Local acceptance, queueing and recorded fixtures are deliberately
    /// insufficient: they do not prove control on the current connection.
    pub fn observe_runtime_acceptance(&mut self, outcome: &CommandOutcome) -> bool {
        let CommandOutcome::AcceptedByRuntime { binding_handle } = outcome else {
            return false;
        };
        if self.evidence_source != EvidenceSource::ObservedInTraffic
            || binding_handle.runtime_id != self.runtime_id
            || binding_handle.kind != ProviderBindingKind::RuntimeAcknowledgement
            || outcome.validate().is_err()
        {
            return false;
        }
        self.prove(Capability::LiveControl);
        true
    }

    /// Everything traffic has proven so far, in the order it was proven.
    pub fn proven(&self) -> &[Capability] {
        &self.proven
    }

    pub fn observed_at_ms(&self) -> i64 {
        self.observed_at_ms
    }

    pub fn advance_observation(&mut self, at_ms: i64) {
        if at_ms > self.observed_at_ms {
            self.observed_at_ms = at_ms;
        }
    }

    /// Renders the probe as the negotiated capability set.
    ///
    /// Unproven capabilities become `NotVerified` with `Absent` evidence. They
    /// are never emitted as `Supported`, and never silently omitted, because a
    /// reader must be able to show the difference.
    pub fn to_capabilities(&self) -> RuntimeCapabilities {
        let entries = ALL_CAPABILITIES
            .iter()
            .map(|capability| {
                let proven = self.proven.contains(capability);
                CapabilityEntry {
                    capability: *capability,
                    state: if proven {
                        CapabilityState::Supported
                    } else {
                        CapabilityState::NotVerified
                    },
                    evidence: CapabilityEvidence {
                        source: if proven {
                            self.evidence_source
                        } else {
                            EvidenceSource::Absent
                        },
                        observed_at_ms: self.observed_at_ms,
                        note_ref: None,
                    },
                }
            })
            .collect();
        RuntimeCapabilities {
            runtime_id: self.runtime_id.clone(),
            negotiated_at_ms: self.observed_at_ms,
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaleido_proto::ids::{
        ProviderBindingHandle, ProviderBindingId, ProviderBindingKind, QueueEntryId,
    };

    #[test]
    fn an_unproven_capability_is_not_verified_rather_than_unsupported() {
        let probe = CapabilityProbe::new(
            ProviderRuntimeId::new("rtm_0123456789abcdef"),
            1_785_378_397_000,
            EvidenceSource::ObservedInTraffic,
        );
        let capabilities = probe.to_capabilities();
        assert!(capabilities.validate().is_ok());
        assert_eq!(
            capabilities.state_of(&Capability::TurnSteer),
            CapabilityState::NotVerified
        );
        assert!(!capabilities.permits(&Capability::TurnSteer));
        assert_eq!(capabilities.entries.len(), ALL_CAPABILITIES.len());
    }

    #[test]
    fn only_proven_capabilities_carry_their_evidence_source() {
        let mut probe = CapabilityProbe::new(
            ProviderRuntimeId::new("rtm_0123456789abcdef"),
            1_785_378_397_000,
            EvidenceSource::ObservedInTraffic,
        );
        probe.prove(Capability::TurnPrompt);
        let capabilities = probe.to_capabilities();
        let prompt = capabilities
            .entries
            .iter()
            .find(|entry| entry.capability == Capability::TurnPrompt);
        let steer = capabilities
            .entries
            .iter()
            .find(|entry| entry.capability == Capability::TurnSteer);
        assert_eq!(
            prompt.map(|entry| entry.evidence.source),
            Some(EvidenceSource::ObservedInTraffic)
        );
        assert_eq!(
            steer.map(|entry| entry.evidence.source),
            Some(EvidenceSource::Absent)
        );
    }

    #[test]
    fn only_live_runtime_acceptance_proves_control() {
        let runtime_id = ProviderRuntimeId::new("rtm_0123456789abcdef");
        let runtime_accepted = CommandOutcome::AcceptedByRuntime {
            binding_handle: ProviderBindingHandle {
                id: ProviderBindingId::new("bnd_0123456789abcdef"),
                runtime_id: runtime_id.clone(),
                kind: ProviderBindingKind::RuntimeAcknowledgement,
            },
        };
        let mut live = CapabilityProbe::new(
            runtime_id.clone(),
            1_785_378_397_000,
            EvidenceSource::ObservedInTraffic,
        );
        assert!(
            !live.observe_runtime_acceptance(&CommandOutcome::AcceptedLocally { note_ref: None })
        );
        assert!(!live.observe_runtime_acceptance(&CommandOutcome::Enqueued {
            entry_id: QueueEntryId::new("queue-local"),
        }));
        assert!(!live.is_proven(Capability::LiveControl));

        let wrong_runtime = CommandOutcome::AcceptedByRuntime {
            binding_handle: ProviderBindingHandle {
                id: ProviderBindingId::new("bnd_wrong_runtime"),
                runtime_id: ProviderRuntimeId::new("rtm_other_runtime"),
                kind: ProviderBindingKind::RuntimeAcknowledgement,
            },
        };
        assert!(!live.observe_runtime_acceptance(&wrong_runtime));
        assert!(!live.is_proven(Capability::LiveControl));

        let wrong_kind = CommandOutcome::AcceptedByRuntime {
            binding_handle: ProviderBindingHandle {
                id: ProviderBindingId::new("bnd_wrong_kind"),
                runtime_id: runtime_id.clone(),
                kind: ProviderBindingKind::Session,
            },
        };
        assert!(!live.observe_runtime_acceptance(&wrong_kind));
        assert!(!live.is_proven(Capability::LiveControl));

        assert!(live.observe_runtime_acceptance(&runtime_accepted));
        assert!(live.is_proven(Capability::LiveControl));
        assert!(!live.is_proven(Capability::TurnSteer));

        let mut replay = CapabilityProbe::new(
            runtime_id,
            1_785_378_397_000,
            EvidenceSource::RecordedFixture,
        );
        assert!(!replay.observe_runtime_acceptance(&runtime_accepted));
        assert!(!replay.is_proven(Capability::LiveControl));
    }
}
