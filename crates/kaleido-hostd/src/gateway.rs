//! Authenticated mobile ingress.
//!
//! The gateway is constructed only after pairing/challenge authentication and
//! injects that directory-backed [`DeviceId`] into every state operation.

use std::sync::{Arc, Mutex, MutexGuard};

use kaleido_proto::command::DeviceCommandRequest;
use kaleido_proto::content::{
    ContentReadRequest, ContentReadResponse, ContentWriteRequest, ContentWriteResponse,
};
use kaleido_proto::ids::DeviceId;
use kaleido_proto::projection::ProjectionSubscribe;
use kaleido_state::DeviceCommandAdmission;
use kaleido_transport::registry::{AtomicFileBackend, SecurityStore};

use crate::broker::{Broker, BrokerError, BrokerSubscription};

type Registry = SecurityStore<AtomicFileBackend>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GatewayError {
    #[error("the authenticated device has been revoked")]
    DeviceRevoked,
    #[error("the canonical broker rejected the operation")]
    Broker,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedGateway {
    device_id: DeviceId,
    broker: Broker,
    registry: Arc<Mutex<Registry>>,
}

impl AuthenticatedGateway {
    pub(crate) fn new(device_id: DeviceId, broker: Broker, registry: Arc<Mutex<Registry>>) -> Self {
        Self {
            device_id,
            broker,
            registry,
        }
    }

    pub(crate) fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    pub(crate) fn subscribe(
        &self,
        request: &ProjectionSubscribe,
        at_ms: i64,
    ) -> Result<BrokerSubscription, GatewayError> {
        let _registry = self.authorize()?;
        self.broker.subscribe(request, at_ms).map_err(map_broker)
    }

    pub(crate) fn write_content(
        &self,
        request: &ContentWriteRequest,
        body: &[u8],
        at_ms: i64,
    ) -> Result<ContentWriteResponse, GatewayError> {
        let _registry = self.authorize()?;
        self.broker
            .write_content(&self.device_id, request, body, at_ms)
            .map_err(map_broker)
    }

    pub(crate) fn read_content(
        &self,
        request: &ContentReadRequest,
        at_ms: i64,
    ) -> Result<ContentReadResponse, GatewayError> {
        let _registry = self.authorize()?;
        self.broker
            .read_content(&self.device_id, request, at_ms)
            .map_err(map_broker)
    }

    pub(crate) fn admit_command(
        &self,
        request: &DeviceCommandRequest,
        at_ms: i64,
    ) -> Result<DeviceCommandAdmission, GatewayError> {
        let _registry = self.authorize()?;
        self.broker
            .admit_device_command(&self.device_id, request, at_ms)
            .map_err(map_broker)
    }

    /// Hold the durable registry lock across the Broker operation, so a
    /// revocation orders entirely before or entirely after this request.
    fn authorize(&self) -> Result<MutexGuard<'_, Registry>, GatewayError> {
        let registry = lock(&self.registry);
        registry
            .device_for_auth(&self.device_id)
            .map_err(|_| GatewayError::DeviceRevoked)?;
        Ok(registry)
    }
}

fn map_broker(_error: BrokerError) -> GatewayError {
    GatewayError::Broker
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::{Arc, Mutex};

    use kaleido_proto::projection::{ProjectionKey, ProjectionSubscribe};
    use kaleido_state::ClockSource;
    use kaleido_transport::control::PairRequest;
    use kaleido_transport::registry::{AtomicFileBackend, IssuePairing, SecurityStore};
    use kaleido_transport::tls::TlsIdentityStore;
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::EncodePublicKey;
    use rand_core::OsRng;

    use super::{AuthenticatedGateway, GatewayError};
    use crate::Broker;

    #[test]
    fn durable_revocation_is_rechecked_under_the_registry_lock() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let broker = Broker::open(
            directory.path().join("canonical"),
            ClockSource::Fixed { at_ms: 1 },
            "gateway-test",
            "gateway-host",
        )
        .expect("broker");
        let security = directory.path().join("security");
        TlsIdentityStore::new(security.join("tls.json"))
            .expect("TLS store")
            .load_or_generate()
            .expect("prepare private security directory");
        let backend = AtomicFileBackend::new(security.join("registry.json")).expect("backend");
        let mut registry = SecurityStore::open(backend).expect("registry");
        let bootstrap = registry
            .issue_pairing(IssuePairing {
                host_id: &broker.host_id(),
                endpoint: "127.0.0.1:1",
                host_public_key_pin: &format!("sha256:{}", "A".repeat(43)),
                now_ms: 1,
            })
            .expect("pairing");
        let signing_key = SigningKey::random(&mut OsRng);
        let public_key = p256::PublicKey::from(signing_key.verifying_key())
            .to_public_key_der()
            .expect("SPKI")
            .as_bytes()
            .to_vec();
        let device = registry
            .pair_device(
                &PairRequest {
                    request_id: 1,
                    secret: bootstrap.secret,
                    device_public_key_spki: public_key,
                    device_label: "gateway phone".to_owned(),
                },
                2,
            )
            .expect("device");
        let registry = Arc::new(Mutex::new(registry));
        let gateway = AuthenticatedGateway::new(
            device.device_id.clone(),
            broker.clone(),
            Arc::clone(&registry),
        );
        super::lock(&registry)
            .revoke_and_then(&device.device_id, 3, |_| {})
            .expect("durable revoke without live cache");
        let result = gateway.subscribe(
            &ProjectionSubscribe {
                key: ProjectionKey::ProjectIndex {
                    host_id: broker.host_id(),
                },
                since: None,
            },
            4,
        );
        assert!(matches!(result, Err(GatewayError::DeviceRevoked)));
    }
}
