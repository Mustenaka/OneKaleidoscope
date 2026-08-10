//! Minimal UniFFI façade for the R1 binding probe.
//!
//! This crate deliberately contains no session, provider, network or storage
//! logic. Its exported signatures use the canonical contract types directly,
//! which makes binding generation test the real API surface instead of a
//! second set of foreign-language DTOs.

use std::sync::{Arc, Mutex};

use kaleido_proto::command::{CommandAck, CommandEnvelope};
use kaleido_proto::effect::StateEffect;
use kaleido_proto::error::CanonicalError;
use kaleido_proto::projection::ProjectionEnvelope;

pub mod cache;
pub mod connection;
pub mod credential;
pub mod mobile;
pub mod product;
pub mod signer;

pub use credential::{PairedHostInfo, SecureCredentialVault, SecureCredentialVaultError};
pub use mobile::{MobileClient, MobileClientError, ProjectionCallback, ProjectionSubscription};
pub use product::{
    MobileActionAvailability, MobileActionBlocker, MobileQuestionAnswer, MobileSessionAction,
    MobileTextContent,
};
pub use signer::{DeviceSigner, DeviceSignerError};

uniffi::setup_scaffolding!();

/// Returns the protocol version implemented by the canonical contract.
#[uniffi::export]
pub fn protocol_version() -> String {
    kaleido_proto::PROTOCOL_VERSION.to_owned()
}

/// Exercises the real command, error, state-effect and projection graph.
///
/// The function has no product semantics: it returns the supplied projection
/// unchanged. The other arguments exist solely so both foreign-language
/// compilers must type-check records, data-carrying enums, `Option`, `Vec`, and
/// their nested canonical types.
#[uniffi::export]
pub fn binding_probe(
    _command: CommandEnvelope,
    projection: ProjectionEnvelope,
    _error: Option<CanonicalError>,
    _effects: Vec<StateEffect>,
) -> ProjectionEnvelope {
    projection
}

/// Receives canonical projection and error values from Rust.
///
/// This legacy callback-interface shape is intentional: T-102 probes the
/// exact callback mechanism that R3 would use for projection push.
#[uniffi::export(callback_interface)]
pub trait ProjectionProbeCallback: Send + Sync {
    fn on_projection(&self, projection: ProjectionEnvelope);

    fn on_error(&self, error: CanonicalError);
}

/// Stateful subscription-handle shape for foreign-language compilation.
///
/// It has no product semantics. `subscribe` calls the foreign implementation
/// once with each canonical value and retains it until `unsubscribe`.
#[derive(uniffi::Object)]
pub struct ProjectionSubscriptionProbe {
    callback: Mutex<Option<Box<dyn ProjectionProbeCallback>>>,
}

impl std::fmt::Debug for ProjectionSubscriptionProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectionSubscriptionProbe")
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl ProjectionSubscriptionProbe {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            callback: Mutex::new(None),
        })
    }

    pub fn subscribe(
        &self,
        callback: Box<dyn ProjectionProbeCallback>,
        projection: ProjectionEnvelope,
        error: CanonicalError,
    ) {
        callback.on_projection(projection);
        callback.on_error(error);

        let mut guard = match self.callback.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(callback);
    }

    pub fn unsubscribe(&self) {
        let mut guard = match self.callback.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = None;
    }
}

/// UniFFI requires thrown errors to be error types rather than records.
///
/// The payload remains the canonical contract type; this wrapper adds no
/// parallel fields or product semantics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum BindingProbeError {
    #[error("canonical binding probe error")]
    Canonical { error: CanonicalError },
}

/// Exercises foreign-language exception handling with a canonical payload.
#[allow(
    clippy::result_large_err,
    reason = "UniFFI error payloads are by-value enums, and T-102 requires the canonical error record"
)]
#[uniffi::export]
pub fn fallible_binding_probe(
    should_fail: bool,
    error: CanonicalError,
) -> Result<(), BindingProbeError> {
    if should_fail {
        Err(BindingProbeError::Canonical { error })
    } else {
        Ok(())
    }
}

/// Exercises the foreign async bridge with the canonical acknowledgement.
#[uniffi::export]
pub async fn async_binding_probe(ack: CommandAck) -> CommandAck {
    ack
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::task::{Context, Poll, Waker};

    use kaleido_proto::command::{CommandAck, CommandOutcome};
    use kaleido_proto::effect::Cursor;
    use kaleido_proto::error::ErrorCode;
    use kaleido_proto::host::HostReachability;
    use kaleido_proto::ids::{CommandId, HostId};
    use kaleido_proto::projection::{
        ProjectIndexView, ProjectionEnvelope, ProjectionKey, ProjectionPayload, PROJECTION_VERSION,
    };

    use super::{
        async_binding_probe, fallible_binding_probe, BindingProbeError, ProjectionProbeCallback,
        ProjectionSubscriptionProbe,
    };

    fn canonical_error() -> kaleido_proto::error::CanonicalError {
        kaleido_proto::error::CanonicalError {
            code: ErrorCode::Internal,
            retriable: false,
            detail_ref: None,
            at_ms: 7,
        }
    }

    fn projection() -> ProjectionEnvelope {
        let host_id = HostId::new("host-probe");
        ProjectionEnvelope {
            projection_version: PROJECTION_VERSION,
            key: ProjectionKey::ProjectIndex {
                host_id: host_id.clone(),
            },
            cursor: Cursor::START,
            payload: ProjectionPayload::ProjectIndex {
                view: ProjectIndexView {
                    host_id,
                    reachability: HostReachability::Offline,
                    groups: Vec::new(),
                },
            },
        }
    }

    struct RecordingCallback {
        projection_count: Arc<AtomicUsize>,
        error_count: Arc<AtomicUsize>,
    }

    impl ProjectionProbeCallback for RecordingCallback {
        fn on_projection(&self, _projection: ProjectionEnvelope) {
            self.projection_count.fetch_add(1, Ordering::SeqCst);
        }

        fn on_error(&self, _error: kaleido_proto::error::CanonicalError) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn has_callback(probe: &ProjectionSubscriptionProbe) -> bool {
        match probe.callback.lock() {
            Ok(guard) => guard.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        }
    }

    #[test]
    fn fallible_probe_preserves_the_canonical_error_payload() {
        let error = canonical_error();

        assert_eq!(
            fallible_binding_probe(true, error.clone()),
            Err(BindingProbeError::Canonical { error })
        );
    }

    #[test]
    fn fallible_probe_has_a_success_path() {
        assert_eq!(fallible_binding_probe(false, canonical_error()), Ok(()));
    }

    #[test]
    fn subscription_calls_and_retains_the_foreign_callback_until_unsubscribe() {
        let projection_count = Arc::new(AtomicUsize::new(0));
        let error_count = Arc::new(AtomicUsize::new(0));
        let probe = ProjectionSubscriptionProbe::new();

        probe.subscribe(
            Box::new(RecordingCallback {
                projection_count: Arc::clone(&projection_count),
                error_count: Arc::clone(&error_count),
            }),
            projection(),
            canonical_error(),
        );

        assert_eq!(projection_count.load(Ordering::SeqCst), 1);
        assert_eq!(error_count.load(Ordering::SeqCst), 1);
        assert!(has_callback(&probe));

        probe.unsubscribe();
        assert!(!has_callback(&probe));
    }

    #[test]
    fn async_probe_returns_the_canonical_ack() {
        let ack = CommandAck {
            command_id: CommandId::new("command-probe"),
            outcome: CommandOutcome::AcceptedLocally { note_ref: None },
            acked_at_ms: 11,
        };
        let mut future = Box::pin(async_binding_probe(ack.clone()));
        let mut context = Context::from_waker(Waker::noop());

        assert_eq!(
            Future::poll(future.as_mut(), &mut context),
            Poll::Ready(ack)
        );
    }
}
