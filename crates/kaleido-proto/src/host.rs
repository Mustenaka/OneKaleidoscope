//! Host, provider runtime and project. See `docs/PROTOCOL.md` section 4.1.

use serde::{Deserialize, Serialize};

use crate::capability::RuntimeCapabilities;
use crate::content::ContentRef;
use crate::ids::{HostId, ProjectBindingId, ProjectId, ProviderBindingHandle, ProviderRuntimeId};
use crate::ContractViolation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Host {
    pub id: HostId,
    pub display_name: String,
    pub platform: HostPlatform,
    pub reachability: HostReachability,
    pub protocol_version: String,
    pub last_seen_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    Windows,
    MacOs,
    Linux,
}

/// Which of the three connection tiers is currently carrying traffic
/// (ADR-0011 D-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum HostReachability {
    Offline,
    LanDirect,
    PeerToPeer,
    Relayed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProviderRuntime {
    pub id: ProviderRuntimeId,
    pub host_id: HostId,
    pub family: ProviderFamily,
    /// Display and diagnostics only. Rule R-P6 forbids branching on it.
    pub version_label: Option<String>,
    pub launch_surface: LaunchSurface,
    pub connection: ConnectionState,
    pub capabilities: RuntimeCapabilities,
    pub binding_handle: Option<ProviderBindingHandle>,
}

/// Grouping label for the mobile project index. Never a capability decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum ProviderFamily {
    Codex,
    ClaudeCode,
    OpenCode,
    Acp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum LaunchSurface {
    BrokerLaunched,
    SharedServer,
    ExternalNativeCli,
    ExternalNativeGui,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected {
        since_at_ms: i64,
    },
    Degraded {
        reason: ConnectionFaultReason,
        since_at_ms: i64,
    },
    Unavailable {
        reason: ConnectionFaultReason,
        since_at_ms: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionFaultReason {
    ProcessExited { exit_code: Option<i64> },
    HandshakeRejected,
    AuthRequired,
    Timeout,
    TransportError,
    ProtocolViolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Project {
    pub id: ProjectId,
    pub display_name: String,
    pub bindings: Vec<ProjectBinding>,
    pub session_counts: SessionCounts,
    pub workflow_count: u32,
    pub attention_count: u32,
    pub last_activity_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProjectBinding {
    pub id: ProjectBindingId,
    pub project_id: ProjectId,
    pub runtime_id: ProviderRuntimeId,
    /// The project root is a full filesystem path and therefore sensitive.
    pub root_ref: ContentRef,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SessionCounts {
    pub total: u32,
    pub running: u32,
    pub waiting_human: u32,
    pub failed: u32,
    pub archived: u32,
}

impl ConnectionState {
    pub fn is_usable(&self) -> bool {
        matches!(
            self,
            ConnectionState::Connected { .. } | ConnectionState::Degraded { .. }
        )
    }
}

impl Host {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier { field: "host.id" });
        }
        Ok(())
    }
}

impl ProviderRuntime {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "provider_runtime.id",
            });
        }
        if self.host_id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "provider_runtime.host_id",
            });
        }
        self.capabilities.validate()?;
        if self.capabilities.runtime_id != self.id {
            return Err(ContractViolation::DanglingReference {
                field: "provider_runtime.capabilities.runtime_id",
            });
        }
        if let Some(binding_handle) = &self.binding_handle {
            binding_handle.validate()?;
            if binding_handle.runtime_id != self.id {
                return Err(ContractViolation::DanglingReference {
                    field: "provider_runtime.binding_handle.runtime_id",
                });
            }
        }
        Ok(())
    }
}

impl Project {
    pub fn validate(&self) -> Result<(), ContractViolation> {
        if self.id.is_empty() {
            return Err(ContractViolation::EmptyIdentifier {
                field: "project.id",
            });
        }

        let mut binding_ids = Vec::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            if binding.id.is_empty() {
                return Err(ContractViolation::EmptyIdentifier {
                    field: "project_binding.id",
                });
            }
            if binding.project_id != self.id {
                return Err(ContractViolation::ProjectBindingMismatch {
                    binding_id: binding.id.clone(),
                });
            }
            if binding.runtime_id.is_empty() {
                return Err(ContractViolation::EmptyIdentifier {
                    field: "project_binding.runtime_id",
                });
            }
            binding
                .root_ref
                .ensure_sensitive("project_binding.root_ref")?;
            if binding_ids.contains(&binding.id) {
                return Err(ContractViolation::DuplicateProjectBinding {
                    binding_id: binding.id.clone(),
                });
            }
            binding_ids.push(binding.id.clone());
        }
        Ok(())
    }
}
