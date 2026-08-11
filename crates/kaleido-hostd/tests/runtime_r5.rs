#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Deterministic tests for provider-neutral host orchestration.
//!
//! `HostTestRuntime` is deliberately a test double. It verifies Broker/worker
//! ordering and routing only; it is not provider wire evidence and must never
//! be copied into `tests/fixtures` or cited as real-provider acceptance.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::content::ContentAccess;
use kaleido_adapter::{
    IdentityMint, ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest,
};
use kaleido_hostd::slice::REPLAY_BASE_AT_MS;
use kaleido_hostd::{
    Broker, CodexLanError, RuntimeBootstrap, RuntimeBootstrapFactory, RuntimeFailureClass,
    RuntimeLifecycleReport, RuntimeLifecycleStage, RuntimeSupervisor, RuntimeSupervisorError,
    StructuredLanConfig, StructuredLanHost,
};
use kaleido_proto::attention::AttentionResponse;
use kaleido_proto::capability::{Capability, EvidenceSource};
use kaleido_proto::command::{
    Command, CommandAck, CommandOutcome, DeviceCommandRequest, RuntimeAcceptanceKind,
};
use kaleido_proto::content::{
    ContentKind, ContentRef, ContentWriteRequest, ContentWriteResponse, Sensitivity,
};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::{
    ConnectionFaultReason, ConnectionState, Host, HostPlatform, HostReachability, LaunchSurface,
    Project, ProjectBinding, ProviderFamily, ProviderRuntime, SessionCounts,
};
use kaleido_proto::ids::{
    CommandId, DeviceId, HostId, ProjectBindingId, ProjectId, ProviderBindingHandle,
    ProviderBindingId, ProviderBindingKind, ProviderRuntimeId, SessionId, TurnId,
};
use kaleido_proto::queue::{QueueEntry, QueueIntent, QueueState};
use kaleido_proto::session::{
    HistorySource, HistorySourceKind, LiveBinding, LiveUnboundReason, OwnershipMode, Session,
    SessionStatus,
};
use kaleido_proto::turn::{Turn, TurnOrigin, TurnStatus};
use kaleido_state::{CanonicalStore, ClockSource};
use sha2::{Digest, Sha256};

const NOW_MS: i64 = REPLAY_BASE_AT_MS + 500_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeCall {
    runtime_id: ProviderRuntimeId,
    operation: &'static str,
    command_id: Option<CommandId>,
    turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainMode {
    Empty,
    DisconnectThenReconnect,
    ProtocolError,
    InvalidEffect,
}

struct HostTestRuntime {
    host_id: HostId,
    runtime_id: ProviderRuntimeId,
    project_id: ProjectId,
    project_binding_id: ProjectBindingId,
    session: Session,
    discovered_session: Session,
    initial_turn: Option<Turn>,
    drain_mode: DrainMode,
    calls: Arc<Mutex<Vec<RuntimeCall>>>,
}

impl HostTestRuntime {
    fn new(
        mint: &IdentityMint,
        label: &str,
        calls: Arc<Mutex<Vec<RuntimeCall>>>,
        drain_mode: DrainMode,
        with_active_turn: bool,
    ) -> Self {
        let host_id = mint.host_id("r5-host");
        let runtime_id = mint.runtime_id(&format!("runtime-{label}"));
        let project_id = mint.project_id(&format!("project-{label}"));
        let project_binding_id = mint.project_binding_id(&format!("project-{label}|{runtime_id}"));
        let session_id = mint.session_id(&format!("session-{label}"));
        let turn_id = mint.turn_id(&format!("turn-{label}"));
        let session = Session {
            id: session_id.clone(),
            project_id: project_id.clone(),
            project_binding_id: project_binding_id.clone(),
            ownership: OwnershipMode::BrokerManaged,
            history_source: HistorySource {
                kind: HistorySourceKind::None,
                runtime_id: None,
                evidence: evidence(),
            },
            live_binding: LiveBinding::NotBound {
                reason: LiveUnboundReason::NeverStarted,
            },
            status: SessionStatus::Idle,
            title: Some(format!("runtime-{label}")),
            created_at_ms: NOW_MS,
            updated_at_ms: NOW_MS,
            last_activity_at_ms: NOW_MS,
            active_turn_id: None,
            queue_depth: 0,
            open_attention_count: 0,
            archived: false,
            binding_handle: Some(binding(
                &runtime_id,
                ProviderBindingKind::Session,
                "session-test",
            )),
        };
        let mut discovered_session = session.clone();
        discovered_session.id = mint.session_id(&format!("discovered-{label}"));
        discovered_session.ownership = OwnershipMode::ProviderManaged;
        discovered_session.history_source = HistorySource {
            kind: HistorySourceKind::ProviderApi,
            runtime_id: Some(runtime_id.clone()),
            evidence: evidence(),
        };
        discovered_session.status = SessionStatus::Offline;
        discovered_session.title = Some(format!("discovered-{label}"));
        discovered_session.binding_handle = Some(binding(
            &runtime_id,
            ProviderBindingKind::Session,
            "discovered-session-test",
        ));
        let initial_turn = with_active_turn.then(|| Turn {
            id: turn_id,
            session_id,
            status: TurnStatus::Running,
            origin: TurnOrigin::LocalSurface,
            started_at_ms: Some(NOW_MS),
            completed_at_ms: None,
            item_ids: Vec::new(),
            error: None,
            binding_handle: None,
        });
        Self {
            host_id,
            runtime_id,
            project_id,
            project_binding_id,
            session,
            discovered_session,
            initial_turn,
            drain_mode,
            calls,
        }
    }

    fn request(&self, root_ref: ContentRef) -> SessionStartRequest {
        SessionStartRequest {
            project_id: self.project_id.clone(),
            project_binding_id: self.project_binding_id.clone(),
            runtime_id: self.runtime_id.clone(),
            project_root_ref: root_ref,
        }
    }

    fn record(
        &self,
        operation: &'static str,
        command_id: Option<CommandId>,
        turn_id: Option<TurnId>,
    ) {
        self.calls.lock().unwrap().push(RuntimeCall {
            runtime_id: self.runtime_id.clone(),
            operation,
            command_id,
            turn_id,
        });
    }

    fn runtime_ack(
        &self,
        command_id: &CommandId,
        acceptance_kind: RuntimeAcceptanceKind,
    ) -> StateEffect {
        StateEffect::CommandAcknowledged {
            ack: CommandAck {
                command_id: command_id.clone(),
                outcome: CommandOutcome::AcceptedByRuntime {
                    session_id: self.session.id.clone(),
                    acceptance_kind,
                    binding_handle: binding(
                        &self.runtime_id,
                        ProviderBindingKind::RuntimeAcknowledgement,
                        "runtime-ack",
                    ),
                },
                acked_at_ms: NOW_MS + 10,
            },
        }
    }
}

impl ProviderRuntimeSession for HostTestRuntime {
    fn discover(
        &mut self,
        request: &SessionStartRequest,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.record("discover", None, None);
        let capabilities = self.capability_probe().to_capabilities();
        Ok(vec![
            StateEffect::HostUpserted {
                host: Host {
                    id: self.host_id.clone(),
                    display_name: "r5-host".to_owned(),
                    platform: HostPlatform::Windows,
                    reachability: HostReachability::Offline,
                    protocol_version: kaleido_proto::PROTOCOL_VERSION.to_owned(),
                    last_seen_at_ms: NOW_MS,
                },
            },
            StateEffect::RuntimeUpserted {
                runtime: ProviderRuntime {
                    id: self.runtime_id.clone(),
                    host_id: self.host_id.clone(),
                    family: ProviderFamily::OpenCode,
                    version_label: Some("host-test-double".to_owned()),
                    launch_surface: LaunchSurface::BrokerLaunched,
                    connection: ConnectionState::Connected {
                        since_at_ms: NOW_MS,
                    },
                    capabilities,
                    binding_handle: None,
                },
            },
            StateEffect::ProjectUpserted {
                project: Project {
                    id: self.project_id.clone(),
                    display_name: format!("test-{}", self.project_id),
                    bindings: vec![ProjectBinding {
                        id: self.project_binding_id.clone(),
                        project_id: self.project_id.clone(),
                        runtime_id: self.runtime_id.clone(),
                        root_ref: request.project_root_ref.clone(),
                    }],
                    session_counts: SessionCounts::default(),
                    workflow_count: 0,
                    attention_count: 0,
                    last_activity_at_ms: NOW_MS,
                },
            },
            StateEffect::SessionUpserted {
                session: self.discovered_session.clone(),
            },
        ])
    }

    fn start(
        &mut self,
        request: &SessionStartRequest,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if request.runtime_id != self.runtime_id
            || request.project_id != self.project_id
            || request.project_binding_id != self.project_binding_id
            || content.load(&request.project_root_ref)?.is_empty()
        {
            return Err(protocol_error("start identity mismatch"));
        }
        self.record("start", None, None);
        let mut effects = vec![StateEffect::SessionUpserted {
            session: self.session.clone(),
        }];
        if let Some(turn) = &self.initial_turn {
            effects.push(StateEffect::TurnUpserted { turn: turn.clone() });
        }
        Ok(effects)
    }

    fn submit_prompt(
        &mut self,
        command_id: &CommandId,
        body: &ContentRef,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if content.load(body)?.is_empty() {
            return Err(protocol_error("empty prompt"));
        }
        self.record("submit_prompt", Some(command_id.clone()), None);
        let turn = Turn {
            id: IdentityMint::new(self.runtime_id.as_str())
                .turn_id(&format!("command-{command_id}")),
            session_id: self.session.id.clone(),
            status: TurnStatus::Running,
            origin: TurnOrigin::RemoteCommand {
                command_id: command_id.clone(),
            },
            started_at_ms: Some(NOW_MS + 10),
            completed_at_ms: None,
            item_ids: Vec::new(),
            error: None,
            binding_handle: None,
        };
        Ok(vec![
            StateEffect::TurnUpserted { turn },
            self.runtime_ack(command_id, RuntimeAcceptanceKind::PromptTurn),
        ])
    }

    fn respond_attention(
        &mut self,
        _command_id: &CommandId,
        _response: &AttentionResponse,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        Err(RuntimeSessionError::CapabilityUnavailable)
    }

    fn reconnect(
        &mut self,
        _request: &SessionStartRequest,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.record("reconnect", None, None);
        let mut session = self.session.clone();
        session.title = Some("reconnected".to_owned());
        session.updated_at_ms = NOW_MS + 20;
        Ok(vec![StateEffect::SessionUpserted { session }])
    }

    fn resume_session(
        &mut self,
        session_id: &SessionId,
        _request: &SessionStartRequest,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if session_id != &self.discovered_session.id {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        self.record("resume_session", None, None);
        let mut session = self.discovered_session.clone();
        session.title = Some("resumed-discovery".to_owned());
        session.updated_at_ms = NOW_MS + 20;
        Ok(vec![StateEffect::SessionUpserted { session }])
    }

    fn interrupt_turn(
        &mut self,
        command_id: &CommandId,
        turn_id: &TurnId,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.record(
            "interrupt_turn",
            Some(command_id.clone()),
            Some(turn_id.clone()),
        );
        Ok(vec![self.runtime_ack(
            command_id,
            RuntimeAcceptanceKind::SessionControl,
        )])
    }

    fn deliver_queue_entry(
        &mut self,
        command_id: &CommandId,
        entry: &QueueEntry,
        content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if entry.intent != QueueIntent::NewTurn || content.load(&entry.body)?.is_empty() {
            return Err(RuntimeSessionError::CapabilityUnavailable);
        }
        self.record("deliver_queue", Some(command_id.clone()), None);
        let turn = Turn {
            id: IdentityMint::new(self.runtime_id.as_str()).turn_id(&format!("queue-{command_id}")),
            session_id: entry.session_id.clone(),
            status: TurnStatus::Pending,
            origin: TurnOrigin::RemoteCommand {
                command_id: command_id.clone(),
            },
            started_at_ms: None,
            completed_at_ms: None,
            item_ids: Vec::new(),
            error: None,
            binding_handle: None,
        };
        let mut delivered = entry.clone();
        delivered.state = QueueState::DeliveredAsNewTurn {
            turn_id: turn.id.clone(),
            delivered_at_ms: NOW_MS + 10,
        };
        delivered.editable = false;
        delivered.updated_at_ms = NOW_MS + 10;
        Ok(vec![
            StateEffect::TurnUpserted { turn },
            StateEffect::QueueEntryUpserted { entry: delivered },
        ])
    }

    fn drain_effects(
        &mut self,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.record("drain", None, None);
        match self.drain_mode {
            DrainMode::Empty => Ok(Vec::new()),
            DrainMode::DisconnectThenReconnect => {
                self.drain_mode = DrainMode::Empty;
                Err(RuntimeSessionError::ConnectionFault {
                    reason: ConnectionFaultReason::TransportError,
                })
            }
            DrainMode::ProtocolError => Err(protocol_error("malformed structured event")),
            DrainMode::InvalidEffect => Ok(vec![StateEffect::SessionStatusChanged {
                session_id: SessionId::new("ses_unknown_runtime_test"),
                status: SessionStatus::Idle,
            }]),
        }
    }

    fn close(&mut self) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.record("close", None, None);
        Ok(Vec::new())
    }

    fn capability_probe(&self) -> CapabilityProbe {
        let mut probe =
            CapabilityProbe::new(self.runtime_id.clone(), NOW_MS, EvidenceSource::Absent);
        probe.prove(Capability::HistoryResume);
        probe
    }
}

fn structured_factory(
    label: &'static str,
    calls: Arc<Mutex<Vec<RuntimeCall>>>,
) -> RuntimeBootstrapFactory {
    Box::new(move |context| {
        let mint = IdentityMint::new(&context.identity_salt);
        let mut runtime = HostTestRuntime::new(&mint, label, calls, DrainMode::Empty, false);
        runtime.host_id = mint.host_id("kaleido-host");
        Ok(RuntimeBootstrap {
            project_id: runtime.project_id.clone(),
            project_binding_id: runtime.project_binding_id.clone(),
            runtime_id: runtime.runtime_id.clone(),
            runtime: Box::new(runtime),
        })
    })
}

#[test]
fn structured_product_host_runs_two_provider_neutral_runtimes_and_closes_both() {
    let directory = tempfile::tempdir().expect("temporary host root");
    let project_root = directory.path().join("project");
    std::fs::create_dir(&project_root).expect("project root");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let host = StructuredLanHost::start(StructuredLanConfig {
        project_root,
        data_directory: directory.path().join("host"),
        bind_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        runtimes: vec![
            structured_factory("alpha", Arc::clone(&calls)),
            structured_factory("beta", Arc::clone(&calls)),
        ],
    })
    .expect("start two structured runtimes");

    assert_eq!(host.session_ids().len(), 2);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.operation == "start")
            .count(),
        2
    );
    host.run_for(Duration::from_millis(10));
    host.shutdown().expect("shutdown structured host");
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.operation == "close")
            .count(),
        2
    );
}

#[test]
fn structured_host_rolls_back_a_started_runtime_when_a_later_factory_fails() {
    let directory = tempfile::tempdir().expect("temporary host root");
    let project_root = directory.path().join("project");
    std::fs::create_dir(&project_root).expect("project root");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let result = StructuredLanHost::start(StructuredLanConfig {
        project_root,
        data_directory: directory.path().join("host"),
        bind_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        runtimes: vec![
            structured_factory("alpha", Arc::clone(&calls)),
            Box::new(|_| Err(CodexLanError::Runtime)),
        ],
    });

    assert!(matches!(result, Err(CodexLanError::Runtime)));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.operation == "start")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.operation == "close")
            .count(),
        1
    );
}

#[test]
fn discovery_is_committed_before_start_and_two_runtimes_do_not_cross_sessions() {
    let fixture = HostFixture::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime_a = HostTestRuntime::new(
        &fixture.mint,
        "a",
        Arc::clone(&calls),
        DrainMode::Empty,
        false,
    );
    let request_a = fixture.request(&runtime_a, "root-a");
    let expected_a = runtime_a.session.id.clone();
    let runtime_b = HostTestRuntime::new(
        &fixture.mint,
        "b",
        Arc::clone(&calls),
        DrainMode::Empty,
        false,
    );
    let request_b = fixture.request(&runtime_b, "root-b");
    let expected_b = runtime_b.session.id.clone();
    let supervisor = RuntimeSupervisor::new(fixture.broker.clone());

    let session_a = supervisor
        .start_runtime(request_a, Box::new(runtime_a))
        .expect("discovery dependencies are committed before session start");
    let session_b = supervisor
        .start_runtime(request_b, Box::new(runtime_b))
        .expect("second runtime has an independent bootstrap");
    assert_eq!(session_a, expected_a);
    assert_eq!(session_b, expected_b);
    assert_ne!(session_a, session_b);

    let command_a = fixture.admit_prompt(&session_a, "prompt-a", "body-a");
    let command_b = fixture.admit_prompt(&session_b, "prompt-b", "body-b");
    let dispatched = supervisor.dispatch_all_ready();
    assert_eq!(dispatched.len(), 2);
    assert!(dispatched.iter().all(|(_, result)| result.is_ok()));
    let reports = wait_for_dispatch_reports(&supervisor, 2);
    assert!(reports.iter().all(|report| report.result.is_ok()));

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.operation == "discover")
            .count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .find(|call| call.command_id.as_ref() == Some(&command_a))
            .map(|call| &call.runtime_id),
        Some(&fixture.mint.runtime_id("runtime-a"))
    );
    assert_eq!(
        calls
            .iter()
            .find(|call| call.command_id.as_ref() == Some(&command_b))
            .map(|call| &call.runtime_id),
        Some(&fixture.mint.runtime_id("runtime-b"))
    );
    drop(calls);
    supervisor.stop_session(&session_a).expect("stop runtime a");
    supervisor.stop_session(&session_b).expect("stop runtime b");
}

#[test]
fn interrupt_turn_is_routed_to_its_session_with_the_correlated_command_ack() {
    let fixture = HostFixture::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = HostTestRuntime::new(
        &fixture.mint,
        "interrupt",
        Arc::clone(&calls),
        DrainMode::Empty,
        true,
    );
    let expected_turn = runtime.initial_turn.as_ref().unwrap().id.clone();
    let request = fixture.request(&runtime, "root-interrupt");
    let supervisor = RuntimeSupervisor::new(fixture.broker.clone());
    let session_id = supervisor
        .start_runtime(request, Box::new(runtime))
        .expect("start interrupt runtime");
    let admission = fixture
        .broker
        .admit_device_command(
            &DeviceId::new("device-interrupt"),
            &DeviceCommandRequest {
                idempotency_key: "interrupt-command".to_owned(),
                ttl_ms: Some(30_000),
                body: Command::InterruptTurn {
                    session_id: session_id.clone(),
                    turn_id: expected_turn.clone(),
                },
            },
            NOW_MS + 5,
        )
        .expect("admit interrupt");
    let command_id = admission.ack.command_id.clone();
    supervisor
        .dispatch_ticket(&admission.dispatch_ticket.expect("interrupt route"))
        .expect("dispatch interrupt");
    let report = wait_for_dispatch_reports(&supervisor, 1)
        .pop()
        .expect("interrupt report");
    assert_eq!(report.command_id, command_id);
    assert_eq!(report.result, Ok(()));
    assert!(calls.lock().unwrap().iter().any(|call| {
        call.operation == "interrupt_turn"
            && call.command_id.as_ref() == Some(&command_id)
            && call.turn_id.as_ref() == Some(&expected_turn)
    }));
    supervisor.stop_session(&session_id).expect("stop runtime");
}

#[test]
fn connection_loss_reconnects_the_same_session_and_applies_recovery_effects() {
    let fixture = HostFixture::new();
    let log_dir = fixture.log_dir.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = HostTestRuntime::new(
        &fixture.mint,
        "reconnect",
        Arc::clone(&calls),
        DrainMode::DisconnectThenReconnect,
        false,
    );
    let request = fixture.request(&runtime, "root-reconnect");
    let supervisor = RuntimeSupervisor::new(fixture.broker.clone());
    let session_id = supervisor
        .start_runtime(request, Box::new(runtime))
        .expect("start reconnect runtime");
    supervisor
        .drain_session(&session_id)
        .expect("request structured drain");
    let loss_report = wait_for_lifecycle_report(&supervisor);
    assert_eq!(loss_report.session_id, session_id);
    assert_eq!(loss_report.stage, RuntimeLifecycleStage::Drain);
    assert_eq!(
        loss_report.result,
        Err(RuntimeSupervisorError::RuntimeFailed)
    );
    assert_eq!(
        loss_report.failure_class,
        Some(RuntimeFailureClass::ConnectionFault)
    );
    let reconnect_report = wait_for_lifecycle_report(&supervisor);
    assert_eq!(reconnect_report.session_id, session_id);
    assert_eq!(reconnect_report.stage, RuntimeLifecycleStage::Reconnect);
    assert_eq!(reconnect_report.result, Ok(()));
    assert_eq!(reconnect_report.failure_class, None);
    assert!(calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call.operation == "reconnect"));
    supervisor.stop_session(&session_id).expect("stop runtime");
    drop(supervisor);
    drop(fixture.broker);

    let state = CanonicalStore::load(&log_dir, ClockSource::Fixed { at_ms: NOW_MS + 30 })
        .expect("reload recovered state");
    assert_eq!(
        state
            .state()
            .session(&session_id)
            .and_then(|session| session.title.as_deref()),
        Some("reconnected")
    );
}

#[test]
fn a_discovered_session_resumes_through_its_runtime_actor_without_exposing_raw_ids() {
    let fixture = HostFixture::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = HostTestRuntime::new(
        &fixture.mint,
        "resume-discovered",
        Arc::clone(&calls),
        DrainMode::Empty,
        false,
    );
    let discovered_session_id = runtime.discovered_session.id.clone();
    let request = fixture.request(&runtime, "root-resume-discovered");
    let supervisor = RuntimeSupervisor::new(fixture.broker.clone());
    let primary_session_id = supervisor
        .start_runtime(request, Box::new(runtime))
        .expect("start runtime with structured discovery");
    let admission = fixture
        .broker
        .admit_device_command(
            &DeviceId::new("device-resume"),
            &DeviceCommandRequest {
                idempotency_key: "resume-discovered".to_owned(),
                ttl_ms: Some(30_000),
                body: Command::ResumeSession {
                    session_id: discovered_session_id.clone(),
                },
            },
            NOW_MS + 5,
        )
        .expect("admit supported resume");
    supervisor
        .dispatch_ticket(&admission.dispatch_ticket.expect("resume route"))
        .expect("route discovered session alias");
    let report = wait_for_dispatch_reports(&supervisor, 1)
        .pop()
        .expect("resume report");
    assert_eq!(report.result, Ok(()));
    assert!(calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call.operation == "resume_session"));
    supervisor
        .stop_session(&discovered_session_id)
        .expect("an alias stops its owning runtime actor");
    drop(supervisor);
    drop(fixture.broker);
    let state = CanonicalStore::load(&fixture.log_dir, ClockSource::Fixed { at_ms: NOW_MS + 30 })
        .expect("reload resumed session");
    assert_eq!(
        state
            .state()
            .session(&discovered_session_id)
            .and_then(|session| session.title.as_deref()),
        Some("resumed-discovery")
    );
    assert_ne!(primary_session_id, discovered_session_id);
}

#[test]
fn drain_protocol_and_broker_apply_failures_are_classified_instead_of_swallowed() {
    let fixture = HostFixture::new();
    let supervisor = RuntimeSupervisor::new(fixture.broker.clone());

    let protocol_runtime = HostTestRuntime::new(
        &fixture.mint,
        "protocol-error",
        Arc::new(Mutex::new(Vec::new())),
        DrainMode::ProtocolError,
        false,
    );
    let protocol_request = fixture.request(&protocol_runtime, "root-protocol-error");
    let protocol_session = supervisor
        .start_runtime(protocol_request, Box::new(protocol_runtime))
        .expect("start protocol error runtime");
    supervisor
        .drain_session(&protocol_session)
        .expect("request protocol-error drain");
    let protocol_report = wait_for_lifecycle_report(&supervisor);
    assert_eq!(protocol_report.stage, RuntimeLifecycleStage::Drain);
    assert_eq!(
        protocol_report.result,
        Err(RuntimeSupervisorError::RuntimeFailed)
    );
    assert_eq!(
        protocol_report.failure_class,
        Some(RuntimeFailureClass::ProtocolViolation)
    );
    let recovery_report = wait_for_lifecycle_report(&supervisor);
    assert_eq!(recovery_report.stage, RuntimeLifecycleStage::Reconnect);
    assert_eq!(recovery_report.result, Ok(()));
    assert_eq!(recovery_report.failure_class, None);

    let invalid_runtime = HostTestRuntime::new(
        &fixture.mint,
        "invalid-effect",
        Arc::new(Mutex::new(Vec::new())),
        DrainMode::InvalidEffect,
        false,
    );
    let invalid_request = fixture.request(&invalid_runtime, "root-invalid-effect");
    let invalid_session = supervisor
        .start_runtime(invalid_request, Box::new(invalid_runtime))
        .expect("start invalid-effect runtime");
    supervisor
        .drain_session(&invalid_session)
        .expect("request invalid-effect drain");
    let apply_report = wait_for_lifecycle_report(&supervisor);
    assert_eq!(apply_report.stage, RuntimeLifecycleStage::ApplyDrain);
    assert_eq!(
        apply_report.result,
        Err(RuntimeSupervisorError::BrokerRejected)
    );
    assert_eq!(apply_report.failure_class, None);
    supervisor
        .stop_session(&protocol_session)
        .expect("stop protocol runtime");
    supervisor
        .stop_session(&invalid_session)
        .expect("stop invalid runtime");
}

#[test]
fn broker_queue_stays_pending_without_an_explicit_provider_delivery_receipt() {
    let fixture = HostFixture::new();
    let log_dir = fixture.log_dir.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = HostTestRuntime::new(
        &fixture.mint,
        "queue",
        Arc::clone(&calls),
        DrainMode::Empty,
        false,
    );
    let request = fixture.request(&runtime, "root-queue");
    let supervisor = RuntimeSupervisor::new(fixture.broker.clone());
    let session_id = supervisor
        .start_runtime(request, Box::new(runtime))
        .expect("start queue runtime");
    let body = fixture.write_body("queue", b"queued body");
    let admission = fixture
        .broker
        .admit_device_command(
            &DeviceId::new("device-queue"),
            &DeviceCommandRequest {
                idempotency_key: "queue-without-receipt".to_owned(),
                ttl_ms: None,
                body: Command::EnqueueInput {
                    session_id: session_id.clone(),
                    body,
                    intent: QueueIntent::SteerActiveTurn,
                },
            },
            NOW_MS + 5,
        )
        .expect("admit broker-local queue entry");
    assert!(matches!(
        admission.ack.outcome,
        CommandOutcome::Enqueued { .. }
    ));
    assert!(admission.dispatch_ticket.is_none());
    assert!(supervisor.dispatch_all_ready().is_empty());
    assert!(!calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call.operation == "submit_prompt"));
    supervisor
        .stop_session(&session_id)
        .expect("stop queue runtime");
    drop(supervisor);
    drop(fixture.broker);

    let store = CanonicalStore::load(&log_dir, ClockSource::Fixed { at_ms: NOW_MS + 30 })
        .expect("reload queue state");
    let entries = store.state().queue_of(&session_id);
    assert_eq!(entries.len(), 1);
    let entry = entries.first().expect("one queue entry");
    assert_eq!(entry.state, QueueState::Pending);
    assert!(entry.editable);
}

#[test]
fn idle_new_turn_queue_advances_only_after_a_structured_provider_receipt() -> Result<(), String> {
    let fixture = HostFixture::new();
    let log_dir = fixture.log_dir.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = HostTestRuntime::new(
        &fixture.mint,
        "queue-new-turn",
        Arc::clone(&calls),
        DrainMode::Empty,
        false,
    );
    let request = fixture.request(&runtime, "root-queue-new-turn");
    let supervisor = RuntimeSupervisor::new(fixture.broker.clone());
    let session_id = supervisor
        .start_runtime(request, Box::new(runtime))
        .expect("start queue runtime");
    let body = fixture.write_body("queue-new-turn", b"queued new turn");
    let admission = fixture
        .broker
        .admit_device_command(
            &DeviceId::new("device-queue-new-turn"),
            &DeviceCommandRequest {
                idempotency_key: "queue-new-turn".to_owned(),
                ttl_ms: None,
                body: Command::EnqueueInput {
                    session_id: session_id.clone(),
                    body,
                    intent: QueueIntent::NewTurn,
                },
            },
            NOW_MS + 5,
        )
        .expect("admit new-turn queue entry");
    let entry_id = match admission.ack.outcome {
        CommandOutcome::Enqueued { entry_id } => entry_id,
        other => return Err(format!("expected queue admission, found {other:?}")),
    };
    let pumped = supervisor.pump_pending_queue();
    assert_eq!(pumped, vec![(entry_id.clone(), Ok(()))]);
    supervisor
        .stop_session(&session_id)
        .expect("drain queued delivery before stop");
    assert!(calls.lock().unwrap().iter().any(|call| {
        call.operation == "deliver_queue"
            && call.command_id.as_ref() == Some(&admission.ack.command_id)
    }));
    drop(supervisor);
    drop(fixture.broker);
    let store = CanonicalStore::load(&log_dir, ClockSource::Fixed { at_ms: NOW_MS + 30 })
        .expect("reload delivered queue state");
    let entry = store
        .state()
        .queue_entry(&entry_id)
        .expect("delivered queue entry");
    let QueueState::DeliveredAsNewTurn { turn_id, .. } = &entry.state else {
        return Err("structured receipt must advance the queue".to_owned());
    };
    assert!(store.state().turns_of(&session_id).iter().any(|turn| {
        &turn.id == turn_id
            && matches!(
                &turn.origin,
                TurnOrigin::RemoteCommand { command_id }
                    if command_id == &admission.ack.command_id
            )
    }));
    Ok(())
}

struct HostFixture {
    _directory: tempfile::TempDir,
    log_dir: std::path::PathBuf,
    broker: Broker,
    mint: IdentityMint,
}

impl HostFixture {
    fn new() -> Self {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let directory = tempfile::tempdir().expect("temporary directory");
        let log_dir = directory.path().join("canonical");
        let broker = Broker::open(
            &log_dir,
            ClockSource::Fixed { at_ms: NOW_MS },
            "r5-test-salt",
            "r5-host",
        )
        .expect("open broker");
        Self {
            _directory: directory,
            log_dir,
            broker,
            mint: IdentityMint::new("r5-test-salt"),
        }
    }

    fn request(&self, runtime: &HostTestRuntime, label: &str) -> SessionStartRequest {
        let root_ref = self
            .broker
            .content_store()
            .store(
                ContentKind::FilePath,
                Sensitivity::Sensitive,
                label.as_bytes(),
            )
            .expect("store root");
        runtime.request(root_ref)
    }

    fn write_body(&self, key: &str, bytes: &[u8]) -> ContentRef {
        let device_id = DeviceId::new(format!("device-{key}"));
        let response = self
            .broker
            .write_content(
                &device_id,
                &ContentWriteRequest {
                    content_kind: ContentKind::PlainText,
                    byte_len: u64::try_from(bytes.len()).expect("body length"),
                    digest: format!("sha256:{:x}", Sha256::digest(bytes)),
                },
                bytes,
                NOW_MS + 1,
            )
            .expect("write body");
        match response {
            ContentWriteResponse::Stored { content_ref } => Some(content_ref),
            ContentWriteResponse::Rejected { .. } => None,
        }
        .expect("body is stored")
    }

    fn admit_prompt(&self, session_id: &SessionId, key: &str, bytes: &str) -> CommandId {
        let device_id = DeviceId::new(format!("device-{key}"));
        let body = self.write_body(key, bytes.as_bytes());
        self.broker
            .admit_device_command(
                &device_id,
                &DeviceCommandRequest {
                    idempotency_key: key.to_owned(),
                    ttl_ms: Some(30_000),
                    body: Command::SubmitPrompt {
                        session_id: session_id.clone(),
                        body,
                    },
                },
                NOW_MS + 2,
            )
            .expect("admit prompt")
            .ack
            .command_id
    }
}

fn binding(
    runtime_id: &ProviderRuntimeId,
    kind: ProviderBindingKind,
    suffix: &str,
) -> ProviderBindingHandle {
    ProviderBindingHandle {
        id: ProviderBindingId::new(format!("bnd_{suffix}")),
        runtime_id: runtime_id.clone(),
        kind,
    }
}

fn evidence() -> kaleido_proto::capability::CapabilityEvidence {
    kaleido_proto::capability::CapabilityEvidence {
        source: EvidenceSource::Absent,
        observed_at_ms: NOW_MS,
        note_ref: None,
    }
}

fn protocol_error(detail: &str) -> RuntimeSessionError {
    RuntimeSessionError::ProtocolViolation {
        detail: detail.to_owned(),
    }
}

fn wait_for_dispatch_reports(
    supervisor: &RuntimeSupervisor,
    count: usize,
) -> Vec<kaleido_hostd::RuntimeDispatchReport> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reports = Vec::new();
    while reports.len() < count {
        if let Some(report) = supervisor.try_report() {
            reports.push(report);
            continue;
        }
        assert!(Instant::now() < deadline, "runtime report timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    reports
}

fn wait_for_lifecycle_report(supervisor: &RuntimeSupervisor) -> RuntimeLifecycleReport {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(report) = supervisor.try_lifecycle_report() {
            return report;
        }
        assert!(Instant::now() < deadline, "lifecycle report timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
