//! Non-secret paired-host metadata.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use kaleido_proto::ids::{DeviceId, HostId};
use serde::{Deserialize, Serialize};

const CREDENTIAL_FILE: &str = "paired-host.json";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedHost {
    pub host_id: HostId,
    pub device_id: DeviceId,
    pub endpoint: String,
    pub host_public_key_pin: String,
}

impl std::fmt::Debug for PairedHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairedHost")
            .field("host_id", &self.host_id)
            .field("device_id", &self.device_id)
            .field("endpoint", &"[redacted]")
            .field("host_public_key_pin", &"[redacted]")
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

#[derive(Debug)]
pub struct CredentialStore {
    root: PathBuf,
}

impl CredentialStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CredentialError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|_| CredentialError::Io)?;
        Ok(Self { root })
    }

    pub fn load(&self) -> Result<Option<PairedHost>, CredentialError> {
        let path = self.root.join(CREDENTIAL_FILE);
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

    pub fn store(&self, host: &PairedHost) -> Result<(), CredentialError> {
        validate(host)?;
        let encoded = serde_json::to_vec(host).map_err(|_| CredentialError::Malformed)?;
        let target = self.root.join(CREDENTIAL_FILE);
        let temporary = self.root.join(format!("{CREDENTIAL_FILE}.tmp"));
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
            // Paired-host metadata is non-secret and can be reconstructed by
            // pairing again. Remove the previous complete record only after
            // the replacement has been fully written and synced; this also
            // gives Windows the replace semantics that `rename` lacks.
            fs::remove_file(&target).map_err(|_| CredentialError::Io)?;
        }
        if fs::rename(&temporary, &target).is_err() {
            drop(fs::remove_file(&temporary));
            return Err(CredentialError::Io);
        }
        Ok(())
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
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use kaleido_proto::ids::{DeviceId, HostId};

    use super::{CredentialStore, PairedHost};

    #[test]
    fn paired_metadata_round_trips_without_any_pairing_secret() {
        let directory = tempfile::tempdir().expect("directory");
        let store = CredentialStore::open(directory.path()).expect("store");
        let host = PairedHost {
            host_id: HostId::new("host-a"),
            device_id: DeviceId::new("device-a"),
            endpoint: "127.0.0.1:7443".to_owned(),
            host_public_key_pin: format!("sha256:{}", "A".repeat(43)),
        };
        store.store(&host).expect("write");
        assert_eq!(store.load().expect("load"), Some(host));
        let bytes = std::fs::read(directory.path().join("paired-host.json")).expect("read");
        assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
    }

    #[test]
    fn a_completed_repair_replaces_the_previous_host_metadata() {
        let directory = tempfile::tempdir().expect("directory");
        let store = CredentialStore::open(directory.path()).expect("store");
        let first = PairedHost {
            host_id: HostId::new("host-a"),
            device_id: DeviceId::new("device-a"),
            endpoint: "127.0.0.1:7443".to_owned(),
            host_public_key_pin: format!("sha256:{}", "A".repeat(43)),
        };
        let replacement = PairedHost {
            host_id: HostId::new("host-b"),
            device_id: DeviceId::new("device-b"),
            endpoint: "host-b.local:7554".to_owned(),
            host_public_key_pin: format!("sha256:{}", "A".repeat(43)),
        };

        store.store(&first).expect("first pairing");
        store.store(&replacement).expect("replacement pairing");

        assert_eq!(store.load().expect("load replacement"), Some(replacement));
    }
}
