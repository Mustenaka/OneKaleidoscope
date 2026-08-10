//! Paired-host credentials and the platform-encrypted persistence boundary.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kaleido_proto::ids::{DeviceId, HostId};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

const CREDENTIAL_FILE: &str = "paired-host.json";
const SECURE_CREDENTIAL_FORMAT_VERSION: u32 = 2;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteAccess {
    pub route_id: String,
    pub route_hint: String,
    pub device_slot_id: String,
    pub access_token: String,
    pub host_endpoint_id: String,
    pub relay_url: String,
    pub service_endpoint: String,
    pub service_public_key_pin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_push: Option<PendingPushOperation>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PendingPushOperation {
    Replace {
        operation_id: String,
        opaque_address: String,
        registered_at_ms: i64,
        expires_at_ms: i64,
    },
    Delete {
        operation_id: String,
    },
}

impl std::fmt::Debug for PendingPushOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Replace { .. } => "PendingPushOperation::Replace([redacted])",
            Self::Delete { .. } => "PendingPushOperation::Delete([redacted])",
        })
    }
}

impl Drop for PendingPushOperation {
    fn drop(&mut self) {
        if let Self::Replace { opaque_address, .. } = self {
            opaque_address.zeroize();
        }
    }
}

impl std::fmt::Debug for RemoteAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteAccess([redacted])")
    }
}

impl Drop for RemoteAccess {
    fn drop(&mut self) {
        self.access_token.zeroize();
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedHost {
    pub host_id: HostId,
    pub device_id: DeviceId,
    pub endpoint: String,
    pub host_public_key_pin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteAccess>,
}

/// The only paired-host identity mobile UI needs to construct host-scoped
/// projection keys. Endpoint and pin remain inside Rust's secure credential
/// path and are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PairedHostInfo {
    pub host_id: HostId,
    pub device_id: DeviceId,
}

impl From<&PairedHost> for PairedHostInfo {
    fn from(host: &PairedHost) -> Self {
        Self {
            host_id: host.host_id.clone(),
            device_id: host.device_id.clone(),
        }
    }
}

impl std::fmt::Debug for PairedHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairedHost")
            .field("host_id", &self.host_id)
            .field("device_id", &self.device_id)
            .field("endpoint", &"[redacted]")
            .field("host_public_key_pin", &"[redacted]")
            .field("remote", &self.remote.as_ref().map(|_| "[configured]"))
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("paired-host metadata I/O failed")]
    Io,
    #[error("paired-host metadata is malformed")]
    Malformed,
}

/// Failures a platform vault may report without exposing provider text or
/// credential bytes across the FFI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum SecureCredentialVaultError {
    #[error("the secure credential vault is unavailable")]
    Unavailable,

    #[error("the secure credential vault contains malformed data")]
    Corrupt,
}

/// Closed storage boundary for paired-host credentials on mobile platforms.
///
/// Rust owns the serialized format and all validation. Platform code treats
/// these bytes as opaque and only persists them in encrypted storage.
#[uniffi::export(callback_interface)]
pub trait SecureCredentialVault: Send + Sync {
    fn load_paired_host(&self) -> Result<Option<Vec<u8>>, SecureCredentialVaultError>;

    fn store_paired_host(&self, credential: Vec<u8>) -> Result<(), SecureCredentialVaultError>;
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecureCredentialEnvelope {
    format_version: u32,
    paired_host: PairedHost,
}

enum CredentialBackend {
    Filesystem {
        root: PathBuf,
    },
    SecureVault {
        vault: Arc<dyn SecureCredentialVault>,
    },
}

pub struct CredentialStore {
    backend: CredentialBackend,
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialStore([redacted backend])")
    }
}

impl CredentialStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CredentialError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|_| CredentialError::Io)?;
        Ok(Self {
            backend: CredentialBackend::Filesystem { root },
        })
    }

    pub fn secure(vault: Arc<dyn SecureCredentialVault>) -> Self {
        Self {
            backend: CredentialBackend::SecureVault { vault },
        }
    }

    pub fn load(&self) -> Result<Option<PairedHost>, CredentialError> {
        match &self.backend {
            CredentialBackend::Filesystem { root } => load_file(root),
            CredentialBackend::SecureVault { vault } => {
                let bytes = vault.load_paired_host().map_err(map_vault_error)?;
                let Some(bytes) = bytes else {
                    return Ok(None);
                };
                let envelope = serde_json::from_slice::<SecureCredentialEnvelope>(&bytes)
                    .map_err(|_| CredentialError::Malformed)?;
                // #[allow(kaleido::version_branch)] reason: secure credential storage must reject incompatible durable records and never selects a product capability
                if !matches!(
                    envelope.format_version,
                    1 | SECURE_CREDENTIAL_FORMAT_VERSION
                ) || (envelope.format_version == 1 && envelope.paired_host.remote.is_some())
                {
                    return Err(CredentialError::Malformed);
                }
                validate(&envelope.paired_host)?;
                Ok(Some(envelope.paired_host))
            }
        }
    }

    pub fn store(&self, host: &PairedHost) -> Result<(), CredentialError> {
        validate(host)?;
        match &self.backend {
            CredentialBackend::Filesystem { root } => store_file(root, host),
            CredentialBackend::SecureVault { vault } => {
                let encoded = serde_json::to_vec(&SecureCredentialEnvelope {
                    format_version: SECURE_CREDENTIAL_FORMAT_VERSION,
                    paired_host: host.clone(),
                })
                .map_err(|_| CredentialError::Malformed)?;
                vault.store_paired_host(encoded).map_err(map_vault_error)
            }
        }
    }
}

fn load_file(root: &Path) -> Result<Option<PairedHost>, CredentialError> {
    let path = root.join(CREDENTIAL_FILE);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CredentialError::Io),
    };
    let host =
        serde_json::from_slice::<PairedHost>(&bytes).map_err(|_| CredentialError::Malformed)?;
    validate(&host)?;
    Ok(Some(host))
}

fn store_file(root: &Path, host: &PairedHost) -> Result<(), CredentialError> {
    let encoded = serde_json::to_vec(host).map_err(|_| CredentialError::Malformed)?;
    let target = root.join(CREDENTIAL_FILE);
    let temporary = root.join(format!("{CREDENTIAL_FILE}.tmp"));
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|_| CredentialError::Io)?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| CredentialError::Io)?;
    if file
        .write_all(&encoded)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        drop(fs::remove_file(&temporary));
        return Err(CredentialError::Io);
    }
    drop(file);
    if target.exists() {
        // This fallback is retained only for native Rust integration tests.
        // Mobile constructors never select filesystem credential storage.
        fs::remove_file(&target).map_err(|_| CredentialError::Io)?;
    }
    if fs::rename(&temporary, &target).is_err() {
        drop(fs::remove_file(&temporary));
        return Err(CredentialError::Io);
    }
    Ok(())
}

fn map_vault_error(error: SecureCredentialVaultError) -> CredentialError {
    match error {
        SecureCredentialVaultError::Unavailable => CredentialError::Io,
        SecureCredentialVaultError::Corrupt => CredentialError::Malformed,
    }
}

fn validate(host: &PairedHost) -> Result<(), CredentialError> {
    kaleido_transport::bootstrap::validate_endpoint(&host.endpoint)
        .map_err(|_| CredentialError::Malformed)?;
    kaleido_transport::tls::SpkiPin::parse(&host.host_public_key_pin)
        .map_err(|_| CredentialError::Malformed)?;
    if host.host_id.is_empty() || host.device_id.is_empty() {
        return Err(CredentialError::Malformed);
    }
    if let Some(remote) = &host.remote {
        kaleido_transport::remote::validate_remote_bootstrap(
            &kaleido_transport::remote::RemotePairingBootstrap {
                route_id: remote.route_id.clone(),
                route_hint: remote.route_hint.clone(),
                device_slot_id: remote.device_slot_id.clone(),
                access_token: remote.access_token.clone(),
                host_endpoint_id: remote.host_endpoint_id.clone(),
                relay_url: remote.relay_url.clone(),
                service_endpoint: remote.service_endpoint.clone(),
                service_public_key_pin: remote.service_public_key_pin.clone(),
                expires_at_ms: 0,
            },
        )
        .map_err(|_| CredentialError::Malformed)?;
        if let Some(pending) = &remote.pending_push {
            match pending {
                PendingPushOperation::Replace {
                    operation_id,
                    opaque_address,
                    registered_at_ms,
                    expires_at_ms,
                } => {
                    kaleido_transport::remote::validate_random_id(operation_id)
                        .map_err(|_| CredentialError::Malformed)?;
                    kaleido_transport::remote::PushAddress {
                        provider: kaleido_transport::remote::PushProvider::FcmFid,
                        opaque_address: opaque_address.clone(),
                        registered_at_ms: *registered_at_ms,
                        expires_at_ms: *expires_at_ms,
                    }
                    .validate()
                    .map_err(|_| CredentialError::Malformed)?;
                }
                PendingPushOperation::Delete { operation_id } => {
                    kaleido_transport::remote::validate_random_id(operation_id)
                        .map_err(|_| CredentialError::Malformed)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::{Arc, Mutex};

    use kaleido_proto::ids::{DeviceId, HostId};

    use super::{
        CredentialError, CredentialStore, PairedHost, SecureCredentialVault,
        SecureCredentialVaultError,
    };

    #[derive(Default)]
    struct MemoryVault {
        bytes: Mutex<Option<Vec<u8>>>,
    }

    impl SecureCredentialVault for MemoryVault {
        fn load_paired_host(&self) -> Result<Option<Vec<u8>>, SecureCredentialVaultError> {
            Ok(self.bytes.lock().expect("vault lock").clone())
        }

        fn store_paired_host(&self, credential: Vec<u8>) -> Result<(), SecureCredentialVaultError> {
            *self.bytes.lock().expect("vault lock") = Some(credential);
            Ok(())
        }
    }

    fn paired_host(host: &str, device: &str) -> PairedHost {
        PairedHost {
            host_id: HostId::new(host),
            device_id: DeviceId::new(device),
            endpoint: "127.0.0.1:7443".to_owned(),
            host_public_key_pin: format!("sha256:{}", "A".repeat(43)),
            remote: None,
        }
    }

    #[test]
    fn paired_metadata_round_trips_without_any_pairing_secret() {
        let directory = tempfile::tempdir().expect("directory");
        let store = CredentialStore::open(directory.path()).expect("store");
        let host = paired_host("host-a", "device-a");
        store.store(&host).expect("write");
        assert_eq!(store.load().expect("load"), Some(host));
        let bytes = std::fs::read(directory.path().join("paired-host.json")).expect("read");
        assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
    }

    #[test]
    fn a_completed_repair_replaces_the_previous_host_metadata() {
        let directory = tempfile::tempdir().expect("directory");
        let store = CredentialStore::open(directory.path()).expect("store");
        store
            .store(&paired_host("host-a", "device-a"))
            .expect("first pairing");
        let replacement = PairedHost {
            endpoint: "host-b.local:7554".to_owned(),
            ..paired_host("host-b", "device-b")
        };
        store.store(&replacement).expect("replacement pairing");
        assert_eq!(store.load().expect("load replacement"), Some(replacement));
    }

    #[test]
    fn secure_vault_bytes_are_versioned_rust_owned_and_round_trip() {
        let vault = Arc::new(MemoryVault::default());
        let store = CredentialStore::secure(vault.clone());
        let host = paired_host("host-secure", "device-secure");

        store.store(&host).expect("secure write");
        let bytes = vault
            .bytes
            .lock()
            .expect("vault lock")
            .clone()
            .expect("opaque bytes");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("Rust envelope");
        assert_eq!(
            json.get("format_version")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(store.load().expect("secure load"), Some(host));
    }

    #[test]
    fn secure_vault_rejects_foreign_or_downgraded_bytes() {
        let vault = Arc::new(MemoryVault::default());
        *vault.bytes.lock().expect("vault lock") =
            Some(br#"{"format_version":0,"paired_host":{}}"#.to_vec());
        let store = CredentialStore::secure(vault);

        assert!(matches!(store.load(), Err(CredentialError::Malformed)));
    }

    #[test]
    fn secure_vault_migrates_lan_only_v1_but_rejects_remote_data_in_v1() {
        let vault = Arc::new(MemoryVault::default());
        let host = paired_host("host-old", "device-old");
        let old = serde_json::json!({"format_version": 1, "paired_host": host});
        *vault.bytes.lock().expect("vault lock") =
            Some(serde_json::to_vec(&old).expect("old envelope"));
        let store = CredentialStore::secure(vault.clone());
        let loaded = store.load().expect("v1 migrates").expect("paired host");
        assert!(loaded.remote.is_none());
        store.store(&loaded).expect("migration rewrite");
        let rewritten: serde_json::Value = serde_json::from_slice(
            vault
                .bytes
                .lock()
                .expect("vault lock")
                .as_ref()
                .expect("rewritten bytes"),
        )
        .expect("rewritten envelope");
        assert_eq!(
            rewritten
                .get("format_version")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );

        let mut smuggled = old;
        smuggled
            .get_mut("paired_host")
            .and_then(serde_json::Value::as_object_mut)
            .expect("paired host object")
            .insert(
                "remote".to_owned(),
                serde_json::json!({
                    "route_id": "AQEBAQEBAQEBAQEBAQEBAQ",
                    "route_hint": "AgICAgICAgICAgICAgICAg",
                    "device_slot_id": "AwMDAwMDAwMDAwMDAwMDAw",
                    "access_token": "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ",
                    "host_endpoint_id": "endpoint123",
                    "relay_url": "https://relay.example.test",
                    "service_endpoint": "service.example.test:443",
                    "service_public_key_pin": "sha256:BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU"
                }),
            );
        *vault.bytes.lock().expect("vault lock") =
            Some(serde_json::to_vec(&smuggled).expect("smuggled envelope"));
        assert!(matches!(store.load(), Err(CredentialError::Malformed)));
    }
}
