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
    if let Err(error) = client.request(&frame, expected) {
        return match error {
            kaleido_transport::remote_client::RemoteClientError::Rejected(
                kaleido_transport::remote::RemoteErrorCode::Replay,
            ) => {
                persist_pending(credentials, paired, rotate_operation(&pending))?;
                Err(RemotePushError::Unavailable)
            }
            kaleido_transport::remote_client::RemoteClientError::Rejected(_) => {
                Err(RemotePushError::Rejected)
            }
            kaleido_transport::remote_client::RemoteClientError::Contract => {
                Err(RemotePushError::Contract)
            }
            _ => Err(RemotePushError::Unavailable),
        };
    }

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

fn rotate_operation(pending: &PendingPushOperation) -> PendingPushOperation {
    match pending {
        PendingPushOperation::Replace {
            opaque_address,
            registered_at_ms,
            expires_at_ms,
            ..
        } => PendingPushOperation::Replace {
            operation_id: generate_remote_id(),
            opaque_address: opaque_address.clone(),
            registered_at_ms: *registered_at_ms,
            expires_at_ms: *expires_at_ms,
        },
        PendingPushOperation::Delete { .. } => PendingPushOperation::Delete {
            operation_id: generate_remote_id(),
        },
    }
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

    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use kaleido_proto::ids::{DeviceId, HostId};
    use kaleido_transport::remote::{
        RemoteControlFrame, RemoteErrorCode, MAX_REMOTE_CONTROL_FRAME_BYTES, REMOTE_CONTROL_VERSION,
    };
    use kaleido_transport::remote_client::{read_frame, write_frame};
    use kaleido_transport::tls::{server_config, TlsIdentityStore};
    use rustls::{ServerConnection, StreamOwned};

    use super::{flush, queue_delete, queue_replace, RemotePushError};
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

    #[test]
    fn a_lost_ack_replay_rotates_the_durable_operation_before_retry() {
        let directory = tempfile::tempdir().unwrap();
        let identity = TlsIdentityStore::new(directory.path().join("private").join("service.json"))
            .unwrap()
            .load_or_generate()
            .unwrap();
        let pin = identity.leaf_pin().unwrap().encode();
        let tls = server_config(identity).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let connection = ServerConnection::new(tls).unwrap();
            let mut stream = StreamOwned::new(connection, socket);
            assert!(matches!(
                read_frame(&mut stream).unwrap(),
                RemoteControlFrame::RemoteHello { request_id: 1, .. }
            ));
            write_frame(
                &mut stream,
                &RemoteControlFrame::RemoteHelloAck {
                    request_id: 1,
                    remote_control_version: REMOTE_CONTROL_VERSION.to_owned(),
                    max_frame_length: u32::try_from(MAX_REMOTE_CONTROL_FRAME_BYTES).unwrap(),
                },
                false,
            )
            .unwrap();
            assert!(matches!(
                read_frame(&mut stream).unwrap(),
                RemoteControlFrame::DeletePushAddress { request_id: 2, .. }
            ));
            write_frame(
                &mut stream,
                &RemoteControlFrame::RemoteError {
                    request_id: Some(2),
                    code: RemoteErrorCode::Replay,
                    retriable: false,
                },
                false,
            )
            .unwrap();
        });

        let vault = Arc::new(MemoryVault::default());
        let store = CredentialStore::secure(vault);
        let mut paired = paired();
        let remote = paired.remote.as_mut().unwrap();
        remote.service_endpoint = endpoint;
        remote.service_public_key_pin = pin;
        queue_delete(&store, &mut paired).unwrap();
        let before = pending_operation_id(&paired);
        assert_eq!(
            flush(&store, &mut paired, 1),
            Err(RemotePushError::Unavailable)
        );
        let after = pending_operation_id(&paired);
        assert_ne!(before, after);
        let reloaded = store.load().unwrap().unwrap();
        assert_eq!(pending_operation_id(&reloaded), after);
        server.join().unwrap();
    }

    fn pending_operation_id(paired: &PairedHost) -> String {
        match paired
            .remote
            .as_ref()
            .and_then(|remote| remote.pending_push.as_ref())
            .unwrap()
        {
            crate::credential::PendingPushOperation::Replace { operation_id, .. }
            | crate::credential::PendingPushOperation::Delete { operation_id } => {
                operation_id.clone()
            }
        }
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
