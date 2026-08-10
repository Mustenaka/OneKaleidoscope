//! Shared REMOTE CONTROL 0.1 service kernel.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use kaleido_transport::remote::{
    PushAddress as WirePushAddress, PushProvider as WirePushProvider, RemoteControlFrame,
    RemoteErrorCode as WireErrorCode, MAX_REMOTE_CONTROL_FRAME_BYTES, OPERATION_CLOCK_SKEW_MS,
    OPERATION_REPLAY_WINDOW_MS, REMOTE_CONTROL_VERSION,
};
use sha2::{Digest, Sha256};

use crate::error::{RemoteError, RemoteErrorCode};
use crate::ids::{AccessToken, DeviceSlotId, OperationId, RouteAdminToken, RouteId};
use crate::protocol::now_ms;
use crate::push::{PushAddress, PushPayload};
use crate::registry::{DeviceGrantRegistration, PresenceRegistration, Registry, RouteRegistration};

const REPLAY_CAPACITY: usize = 4_096;

#[derive(Debug, Default)]
pub struct ControlConnection {
    hello_complete: bool,
    last_request_id: u64,
}

#[derive(Debug, Clone)]
pub struct WakeDispatch {
    route_id: RouteId,
    slot_id: DeviceSlotId,
    pub address: PushAddress,
    pub payload: PushPayload,
}

#[derive(Debug, Clone)]
pub struct ControlOutcome {
    pub response: RemoteControlFrame,
    pub wake: Option<WakeDispatch>,
}

pub struct ControlService {
    registry: Registry,
    replay: Mutex<HashMap<ReplayKey, u64>>,
}

impl std::fmt::Debug for ControlService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlService")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ReplayKey {
    credential_digest: [u8; 32],
    operation_id: OperationId,
}

impl ControlService {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            replay: Mutex::new(HashMap::new()),
        }
    }

    pub fn handle(
        &self,
        connection: &mut ControlConnection,
        frame: RemoteControlFrame,
    ) -> ControlOutcome {
        let request_id = frame.request_id();
        match self.handle_inner(connection, frame) {
            Ok(outcome) => outcome,
            Err(code) => ControlOutcome {
                response: RemoteControlFrame::RemoteError {
                    request_id,
                    code,
                    retriable: is_retriable(code),
                },
                wake: None,
            },
        }
    }

    pub fn delete_unregistered_push(&self, dispatch: &WakeDispatch) -> Result<(), WireErrorCode> {
        self.registry
            .delete_push_for_service(dispatch.route_id, dispatch.slot_id)
            .map_err(map_error)
    }

    fn handle_inner(
        &self,
        connection: &mut ControlConnection,
        frame: RemoteControlFrame,
    ) -> Result<ControlOutcome, WireErrorCode> {
        let request_id = frame.request_id().ok_or(WireErrorCode::MalformedFrame)?;
        let expected = connection
            .last_request_id
            .checked_add(1)
            .ok_or(WireErrorCode::MalformedFrame)?;
        if request_id != expected {
            return Err(WireErrorCode::MalformedFrame);
        }
        if !connection.hello_complete {
            let RemoteControlFrame::RemoteHello {
                remote_control_version,
                max_frame_length,
                ..
            } = frame
            else {
                return Err(WireErrorCode::VersionMismatch);
            };
            // #[allow(kaleido::version_branch)] reason: the protocol hello must reject an incompatible closed wire version before any operation
            if remote_control_version != REMOTE_CONTROL_VERSION
                || usize::try_from(max_frame_length).ok() != Some(MAX_REMOTE_CONTROL_FRAME_BYTES)
            {
                return Err(WireErrorCode::VersionMismatch);
            }
            connection.hello_complete = true;
            connection.last_request_id = request_id;
            return Ok(ControlOutcome {
                response: RemoteControlFrame::RemoteHelloAck {
                    request_id,
                    remote_control_version: REMOTE_CONTROL_VERSION.to_owned(),
                    max_frame_length,
                },
                wake: None,
            });
        }
        connection.last_request_id = request_id;
        let now = now_ms();
        let (response, wake) = match frame {
            RemoteControlFrame::RegisterRoute {
                operation_id,
                issued_at_ms,
                route_id,
                route_hint,
                admin_token,
                host_endpoint_id,
                relay_url,
                ..
            } => {
                self.accept_operation(&operation_id, issued_at_ms, &admin_token, now)?;
                self.registry
                    .register_route(RouteRegistration {
                        route_id: parse_route(&route_id)?,
                        route_hint: parse_route(&route_hint)?,
                        admin_token: parse_admin(&admin_token)?,
                        host_endpoint: crate::HostEndpointId::from_opaque(&host_endpoint_id)
                            .ok_or(WireErrorCode::MalformedFrame)?,
                        relay_url,
                    })
                    .map_err(map_error)?;
                (RemoteControlFrame::RouteRegistered { request_id }, None)
            }
            RemoteControlFrame::RegisterPresence {
                operation_id,
                issued_at_ms,
                route_id,
                admin_token,
                host_endpoint_id,
                relay_url,
                ttl_seconds,
                ..
            } => {
                let route_id = parse_route(&route_id)?;
                let admin_token_value = parse_admin(&admin_token)?;
                self.registry
                    .authorize_admin(route_id, &admin_token_value)
                    .map_err(map_error)?;
                self.accept_operation(&operation_id, issued_at_ms, &admin_token, now)?;
                let host_endpoint = crate::HostEndpointId::from_opaque(&host_endpoint_id)
                    .ok_or(WireErrorCode::MalformedFrame)?;
                self.registry
                    .register_presence(PresenceRegistration {
                        route_id,
                        admin_token: admin_token_value,
                        host_endpoint,
                        relay_url,
                        ttl_secs: u64::from(ttl_seconds),
                    })
                    .map_err(map_error)?;
                let expires_at_ms = now.saturating_add(u64::from(ttl_seconds) * 1_000);
                (
                    RemoteControlFrame::PresenceRegistered {
                        request_id,
                        expires_at_ms: i64::try_from(expires_at_ms)
                            .map_err(|_| WireErrorCode::Internal)?,
                    },
                    None,
                )
            }
            RemoteControlFrame::RegisterDeviceGrant {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                admin_token,
                ..
            } => {
                let route_id = parse_route(&route_id)?;
                let admin_token_value = parse_admin(&admin_token)?;
                self.registry
                    .authorize_admin(route_id, &admin_token_value)
                    .map_err(map_error)?;
                self.accept_operation(&operation_id, issued_at_ms, &admin_token, now)?;
                self.registry
                    .register_device_grant(DeviceGrantRegistration {
                        route_id,
                        slot_id: parse_slot(&device_slot_id)?,
                        access_token: parse_access(&access_token)?,
                        admin_token: admin_token_value,
                    })
                    .map_err(map_error)?;
                (
                    RemoteControlFrame::DeviceGrantRegistered { request_id },
                    None,
                )
            }
            RemoteControlFrame::ResolveRoute {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                ..
            } => {
                let route_id = parse_route(&route_id)?;
                let slot_id = parse_slot(&device_slot_id)?;
                let access_token_value = parse_access(&access_token)?;
                self.registry
                    .authorize_device(route_id, slot_id, &access_token_value)
                    .map_err(map_error)?;
                self.accept_operation(&operation_id, issued_at_ms, &access_token, now)?;
                let resolved = self
                    .registry
                    .resolve(route_id, slot_id, &access_token_value, now)
                    .map_err(map_error)?;
                (
                    RemoteControlFrame::RouteResolved {
                        request_id,
                        host_endpoint_id: resolved.host_endpoint.opaque(),
                        relay_url: resolved.relay_url,
                        expires_at_ms: i64::try_from(resolved.expires_at_ms)
                            .map_err(|_| WireErrorCode::Internal)?,
                    },
                    None,
                )
            }
            RemoteControlFrame::ReplacePushAddress {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                address,
                ..
            } => {
                let route_id = parse_route(&route_id)?;
                let slot_id = parse_slot(&device_slot_id)?;
                let access_token_value = parse_access(&access_token)?;
                self.registry
                    .authorize_device(route_id, slot_id, &access_token_value)
                    .map_err(map_error)?;
                self.accept_operation(&operation_id, issued_at_ms, &access_token, now)?;
                let address = convert_push(address)?;
                let expires_at_ms = address.expires_at_ms;
                self.registry
                    .register_push(route_id, slot_id, &access_token_value, address)
                    .map_err(map_error)?;
                (
                    RemoteControlFrame::PushAddressReplaced {
                        request_id,
                        expires_at_ms: i64::try_from(expires_at_ms)
                            .map_err(|_| WireErrorCode::Internal)?,
                    },
                    None,
                )
            }
            RemoteControlFrame::DeletePushAddress {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                ..
            } => {
                let route_id = parse_route(&route_id)?;
                let slot_id = parse_slot(&device_slot_id)?;
                let access_token_value = parse_access(&access_token)?;
                self.registry
                    .authorize_device(route_id, slot_id, &access_token_value)
                    .map_err(map_error)?;
                self.accept_operation(&operation_id, issued_at_ms, &access_token, now)?;
                self.registry
                    .delete_push(route_id, slot_id, &access_token_value)
                    .map_err(map_error)?;
                (RemoteControlFrame::PushAddressDeleted { request_id }, None)
            }
            RemoteControlFrame::WakeDevice {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                admin_token,
                wake_id,
                ..
            } => {
                let route = parse_route(&route_id)?;
                let slot_id = parse_slot(&device_slot_id)?;
                let admin_token_value = parse_admin(&admin_token)?;
                self.registry
                    .authorize_admin(route, &admin_token_value)
                    .map_err(map_error)?;
                self.accept_operation(&operation_id, issued_at_ms, &admin_token, now)?;
                let (address, route_hint) = self
                    .registry
                    .push_address(route, slot_id, &admin_token_value)
                    .map_err(map_error)?
                    .ok_or(WireErrorCode::RouteUnavailable)?;
                let payload = PushPayload::wake_with_id(&route_hint, &wake_id)
                    .ok_or(WireErrorCode::MalformedFrame)?;
                (
                    RemoteControlFrame::WakeAccepted { request_id },
                    Some(WakeDispatch {
                        route_id: route,
                        slot_id,
                        address,
                        payload,
                    }),
                )
            }
            RemoteControlFrame::RevokeDeviceGrant {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                admin_token,
                ..
            } => {
                let route_id = parse_route(&route_id)?;
                let slot_id = parse_slot(&device_slot_id)?;
                let admin_token_value = parse_admin(&admin_token)?;
                self.registry
                    .authorize_admin(route_id, &admin_token_value)
                    .map_err(map_error)?;
                let replayed =
                    match self.accept_operation(&operation_id, issued_at_ms, &admin_token, now) {
                        Ok(()) => false,
                        Err(WireErrorCode::Replay) => true,
                        Err(error) => return Err(error),
                    };
                match self
                    .registry
                    .revoke_device(route_id, &admin_token_value, slot_id)
                {
                    Ok(()) => {}
                    Err(error) if replayed && error.code() == RemoteErrorCode::Revoked => {}
                    Err(error) => return Err(map_error(error)),
                }
                (RemoteControlFrame::DeviceGrantRevoked { request_id }, None)
            }
            _ => return Err(WireErrorCode::MalformedFrame),
        };
        Ok(ControlOutcome { response, wake })
    }

    fn accept_operation(
        &self,
        operation_id: &str,
        issued_at_ms: i64,
        credential: &str,
        now: u64,
    ) -> Result<(), WireErrorCode> {
        let issued = u64::try_from(issued_at_ms).map_err(|_| WireErrorCode::Expired)?;
        if issued.abs_diff(now)
            > u64::try_from(OPERATION_CLOCK_SKEW_MS).map_err(|_| WireErrorCode::Internal)?
        {
            return Err(WireErrorCode::Expired);
        }
        let operation_id =
            OperationId::from_opaque(operation_id).ok_or(WireErrorCode::MalformedFrame)?;
        let mut hasher = Sha256::new();
        hasher.update(credential.as_bytes());
        let key = ReplayKey {
            credential_digest: hasher.finalize().into(),
            operation_id,
        };
        let mut replay = lock(&self.replay)?;
        let replay_window =
            u64::try_from(OPERATION_REPLAY_WINDOW_MS).map_err(|_| WireErrorCode::Internal)?;
        replay.retain(|_, seen| now.saturating_sub(*seen) <= replay_window);
        if replay.contains_key(&key) {
            return Err(WireErrorCode::Replay);
        }
        if replay.len() >= REPLAY_CAPACITY {
            return Err(WireErrorCode::LimitExceeded);
        }
        replay.insert(key, now);
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, WireErrorCode> {
    mutex.lock().map_err(|_| WireErrorCode::Internal)
}

fn parse_route(value: &str) -> Result<RouteId, WireErrorCode> {
    RouteId::from_opaque(value).ok_or(WireErrorCode::MalformedFrame)
}

fn parse_slot(value: &str) -> Result<DeviceSlotId, WireErrorCode> {
    DeviceSlotId::from_opaque(value).ok_or(WireErrorCode::MalformedFrame)
}

fn parse_access(value: &str) -> Result<AccessToken, WireErrorCode> {
    AccessToken::from_opaque(value).ok_or(WireErrorCode::MalformedFrame)
}

fn parse_admin(value: &str) -> Result<RouteAdminToken, WireErrorCode> {
    RouteAdminToken::from_opaque(value).ok_or(WireErrorCode::MalformedFrame)
}

fn convert_push(address: WirePushAddress) -> Result<PushAddress, WireErrorCode> {
    if address.provider != WirePushProvider::FcmFid {
        return Err(WireErrorCode::MalformedFrame);
    }
    PushAddress::fcm_fid(
        address.opaque_address,
        u64::try_from(address.registered_at_ms).map_err(|_| WireErrorCode::MalformedFrame)?,
        u64::try_from(address.expires_at_ms).map_err(|_| WireErrorCode::MalformedFrame)?,
    )
    .ok_or(WireErrorCode::MalformedFrame)
}

fn map_error(error: RemoteError) -> WireErrorCode {
    match error.code() {
        RemoteErrorCode::VersionMismatch => WireErrorCode::VersionMismatch,
        RemoteErrorCode::MalformedFrame => WireErrorCode::MalformedFrame,
        RemoteErrorCode::AuthenticationFailed => WireErrorCode::AuthenticationFailed,
        RemoteErrorCode::RouteUnavailable => WireErrorCode::RouteUnavailable,
        RemoteErrorCode::Expired => WireErrorCode::Expired,
        RemoteErrorCode::Replay => WireErrorCode::Replay,
        RemoteErrorCode::RateLimited => WireErrorCode::RateLimited,
        RemoteErrorCode::LimitExceeded => WireErrorCode::LimitExceeded,
        RemoteErrorCode::Revoked => WireErrorCode::Revoked,
        RemoteErrorCode::Internal => WireErrorCode::Internal,
    }
}

const fn is_retriable(code: WireErrorCode) -> bool {
    matches!(
        code,
        WireErrorCode::RouteUnavailable
            | WireErrorCode::Expired
            | WireErrorCode::RateLimited
            | WireErrorCode::Internal
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{ControlConnection, ControlService};
    use crate::{Registry, RouteAdminToken, RouteId};
    use kaleido_transport::remote::{
        RemoteControlFrame, RemoteErrorCode, MAX_REMOTE_CONTROL_FRAME_BYTES, REMOTE_CONTROL_VERSION,
    };

    #[test]
    fn service_requires_hello_monotonic_ids_and_global_replay_protection() {
        let service = ControlService::new(Registry::new_ephemeral());
        let route = RouteId::from_bytes([1; 16]);
        let admin = RouteAdminToken::from_bytes([2; 32]);
        let operation = RouteId::from_bytes([3; 16]).opaque();
        let request = RemoteControlFrame::RegisterPresence {
            request_id: 3,
            operation_id: operation,
            issued_at_ms: i64::try_from(crate::protocol::now_ms()).unwrap(),
            route_id: route.opaque(),
            admin_token: admin.opaque(),
            host_endpoint_id: crate::HostEndpointId::from_bytes([4; 32]).opaque(),
            relay_url: "https://relay.example.test".to_owned(),
            ttl_seconds: 30,
        };
        let mut first = ControlConnection::default();
        assert!(matches!(
            service.handle(&mut first, request.clone()).response,
            RemoteControlFrame::RemoteError {
                code: RemoteErrorCode::MalformedFrame,
                ..
            }
        ));
        hello(&service, &mut first);
        assert!(matches!(
            service
                .handle(
                    &mut first,
                    RemoteControlFrame::RegisterRoute {
                        request_id: 2,
                        operation_id: RouteId::from_bytes([5; 16]).opaque(),
                        issued_at_ms: i64::try_from(crate::protocol::now_ms()).unwrap(),
                        route_id: route.opaque(),
                        route_hint: RouteId::from_bytes([6; 16]).opaque(),
                        admin_token: admin.opaque(),
                        host_endpoint_id: crate::HostEndpointId::from_bytes([4; 32]).opaque(),
                        relay_url: "https://relay.example.test".to_owned(),
                    },
                )
                .response,
            RemoteControlFrame::RouteRegistered { request_id: 2 }
        ));
        assert!(matches!(
            service.handle(&mut first, request.clone()).response,
            RemoteControlFrame::PresenceRegistered { request_id: 3, .. }
        ));

        let mut second = ControlConnection::default();
        hello(&service, &mut second);
        let replay = match request {
            RemoteControlFrame::RegisterPresence {
                operation_id,
                issued_at_ms,
                route_id,
                admin_token,
                host_endpoint_id,
                relay_url,
                ttl_seconds,
                ..
            } => RemoteControlFrame::RegisterPresence {
                request_id: 2,
                operation_id,
                issued_at_ms,
                route_id,
                admin_token,
                host_endpoint_id,
                relay_url,
                ttl_seconds,
            },
            _ => unreachable!("test request is presence"),
        };
        assert!(matches!(
            service.handle(&mut second, replay).response,
            RemoteControlFrame::RemoteError {
                code: RemoteErrorCode::Replay,
                ..
            }
        ));
    }

    #[test]
    fn revoke_retry_after_lost_ack_is_idempotently_satisfied() {
        let service = ControlService::new(Registry::new_ephemeral());
        let route = RouteId::from_bytes([11; 16]);
        let hint = RouteId::from_bytes([12; 16]);
        let slot = crate::DeviceSlotId::from_bytes([13; 16]);
        let admin = RouteAdminToken::from_bytes([14; 32]);
        let access = crate::AccessToken::from_bytes([15; 32]);
        let endpoint = crate::HostEndpointId::from_bytes([16; 32]);
        let issued_at_ms = i64::try_from(crate::protocol::now_ms()).unwrap();

        let mut first = ControlConnection::default();
        hello(&service, &mut first);
        assert!(matches!(
            service
                .handle(
                    &mut first,
                    RemoteControlFrame::RegisterRoute {
                        request_id: 2,
                        operation_id: RouteId::from_bytes([17; 16]).opaque(),
                        issued_at_ms,
                        route_id: route.opaque(),
                        route_hint: hint.opaque(),
                        admin_token: admin.opaque(),
                        host_endpoint_id: endpoint.opaque(),
                        relay_url: "https://relay.example.test".to_owned(),
                    },
                )
                .response,
            RemoteControlFrame::RouteRegistered { request_id: 2 }
        ));
        assert!(matches!(
            service
                .handle(
                    &mut first,
                    RemoteControlFrame::RegisterDeviceGrant {
                        request_id: 3,
                        operation_id: RouteId::from_bytes([18; 16]).opaque(),
                        issued_at_ms,
                        route_id: route.opaque(),
                        device_slot_id: slot.opaque(),
                        access_token: access.opaque(),
                        admin_token: admin.opaque(),
                    },
                )
                .response,
            RemoteControlFrame::DeviceGrantRegistered { request_id: 3 }
        ));
        let revoke_operation = RouteId::from_bytes([19; 16]).opaque();
        assert!(matches!(
            service
                .handle(
                    &mut first,
                    RemoteControlFrame::RevokeDeviceGrant {
                        request_id: 4,
                        operation_id: revoke_operation.clone(),
                        issued_at_ms,
                        route_id: route.opaque(),
                        device_slot_id: slot.opaque(),
                        admin_token: admin.opaque(),
                    },
                )
                .response,
            RemoteControlFrame::DeviceGrantRevoked { request_id: 4 }
        ));

        let mut reconnected = ControlConnection::default();
        hello(&service, &mut reconnected);
        assert!(matches!(
            service
                .handle(
                    &mut reconnected,
                    RemoteControlFrame::RevokeDeviceGrant {
                        request_id: 2,
                        operation_id: revoke_operation,
                        issued_at_ms,
                        route_id: route.opaque(),
                        device_slot_id: slot.opaque(),
                        admin_token: admin.opaque(),
                    },
                )
                .response,
            RemoteControlFrame::DeviceGrantRevoked { request_id: 2 }
        ));
    }

    fn hello(service: &ControlService, connection: &mut ControlConnection) {
        let response = service.handle(
            connection,
            RemoteControlFrame::RemoteHello {
                request_id: 1,
                remote_control_version: REMOTE_CONTROL_VERSION.to_owned(),
                max_frame_length: u32::try_from(MAX_REMOTE_CONTROL_FRAME_BYTES).unwrap(),
            },
        );
        assert!(matches!(
            response.response,
            RemoteControlFrame::RemoteHelloAck { request_id: 1, .. }
        ));
    }
}
