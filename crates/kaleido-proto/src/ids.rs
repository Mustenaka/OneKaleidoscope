//! Opaque identifiers. See `docs/PROTOCOL.md` section 3.
//!
//! Every identifier is a record with a single named `value` field rather than a
//! tuple struct, because rule R-P1 restricts the contract to constructs a
//! foreign-function binding generator can express.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::ContractViolation;

macro_rules! id_type {
    ($($name:ident),* $(,)?) => {
        $(
            #[derive(
                Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            )]
            #[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
            pub struct $name {
                pub value: String,
            }

            impl $name {
                pub fn new(value: impl Into<String>) -> Self {
                    Self { value: value.into() }
                }

                pub fn as_str(&self) -> &str {
                    &self.value
                }

                pub fn is_empty(&self) -> bool {
                    self.value.is_empty()
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str(&self.value)
                }
            }
        )*
    };
}

id_type!(
    HostId,
    ProviderRuntimeId,
    ProjectId,
    SessionId,
    TurnId,
    ItemId,
    QueueEntryId,
    AttentionId,
    WorkflowId,
    StepId,
    ArtifactId,
    CommandId,
    ContentId,
    BlockerId,
    ProjectBindingId,
    ProviderBindingId,
    AgentTaskId,
);

/// Broker-assigned opaque handle for provider-private binding data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProviderBindingHandle {
    pub id: ProviderBindingId,
    pub runtime_id: ProviderRuntimeId,
    pub kind: ProviderBindingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ProviderBindingKind {
    Session,
    Turn,
    Item,
    InteractionRequest,
    RuntimeAcknowledgement,
}

impl ProviderBindingHandle {
    pub fn is_empty(&self) -> bool {
        self.id.is_empty() || self.runtime_id.is_empty()
    }

    pub fn validate(&self) -> Result<(), ContractViolation> {
        let Some(suffix) = self.id.value.strip_prefix("bnd_") else {
            return Err(ContractViolation::InvalidProviderBindingId);
        };
        if suffix.len() < 8
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ContractViolation::InvalidProviderBindingId);
        }
        if self.runtime_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "binding_handle.runtime_id",
            });
        }
        Ok(())
    }

    pub fn validate_for(&self, expected: ProviderBindingKind) -> Result<(), ContractViolation> {
        self.validate()?;
        if self.kind != expected {
            return Err(ContractViolation::ProviderBindingKindMismatch {
                expected,
                actual: self.kind,
            });
        }
        Ok(())
    }
}
