//! Ubuntu rendezvous and self-hosted relay policy for R4.
//!
//! This crate deliberately stops at the opaque byte-pipe boundary.  It owns
//! route/presence admission, bounded replay and quota state, and the push
//! wake-up contract; it never imports or parses UACP/business frames.

mod admission;
mod control;
mod error;
mod ids;
mod protocol;
mod push;
mod registry;

#[cfg(feature = "iroh-server")]
mod iroh_server;

pub use admission::{AdmissionLimits, AdmissionPrincipal, ConnectionLease, RelayAdmission};
pub use control::{ControlConnection, ControlOutcome, ControlService, RevokedDevice, WakeDispatch};
pub use error::{RemoteError, RemoteErrorCode, RemoteErrorFrame, RemoteResult};
pub use ids::{AccessToken, DeviceSlotId, HostEndpointId, OperationId, RouteAdminToken, RouteId};
pub use protocol::{
    RemoteHello, RemoteHelloAck, RemoteRequest, RequestTracker, REMOTE_CONTROL_VERSION,
};
pub use push::{
    FcmHttpV1Request, FcmSendError, FcmSender, PushAddress, PushPayload, PushProvider, FCM_SCOPE,
    MAX_PUSH_PAYLOAD_BYTES,
};
pub use registry::{
    DeviceGrant, DeviceGrantRegistration, PresenceRegistration, Registry, RegistryConfig,
    ResolveResponse, RouteBootstrap, RouteRegistration, MAX_PRESENCE_TTL_SECS,
    MIN_PRESENCE_TTL_SECS,
};

#[cfg(feature = "iroh-server")]
pub use iroh_server::{IrohAccessControl, SelfHostedRelayUrl};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::num::NonZeroU64;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn registry_rejects_wrong_token_expired_presence_and_revoke() {
        let registry = Registry::new_ephemeral();
        let route = registry
            .create_route(
                HostEndpointId::from_bytes([7; 32]),
                "https://relay.example.test/relay".to_owned(),
            )
            .unwrap();
        let grant = registry
            .grant_device(route.route_id, &route.admin_token)
            .unwrap();
        registry
            .register_presence(PresenceRegistration {
                route_id: route.route_id,
                admin_token: route.admin_token,
                host_endpoint: route.host_endpoint,
                relay_url: route.relay_url.clone(),
                ttl_secs: MIN_PRESENCE_TTL_SECS,
            })
            .unwrap();
        let wrong = AccessToken::from_bytes([8; 32]);
        assert_eq!(
            registry
                .resolve(route.route_id, grant.slot_id, &wrong, 0)
                .unwrap_err()
                .code(),
            RemoteErrorCode::RouteUnavailable
        );
        assert_eq!(
            registry
                .resolve(route.route_id, grant.slot_id, &grant.access_token, u64::MAX)
                .unwrap_err()
                .code(),
            RemoteErrorCode::RouteUnavailable
        );
        registry
            .revoke_device(route.route_id, &route.admin_token, grant.slot_id)
            .unwrap();
        assert_eq!(
            registry
                .resolve(route.route_id, grant.slot_id, &grant.access_token, 0)
                .unwrap_err()
                .code(),
            RemoteErrorCode::RouteUnavailable
        );
    }

    #[test]
    fn request_tracker_rejects_replay_out_of_order_and_stale_requests() {
        let mut tracker = RequestTracker::new();
        let request = RemoteRequest::new(1, (), 1_000);
        tracker.accept(&request, b"credential", 1_000).unwrap();
        assert_eq!(
            tracker
                .accept(&request, b"credential", 1_000)
                .unwrap_err()
                .code(),
            RemoteErrorCode::MalformedFrame
        );

        let mut tracker = RequestTracker::new();
        let stale = RemoteRequest::new(1, (), 1_000);
        assert_eq!(
            tracker
                .accept(&stale, b"credential", 1_000 + 60_001)
                .unwrap_err()
                .code(),
            RemoteErrorCode::Expired
        );

        let mut tracker = RequestTracker::new();
        let out_of_order = RemoteRequest::new(2, (), 1_000);
        assert_eq!(
            tracker
                .accept(&out_of_order, b"credential", 1_000)
                .unwrap_err()
                .code(),
            RemoteErrorCode::MalformedFrame
        );
    }

    #[test]
    fn push_payload_is_data_only_and_bounded() {
        let route = RouteId::from_bytes([1; 16]);
        let wake = DeviceSlotId::from_bytes([2; 16]);
        let payload = PushPayload::wake(&route, &wake).unwrap();
        let bytes = payload.to_json_bytes().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 4);
        assert!(object.get("cursor").is_none());
        let request = FcmHttpV1Request::new("fid-redacted-in-tests".to_owned(), payload).unwrap();
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"fid\""));
        assert!(!encoded.contains("token"));
    }

    #[test]
    fn admission_enforces_device_limit_and_byte_budget() {
        let limits = AdmissionLimits {
            global_connections: 2,
            route_connections: 2,
            device_connections: 1,
            preauth_connections: 1,
            bytes_per_second: NonZeroU64::new(10).unwrap(),
            burst_bytes: NonZeroU64::new(10).unwrap(),
            idle_timeout_ms: 90_000,
        };
        let admission = RelayAdmission::new(Arc::new(Registry::new_ephemeral()), limits);
        let principal = AdmissionPrincipal {
            route_id: RouteId::from_bytes([3; 16]),
            slot_id: Some(DeviceSlotId::from_bytes([4; 16])),
        };
        let lease = admission.admit(principal).unwrap();
        assert_eq!(
            admission.admit(principal).unwrap_err().code(),
            RemoteErrorCode::LimitExceeded
        );
        lease.consume(10, 1_000).unwrap();
        assert_eq!(
            lease.consume(1, 1_000).unwrap_err().code(),
            RemoteErrorCode::RateLimited
        );
    }

    #[test]
    fn relay_admin_token_is_bound_to_the_registered_host_endpoint() {
        let registry = Arc::new(Registry::new_ephemeral());
        let host = HostEndpointId::from_bytes([21; 32]);
        let route = registry
            .create_route(host, "https://relay.example.test".to_owned())
            .unwrap();
        let admission = RelayAdmission::new(Arc::clone(&registry), AdmissionLimits::default());
        assert_eq!(
            admission
                .admit_bearer_for_endpoint(&route.admin_token.opaque(), Some(&[22; 32]))
                .unwrap_err(),
            RemoteErrorCode::AuthenticationFailed
        );
        let lease = admission
            .admit_bearer_for_endpoint(&route.admin_token.opaque(), Some(host.as_bytes()))
            .unwrap();
        assert_eq!(lease.principal().unwrap().route_id, route.route_id);
    }

    #[test]
    fn wake_address_returns_only_the_registered_route_hint() {
        let registry = Registry::new_ephemeral();
        let route_id = RouteId::from_bytes([31; 16]);
        let route_hint = RouteId::from_bytes([32; 16]);
        let admin_token = RouteAdminToken::from_bytes([33; 32]);
        let slot_id = DeviceSlotId::from_bytes([34; 16]);
        let access_token = AccessToken::from_bytes([35; 32]);
        registry
            .register_route(RouteRegistration {
                route_id,
                route_hint,
                admin_token,
                host_endpoint: HostEndpointId::from_bytes([36; 32]),
                relay_url: "https://relay.example.test".to_owned(),
            })
            .unwrap();
        registry
            .register_device_grant(DeviceGrantRegistration {
                route_id,
                slot_id,
                access_token,
                admin_token,
            })
            .unwrap();
        let registered_at_ms = crate::protocol::now_ms();
        registry
            .register_push(
                route_id,
                slot_id,
                &access_token,
                PushAddress::fcm_fid(
                    "fid-redacted-in-tests".to_owned(),
                    registered_at_ms,
                    registered_at_ms + 60_000,
                )
                .unwrap(),
            )
            .unwrap();
        let (_, returned_hint) = registry
            .push_address(route_id, slot_id, &admin_token)
            .unwrap()
            .unwrap();
        assert_eq!(returned_hint, route_hint);
        assert_ne!(returned_hint, route_id);
    }

    #[test]
    fn identical_slot_ids_are_isolated_by_route_for_push_and_revoke() {
        let registry = Registry::new_ephemeral();
        let slot_id = DeviceSlotId::from_bytes([41; 16]);
        let registered_at_ms = crate::protocol::now_ms();
        let mut routes = Vec::new();
        for marker in [42_u8, 43_u8] {
            let route_id = RouteId::from_bytes([marker; 16]);
            let admin_token = RouteAdminToken::from_bytes([marker; 32]);
            let access_token = AccessToken::from_bytes([marker.saturating_add(10); 32]);
            registry
                .register_route(RouteRegistration {
                    route_id,
                    route_hint: RouteId::from_bytes([marker.saturating_add(20); 16]),
                    admin_token,
                    host_endpoint: HostEndpointId::from_bytes([marker; 32]),
                    relay_url: "https://relay.example.test".to_owned(),
                })
                .unwrap();
            registry
                .register_device_grant(DeviceGrantRegistration {
                    route_id,
                    slot_id,
                    access_token,
                    admin_token,
                })
                .unwrap();
            registry
                .register_push(
                    route_id,
                    slot_id,
                    &access_token,
                    PushAddress::fcm_fid(
                        format!("fid-{marker}"),
                        registered_at_ms,
                        registered_at_ms.saturating_add(60_000),
                    )
                    .unwrap(),
                )
                .unwrap();
            routes.push((route_id, admin_token));
        }

        let mut routes = routes.into_iter();
        let (first_route, first_admin) = routes.next().unwrap();
        let (second_route, second_admin) = routes.next().unwrap();
        registry
            .revoke_device(first_route, &first_admin, slot_id)
            .unwrap();
        assert_eq!(
            registry
                .push_address(first_route, slot_id, &first_admin)
                .unwrap_err()
                .code(),
            RemoteErrorCode::RouteUnavailable
        );
        let (remaining, _) = registry
            .push_address(second_route, slot_id, &second_admin)
            .unwrap()
            .unwrap();
        assert_eq!(remaining.opaque_address, "fid-43");
    }

    #[cfg(unix)]
    #[test]
    fn durable_registry_is_owner_only_and_corruption_fails_loud() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        let path = private.join("registry.json");
        let registry = Registry::open(RegistryConfig::durable(path.clone())).unwrap();
        let route = registry
            .create_route(
                HostEndpointId::from_bytes([9; 32]),
                "https://relay.example.test/relay".to_owned(),
            )
            .unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(registry);
        let reopened = Registry::open(RegistryConfig::durable(path.clone())).unwrap();
        reopened
            .grant_device(route.route_id, &route.admin_token)
            .unwrap();
        drop(reopened);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            Registry::open(RegistryConfig::durable(path.clone())).unwrap_err(),
            RemoteError::UnsafeStorage
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, b"not-json").unwrap();
        assert_eq!(
            Registry::open(RegistryConfig::durable(path))
                .unwrap_err()
                .code(),
            RemoteErrorCode::Internal
        );
    }

    #[cfg(unix)]
    #[test]
    fn service_restart_forgets_presence_but_preserves_route_and_grant() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private").join("registry.json");
        let registry = Registry::open(RegistryConfig::durable(path.clone())).unwrap();
        let route = registry
            .create_route(
                HostEndpointId::from_bytes([31; 32]),
                "https://relay.example.test".to_owned(),
            )
            .unwrap();
        let grant = registry
            .grant_device(route.route_id, &route.admin_token)
            .unwrap();
        registry
            .register_presence(PresenceRegistration {
                route_id: route.route_id,
                admin_token: route.admin_token,
                host_endpoint: route.host_endpoint,
                relay_url: route.relay_url,
                ttl_secs: 30,
            })
            .unwrap();
        registry
            .resolve(route.route_id, grant.slot_id, &grant.access_token, 0)
            .unwrap();
        drop(registry);

        let reopened = Registry::open(RegistryConfig::durable(path)).unwrap();
        assert_eq!(
            reopened
                .resolve(route.route_id, grant.slot_id, &grant.access_token, 0)
                .unwrap_err()
                .code(),
            RemoteErrorCode::RouteUnavailable
        );
    }
}
