//! Closed REMOTE CONTROL 0.1 wire types.
//!
//! These frames carry rendezvous and push metadata only. They must never carry
//! UACP business frames; the public data path is an opaque byte pipe containing
//! the existing pinned TLS TRANSPORT session.

use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::error::TransportError;

pub const REMOTE_CONTROL_VERSION: &str = "0.1.0";
pub const MAX_REMOTE_CONTROL_FRAME_BYTES: usize = 4_096;
pub const MAX_PUSH_PAYLOAD_BYTES: usize = 256;
pub const OPERATION_CLOCK_SKEW_MS: i64 = 60_000;
pub const OPERATION_REPLAY_WINDOW_MS: i64 = 120_000;
pub const MIN_PRESENCE_TTL_SECONDS: u16 = 15;
pub const DEFAULT_PRESENCE_TTL_SECONDS: u16 = 30;
pub const MAX_PRESENCE_TTL_SECONDS: u16 = 90;

const RANDOM_ID_BYTES: usize = 16;
const TOKEN_BYTES: usize = 32;
const MAX_ENDPOINT_ID_BYTES: usize = 128;
const MAX_RELAY_URL_BYTES: usize = 512;
const MAX_PUSH_ADDRESS_BYTES: usize = 512;
const REMOTE_BOOTSTRAP_URI_PREFIX: &str = "onekaleidoscope://remote/v1?data=";

pub fn generate_remote_id() -> String {
    let mut bytes = [0_u8; RANDOM_ID_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate_remote_token() -> String {
    let mut bytes = [0_u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Clone, PartialEq, Eq)]
pub struct RemotePairingBootstrap {
    pub route_id: String,
    pub route_hint: String,
    pub device_slot_id: String,
    pub access_token: String,
    pub host_endpoint_id: String,
    pub relay_url: String,
    pub service_endpoint: String,
    pub service_public_key_pin: String,
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for RemotePairingBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemotePairingBootstrap")
            .field("route_id", &"[redacted]")
            .field("route_hint", &"[redacted]")
            .field("device_slot_id", &"[redacted]")
            .field("access_token", &"[redacted]")
            .field("host_endpoint_id", &"[redacted]")
            .field("relay_url", &"[redacted]")
            .field("service_endpoint", &"[redacted]")
            .field("service_public_key_pin", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl Drop for RemotePairingBootstrap {
    fn drop(&mut self) {
        self.access_token.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteBootstrapWire {
    #[serde(rename = "version")]
    revision: u64,
    route_id: String,
    route_hint: String,
    device_slot_id: String,
    access_token: String,
    host_endpoint_id: String,
    relay_url: String,
    service_endpoint: String,
    service_public_key_pin: String,
    expires_at_ms: i64,
}

pub fn encode_remote_bootstrap(
    bootstrap: &RemotePairingBootstrap,
) -> Result<String, TransportError> {
    validate_remote_bootstrap(bootstrap)?;
    let wire = RemoteBootstrapWire {
        revision: 1,
        route_id: bootstrap.route_id.clone(),
        route_hint: bootstrap.route_hint.clone(),
        device_slot_id: bootstrap.device_slot_id.clone(),
        access_token: bootstrap.access_token.clone(),
        host_endpoint_id: bootstrap.host_endpoint_id.clone(),
        relay_url: bootstrap.relay_url.clone(),
        service_endpoint: bootstrap.service_endpoint.clone(),
        service_public_key_pin: bootstrap.service_public_key_pin.clone(),
        expires_at_ms: bootstrap.expires_at_ms,
    };
    let encoded = serde_json::to_vec(&wire).map_err(|_| TransportError::MalformedFrame)?;
    if encoded.len() > crate::MAX_BOOTSTRAP_JSON_BYTES {
        return Err(TransportError::FrameTooLarge);
    }
    Ok(format!(
        "{REMOTE_BOOTSTRAP_URI_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(encoded)
    ))
}

pub fn decode_remote_bootstrap(uri: &str) -> Result<RemotePairingBootstrap, TransportError> {
    let encoded = uri
        .strip_prefix(REMOTE_BOOTSTRAP_URI_PREFIX)
        .ok_or(TransportError::MalformedFrame)?;
    if encoded.is_empty()
        || encoded.contains('=')
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(TransportError::MalformedFrame);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| TransportError::MalformedFrame)?;
    if decoded.len() > crate::MAX_BOOTSTRAP_JSON_BYTES
        || URL_SAFE_NO_PAD.encode(&decoded) != encoded
    {
        return Err(TransportError::MalformedFrame);
    }
    let wire: RemoteBootstrapWire =
        serde_json::from_slice(&decoded).map_err(|_| TransportError::MalformedFrame)?;
    let canonical = serde_json::to_vec(&wire).map_err(|_| TransportError::MalformedFrame)?;
    if canonical != decoded || wire.revision != 1 {
        return Err(TransportError::MalformedFrame);
    }
    let bootstrap = RemotePairingBootstrap {
        route_id: wire.route_id,
        route_hint: wire.route_hint,
        device_slot_id: wire.device_slot_id,
        access_token: wire.access_token,
        host_endpoint_id: wire.host_endpoint_id,
        relay_url: wire.relay_url,
        service_endpoint: wire.service_endpoint,
        service_public_key_pin: wire.service_public_key_pin,
        expires_at_ms: wire.expires_at_ms,
    };
    validate_remote_bootstrap(&bootstrap)?;
    Ok(bootstrap)
}

pub fn validate_remote_bootstrap(bootstrap: &RemotePairingBootstrap) -> Result<(), TransportError> {
    validate_random_id(&bootstrap.route_id)?;
    validate_random_id(&bootstrap.route_hint)?;
    validate_random_id(&bootstrap.device_slot_id)?;
    validate_token(&bootstrap.access_token)?;
    validate_endpoint_id(&bootstrap.host_endpoint_id)?;
    validate_relay_url(&bootstrap.relay_url)?;
    crate::bootstrap::validate_endpoint(&bootstrap.service_endpoint)?;
    crate::tls::SpkiPin::parse(&bootstrap.service_public_key_pin)?;
    if bootstrap.expires_at_ms < 0 {
        return Err(TransportError::MalformedFrame);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteErrorCode {
    VersionMismatch,
    MalformedFrame,
    AuthenticationFailed,
    RouteUnavailable,
    Expired,
    Replay,
    RateLimited,
    LimitExceeded,
    Revoked,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushProvider {
    FcmFid,
    ApnsToken,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushAddress {
    pub provider: PushProvider,
    pub opaque_address: String,
    pub registered_at_ms: i64,
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for PushAddress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PushAddress")
            .field("provider", &self.provider)
            .field("opaque_address", &"[redacted]")
            .field("registered_at_ms", &self.registered_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl PushAddress {
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.opaque_address.is_empty()
            || self.opaque_address.len() > MAX_PUSH_ADDRESS_BYTES
            || self
                .opaque_address
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            || self.registered_at_ms < 0
            || self.expires_at_ms <= self.registered_at_ms
        {
            return Err(TransportError::MalformedFrame);
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteControlFrame {
    RemoteHello {
        request_id: u64,
        remote_control_version: String,
        max_frame_length: u32,
    },
    RemoteHelloAck {
        request_id: u64,
        remote_control_version: String,
        max_frame_length: u32,
    },
    RegisterRoute {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        route_hint: String,
        admin_token: String,
        host_endpoint_id: String,
        relay_url: String,
    },
    RouteRegistered {
        request_id: u64,
    },
    RegisterPresence {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        admin_token: String,
        host_endpoint_id: String,
        relay_url: String,
        ttl_seconds: u16,
    },
    PresenceRegistered {
        request_id: u64,
        expires_at_ms: i64,
    },
    RegisterDeviceGrant {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        device_slot_id: String,
        access_token: String,
        admin_token: String,
    },
    DeviceGrantRegistered {
        request_id: u64,
    },
    ResolveRoute {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        device_slot_id: String,
        access_token: String,
    },
    RouteResolved {
        request_id: u64,
        host_endpoint_id: String,
        relay_url: String,
        expires_at_ms: i64,
    },
    ReplacePushAddress {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        device_slot_id: String,
        access_token: String,
        address: PushAddress,
    },
    PushAddressReplaced {
        request_id: u64,
        expires_at_ms: i64,
    },
    DeletePushAddress {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        device_slot_id: String,
        access_token: String,
    },
    PushAddressDeleted {
        request_id: u64,
    },
    WakeDevice {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        device_slot_id: String,
        admin_token: String,
        wake_id: String,
    },
    WakeAccepted {
        request_id: u64,
    },
    RevokeDeviceGrant {
        request_id: u64,
        operation_id: String,
        issued_at_ms: i64,
        route_id: String,
        device_slot_id: String,
        admin_token: String,
    },
    DeviceGrantRevoked {
        request_id: u64,
    },
    RemoteError {
        request_id: Option<u64>,
        code: RemoteErrorCode,
        retriable: bool,
    },
}

impl std::fmt::Debug for RemoteControlFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteControlFrame")
            .field("kind", &self.kind())
            .finish()
    }
}

impl RemoteControlFrame {
    pub fn decode(bytes: &[u8]) -> Result<Self, TransportError> {
        if bytes.is_empty()
            || bytes.len() > MAX_REMOTE_CONTROL_FRAME_BYTES
            || std::str::from_utf8(bytes).is_err()
        {
            return Err(if bytes.len() > MAX_REMOTE_CONTROL_FRAME_BYTES {
                TransportError::FrameTooLarge
            } else {
                TransportError::MalformedFrame
            });
        }

        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let mut ignored = false;
        let frame: Self = serde_ignored::deserialize(&mut deserializer, |_| ignored = true)
            .map_err(|_| TransportError::MalformedFrame)?;
        deserializer
            .end()
            .map_err(|_| TransportError::MalformedFrame)?;
        if ignored {
            return Err(TransportError::MalformedFrame);
        }
        frame.validate()?;
        Ok(frame)
    }

    pub fn encode(&self) -> Result<Vec<u8>, TransportError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| TransportError::MalformedFrame)?;
        if encoded.len() > MAX_REMOTE_CONTROL_FRAME_BYTES {
            return Err(TransportError::FrameTooLarge);
        }
        Ok(encoded)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::RemoteHello { .. } => "remote_hello",
            Self::RemoteHelloAck { .. } => "remote_hello_ack",
            Self::RegisterRoute { .. } => "register_route",
            Self::RouteRegistered { .. } => "route_registered",
            Self::RegisterPresence { .. } => "register_presence",
            Self::PresenceRegistered { .. } => "presence_registered",
            Self::RegisterDeviceGrant { .. } => "register_device_grant",
            Self::DeviceGrantRegistered { .. } => "device_grant_registered",
            Self::ResolveRoute { .. } => "resolve_route",
            Self::RouteResolved { .. } => "route_resolved",
            Self::ReplacePushAddress { .. } => "replace_push_address",
            Self::PushAddressReplaced { .. } => "push_address_replaced",
            Self::DeletePushAddress { .. } => "delete_push_address",
            Self::PushAddressDeleted { .. } => "push_address_deleted",
            Self::WakeDevice { .. } => "wake_device",
            Self::WakeAccepted { .. } => "wake_accepted",
            Self::RevokeDeviceGrant { .. } => "revoke_device_grant",
            Self::DeviceGrantRevoked { .. } => "device_grant_revoked",
            Self::RemoteError { .. } => "remote_error",
        }
    }

    pub fn request_id(&self) -> Option<u64> {
        match self {
            Self::RemoteHello { request_id, .. }
            | Self::RemoteHelloAck { request_id, .. }
            | Self::RegisterRoute { request_id, .. }
            | Self::RouteRegistered { request_id }
            | Self::RegisterPresence { request_id, .. }
            | Self::PresenceRegistered { request_id, .. }
            | Self::RegisterDeviceGrant { request_id, .. }
            | Self::DeviceGrantRegistered { request_id }
            | Self::ResolveRoute { request_id, .. }
            | Self::RouteResolved { request_id, .. }
            | Self::ReplacePushAddress { request_id, .. }
            | Self::PushAddressReplaced { request_id, .. }
            | Self::DeletePushAddress { request_id, .. }
            | Self::PushAddressDeleted { request_id }
            | Self::WakeDevice { request_id, .. }
            | Self::WakeAccepted { request_id }
            | Self::RevokeDeviceGrant { request_id, .. }
            | Self::DeviceGrantRevoked { request_id } => Some(*request_id),
            Self::RemoteError { request_id, .. } => *request_id,
        }
    }

    fn validate(&self) -> Result<(), TransportError> {
        if self.request_id().is_some_and(|request_id| request_id == 0) {
            return Err(TransportError::MalformedFrame);
        }
        match self {
            Self::RemoteHello {
                remote_control_version,
                max_frame_length,
                ..
            }
            | Self::RemoteHelloAck {
                remote_control_version,
                max_frame_length,
                ..
            } => {
                if !remote_version_is_compatible(remote_control_version)
                    || usize::try_from(*max_frame_length).ok()
                        != Some(MAX_REMOTE_CONTROL_FRAME_BYTES)
                {
                    return Err(TransportError::VersionMismatch);
                }
            }
            Self::RegisterPresence {
                operation_id,
                issued_at_ms,
                route_id,
                admin_token,
                host_endpoint_id,
                relay_url,
                ttl_seconds,
                ..
            } => {
                validate_operation(operation_id, *issued_at_ms)?;
                validate_random_id(route_id)?;
                validate_token(admin_token)?;
                validate_endpoint_id(host_endpoint_id)?;
                validate_relay_url(relay_url)?;
                if !(MIN_PRESENCE_TTL_SECONDS..=MAX_PRESENCE_TTL_SECONDS).contains(ttl_seconds) {
                    return Err(TransportError::MalformedFrame);
                }
            }
            Self::RegisterRoute {
                operation_id,
                issued_at_ms,
                route_id,
                route_hint,
                admin_token,
                host_endpoint_id,
                relay_url,
                ..
            } => {
                validate_operation(operation_id, *issued_at_ms)?;
                validate_random_id(route_id)?;
                validate_random_id(route_hint)?;
                validate_token(admin_token)?;
                validate_endpoint_id(host_endpoint_id)?;
                validate_relay_url(relay_url)?;
            }
            Self::PresenceRegistered { expires_at_ms, .. }
            | Self::PushAddressReplaced { expires_at_ms, .. } => {
                if *expires_at_ms < 0 {
                    return Err(TransportError::MalformedFrame);
                }
            }
            Self::RegisterDeviceGrant {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                admin_token,
                ..
            } => {
                validate_operation(operation_id, *issued_at_ms)?;
                validate_random_id(route_id)?;
                validate_random_id(device_slot_id)?;
                validate_token(access_token)?;
                validate_token(admin_token)?;
            }
            Self::ResolveRoute {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                ..
            }
            | Self::DeletePushAddress {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                ..
            } => {
                validate_device_operation(
                    operation_id,
                    *issued_at_ms,
                    route_id,
                    device_slot_id,
                    access_token,
                )?;
            }
            Self::RouteResolved {
                host_endpoint_id,
                relay_url,
                expires_at_ms,
                ..
            } => {
                validate_endpoint_id(host_endpoint_id)?;
                validate_relay_url(relay_url)?;
                if *expires_at_ms < 0 {
                    return Err(TransportError::MalformedFrame);
                }
            }
            Self::ReplacePushAddress {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                access_token,
                address,
                ..
            } => {
                validate_device_operation(
                    operation_id,
                    *issued_at_ms,
                    route_id,
                    device_slot_id,
                    access_token,
                )?;
                address.validate()?;
            }
            Self::RevokeDeviceGrant {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                admin_token,
                ..
            } => {
                validate_operation(operation_id, *issued_at_ms)?;
                validate_random_id(route_id)?;
                validate_random_id(device_slot_id)?;
                validate_token(admin_token)?;
            }
            Self::WakeDevice {
                operation_id,
                issued_at_ms,
                route_id,
                device_slot_id,
                admin_token,
                wake_id,
                ..
            } => {
                validate_operation(operation_id, *issued_at_ms)?;
                validate_random_id(route_id)?;
                validate_random_id(device_slot_id)?;
                validate_token(admin_token)?;
                validate_random_id(wake_id)?;
            }
            Self::RouteRegistered { .. }
            | Self::DeviceGrantRegistered { .. }
            | Self::PushAddressDeleted { .. }
            | Self::WakeAccepted { .. }
            | Self::DeviceGrantRevoked { .. }
            | Self::RemoteError { .. } => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedRemoteResponse {
    RemoteHelloAck,
    RouteRegistered,
    PresenceRegistered,
    DeviceGrantRegistered,
    RouteResolved,
    PushAddressReplaced,
    PushAddressDeleted,
    WakeAccepted,
    DeviceGrantRevoked,
}

#[derive(Debug, Default)]
pub struct RemoteCorrelationState {
    last_incoming_request_id: u64,
    last_outgoing_request_id: u64,
    outgoing: BTreeMap<u64, ExpectedRemoteResponse>,
}

impl RemoteCorrelationState {
    pub fn accept_incoming_request(&mut self, request_id: u64) -> Result<(), TransportError> {
        if request_id == 0 || request_id <= self.last_incoming_request_id {
            return Err(TransportError::MalformedFrame);
        }
        self.last_incoming_request_id = request_id;
        Ok(())
    }

    pub fn register_outgoing_request(
        &mut self,
        request_id: u64,
        expected: ExpectedRemoteResponse,
    ) -> Result<(), TransportError> {
        if request_id == 0 || request_id <= self.last_outgoing_request_id {
            return Err(TransportError::MalformedFrame);
        }
        self.last_outgoing_request_id = request_id;
        if self.outgoing.insert(request_id, expected).is_some() {
            return Err(TransportError::MalformedFrame);
        }
        Ok(())
    }

    pub fn accept_response(&mut self, frame: &RemoteControlFrame) -> Result<(), TransportError> {
        let request_id = frame.request_id().ok_or(TransportError::MalformedFrame)?;
        let expected = self
            .outgoing
            .get(&request_id)
            .copied()
            .ok_or(TransportError::MalformedFrame)?;
        let matches = matches!(frame, RemoteControlFrame::RemoteError { .. })
            || matches!(
                (expected, frame),
                (
                    ExpectedRemoteResponse::RemoteHelloAck,
                    RemoteControlFrame::RemoteHelloAck { .. }
                ) | (
                    ExpectedRemoteResponse::RouteRegistered,
                    RemoteControlFrame::RouteRegistered { .. }
                ) | (
                    ExpectedRemoteResponse::PresenceRegistered,
                    RemoteControlFrame::PresenceRegistered { .. }
                ) | (
                    ExpectedRemoteResponse::DeviceGrantRegistered,
                    RemoteControlFrame::DeviceGrantRegistered { .. }
                ) | (
                    ExpectedRemoteResponse::RouteResolved,
                    RemoteControlFrame::RouteResolved { .. }
                ) | (
                    ExpectedRemoteResponse::PushAddressReplaced,
                    RemoteControlFrame::PushAddressReplaced { .. }
                ) | (
                    ExpectedRemoteResponse::PushAddressDeleted,
                    RemoteControlFrame::PushAddressDeleted { .. }
                ) | (
                    ExpectedRemoteResponse::WakeAccepted,
                    RemoteControlFrame::WakeAccepted { .. }
                ) | (
                    ExpectedRemoteResponse::DeviceGrantRevoked,
                    RemoteControlFrame::DeviceGrantRevoked { .. }
                )
            );
        if !matches {
            return Err(TransportError::MalformedFrame);
        }
        self.outgoing.remove(&request_id);
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakePayload {
    v: String,
    kind: String,
    route: String,
    wake: String,
}

impl std::fmt::Debug for WakePayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WakePayload")
            .field("kind", &"wake")
            .finish()
    }
}

impl WakePayload {
    pub fn new(route: String, wake: String) -> Result<Self, TransportError> {
        validate_random_id(&route)?;
        validate_random_id(&wake)?;
        Ok(Self {
            v: "1".to_owned(),
            kind: "wake".to_owned(),
            route,
            wake,
        })
    }

    pub fn route(&self) -> &str {
        &self.route
    }

    pub fn wake(&self) -> &str {
        &self.wake
    }

    pub fn encode(&self) -> Result<Vec<u8>, TransportError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| TransportError::MalformedFrame)?;
        if encoded.len() > MAX_PUSH_PAYLOAD_BYTES {
            return Err(TransportError::FrameTooLarge);
        }
        Ok(encoded)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, TransportError> {
        if bytes.is_empty()
            || bytes.len() > MAX_PUSH_PAYLOAD_BYTES
            || std::str::from_utf8(bytes).is_err()
        {
            return Err(if bytes.len() > MAX_PUSH_PAYLOAD_BYTES {
                TransportError::FrameTooLarge
            } else {
                TransportError::MalformedFrame
            });
        }
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let mut ignored = false;
        let payload: Self = serde_ignored::deserialize(&mut deserializer, |_| ignored = true)
            .map_err(|_| TransportError::MalformedFrame)?;
        deserializer
            .end()
            .map_err(|_| TransportError::MalformedFrame)?;
        if ignored {
            return Err(TransportError::MalformedFrame);
        }
        payload.validate()?;
        Ok(payload)
    }

    fn validate(&self) -> Result<(), TransportError> {
        if self.v != "1" || self.kind != "wake" {
            return Err(TransportError::MalformedFrame);
        }
        validate_random_id(&self.route)?;
        validate_random_id(&self.wake)
    }
}

pub fn remote_version_is_compatible(peer_version: &str) -> bool {
    parse_version(peer_version).is_some_and(|(major, minor, _)| major == 0 && minor == 1)
}

fn validate_operation(operation_id: &str, issued_at_ms: i64) -> Result<(), TransportError> {
    validate_random_id(operation_id)?;
    if issued_at_ms < 0 {
        return Err(TransportError::MalformedFrame);
    }
    Ok(())
}

fn validate_device_operation(
    operation_id: &str,
    issued_at_ms: i64,
    route_id: &str,
    device_slot_id: &str,
    access_token: &str,
) -> Result<(), TransportError> {
    validate_operation(operation_id, issued_at_ms)?;
    validate_random_id(route_id)?;
    validate_random_id(device_slot_id)?;
    validate_token(access_token)
}

pub fn validate_random_id(value: &str) -> Result<(), TransportError> {
    validate_canonical_base64(value, RANDOM_ID_BYTES)
}

fn validate_token(value: &str) -> Result<(), TransportError> {
    validate_canonical_base64(value, TOKEN_BYTES)
}

fn validate_canonical_base64(value: &str, expected_len: usize) -> Result<(), TransportError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| TransportError::MalformedFrame)?;
    if decoded.len() != expected_len || URL_SAFE_NO_PAD.encode(decoded) != value {
        return Err(TransportError::MalformedFrame);
    }
    Ok(())
}

fn validate_endpoint_id(endpoint_id: &str) -> Result<(), TransportError> {
    if endpoint_id.is_empty()
        || endpoint_id.len() > MAX_ENDPOINT_ID_BYTES
        || !endpoint_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(TransportError::MalformedFrame);
    }
    Ok(())
}

fn validate_relay_url(relay_url: &str) -> Result<(), TransportError> {
    if relay_url.len() > MAX_RELAY_URL_BYTES
        || !relay_url.starts_with("https://")
        || relay_url.bytes().any(|byte| {
            byte.is_ascii_control() || byte.is_ascii_whitespace() || matches!(byte, b'@' | b'#')
        })
    {
        return Err(TransportError::MalformedFrame);
    }
    Ok(())
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parse_version_component(parts.next()?)?;
    let minor = parse_version_component(parts.next()?)?;
    let patch = parse_version_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn parse_version_component(component: &str) -> Option<u64> {
    if component.is_empty()
        || (component.len() > 1 && component.starts_with('0'))
        || !component.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    component.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn id(byte: u8) -> String {
        URL_SAFE_NO_PAD.encode([byte; RANDOM_ID_BYTES])
    }

    fn token(byte: u8) -> String {
        URL_SAFE_NO_PAD.encode([byte; TOKEN_BYTES])
    }

    fn resolve(request_id: u64) -> RemoteControlFrame {
        RemoteControlFrame::ResolveRoute {
            request_id,
            operation_id: id(1),
            issued_at_ms: 42,
            route_id: id(2),
            device_slot_id: id(3),
            access_token: token(4),
        }
    }

    #[test]
    fn remote_version_has_a_closed_compatibility_line() {
        assert!(remote_version_is_compatible("0.1.99"));
        for invalid in ["0.2.0", "1.1.0", "0.1", "0.1.0-extra", "00.1.0"] {
            assert!(!remote_version_is_compatible(invalid), "accepted {invalid}");
        }
    }

    #[test]
    fn remote_frame_rejects_unknown_fields_and_noncanonical_credentials() {
        let valid = resolve(1).encode().expect("valid frame encodes");
        assert_eq!(
            RemoteControlFrame::decode(&valid).expect("valid frame decodes"),
            resolve(1)
        );

        let with_unknown = String::from_utf8(valid)
            .expect("json is UTF-8")
            .replace("}", ",\"business_body\":\"canary\"}");
        assert_eq!(
            RemoteControlFrame::decode(with_unknown.as_bytes()),
            Err(TransportError::MalformedFrame)
        );

        let mut noncanonical = resolve(1);
        if let RemoteControlFrame::ResolveRoute { access_token, .. } = &mut noncanonical {
            access_token.push('=');
        }
        assert_eq!(noncanonical.encode(), Err(TransportError::MalformedFrame));
    }

    #[test]
    fn correlation_rejects_reused_requests_and_wrong_response_kind() {
        let mut correlation = RemoteCorrelationState::default();
        correlation
            .accept_incoming_request(7)
            .expect("first incoming request is accepted");
        assert_eq!(
            correlation.accept_incoming_request(7),
            Err(TransportError::MalformedFrame)
        );
        correlation
            .register_outgoing_request(9, ExpectedRemoteResponse::RouteResolved)
            .expect("request is registered");
        assert_eq!(
            correlation.accept_response(&RemoteControlFrame::PushAddressDeleted { request_id: 9 }),
            Err(TransportError::MalformedFrame)
        );
        correlation
            .accept_response(&RemoteControlFrame::RouteResolved {
                request_id: 9,
                host_endpoint_id: "endpoint123".to_owned(),
                relay_url: "https://relay.example.test".to_owned(),
                expires_at_ms: 100,
            })
            .expect("matching response is accepted");
    }

    #[test]
    fn wake_payload_is_exact_bounded_and_contains_no_business_data() {
        let payload = WakePayload::new(id(5), id(6)).expect("valid payload");
        let encoded = payload.encode().expect("payload encodes");
        assert!(encoded.len() <= MAX_PUSH_PAYLOAD_BYTES);
        assert_eq!(
            String::from_utf8(encoded.clone()).expect("json is UTF-8"),
            format!(
                "{{\"v\":\"1\",\"kind\":\"wake\",\"route\":\"{}\",\"wake\":\"{}\"}}",
                id(5),
                id(6)
            )
        );
        assert_eq!(
            WakePayload::decode(&encoded).expect("payload decodes"),
            payload
        );

        let unknown = format!(
            "{{\"v\":\"1\",\"kind\":\"wake\",\"route\":\"{}\",\"wake\":\"{}\",\"body\":\"secret canary\"}}",
            id(5),
            id(6)
        );
        assert_eq!(
            WakePayload::decode(unknown.as_bytes()),
            Err(TransportError::MalformedFrame)
        );
    }

    #[test]
    fn sensitive_remote_values_are_redacted_from_debug() {
        let frame = resolve(1);
        let debug = format!("{frame:?}");
        assert!(!debug.contains(&token(4)));
        assert!(!debug.contains(&id(2)));

        let address = PushAddress {
            provider: PushProvider::FcmFid,
            opaque_address: "sensitive-fid".to_owned(),
            registered_at_ms: 1,
            expires_at_ms: 2,
        };
        assert!(!format!("{address:?}").contains("sensitive-fid"));
    }

    #[test]
    fn remote_bootstrap_is_canonical_sensitive_and_fail_closed() {
        let bootstrap = RemotePairingBootstrap {
            route_id: id(1),
            route_hint: id(2),
            device_slot_id: id(3),
            access_token: token(4),
            host_endpoint_id: "hostendpoint123".to_owned(),
            relay_url: "https://relay.example.test".to_owned(),
            service_endpoint: "rendezvous.example.test:443".to_owned(),
            service_public_key_pin: format!("sha256:{}", token(5)),
            expires_at_ms: 123,
        };
        let uri = encode_remote_bootstrap(&bootstrap).expect("bootstrap encodes");
        assert_eq!(
            decode_remote_bootstrap(&uri).expect("bootstrap decodes"),
            bootstrap
        );
        let debug = format!("{bootstrap:?}");
        assert!(!debug.contains(&bootstrap.access_token));
        assert!(!debug.contains(&bootstrap.service_endpoint));

        let padded = format!("{uri}=");
        assert_eq!(
            decode_remote_bootstrap(&padded),
            Err(TransportError::MalformedFrame)
        );
    }
}
