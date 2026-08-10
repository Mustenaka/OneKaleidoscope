//! Durable mobile push-address outbox backed by the platform secure vault.

use kaleido_transport::remote::{
    generate_remote_id, ExpectedRemoteResponse, PushAddress, PushProvider, RemoteControlFrame,
};
use kaleido_transport::remote_client::RemoteControlClient;

use crate::credential::{CredentialStore, PairedHost, PendingPushOperation, RemoteAccess};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemotePushError {
    NotConfigured,
    Storage,
    Unavailable,
    Rejected,
    Contract,
}

pub(crate) fn queue_replace(
    credentials: &CredentialStore,
    paired: &mut PairedHost,
    opaque_address: String,
    registered_at_ms: i64,
    expires_at_ms: i64,
) -> Result<(), RemotePushError> {
    PushAddress {
        provider: PushProvider::FcmFid,
        opaque_address: opaque_address.clone(),
        registered_at_ms,
        expires_at_ms,
    }
    .validate()
    .map_err(|_| RemotePushError::Contract)?;
    persist_pending(
        credentials,
        paired,
        PendingPushOperation::Replace {
            operation_id: generate_remote_id(),
            opaque_address,
            registered_at_ms,
            expires_at_ms,
        },
    )
}

pub(crate) fn queue_delete(
    credentials: &CredentialStore,
    paired: &mut PairedHost,
) -> Result<(), RemotePushError> {
    persist_pending(
        credentials,
        paired,
        PendingPushOperation::Delete {
            operation_id: generate_remote_id(),
        },
    )
}

pub(crate) fn flush(
    credentials: &CredentialStore,
    paired: &mut PairedHost,
    issued_at_ms: i64,
) -> Result<bool, RemotePushError> {
    let remote = paired
        .remote
        .as_ref()
        .ok_or(RemotePushError::NotConfigured)?;
    let Some(pending) = remote.pending_push.clone() else {
        return Ok(false);
    };
    let mut client =
        RemoteControlClient::connect(&remote.service_endpoint, &remote.service_public_key_pin)
            .map_err(|_| RemotePushError::Unavailable)?;
    let (frame, expected) = request(remote, &pending, issued_at_ms);
    client
        .request(&frame, expected)
        .map_err(|error| match error {
            kaleido_transport::remote_client::RemoteClientError::Rejected(_) => {
                RemotePushError::Rejected
            }
            kaleido_transport::remote_client::RemoteClientError::Contract => {
                RemotePushError::Contract
            }
            _ => RemotePushError::Unavailable,
        })?;

    let mut committed = paired.clone();
    committed
        .remote
        .as_mut()
        .ok_or(RemotePushError::NotConfigured)?
        .pending_push = None;
    credentials
        .store(&committed)
        .map_err(|_| RemotePushError::Storage)?;
    paired
        .remote
        .as_mut()
        .ok_or(RemotePushError::NotConfigured)?
        .pending_push = None;
    Ok(true)
}

fn persist_pending(
    credentials: &CredentialStore,
    paired: &mut PairedHost,
    pending: PendingPushOperation,
) -> Result<(), RemotePushError> {
    let mut committed = paired.clone();
    committed
        .remote
        .as_mut()
        .ok_or(RemotePushError::NotConfigured)?
        .pending_push = Some(pending.clone());
    credentials
        .store(&committed)
        .map_err(|_| RemotePushError::Storage)?;
    paired
        .remote
        .as_mut()
        .ok_or(RemotePushError::NotConfigured)?
        .pending_push = Some(pending);
    Ok(())
}

fn request(
    remote: &RemoteAccess,
    pending: &PendingPushOperation,
    issued_at_ms: i64,
) -> (RemoteControlFrame, ExpectedRemoteResponse) {
    match pending {
        PendingPushOperation::Replace {
            operation_id,
            opaque_address,
            registered_at_ms,
            expires_at_ms,
        } => (
            RemoteControlFrame::ReplacePushAddress {
                request_id: 2,
                operation_id: operation_id.clone(),
                issued_at_ms,
                route_id: remote.route_id.clone(),
                device_slot_id: remote.device_slot_id.clone(),
                access_token: remote.access_token.clone(),
                address: PushAddress {
                    provider: PushProvider::FcmFid,
                    opaque_address: opaque_address.clone(),
                    registered_at_ms: *registered_at_ms,
                    expires_at_ms: *expires_at_ms,
                },
            },
            ExpectedRemoteResponse::PushAddressReplaced,
        ),
        PendingPushOperation::Delete { operation_id } => (
            RemoteControlFrame::DeletePushAddress {
                request_id: 2,
                operation_id: operation_id.clone(),
                issued_at_ms,
                route_id: remote.route_id.clone(),
                device_slot_id: remote.device_slot_id.clone(),
                access_token: remote.access_token.clone(),
            },
            ExpectedRemoteResponse::PushAddressDeleted,
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::sync::{Arc, Mutex};

    use kaleido_proto::ids::{DeviceId, HostId};

    use super::{queue_delete, queue_replace, RemotePushError};
    use crate::credential::{
        CredentialStore, PairedHost, RemoteAccess, SecureCredentialVault,
        SecureCredentialVaultError,
    };

    #[derive(Default)]
    struct MemoryVault(Mutex<Option<Vec<u8>>>);

    impl SecureCredentialVault for MemoryVault {
        fn load_paired_host(&self) -> Result<Option<Vec<u8>>, SecureCredentialVaultError> {
            Ok(self.0.lock().expect("vault").clone())
        }

        fn store_paired_host(&self, bytes: Vec<u8>) -> Result<(), SecureCredentialVaultError> {
            *self.0.lock().expect("vault") = Some(bytes);
            Ok(())
        }
    }

    #[test]
    fn replace_and_delete_are_durable_before_network_flush() {
        let vault = Arc::new(MemoryVault::default());
        let store = CredentialStore::secure(vault);
        let mut paired = paired();
        queue_replace(&store, &mut paired, "opaque-fid".to_owned(), 1, 2).unwrap();
        let reloaded = store.load().unwrap().unwrap();
        assert!(matches!(
            reloaded.remote.unwrap().pending_push,
            Some(crate::credential::PendingPushOperation::Replace { .. })
        ));
        queue_delete(&store, &mut paired).unwrap();
        let reloaded = store.load().unwrap().unwrap();
        assert!(matches!(
            reloaded.remote.unwrap().pending_push,
            Some(crate::credential::PendingPushOperation::Delete { .. })
        ));
    }

    #[test]
    fn invalid_or_unconfigured_address_is_rejected_without_a_write() {
        let vault = Arc::new(MemoryVault::default());
        let store = CredentialStore::secure(vault);
        let mut paired = paired();
        assert_eq!(
            queue_replace(&store, &mut paired, "contains whitespace".to_owned(), 1, 2),
            Err(RemotePushError::Contract)
        );
        paired.remote = None;
        assert_eq!(
            queue_delete(&store, &mut paired),
            Err(RemotePushError::NotConfigured)
        );
        assert!(store.load().unwrap().is_none());
    }

    fn paired() -> PairedHost {
        PairedHost {
            host_id: HostId::new("host"),
            device_id: DeviceId::new("device"),
            endpoint: "127.0.0.1:7443".to_owned(),
            host_public_key_pin: format!("sha256:{}", "A".repeat(43)),
            remote: Some(RemoteAccess {
                route_id: "AQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
                route_hint: "AgICAgICAgICAgICAgICAg".to_owned(),
                device_slot_id: "AwMDAwMDAwMDAwMDAwMDAw".to_owned(),
                access_token: "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
                host_endpoint_id: "BQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICEiIyQ".to_owned(),
                relay_url: "https://relay.example.test".to_owned(),
                service_endpoint: "127.0.0.1:7443".to_owned(),
                service_public_key_pin: format!("sha256:{}", "E".repeat(43)),
                pending_push: None,
            }),
        }
    }
}
