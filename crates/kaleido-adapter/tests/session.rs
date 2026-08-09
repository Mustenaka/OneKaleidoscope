#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! The provider-neutral session trait, exercised through a stand-in adapter.
//!
//! The point is not to test a fake. It is to prove the trait can express a
//! session lifecycle without a provider concept leaking into its signature, and
//! that a connection fault is reportable without being confused for a refused
//! approval — rule R-P8 keeps those two apart.

use std::collections::BTreeMap;

use kaleido_adapter::capability::CapabilityProbe;
use kaleido_adapter::content::{ContentAccess, ContentAccessError};
use kaleido_adapter::{
    IdentityMint, ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest,
};
use kaleido_proto::attention::AttentionResponse;
use kaleido_proto::capability::{Capability, EvidenceSource};
use kaleido_proto::content::{ContentAvailability, ContentKind, ContentRef, Sensitivity};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::host::ConnectionFaultReason;
use kaleido_proto::ids::{CommandId, ContentId};
use kaleido_proto::session::SessionStatus;

#[derive(Debug, Default)]
struct MemoryContent {
    bodies: BTreeMap<String, Vec<u8>>,
}

impl ContentAccess for MemoryContent {
    fn store(
        &mut self,
        kind: ContentKind,
        sensitivity: Sensitivity,
        bytes: &[u8],
    ) -> Result<ContentRef, ContentAccessError> {
        let key = format!(
            "{:016x}{}",
            bytes.len(),
            bytes.iter().map(|b| u64::from(*b)).sum::<u64>()
        );
        self.bodies.insert(key.clone(), bytes.to_vec());
        let reference = ContentRef {
            content_id: ContentId::new(key.clone()),
            kind,
            byte_len: bytes.len() as u64,
            digest: format!("sha256:{:0>64}", key),
            preview: None,
            sensitivity,
            availability: ContentAvailability::Stored,
        };
        reference.validate()?;
        Ok(reference)
    }

    fn load(&self, reference: &ContentRef) -> Result<Vec<u8>, ContentAccessError> {
        self.bodies
            .get(reference.content_id.as_str())
            .cloned()
            .ok_or_else(|| ContentAccessError::Missing {
                content_id: reference.content_id.clone(),
            })
    }
}

/// A stand-in runtime that can be told to drop its connection.
#[derive(Debug)]
struct StandInSession {
    probe: CapabilityProbe,
    started: bool,
    connected: bool,
    session_id: kaleido_proto::ids::SessionId,
}

impl StandInSession {
    fn new(mint: &IdentityMint) -> Self {
        let runtime_id = mint.runtime_id("stand-in");
        let mut probe = CapabilityProbe::new(runtime_id, 1_000, EvidenceSource::ObservedInTraffic);
        probe.prove(Capability::TurnPrompt);
        Self {
            probe,
            started: false,
            connected: true,
            session_id: mint.session_id("stand-in"),
        }
    }

    fn drop_connection(&mut self) {
        self.connected = false;
    }
}

impl ProviderRuntimeSession for StandInSession {
    fn start(
        &mut self,
        _request: &SessionStartRequest,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if self.started {
            return Err(RuntimeSessionError::AlreadyStarted);
        }
        self.started = true;
        Ok(vec![StateEffect::SessionStatusChanged {
            session_id: self.session_id.clone(),
            status: SessionStatus::Idle,
        }])
    }

    fn submit_prompt(
        &mut self,
        _command_id: &CommandId,
        _body: &ContentRef,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if !self.connected {
            return Err(RuntimeSessionError::ConnectionFault {
                reason: ConnectionFaultReason::ProcessExited { exit_code: Some(1) },
            });
        }
        Ok(vec![StateEffect::SessionStatusChanged {
            session_id: self.session_id.clone(),
            status: SessionStatus::Running,
        }])
    }

    fn respond_attention(
        &mut self,
        _response: &AttentionResponse,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        if !self.connected {
            return Err(RuntimeSessionError::NotConnected);
        }
        Ok(Vec::new())
    }

    fn drain_effects(
        &mut self,
        _content: &mut dyn ContentAccess,
    ) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        Ok(Vec::new())
    }

    fn close(&mut self) -> Result<Vec<StateEffect>, RuntimeSessionError> {
        self.connected = false;
        Ok(vec![StateEffect::SessionStatusChanged {
            session_id: self.session_id.clone(),
            status: SessionStatus::Completed,
        }])
    }

    fn capability_probe(&self) -> CapabilityProbe {
        self.probe.clone()
    }
}

fn request(mint: &IdentityMint, content: &mut dyn ContentAccess) -> SessionStartRequest {
    let project_root_ref = content
        .store(
            ContentKind::FilePath,
            Sensitivity::Sensitive,
            b"/projects/slice",
        )
        .expect("store the project root");
    SessionStartRequest {
        project_id: mint.project_id("slice"),
        project_binding_id: mint.project_binding_id("slice"),
        runtime_id: mint.runtime_id("stand-in"),
        project_root_ref,
    }
}

#[test]
fn a_session_can_be_driven_entirely_through_the_neutral_trait() {
    let mint = IdentityMint::new("test");
    let mut content = MemoryContent::default();
    let mut session = StandInSession::new(&mint);
    let start = request(&mint, &mut content);

    let effects = session.start(&start, &mut content).expect("start");
    assert_eq!(effects.len(), 1);
    let command_id = CommandId::new("cmd_submit_prompt");
    assert!(session
        .submit_prompt(&command_id, &start.project_root_ref, &mut content)
        .is_ok());
    assert!(session.drain_effects(&mut content).is_ok());
    assert!(!session.close().expect("close").is_empty());
}

#[test]
fn starting_twice_is_refused_rather_than_silently_ignored() {
    let mint = IdentityMint::new("test");
    let mut content = MemoryContent::default();
    let mut session = StandInSession::new(&mint);
    let start = request(&mint, &mut content);
    session.start(&start, &mut content).expect("first start");
    let error = session
        .start(&start, &mut content)
        .expect_err("a second start must be refused");
    assert!(matches!(error, RuntimeSessionError::AlreadyStarted));
    assert!(!error.ends_the_connection());
}

#[test]
fn a_lost_connection_is_reported_as_a_connection_fault() {
    let mint = IdentityMint::new("test");
    let mut content = MemoryContent::default();
    let mut session = StandInSession::new(&mint);
    let start = request(&mint, &mut content);
    session.start(&start, &mut content).expect("start");
    session.drop_connection();
    let command_id = CommandId::new("cmd_submit_prompt");

    let error = session
        .submit_prompt(&command_id, &start.project_root_ref, &mut content)
        .expect_err("a dead runtime must be reported");
    assert!(matches!(
        error,
        RuntimeSessionError::ConnectionFault {
            reason: ConnectionFaultReason::ProcessExited { .. }
        }
    ));
    assert!(error.ends_the_connection());
}

#[test]
fn an_unprobed_capability_is_never_reported_as_supported() {
    let mint = IdentityMint::new("test");
    let session = StandInSession::new(&mint);
    let capabilities = session.capability_probe().to_capabilities();
    assert!(capabilities.permits(&Capability::TurnPrompt));
    assert!(
        !capabilities.permits(&Capability::TurnSteer),
        "steering was never demonstrated, so it cannot be offered"
    );
}
