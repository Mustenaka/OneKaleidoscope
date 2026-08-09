use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use kaleido_proto::ids::{DeviceId, HostId};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::auth::validate_p256_spki;
use crate::bootstrap::{validate_endpoint, PairingBootstrap};
use crate::control::PairRequest;
use crate::error::TransportError;
use crate::platform;
use crate::tls::SpkiPin;
use crate::PAIRING_LIFETIME_MS;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub device_id: DeviceId,
    public_key_spki: Vec<u8>,
    device_label: String,
    pub created_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

impl DeviceRecord {
    pub fn public_key_spki(&self) -> &[u8] {
        &self.public_key_spki
    }

    pub fn device_label(&self) -> &str {
        &self.device_label
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked_at_ms.is_some()
    }
}

impl fmt::Debug for DeviceRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceRecord")
            .field("device_id", &self.device_id)
            .field("public_key_spki", &"[redacted]")
            .field("device_label", &"[redacted]")
            .field("created_at_ms", &self.created_at_ms)
            .field("revoked_at_ms", &self.revoked_at_ms)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PairingRecord {
    digest: [u8; 32],
    expires_at_ms: i64,
    consumed_at_ms: Option<i64>,
}

impl fmt::Debug for PairingRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingRecord")
            .field("digest", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("consumed_at_ms", &self.consumed_at_ms)
            .finish()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pairings: Vec<PairingRecord>,
    devices: BTreeMap<String, DeviceRecord>,
}

impl fmt::Debug for RegistrySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistrySnapshot")
            .field("pairing_count", &self.pairings.len())
            .field("device_count", &self.devices.len())
            .finish()
    }
}

pub trait DurableBackend: fmt::Debug {
    fn load(&mut self) -> Result<RegistrySnapshot, TransportError>;
    fn commit(&mut self, snapshot: &RegistrySnapshot) -> Result<(), TransportError>;
}

#[derive(Debug, Default)]
pub struct MemoryBackend {
    committed: RegistrySnapshot,
}

impl DurableBackend for MemoryBackend {
    fn load(&mut self) -> Result<RegistrySnapshot, TransportError> {
        Ok(self.committed.clone())
    }

    fn commit(&mut self, snapshot: &RegistrySnapshot) -> Result<(), TransportError> {
        self.committed = snapshot.clone();
        Ok(())
    }
}

pub struct AtomicFileBackend {
    path: PathBuf,
}

impl fmt::Debug for AtomicFileBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AtomicFileBackend([redacted path])")
    }
}

impl AtomicFileBackend {
    pub fn new(path: PathBuf) -> Result<Self, TransportError> {
        let parent = path.parent().ok_or(TransportError::InsecurePermissions)?;
        platform::prepare_private_directory(parent)
            .map_err(|_| TransportError::InsecurePermissions)?;
        if path.exists() {
            platform::verify_private_path(&path)
                .map_err(|_| TransportError::InsecurePermissions)?;
        }
        Ok(Self { path })
    }

    fn read_snapshot(&self) -> Result<RegistrySnapshot, TransportError> {
        if !self.path.exists() {
            return Ok(RegistrySnapshot::default());
        }
        platform::verify_private_path(&self.path)
            .map_err(|_| TransportError::InsecurePermissions)?;
        let mut file = fs::File::open(&self.path).map_err(TransportError::from)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.read_to_end(&mut bytes).map_err(TransportError::from)?;
        serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidKeyMaterial)
    }

    fn write_snapshot(&self, snapshot: &RegistrySnapshot) -> Result<(), TransportError> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(snapshot).map_err(|_| TransportError::InvalidKeyMaterial)?,
        );
        let parent = self
            .path
            .parent()
            .ok_or(TransportError::InsecurePermissions)?;
        platform::prepare_private_directory(parent)
            .map_err(|_| TransportError::InsecurePermissions)?;
        if self.path.exists() {
            platform::verify_private_path(&self.path)
                .map_err(|_| TransportError::InsecurePermissions)?;
        }
        let temporary = temporary_path(parent);
        let mut file =
            platform::secure_private_file(&temporary, true).map_err(TransportError::from)?;
        let result = (|| {
            file.write_all(&bytes).map_err(TransportError::from)?;
            file.sync_all().map_err(TransportError::from)?;
            drop(file);
            platform::atomic_replace(&temporary, &self.path).map_err(TransportError::from)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

impl DurableBackend for AtomicFileBackend {
    fn load(&mut self) -> Result<RegistrySnapshot, TransportError> {
        self.read_snapshot()
    }

    fn commit(&mut self, snapshot: &RegistrySnapshot) -> Result<(), TransportError> {
        self.write_snapshot(snapshot)
    }
}

#[derive(Debug)]
pub struct SecurityStore<B: DurableBackend> {
    backend: B,
    snapshot: RegistrySnapshot,
}

#[derive(Clone)]
pub struct IssuePairing<'a> {
    pub host_id: &'a HostId,
    pub endpoint: &'a str,
    pub host_public_key_pin: &'a str,
    pub now_ms: i64,
}

impl fmt::Debug for IssuePairing<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuePairing")
            .field("host_id", &self.host_id)
            .field("endpoint", &"[redacted]")
            .field("host_public_key_pin", &"[redacted]")
            .field("now_ms", &self.now_ms)
            .finish()
    }
}

impl<B: DurableBackend> SecurityStore<B> {
    pub fn open(mut backend: B) -> Result<Self, TransportError> {
        let snapshot = backend.load()?;
        for device in snapshot.devices.values() {
            validate_p256_spki(&device.public_key_spki)?;
        }
        Ok(Self { backend, snapshot })
    }

    pub fn issue_pairing(
        &mut self,
        input: IssuePairing<'_>,
    ) -> Result<PairingBootstrap, TransportError> {
        if input.host_id.is_empty() {
            return Err(TransportError::MalformedFrame);
        }
        validate_endpoint(input.endpoint)?;
        SpkiPin::parse(input.host_public_key_pin)?;
        let expires_at_ms = input
            .now_ms
            .checked_add(PAIRING_LIFETIME_MS)
            .ok_or(TransportError::TimeOverflow)?;
        let mut secret = Zeroizing::new(vec![0_u8; 32]);
        OsRng.fill_bytes(&mut secret);
        let digest: [u8; 32] = Sha256::digest(&secret).into();
        if self
            .snapshot
            .pairings
            .iter()
            .any(|record| bool::from(record.digest.ct_eq(&digest)))
        {
            return Err(TransportError::Internal);
        }
        let mut next = self.snapshot.clone();
        next.pairings.push(PairingRecord {
            digest,
            expires_at_ms,
            consumed_at_ms: None,
        });
        self.backend.commit(&next)?;
        self.snapshot = next;
        Ok(PairingBootstrap {
            host_id: input.host_id.clone(),
            endpoint: input.endpoint.to_owned(),
            host_public_key_pin: input.host_public_key_pin.to_owned(),
            secret: secret.to_vec(),
            expires_at_ms,
        })
    }

    pub fn pair_device(
        &mut self,
        request: &PairRequest,
        now_ms: i64,
    ) -> Result<DeviceRecord, TransportError> {
        if request.request_id == 0 {
            return Err(TransportError::MalformedFrame);
        }
        if request.secret.len() != 32 {
            return Err(TransportError::PairingInvalid);
        }
        validate_p256_spki(&request.device_public_key_spki)
            .map_err(|_| TransportError::AuthenticationFailed)?;
        let label = request.device_label.trim();
        if !(1..=80).contains(&label.chars().count()) {
            return Err(TransportError::AuthenticationFailed);
        }
        let digest: [u8; 32] = Sha256::digest(&request.secret).into();
        let mut matching = None;
        for (index, record) in self.snapshot.pairings.iter().enumerate() {
            if bool::from(record.digest.ct_eq(&digest)) {
                matching = Some(index);
            }
        }
        let index = matching.ok_or(TransportError::PairingInvalid)?;
        let record = self
            .snapshot
            .pairings
            .get(index)
            .ok_or(TransportError::PairingInvalid)?;
        if record.consumed_at_ms.is_some() || now_ms >= record.expires_at_ms {
            return Err(TransportError::PairingInvalid);
        }
        let device_id = fresh_device_id(&self.snapshot.devices)?;
        let device = DeviceRecord {
            device_id: device_id.clone(),
            public_key_spki: request.device_public_key_spki.clone(),
            device_label: label.to_owned(),
            created_at_ms: now_ms,
            revoked_at_ms: None,
        };
        let mut next = self.snapshot.clone();
        let next_record = next
            .pairings
            .get_mut(index)
            .ok_or(TransportError::PairingInvalid)?;
        next_record.consumed_at_ms = Some(now_ms);
        next.devices.insert(device_id.value.clone(), device.clone());
        self.backend.commit(&next)?;
        self.snapshot = next;
        Ok(device)
    }

    pub fn device_for_auth(&self, device_id: &DeviceId) -> Result<&DeviceRecord, TransportError> {
        let device = self
            .snapshot
            .devices
            .get(device_id.as_str())
            .ok_or(TransportError::AuthenticationFailed)?;
        if device.is_revoked() {
            Err(TransportError::AuthenticationFailed)
        } else {
            Ok(device)
        }
    }

    pub fn revoke_and_then<F>(
        &mut self,
        device_id: &DeviceId,
        revoked_at_ms: i64,
        after_durable: F,
    ) -> Result<(), TransportError>
    where
        F: FnOnce(&DeviceId),
    {
        let current = self
            .snapshot
            .devices
            .get(device_id.as_str())
            .ok_or(TransportError::AuthenticationFailed)?;
        if current.revoked_at_ms.is_some() {
            return Err(TransportError::DeviceRevoked);
        }
        let mut next = self.snapshot.clone();
        let device = next
            .devices
            .get_mut(device_id.as_str())
            .ok_or(TransportError::AuthenticationFailed)?;
        device.revoked_at_ms = Some(revoked_at_ms);
        self.backend.commit(&next)?;
        self.snapshot = next;
        after_durable(device_id);
        Ok(())
    }

    pub fn device_count(&self) -> usize {
        self.snapshot.devices.len()
    }
}

fn fresh_device_id(existing: &BTreeMap<String, DeviceRecord>) -> Result<DeviceId, TransportError> {
    for _ in 0..8 {
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let value = format!("dev_{}", URL_SAFE_NO_PAD.encode(random));
        if !existing.contains_key(&value) {
            return Ok(DeviceId::new(value));
        }
    }
    Err(TransportError::Internal)
}

fn temporary_path(parent: &Path) -> PathBuf {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    parent.join(format!(".registry-{}.tmp", URL_SAFE_NO_PAD.encode(random)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::cell::Cell;
    use std::rc::Rc;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use kaleido_proto::ids::HostId;
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::EncodePublicKey;
    use rand_core::OsRng;

    use super::{
        AtomicFileBackend, DurableBackend, IssuePairing, MemoryBackend, RegistrySnapshot,
        SecurityStore,
    };
    use crate::control::PairRequest;
    use crate::error::TransportError;

    fn pin() -> String {
        format!("sha256:{}", URL_SAFE_NO_PAD.encode([1_u8; 32]))
    }

    fn request(secret: Vec<u8>) -> PairRequest {
        let signing_key = SigningKey::random(&mut OsRng);
        PairRequest {
            request_id: 1,
            secret,
            device_public_key_spki: p256::PublicKey::from(signing_key.verifying_key())
                .to_public_key_der()
                .expect("SPKI")
                .as_bytes()
                .to_vec(),
            device_label: " Phone ".to_owned(),
        }
    }

    #[test]
    fn pairing_is_atomic_one_time_and_all_secret_failures_are_uniform() {
        let mut store = SecurityStore::open(MemoryBackend::default()).expect("store");
        let bootstrap = store
            .issue_pairing(IssuePairing {
                host_id: &HostId::new("host"),
                endpoint: "host.local:443",
                host_public_key_pin: &pin(),
                now_ms: 1_000,
            })
            .expect("issue");
        let device = store
            .pair_device(&request(bootstrap.secret.clone()), 2_000)
            .expect("pair");
        assert_eq!(device.device_label(), "Phone");
        for invalid in [
            request(bootstrap.secret.clone()),
            request(vec![9_u8; 32]),
            request(vec![9_u8; 31]),
        ] {
            assert_eq!(
                store.pair_device(&invalid, 2_001),
                Err(TransportError::PairingInvalid)
            );
        }
    }

    #[test]
    fn expired_secret_is_pairing_invalid() {
        let mut store = SecurityStore::open(MemoryBackend::default()).expect("store");
        let bootstrap = store
            .issue_pairing(IssuePairing {
                host_id: &HostId::new("host"),
                endpoint: "host.local:443",
                host_public_key_pin: &pin(),
                now_ms: 0,
            })
            .expect("issue");
        assert_eq!(
            store.pair_device(&request(bootstrap.secret), bootstrap.expires_at_ms),
            Err(TransportError::PairingInvalid)
        );
    }

    #[test]
    fn revocation_callback_runs_only_after_durable_commit() {
        let mut store = SecurityStore::open(MemoryBackend::default()).expect("store");
        let bootstrap = store
            .issue_pairing(IssuePairing {
                host_id: &HostId::new("host"),
                endpoint: "host.local:443",
                host_public_key_pin: &pin(),
                now_ms: 0,
            })
            .expect("issue");
        let device = store
            .pair_device(&request(bootstrap.secret), 1)
            .expect("pair");
        let called = Rc::new(Cell::new(false));
        let marker = called.clone();
        store
            .revoke_and_then(&device.device_id, 2, move |_| marker.set(true))
            .expect("revoke");
        assert!(called.get());
        assert_eq!(
            store.device_for_auth(&device.device_id),
            Err(TransportError::AuthenticationFailed)
        );
    }

    #[derive(Debug)]
    struct FailingBackend {
        state: RegistrySnapshot,
        commits_before_failure: usize,
    }

    impl DurableBackend for FailingBackend {
        fn load(&mut self) -> Result<RegistrySnapshot, TransportError> {
            Ok(self.state.clone())
        }

        fn commit(&mut self, snapshot: &RegistrySnapshot) -> Result<(), TransportError> {
            if self.commits_before_failure == 0 {
                return Err(TransportError::Persistence);
            }
            self.commits_before_failure -= 1;
            self.state = snapshot.clone();
            Ok(())
        }
    }

    #[test]
    fn failed_pairing_transaction_does_not_consume_secret() {
        let backend = FailingBackend {
            state: RegistrySnapshot::default(),
            commits_before_failure: 1,
        };
        let mut store = SecurityStore::open(backend).expect("store");
        let bootstrap = store
            .issue_pairing(IssuePairing {
                host_id: &HostId::new("host"),
                endpoint: "host.local:443",
                host_public_key_pin: &pin(),
                now_ms: 0,
            })
            .expect("issue");
        assert!(matches!(
            store.pair_device(&request(bootstrap.secret), 1),
            Err(TransportError::Persistence)
        ));
        assert_eq!(store.device_count(), 0);
    }

    #[test]
    fn atomic_registry_survives_restart_and_durable_revocation() {
        let directory =
            std::env::temp_dir().join(format!("kaleido-transport-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let path = directory.join("registry.json");
        let device_id = {
            let backend = AtomicFileBackend::new(path.clone()).expect("file backend");
            let mut store = SecurityStore::open(backend).expect("store");
            let bootstrap = store
                .issue_pairing(IssuePairing {
                    host_id: &HostId::new("host"),
                    endpoint: "host.local:443",
                    host_public_key_pin: &pin(),
                    now_ms: 0,
                })
                .expect("issue");
            store
                .pair_device(&request(bootstrap.secret), 1)
                .expect("pair")
                .device_id
        };
        {
            let backend = AtomicFileBackend::new(path.clone()).expect("reopen backend");
            let mut store = SecurityStore::open(backend).expect("reopen store");
            store.device_for_auth(&device_id).expect("persisted device");
            store
                .revoke_and_then(&device_id, 2, |_| {})
                .expect("durable revoke");
        }
        let backend = AtomicFileBackend::new(path).expect("final backend");
        let store = SecurityStore::open(backend).expect("final store");
        assert_eq!(
            store.device_for_auth(&device_id),
            Err(TransportError::AuthenticationFailed)
        );
        std::fs::remove_dir_all(&directory).expect("cleanup");
    }
}
