use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kaleido_adapter::content::{ContentAccess, ContentAccessError};
use kaleido_adapter::{ProviderRuntimeSession, SessionStartRequest};
use kaleido_adapter_opencode::{
    OpenCodeClientConfig, OpenCodeRuntimeConfig, OpenCodeRuntimeSession, ReducerConfig,
};
use kaleido_proto::capability::{CapabilityUnavailableReason, EvidenceSource};
use kaleido_proto::content::{ContentAvailability, ContentKind, ContentRef, Sensitivity};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::HostPlatform;
use kaleido_proto::ids::{CommandId, ContentId, QueueEntryId};
use kaleido_proto::queue::{QueueEntry, QueueIntent, QueueState};
use kaleido_proto::session::SessionStatus;

#[derive(Debug, Default)]
struct MemoryContent {
    next: u64,
    values: BTreeMap<ContentId, Vec<u8>>,
}

impl ContentAccess for MemoryContent {
    fn store(
        &mut self,
        kind: ContentKind,
        sensitivity: Sensitivity,
        bytes: &[u8],
    ) -> Result<ContentRef, ContentAccessError> {
        self.next = self.next.saturating_add(1);
        let content_id = ContentId::new(format!("cnt_live_{:016x}", self.next));
        self.values.insert(content_id.clone(), bytes.to_vec());
        Ok(ContentRef {
            content_id,
            kind,
            byte_len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            digest: format!("sha256:{}", "0".repeat(64)),
            preview: None,
            sensitivity,
            availability: ContentAvailability::Stored,
        })
    }

    fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, ContentAccessError> {
        self.values
            .get(&reference.content_id)
            .cloned()
            .ok_or_else(|| ContentAccessError::Missing {
                content_id: reference.content_id.clone(),
            })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let base_url = arguments.next().ok_or("missing OpenCode base URL")?;
    let project_root = PathBuf::from(arguments.next().ok_or("missing project root")?);
    if !project_root.is_absolute() {
        return Err("project root must be absolute".into());
    }
    let project_directory = project_root.to_string_lossy().into_owned();
    let reducer = ReducerConfig {
        host_display_name: "kaleido-live-probe".to_owned(),
        host_platform: host_platform(),
        project_display_name: "kaleido-live-project".to_owned(),
        project_directory: project_directory.clone(),
        identity_salt: "kaleido-live-probe".to_owned(),
        evidence: EvidenceSource::ObservedInTraffic,
        base_at_ms: now_ms(),
        runtime_version_label: None,
    };
    let mut runtime = OpenCodeRuntimeSession::new(OpenCodeRuntimeConfig {
        client: OpenCodeClientConfig {
            base_url,
            project_directory: Some(project_directory),
            request_timeout: Duration::from_secs(30),
        },
        reducer,
    })?;
    let mut content = MemoryContent::default();
    let root_ref = content.store(
        ContentKind::FilePath,
        Sensitivity::Sensitive,
        project_root.to_string_lossy().as_bytes(),
    )?;
    let request = SessionStartRequest {
        project_id: runtime.project_id().clone(),
        project_binding_id: runtime.project_binding_id().clone(),
        runtime_id: runtime.runtime_id().clone(),
        project_root_ref: root_ref,
    };
    let discovery = runtime
        .discover(now_ms(), &mut content)
        .map_err(|_| "OpenCode discovery failed")?;
    let start = runtime
        .start(&request, &mut content)
        .map_err(|_| "OpenCode start failed")?;
    let prompt = content.store(
        ContentKind::PlainText,
        Sensitivity::Sensitive,
        b"Reply with the single word READY.",
    )?;
    let accepted = runtime
        .submit_prompt(
            &CommandId::new("cmd_live_opencode_probe"),
            &prompt,
            &mut content,
        )
        .map_err(|_| "OpenCode prompt admission failed")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut streamed = 0_usize;
    let mut recovery_effects = 0_usize;
    let mut protocol_recoveries = 0_usize;
    let mut protocol_failure = None;
    let mut idle = false;
    while Instant::now() < deadline && !idle {
        let effects = match runtime.drain_effects(&mut content) {
            Ok(effects) => effects,
            Err(error) if error.ends_the_connection() => {
                recovery_effects = recovery_effects.saturating_add(
                    runtime
                        .connection_lost_effects(
                            CapabilityUnavailableReason::SubscriptionLost,
                            now_ms(),
                        )
                        .len(),
                );
                recovery_effects = recovery_effects.saturating_add(
                    ProviderRuntimeSession::reconnect(&mut runtime, &request, &mut content)
                        .map_err(|reconnect| {
                            format!("OpenCode recovery after `{error}` failed: {reconnect}")
                        })?
                        .len(),
                );
                protocol_recoveries = protocol_recoveries.saturating_add(1);
                protocol_failure = Some(error.to_string());
                break;
            }
            Err(error) => {
                return Err(format!("OpenCode live SSE reduction failed: {error}").into());
            }
        };
        streamed = streamed.saturating_add(effects.len());
        idle = effects.iter().any(|effect| match effect {
            StateEffect::SessionStatusChanged { status, .. } => *status == SessionStatus::Idle,
            StateEffect::SessionUpserted { session } => session.status == SessionStatus::Idle,
            _ => false,
        });
    }
    let _ = runtime.close()?;
    let session_id = runtime
        .session_id()
        .cloned()
        .ok_or("OpenCode start did not expose a canonical session")?;
    let resumed =
        ProviderRuntimeSession::resume_session(&mut runtime, &session_id, &request, &mut content)
            .map_err(|error| format!("OpenCode canonical resume failed: {error}"))?;
    let queue_command_id = CommandId::new("cmd_live_opencode_queue_probe");
    let queue_body = content.store(
        ContentKind::PlainText,
        Sensitivity::Sensitive,
        b"Reply with the single word QUEUED.",
    )?;
    let queue_entry = QueueEntry {
        id: QueueEntryId::new("que_live_opencode_probe"),
        session_id,
        position: 0,
        intent: QueueIntent::NewTurn,
        body: queue_body,
        state: QueueState::Submitting {
            command_id: queue_command_id.clone(),
        },
        editable: false,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
    };
    let queue_effects = runtime
        .deliver_queue_entry(&queue_command_id, &queue_entry, &mut content)
        .map_err(|error| format!("OpenCode structured queue delivery failed: {error}"))?;
    let queue_delivered = queue_effects.iter().any(|effect| {
        matches!(
            effect,
            StateEffect::QueueEntryUpserted {
                entry: QueueEntry {
                    state: QueueState::DeliveredAsNewTurn { .. },
                    ..
                }
            }
        )
    });
    if !queue_delivered {
        return Err("OpenCode queue receipt did not produce a delivered queue entry".into());
    }
    let _ = runtime.close()?;
    println!(
        "{{\"discovery_effects\":{},\"start_effects\":{},\"acceptance_effects\":{},\"stream_effects\":{},\"recovery_effects\":{},\"protocol_recoveries\":{},\"resume_effects\":{},\"queue_effects\":{},\"queue_delivered\":true,\"idle\":{},\"realtime_converged\":{},\"lossless_replay\":false}}",
        discovery.len(),
        start.len(),
        accepted.len(),
        streamed,
        recovery_effects,
        protocol_recoveries,
        resumed.len(),
        queue_effects.len(),
        idle,
        idle && protocol_failure.is_none()
    );
    if let Some(error) = protocol_failure {
        return Err(format!("OpenCode 1.18.16 live schema gate failed: {error}").into());
    }
    if !idle {
        return Err("OpenCode live probe timed out before idle".into());
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(target_os = "windows")]
fn host_platform() -> HostPlatform {
    HostPlatform::Windows
}

#[cfg(target_os = "macos")]
fn host_platform() -> HostPlatform {
    HostPlatform::MacOs
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn host_platform() -> HostPlatform {
    HostPlatform::Linux
}

#[cfg(target_os = "android")]
compile_error!("the desktop OpenCode live probe is not available on Android");
