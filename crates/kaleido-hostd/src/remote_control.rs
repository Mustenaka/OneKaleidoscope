//! Host-side REMOTE CONTROL 0.1 state and request orchestration.
//!
//! This module owns only rendezvous metadata. It never handles UACP frames or
//! plaintext business data. The public data-plane adapter can use
//! [`RemoteHostRoute`] to configure an iroh endpoint with one explicitly
//! configured self-hosted relay and can use [`RevokeCompletion`] to disconnect
//! a slot only after the Ubuntu service has durably acknowledged revocation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use kaleido_proto::ids::DeviceId;
use kaleido_transport::bootstrap::validate_endpoint;
use kaleido_transport::private_file::PrivateFileStore;
use kaleido_transport::remote::{
    encode_remote_bootstrap, generate_remote_id, generate_remote_token, validate_random_id,
    ExpectedRemoteResponse, RemoteControlFrame, RemotePairingBootstrap,
    DEFAULT_PRESENCE_TTL_SECONDS,
};
use kaleido_transport::remote_client::{
    RemoteClientError, RemoteControlClient as TransportRemoteControlClient,
};
use kaleido_transport::tls::SpkiPin;
use serde_json::{Map, Value};
use zeroize::{Zeroize, Zeroizing};

pub const PRESENCE_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
pub const PRESENCE_TTL_SECONDS: u16 = DEFAULT_PRESENCE_TTL_SECONDS;
pub const MAX_REMOTE_DEVICES: usize = 128;
pub const MAX_WAKE_REQUESTS_PER_MINUTE: usize = 4;

const STATE_REVISION: u64 = 1;
const WAKE_WINDOW_MS: i64 = 60_000;
const MAX_DEVICE_ID_BYTES: usize = 256;

#[derive(Clone)]
pub struct RemoteControlConfig {
    pub state_file: PathBuf,
    pub service_endpoint: String,
    pub service_public_key_pin: String,
    pub host_endpoint_id: String,
    pub relay_url: String,
}

impl std::fmt::Debug for RemoteControlConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteControlConfig")
            .field("state_file", &"<PATH>")
            .field("service_endpoint", &"[redacted]")
            .field("service_public_key_pin", &"[redacted]")
            .field("host_endpoint_id", &"[redacted]")
            .field("relay_url", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RemoteControlError {
    #[error("remote control configuration is invalid")]
    Configuration,
    #[error("remote control state storage failed")]
    Storage,
    #[error("remote control state is corrupt")]
    CorruptState,
    #[error("remote control service identity validation failed")]
    ServiceIdentity,
    #[error("remote control service is unavailable")]
    Unavailable,
    #[error("remote control request was rejected")]
    Rejected,
    #[error("remote control response violated the contract")]
    Contract,
    #[error("remote control device is unknown")]
    UnknownDevice,
    #[error("remote control device is revoked")]
    Revoked,
    #[error("remote control device limit was reached")]
    DeviceLimit,
    #[error("remote wake request limit was reached")]
    WakeRateLimited,
    #[error("remote control clock moved backwards")]
    Clock,
}

/// A connected, exact-SPKI-pinned REMOTE CONTROL session.
///
/// Any ambiguous I/O or protocol failure poisons the session. The caller must
/// reconnect, which restarts the service-side request sequence at two after the
/// hello exchange performed by `RemoteControlClient::connect`.
pub struct PinnedRemoteControlSession {
    client: TransportRemoteControlClient,
    next_request_id: u64,
    usable: bool,
}

impl std::fmt::Debug for PinnedRemoteControlSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PinnedRemoteControlSession")
            .field("endpoint", &"[redacted]")
            .field("usable", &self.usable)
            .finish_non_exhaustive()
    }
}

/// Narrow request seam used by the production pinned client and deterministic
/// state-machine tests. It carries only the closed REMOTE CONTROL frame enum.
pub trait RemoteControlExchange: std::fmt::Debug {
    fn next_request_id(&self) -> Result<u64, RemoteControlError>;

    fn exchange(
        &mut self,
        frame: RemoteControlFrame,
        expected: ExpectedRemoteResponse,
    ) -> Result<RemoteControlFrame, RemoteControlError>;
}

impl RemoteControlExchange for PinnedRemoteControlSession {
    fn next_request_id(&self) -> Result<u64, RemoteControlError> {
        if self.usable {
            Ok(self.next_request_id)
        } else {
            Err(RemoteControlError::Contract)
        }
    }

    fn exchange(
        &mut self,
        frame: RemoteControlFrame,
        expected: ExpectedRemoteResponse,
    ) -> Result<RemoteControlFrame, RemoteControlError> {
        if !self.usable || frame.request_id() != Some(self.next_request_id) {
            return Err(RemoteControlError::Contract);
        }
        let following = self.next_request_id.checked_add(1);
        self.usable = false;
        let response = self
            .client
            .request(&frame, expected)
            .map_err(map_client_error)?;
        if let Some(following) = following {
            self.next_request_id = following;
            self.usable = true;
        }
        Ok(response)
    }
}

pub struct RemoteControlPlane {
    store: PrivateFileStore,
    state: DurableState,
    wake_history: BTreeMap<String, VecDeque<i64>>,
}

impl std::fmt::Debug for RemoteControlPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteControlPlane")
            .field("store", &self.store)
            .field("route", &"[redacted]")
            .field("device_count", &self.state.devices.len())
            .field("pending_revoke_count", &self.state.revoke_outbox.len())
            .finish()
    }
}

impl RemoteControlPlane {
    pub fn open(config: RemoteControlConfig) -> Result<Self, RemoteControlError> {
        validate_config(&config)?;
        let store = PrivateFileStore::new(config.state_file.clone())
            .map_err(|_| RemoteControlError::Storage)?;
        let state = match store.load().map_err(|_| RemoteControlError::Storage)? {
            Some(bytes) => parse_state(&bytes)?,
            None => {
                let generated = DurableState::new(&config);
                validate_state(&generated)?;
                persist_state(&store, &generated)?;
                let committed = store
                    .load()
                    .map_err(|_| RemoteControlError::Storage)?
                    .ok_or(RemoteControlError::Storage)?;
                parse_state(&committed)?
            }
        };
        if !state.matches_config(&config) {
            return Err(RemoteControlError::Configuration);
        }
        Ok(Self {
            store,
            state,
            wake_history: BTreeMap::new(),
        })
    }

    /// Connects with TLS 1.3 and the exact persisted Ubuntu service SPKI pin.
    pub fn connect(&self) -> Result<PinnedRemoteControlSession, RemoteControlError> {
        let client = TransportRemoteControlClient::connect(
            &self.state.service_endpoint,
            &self.state.service_public_key_pin,
        )
        .map_err(map_client_error)?;
        Ok(PinnedRemoteControlSession {
            client,
            next_request_id: 2,
            usable: true,
        })
    }

    /// Material needed by the host iroh adapter. There is no default/public
    /// relay option: the only relay value is the self-hosted URL validated at
    /// `open` time.
    pub fn host_route(&self) -> RemoteHostRoute<'_> {
        RemoteHostRoute {
            route_id: &self.state.route_id,
            admin_token: &self.state.admin_token,
            host_endpoint_id: &self.state.host_endpoint_id,
            relay_url: &self.state.relay_url,
        }
    }

    /// Durably registers the route identity without publishing presence.
    /// The host uses this to authorize its custom-relay connection, then waits
    /// for the iroh endpoint to be online before the first presence refresh.
    pub fn register_route<C: RemoteControlExchange>(
        &self,
        connection: &mut C,
        issued_at_ms: i64,
    ) -> Result<(), RemoteControlError> {
        if issued_at_ms < 0 {
            return Err(RemoteControlError::Clock);
        }
        let request_id = connection.next_request_id()?;
        let response = connection.exchange(
            RemoteControlFrame::RegisterRoute {
                request_id,
                operation_id: generate_remote_id(),
                issued_at_ms,
                route_id: self.state.route_id.clone(),
                route_hint: self.state.route_hint.clone(),
                admin_token: self.state.admin_token.clone(),
                host_endpoint_id: self.state.host_endpoint_id.clone(),
                relay_url: self.state.relay_url.clone(),
            },
            ExpectedRemoteResponse::RouteRegistered,
        )?;
        if !matches!(response, RemoteControlFrame::RouteRegistered { .. }) {
            return Err(RemoteControlError::Contract);
        }
        Ok(())
    }

    /// Registers the fixed 30-second presence. The host lifecycle must call
    /// this every [`PRESENCE_REFRESH_INTERVAL`] while the data endpoint is
    /// ready; connecting candidates must not be published as presence.
    pub fn refresh_presence<C: RemoteControlExchange>(
        &self,
        connection: &mut C,
        issued_at_ms: i64,
    ) -> Result<i64, RemoteControlError> {
        if issued_at_ms < 0 {
            return Err(RemoteControlError::Clock);
        }
        let request_id = connection.next_request_id()?;
        let response = connection.exchange(
            RemoteControlFrame::RegisterPresence {
                request_id,
                operation_id: generate_remote_id(),
                issued_at_ms,
                route_id: self.state.route_id.clone(),
                admin_token: self.state.admin_token.clone(),
                host_endpoint_id: self.state.host_endpoint_id.clone(),
                relay_url: self.state.relay_url.clone(),
                ttl_seconds: PRESENCE_TTL_SECONDS,
            },
            ExpectedRemoteResponse::PresenceRegistered,
        )?;
        let RemoteControlFrame::PresenceRegistered { expires_at_ms, .. } = response else {
            return Err(RemoteControlError::Contract);
        };
        if expires_at_ms <= issued_at_ms {
            return Err(RemoteControlError::Contract);
        }
        Ok(expires_at_ms)
    }

    /// Durably assigns a random slot/token, registers it at Ubuntu, then emits
    /// the sensitive remote pairing URI. A failed or lost response leaves a
    /// retryable pending grant with the same slot/token on disk.
    pub fn register_device_and_issue_pairing<C: RemoteControlExchange>(
        &mut self,
        connection: &mut C,
        device_id: &DeviceId,
        issued_at_ms: i64,
        bootstrap_expires_at_ms: i64,
    ) -> Result<RemotePairingUri, RemoteControlError> {
        validate_device_id(device_id.as_str())?;
        if issued_at_ms < 0 || bootstrap_expires_at_ms <= issued_at_ms {
            return Err(RemoteControlError::Clock);
        }
        if !self.state.devices.contains_key(device_id.as_str()) {
            if self.state.devices.len() >= MAX_REMOTE_DEVICES {
                return Err(RemoteControlError::DeviceLimit);
            }
            let record = DeviceRecord {
                slot_id: generate_remote_id(),
                access_token: Some(generate_remote_token()),
                status: DeviceStatus::PendingGrant,
            };
            self.commit(|state| {
                state.devices.insert(device_id.as_str().to_owned(), record);
                Ok(())
            })?;
        }

        let status = self
            .state
            .devices
            .get(device_id.as_str())
            .map(|record| record.status)
            .ok_or(RemoteControlError::UnknownDevice)?;
        match status {
            DeviceStatus::RevokePending | DeviceStatus::Revoked => {
                return Err(RemoteControlError::Revoked);
            }
            DeviceStatus::PendingGrant => {
                let (slot_id, access_token) = self.active_credential(device_id)?;
                let request_id = connection.next_request_id()?;
                let response = connection.exchange(
                    RemoteControlFrame::RegisterDeviceGrant {
                        request_id,
                        operation_id: generate_remote_id(),
                        issued_at_ms,
                        route_id: self.state.route_id.clone(),
                        device_slot_id: slot_id,
                        access_token,
                        admin_token: self.state.admin_token.clone(),
                    },
                    ExpectedRemoteResponse::DeviceGrantRegistered,
                )?;
                if !matches!(response, RemoteControlFrame::DeviceGrantRegistered { .. }) {
                    return Err(RemoteControlError::Contract);
                }
                self.commit(|state| {
                    let record = state
                        .devices
                        .get_mut(device_id.as_str())
                        .ok_or(RemoteControlError::UnknownDevice)?;
                    if record.status != DeviceStatus::PendingGrant {
                        return Err(RemoteControlError::Contract);
                    }
                    record.status = DeviceStatus::Registered;
                    Ok(())
                })?;
            }
            DeviceStatus::Registered => {}
        }
        self.pairing_uri(device_id, bootstrap_expires_at_ms)
    }

    /// Must be called only after `LanServer::revoke_device` has durably fsynced
    /// the local device registry. This method fsyncs the remote outbox before it
    /// returns, clears the access token, and immediately suppresses wake calls.
    pub fn enqueue_revoke_after_local_registry(
        &mut self,
        device_id: &DeviceId,
        enqueued_at_ms: i64,
    ) -> Result<(), RemoteControlError> {
        if enqueued_at_ms < 0 {
            return Err(RemoteControlError::Clock);
        }
        let Some(existing) = self.state.devices.get(device_id.as_str()) else {
            return Err(RemoteControlError::UnknownDevice);
        };
        if matches!(
            existing.status,
            DeviceStatus::RevokePending | DeviceStatus::Revoked
        ) {
            return Ok(());
        }
        let slot_id = existing.slot_id.clone();
        let pending = PendingRevoke {
            device_id: device_id.as_str().to_owned(),
            slot_id,
            operation_id: generate_remote_id(),
            enqueued_at_ms,
        };
        self.commit(|state| {
            let record = state
                .devices
                .get_mut(device_id.as_str())
                .ok_or(RemoteControlError::UnknownDevice)?;
            record.status = DeviceStatus::RevokePending;
            if let Some(token) = &mut record.access_token {
                token.zeroize();
            }
            record.access_token = None;
            state.revoke_outbox.push_back(pending);
            Ok(())
        })
    }

    /// Repairs the cross-store crash window where the local registry commit
    /// succeeded but the remote revoke outbox commit did not. Devices without
    /// a remote slot are ignored; known slots are durably enqueued
    /// idempotently before presence is published after restart.
    pub fn reconcile_local_revocations(
        &mut self,
        revoked_devices: &[DeviceId],
        reconciled_at_ms: i64,
    ) -> Result<usize, RemoteControlError> {
        if reconciled_at_ms < 0 {
            return Err(RemoteControlError::Clock);
        }
        let mut enqueued = 0_usize;
        for device_id in revoked_devices {
            let status = self
                .state
                .devices
                .get(device_id.as_str())
                .map(|record| record.status);
            let Some(status) = status else {
                continue;
            };
            if matches!(status, DeviceStatus::RevokePending | DeviceStatus::Revoked) {
                continue;
            }
            self.enqueue_revoke_after_local_registry(device_id, reconciled_at_ms)?;
            enqueued = enqueued
                .checked_add(1)
                .ok_or(RemoteControlError::DeviceLimit)?;
        }
        Ok(enqueued)
    }

    /// Retries the oldest durable revoke only. On success the acknowledged
    /// state is fsynced before the completion is returned to the caller, which
    /// may then disconnect the matching relay and inner-TLS connections.
    pub fn flush_next_revoke<C: RemoteControlExchange>(
        &mut self,
        connection: &mut C,
        issued_at_ms: i64,
    ) -> Result<Option<RevokeCompletion>, RemoteControlError> {
        if issued_at_ms < 0 {
            return Err(RemoteControlError::Clock);
        }
        let Some(pending) = self.state.revoke_outbox.front().cloned() else {
            return Ok(None);
        };
        let request_id = connection.next_request_id()?;
        let response = connection.exchange(
            RemoteControlFrame::RevokeDeviceGrant {
                request_id,
                operation_id: pending.operation_id.clone(),
                issued_at_ms,
                route_id: self.state.route_id.clone(),
                device_slot_id: pending.slot_id.clone(),
                admin_token: self.state.admin_token.clone(),
            },
            ExpectedRemoteResponse::DeviceGrantRevoked,
        );
        match response {
            Ok(RemoteControlFrame::DeviceGrantRevoked { .. })
            | Err(RemoteControlError::Revoked) => {}
            Ok(_) => return Err(RemoteControlError::Contract),
            Err(error) => return Err(error),
        }
        self.commit(|state| {
            let front = state
                .revoke_outbox
                .front()
                .ok_or(RemoteControlError::Contract)?;
            if front.device_id != pending.device_id
                || front.slot_id != pending.slot_id
                || front.operation_id != pending.operation_id
            {
                return Err(RemoteControlError::Contract);
            }
            let record = state
                .devices
                .get_mut(&pending.device_id)
                .ok_or(RemoteControlError::UnknownDevice)?;
            if record.status != DeviceStatus::RevokePending || record.slot_id != pending.slot_id {
                return Err(RemoteControlError::Contract);
            }
            record.status = DeviceStatus::Revoked;
            state.revoke_outbox.pop_front();
            Ok(())
        })?;
        Ok(Some(RevokeCompletion {
            device_id: DeviceId::new(pending.device_id),
            device_slot_id: pending.slot_id,
        }))
    }

    /// Sends a synchronous, random-ID wake hint. Attempts are bounded per
    /// device in memory and are suppressed as soon as revoke is enqueued.
    pub fn wake_device<C: RemoteControlExchange>(
        &mut self,
        connection: &mut C,
        device_id: &DeviceId,
        issued_at_ms: i64,
    ) -> Result<(), RemoteControlError> {
        if issued_at_ms < 0 {
            return Err(RemoteControlError::Clock);
        }
        let record = self
            .state
            .devices
            .get(device_id.as_str())
            .ok_or(RemoteControlError::UnknownDevice)?;
        if record.status != DeviceStatus::Registered {
            return Err(RemoteControlError::Revoked);
        }
        let slot_id = record.slot_id.clone();
        let history = self
            .wake_history
            .entry(device_id.as_str().to_owned())
            .or_default();
        if history.iter().any(|seen_at| *seen_at > issued_at_ms) {
            return Err(RemoteControlError::Clock);
        }
        history.retain(|seen_at| issued_at_ms.saturating_sub(*seen_at) < WAKE_WINDOW_MS);
        if history.len() >= MAX_WAKE_REQUESTS_PER_MINUTE {
            return Err(RemoteControlError::WakeRateLimited);
        }
        history.push_back(issued_at_ms);

        let request_id = connection.next_request_id()?;
        let response = connection.exchange(
            RemoteControlFrame::WakeDevice {
                request_id,
                operation_id: generate_remote_id(),
                issued_at_ms,
                route_id: self.state.route_id.clone(),
                device_slot_id: slot_id,
                admin_token: self.state.admin_token.clone(),
                wake_id: generate_remote_id(),
            },
            ExpectedRemoteResponse::WakeAccepted,
        )?;
        if !matches!(response, RemoteControlFrame::WakeAccepted { .. }) {
            return Err(RemoteControlError::Contract);
        }
        Ok(())
    }

    pub fn pending_revoke_count(&self) -> usize {
        self.state.revoke_outbox.len()
    }

    pub fn device_slot(&self, device_id: &DeviceId) -> Option<RemoteDeviceSlot<'_>> {
        self.state
            .devices
            .get(device_id.as_str())
            .map(|record| RemoteDeviceSlot {
                device_slot_id: &record.slot_id,
                revoked: record.status != DeviceStatus::Registered,
            })
    }

    fn pairing_uri(
        &self,
        device_id: &DeviceId,
        expires_at_ms: i64,
    ) -> Result<RemotePairingUri, RemoteControlError> {
        let (device_slot_id, access_token) = self.active_credential(device_id)?;
        let bootstrap = RemotePairingBootstrap {
            route_id: self.state.route_id.clone(),
            route_hint: self.state.route_hint.clone(),
            device_slot_id,
            access_token,
            host_endpoint_id: self.state.host_endpoint_id.clone(),
            relay_url: self.state.relay_url.clone(),
            service_endpoint: self.state.service_endpoint.clone(),
            service_public_key_pin: self.state.service_public_key_pin.clone(),
            expires_at_ms,
        };
        encode_remote_bootstrap(&bootstrap)
            .map(RemotePairingUri)
            .map_err(|_| RemoteControlError::Contract)
    }

    fn active_credential(
        &self,
        device_id: &DeviceId,
    ) -> Result<(String, String), RemoteControlError> {
        let record = self
            .state
            .devices
            .get(device_id.as_str())
            .ok_or(RemoteControlError::UnknownDevice)?;
        if matches!(
            record.status,
            DeviceStatus::RevokePending | DeviceStatus::Revoked
        ) {
            return Err(RemoteControlError::Revoked);
        }
        let token = record
            .access_token
            .as_ref()
            .ok_or(RemoteControlError::CorruptState)?;
        Ok((record.slot_id.clone(), token.clone()))
    }

    fn commit<F>(&mut self, mutate: F) -> Result<(), RemoteControlError>
    where
        F: FnOnce(&mut DurableState) -> Result<(), RemoteControlError>,
    {
        let mut candidate = self.state.clone();
        mutate(&mut candidate)?;
        persist_state(&self.store, &candidate)?;
        validate_state(&candidate)?;
        self.state = candidate;
        Ok(())
    }
}

/// Sensitive bootstrap URI. `Debug` never renders it and drop clears its
/// backing buffer. Callers should show `as_str()` directly to the local pairing
/// surface and must not send it to tracing, clipboard history, or analytics.
pub struct RemotePairingUri(String);

impl RemotePairingUri {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RemotePairingUri {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemotePairingUri([redacted])")
    }
}

impl Drop for RemotePairingUri {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub struct RemoteHostRoute<'a> {
    route_id: &'a str,
    admin_token: &'a str,
    host_endpoint_id: &'a str,
    relay_url: &'a str,
}

impl std::fmt::Debug for RemoteHostRoute<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteHostRoute([redacted])")
    }
}

impl<'a> RemoteHostRoute<'a> {
    pub fn route_id(&self) -> &'a str {
        self.route_id
    }

    pub fn admin_token(&self) -> &'a str {
        self.admin_token
    }

    pub fn host_endpoint_id(&self) -> &'a str {
        self.host_endpoint_id
    }

    pub fn relay_url(&self) -> &'a str {
        self.relay_url
    }
}

#[derive(Clone, Copy)]
pub struct RemoteDeviceSlot<'a> {
    device_slot_id: &'a str,
    revoked: bool,
}

impl std::fmt::Debug for RemoteDeviceSlot<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteDeviceSlot")
            .field("device_slot_id", &"[redacted]")
            .field("revoked", &self.revoked)
            .finish()
    }
}

impl<'a> RemoteDeviceSlot<'a> {
    pub fn device_slot_id(&self) -> &'a str {
        self.device_slot_id
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RevokeCompletion {
    pub device_id: DeviceId,
    device_slot_id: String,
}

impl std::fmt::Debug for RevokeCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevokeCompletion")
            .field("device_id", &"[redacted]")
            .field("device_slot_id", &"[redacted]")
            .finish()
    }
}

impl RevokeCompletion {
    pub fn device_slot_id(&self) -> &str {
        &self.device_slot_id
    }
}

#[derive(Clone)]
struct DurableState {
    route_id: String,
    route_hint: String,
    admin_token: String,
    service_endpoint: String,
    service_public_key_pin: String,
    host_endpoint_id: String,
    relay_url: String,
    devices: BTreeMap<String, DeviceRecord>,
    revoke_outbox: VecDeque<PendingRevoke>,
}

impl DurableState {
    fn new(config: &RemoteControlConfig) -> Self {
        Self {
            route_id: generate_remote_id(),
            route_hint: generate_remote_id(),
            admin_token: generate_remote_token(),
            service_endpoint: config.service_endpoint.clone(),
            service_public_key_pin: config.service_public_key_pin.clone(),
            host_endpoint_id: config.host_endpoint_id.clone(),
            relay_url: config.relay_url.clone(),
            devices: BTreeMap::new(),
            revoke_outbox: VecDeque::new(),
        }
    }

    fn matches_config(&self, config: &RemoteControlConfig) -> bool {
        self.service_endpoint == config.service_endpoint
            && self.service_public_key_pin == config.service_public_key_pin
            && self.host_endpoint_id == config.host_endpoint_id
            && self.relay_url == config.relay_url
    }
}

impl Drop for DurableState {
    fn drop(&mut self) {
        self.admin_token.zeroize();
        for record in self.devices.values_mut() {
            if let Some(token) = &mut record.access_token {
                token.zeroize();
            }
        }
    }
}

#[derive(Clone)]
struct DeviceRecord {
    slot_id: String,
    access_token: Option<String>,
    status: DeviceStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceStatus {
    PendingGrant,
    Registered,
    RevokePending,
    Revoked,
}

impl DeviceStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PendingGrant => "pending_grant",
            Self::Registered => "registered",
            Self::RevokePending => "revoke_pending",
            Self::Revoked => "revoked",
        }
    }

    fn parse(value: &str) -> Result<Self, RemoteControlError> {
        match value {
            "pending_grant" => Ok(Self::PendingGrant),
            "registered" => Ok(Self::Registered),
            "revoke_pending" => Ok(Self::RevokePending),
            "revoked" => Ok(Self::Revoked),
            _ => Err(RemoteControlError::CorruptState),
        }
    }
}

#[derive(Clone)]
struct PendingRevoke {
    device_id: String,
    slot_id: String,
    operation_id: String,
    enqueued_at_ms: i64,
}

fn validate_config(config: &RemoteControlConfig) -> Result<(), RemoteControlError> {
    validate_endpoint(&config.service_endpoint).map_err(|_| RemoteControlError::Configuration)?;
    SpkiPin::parse(&config.service_public_key_pin)
        .map_err(|_| RemoteControlError::Configuration)?;
    validate_host_endpoint_id(&config.host_endpoint_id)?;
    validate_self_hosted_relay(&config.relay_url)
}

fn validate_host_endpoint_id(value: &str) -> Result<(), RemoteControlError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(RemoteControlError::Configuration);
    }
    Ok(())
}

fn validate_self_hosted_relay(value: &str) -> Result<(), RemoteControlError> {
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("https://")
        || lower.contains("staging")
        || lower.contains("n0.computer")
        || lower.contains("n0.iroh")
        || lower.contains("iroh.link")
        || value.bytes().any(|byte| {
            byte.is_ascii_whitespace() || byte.is_ascii_control() || matches!(byte, b'@' | b'#')
        })
    {
        return Err(RemoteControlError::Configuration);
    }
    Ok(())
}

fn validate_device_id(value: &str) -> Result<(), RemoteControlError> {
    if value.is_empty()
        || value.len() > MAX_DEVICE_ID_BYTES
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(RemoteControlError::Contract);
    }
    Ok(())
}

fn validate_state(state: &DurableState) -> Result<(), RemoteControlError> {
    validate_random_id(&state.route_id).map_err(|_| RemoteControlError::CorruptState)?;
    validate_random_id(&state.route_hint).map_err(|_| RemoteControlError::CorruptState)?;
    let config = RemoteControlConfig {
        state_file: PathBuf::from("redacted"),
        service_endpoint: state.service_endpoint.clone(),
        service_public_key_pin: state.service_public_key_pin.clone(),
        host_endpoint_id: state.host_endpoint_id.clone(),
        relay_url: state.relay_url.clone(),
    };
    validate_config(&config).map_err(|_| RemoteControlError::CorruptState)?;
    let validation_frame = RemoteControlFrame::RegisterPresence {
        request_id: 2,
        operation_id: state.route_hint.clone(),
        issued_at_ms: 0,
        route_id: state.route_id.clone(),
        admin_token: state.admin_token.clone(),
        host_endpoint_id: state.host_endpoint_id.clone(),
        relay_url: state.relay_url.clone(),
        ttl_seconds: PRESENCE_TTL_SECONDS,
    };
    validation_frame
        .encode()
        .map_err(|_| RemoteControlError::CorruptState)?;
    if state.devices.len() > MAX_REMOTE_DEVICES {
        return Err(RemoteControlError::CorruptState);
    }

    let mut slots = BTreeSet::new();
    for (device_id, record) in &state.devices {
        validate_device_id(device_id).map_err(|_| RemoteControlError::CorruptState)?;
        validate_random_id(&record.slot_id).map_err(|_| RemoteControlError::CorruptState)?;
        if !slots.insert(record.slot_id.as_str()) {
            return Err(RemoteControlError::CorruptState);
        }
        let should_have_token = matches!(
            record.status,
            DeviceStatus::PendingGrant | DeviceStatus::Registered
        );
        if should_have_token != record.access_token.is_some() {
            return Err(RemoteControlError::CorruptState);
        }
        if let Some(access_token) = &record.access_token {
            RemoteControlFrame::RegisterDeviceGrant {
                request_id: 2,
                operation_id: state.route_hint.clone(),
                issued_at_ms: 0,
                route_id: state.route_id.clone(),
                device_slot_id: record.slot_id.clone(),
                access_token: access_token.clone(),
                admin_token: state.admin_token.clone(),
            }
            .encode()
            .map_err(|_| RemoteControlError::CorruptState)?;
        }
    }

    let mut pending_devices = BTreeSet::new();
    for pending in &state.revoke_outbox {
        validate_device_id(&pending.device_id).map_err(|_| RemoteControlError::CorruptState)?;
        validate_random_id(&pending.slot_id).map_err(|_| RemoteControlError::CorruptState)?;
        validate_random_id(&pending.operation_id).map_err(|_| RemoteControlError::CorruptState)?;
        if pending.enqueued_at_ms < 0 || !pending_devices.insert(pending.device_id.as_str()) {
            return Err(RemoteControlError::CorruptState);
        }
        let record = state
            .devices
            .get(&pending.device_id)
            .ok_or(RemoteControlError::CorruptState)?;
        if record.status != DeviceStatus::RevokePending || record.slot_id != pending.slot_id {
            return Err(RemoteControlError::CorruptState);
        }
    }
    for (device_id, record) in &state.devices {
        if (record.status == DeviceStatus::RevokePending)
            != pending_devices.contains(device_id.as_str())
        {
            return Err(RemoteControlError::CorruptState);
        }
    }
    Ok(())
}

fn persist_state(store: &PrivateFileStore, state: &DurableState) -> Result<(), RemoteControlError> {
    let mut value = state_to_value(state);
    let encoded = serde_json::to_vec(&value).map_err(|_| RemoteControlError::Storage);
    zeroize_json(&mut value);
    let encoded = Zeroizing::new(encoded?);
    store
        .store(&encoded)
        .map_err(|_| RemoteControlError::Storage)
}

fn state_to_value(state: &DurableState) -> Value {
    let devices = state
        .devices
        .iter()
        .map(|(device_id, record)| {
            let mut object = Map::new();
            object.insert("device_id".to_owned(), Value::String(device_id.clone()));
            object.insert("slot_id".to_owned(), Value::String(record.slot_id.clone()));
            object.insert(
                "access_token".to_owned(),
                record
                    .access_token
                    .as_ref()
                    .map_or(Value::Null, |value| Value::String(value.clone())),
            );
            object.insert(
                "status".to_owned(),
                Value::String(record.status.as_str().to_owned()),
            );
            Value::Object(object)
        })
        .collect();
    let revoke_outbox = state
        .revoke_outbox
        .iter()
        .map(|pending| {
            let mut object = Map::new();
            object.insert(
                "device_id".to_owned(),
                Value::String(pending.device_id.clone()),
            );
            object.insert("slot_id".to_owned(), Value::String(pending.slot_id.clone()));
            object.insert(
                "operation_id".to_owned(),
                Value::String(pending.operation_id.clone()),
            );
            object.insert(
                "enqueued_at_ms".to_owned(),
                Value::Number(pending.enqueued_at_ms.into()),
            );
            Value::Object(object)
        })
        .collect();
    let mut root = Map::new();
    root.insert("revision".to_owned(), Value::Number(STATE_REVISION.into()));
    root.insert("route_id".to_owned(), Value::String(state.route_id.clone()));
    root.insert(
        "route_hint".to_owned(),
        Value::String(state.route_hint.clone()),
    );
    root.insert(
        "admin_token".to_owned(),
        Value::String(state.admin_token.clone()),
    );
    root.insert(
        "service_endpoint".to_owned(),
        Value::String(state.service_endpoint.clone()),
    );
    root.insert(
        "service_public_key_pin".to_owned(),
        Value::String(state.service_public_key_pin.clone()),
    );
    root.insert(
        "host_endpoint_id".to_owned(),
        Value::String(state.host_endpoint_id.clone()),
    );
    root.insert(
        "relay_url".to_owned(),
        Value::String(state.relay_url.clone()),
    );
    root.insert("devices".to_owned(), Value::Array(devices));
    root.insert("revoke_outbox".to_owned(), Value::Array(revoke_outbox));
    Value::Object(root)
}

fn parse_state(bytes: &[u8]) -> Result<DurableState, RemoteControlError> {
    if bytes.is_empty() || bytes.len() > 1_048_576 {
        return Err(RemoteControlError::CorruptState);
    }
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|_| RemoteControlError::CorruptState)?;
    let parsed = (|| {
        let root = exact_object(
            &value,
            &[
                "revision",
                "route_id",
                "route_hint",
                "admin_token",
                "service_endpoint",
                "service_public_key_pin",
                "host_endpoint_id",
                "relay_url",
                "devices",
                "revoke_outbox",
            ],
        )?;
        if integer(root, "revision")?
            != i64::try_from(STATE_REVISION).map_err(|_| RemoteControlError::CorruptState)?
        {
            return Err(RemoteControlError::CorruptState);
        }
        let mut devices = BTreeMap::new();
        let device_values = root
            .get("devices")
            .and_then(Value::as_array)
            .ok_or(RemoteControlError::CorruptState)?;
        for value in device_values {
            let object = exact_object(value, &["device_id", "slot_id", "access_token", "status"])?;
            let device_id = string(object, "device_id")?;
            let record = DeviceRecord {
                slot_id: string(object, "slot_id")?,
                access_token: optional_string(object, "access_token")?,
                status: DeviceStatus::parse(string_ref(object, "status")?)?,
            };
            if devices.insert(device_id, record).is_some() {
                return Err(RemoteControlError::CorruptState);
            }
        }
        let mut revoke_outbox = VecDeque::new();
        let pending_values = root
            .get("revoke_outbox")
            .and_then(Value::as_array)
            .ok_or(RemoteControlError::CorruptState)?;
        for value in pending_values {
            let object = exact_object(
                value,
                &["device_id", "slot_id", "operation_id", "enqueued_at_ms"],
            )?;
            revoke_outbox.push_back(PendingRevoke {
                device_id: string(object, "device_id")?,
                slot_id: string(object, "slot_id")?,
                operation_id: string(object, "operation_id")?,
                enqueued_at_ms: integer(object, "enqueued_at_ms")?,
            });
        }
        let state = DurableState {
            route_id: string(root, "route_id")?,
            route_hint: string(root, "route_hint")?,
            admin_token: string(root, "admin_token")?,
            service_endpoint: string(root, "service_endpoint")?,
            service_public_key_pin: string(root, "service_public_key_pin")?,
            host_endpoint_id: string(root, "host_endpoint_id")?,
            relay_url: string(root, "relay_url")?,
            devices,
            revoke_outbox,
        };
        validate_state(&state)?;
        Ok(state)
    })();
    zeroize_json(&mut value);
    parsed
}

fn exact_object<'a>(
    value: &'a Value,
    fields: &[&str],
) -> Result<&'a Map<String, Value>, RemoteControlError> {
    let object = value.as_object().ok_or(RemoteControlError::CorruptState)?;
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return Err(RemoteControlError::CorruptState);
    }
    Ok(object)
}

fn string(object: &Map<String, Value>, field: &str) -> Result<String, RemoteControlError> {
    string_ref(object, field).map(str::to_owned)
}

fn string_ref<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, RemoteControlError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or(RemoteControlError::CorruptState)
}

fn optional_string(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, RemoteControlError> {
    let value = object.get(field).ok_or(RemoteControlError::CorruptState)?;
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or(RemoteControlError::CorruptState)
    }
}

fn integer(object: &Map<String, Value>, field: &str) -> Result<i64, RemoteControlError> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .ok_or(RemoteControlError::CorruptState)
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        Value::Object(object) => object.values_mut().for_each(zeroize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn map_client_error(error: RemoteClientError) -> RemoteControlError {
    match error {
        RemoteClientError::Security => RemoteControlError::ServiceIdentity,
        RemoteClientError::Rejected(kaleido_transport::remote::RemoteErrorCode::Revoked) => {
            RemoteControlError::Revoked
        }
        RemoteClientError::Rejected(_) => RemoteControlError::Rejected,
        RemoteClientError::Contract => RemoteControlError::Contract,
        RemoteClientError::Connect | RemoteClientError::Io => RemoteControlError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::fs;

    use kaleido_transport::remote::{decode_remote_bootstrap, RemoteControlFrame};

    use super::*;

    #[derive(Debug, Default)]
    struct FakeExchange {
        next_request_id: u64,
        requests: Vec<RemoteControlFrame>,
        fail_next: Option<RemoteControlError>,
        inspect: Option<RemoteControlConfig>,
    }

    impl FakeExchange {
        fn ready() -> Self {
            Self {
                next_request_id: 2,
                ..Self::default()
            }
        }
    }

    impl RemoteControlExchange for FakeExchange {
        fn next_request_id(&self) -> Result<u64, RemoteControlError> {
            Ok(self.next_request_id)
        }

        fn exchange(
            &mut self,
            frame: RemoteControlFrame,
            expected: ExpectedRemoteResponse,
        ) -> Result<RemoteControlFrame, RemoteControlError> {
            assert_eq!(frame.request_id(), Some(self.next_request_id));
            self.next_request_id = self.next_request_id.checked_add(1).expect("request ID");
            if let Some(config) = &self.inspect {
                let reloaded = RemoteControlPlane::open(config.clone()).expect("inspect state");
                assert!(reloaded.pending_revoke_count() > 0);
            }
            self.requests.push(frame);
            if let Some(error) = self.fail_next.take() {
                return Err(error);
            }
            let request_id = self.next_request_id - 1;
            Ok(match expected {
                ExpectedRemoteResponse::RouteRegistered => {
                    RemoteControlFrame::RouteRegistered { request_id }
                }
                ExpectedRemoteResponse::PresenceRegistered => {
                    RemoteControlFrame::PresenceRegistered {
                        request_id,
                        expires_at_ms: 31_000,
                    }
                }
                ExpectedRemoteResponse::DeviceGrantRegistered => {
                    RemoteControlFrame::DeviceGrantRegistered { request_id }
                }
                ExpectedRemoteResponse::WakeAccepted => {
                    RemoteControlFrame::WakeAccepted { request_id }
                }
                ExpectedRemoteResponse::DeviceGrantRevoked => {
                    RemoteControlFrame::DeviceGrantRevoked { request_id }
                }
                _ => return Err(RemoteControlError::Contract),
            })
        }
    }

    #[test]
    fn route_registration_precedes_presence_without_publishing_a_candidate() {
        let directory = tempfile::tempdir().expect("directory");
        let plane =
            RemoteControlPlane::open(config(directory.path().join("private").join("remote.json")))
                .expect("plane");
        let mut exchange = FakeExchange::ready();

        plane
            .register_route(&mut exchange, 1_000)
            .expect("durable route");
        assert!(matches!(
            exchange.requests.as_slice(),
            [RemoteControlFrame::RegisterRoute { .. }]
        ));

        plane
            .refresh_presence(&mut exchange, 1_001)
            .expect("ready presence");
        assert!(matches!(
            exchange.requests.as_slice(),
            [
                RemoteControlFrame::RegisterRoute { .. },
                RemoteControlFrame::RegisterPresence { .. }
            ]
        ));
    }

    #[test]
    fn route_and_device_material_survive_restart_and_corruption_fails_loud() {
        let directory = tempfile::tempdir().expect("directory");
        let config = config(directory.path().join("private").join("remote.json"));
        let mut plane = RemoteControlPlane::open(config.clone()).expect("first open");
        let route_id = plane.host_route().route_id().to_owned();
        let admin_token = plane.host_route().admin_token().to_owned();
        let device = DeviceId::new("dev_persistence");
        let mut exchange = FakeExchange::ready();
        let uri = plane
            .register_device_and_issue_pairing(&mut exchange, &device, 1_000, 61_000)
            .expect("register");
        let bootstrap = decode_remote_bootstrap(uri.as_str()).expect("decode bootstrap");
        let slot = bootstrap.device_slot_id.clone();
        let access = bootstrap.access_token.clone();
        drop(uri);
        drop(plane);

        let reopened = RemoteControlPlane::open(config.clone()).expect("reopen");
        assert_eq!(reopened.host_route().route_id(), route_id);
        assert_eq!(reopened.host_route().admin_token(), admin_token);
        assert_eq!(
            reopened
                .device_slot(&device)
                .expect("device slot")
                .device_slot_id(),
            slot
        );
        let persisted = fs::read(&config.state_file).expect("state bytes");
        assert!(String::from_utf8_lossy(&persisted).contains(&access));

        fs::write(
            &config.state_file,
            b"{\"revision\":1,\"admin_token\":\"truncated\"}",
        )
        .expect("corrupt state");
        assert!(matches!(
            RemoteControlPlane::open(config),
            Err(RemoteControlError::CorruptState)
        ));
    }

    #[test]
    fn revoke_is_outboxed_before_network_retried_in_fifo_and_completed_before_disconnect() {
        let directory = tempfile::tempdir().expect("directory");
        let config = config(directory.path().join("private").join("remote.json"));
        let mut plane = RemoteControlPlane::open(config.clone()).expect("open");
        let first = DeviceId::new("dev_first");
        let second = DeviceId::new("dev_second");
        let mut registration = FakeExchange::ready();
        plane
            .register_device_and_issue_pairing(&mut registration, &first, 1_000, 61_000)
            .expect("first grant");
        plane
            .register_device_and_issue_pairing(&mut registration, &second, 1_001, 61_001)
            .expect("second grant");
        let first_slot = plane
            .device_slot(&first)
            .expect("first slot")
            .device_slot_id()
            .to_owned();
        plane
            .enqueue_revoke_after_local_registry(&first, 2_000)
            .expect("queue first");
        plane
            .enqueue_revoke_after_local_registry(&second, 2_001)
            .expect("queue second");
        assert_eq!(plane.pending_revoke_count(), 2);
        assert!(plane.device_slot(&first).expect("first").is_revoked());

        let mut failed = FakeExchange::ready();
        failed.fail_next = Some(RemoteControlError::Unavailable);
        assert_eq!(
            plane.flush_next_revoke(&mut failed, 3_000),
            Err(RemoteControlError::Unavailable)
        );
        assert_eq!(plane.pending_revoke_count(), 2);

        let mut retry = FakeExchange::ready();
        retry.inspect = Some(config.clone());
        let completion = plane
            .flush_next_revoke(&mut retry, 3_001)
            .expect("retry")
            .expect("completion");
        assert_eq!(completion.device_id, first);
        assert_eq!(completion.device_slot_id(), first_slot);
        assert_eq!(plane.pending_revoke_count(), 1);
        let requested_slot = retry.requests.first().and_then(|frame| match frame {
            RemoteControlFrame::RevokeDeviceGrant { device_slot_id, .. } => {
                Some(device_slot_id.as_str())
            }
            _ => None,
        });
        assert_eq!(requested_slot, Some(first_slot.as_str()));

        let reopened = RemoteControlPlane::open(config).expect("reopen after ack");
        assert_eq!(reopened.pending_revoke_count(), 1);
        assert!(reopened.device_slot(&first).expect("first").is_revoked());
    }

    #[test]
    fn startup_reconciliation_repairs_a_local_commit_remote_outbox_crash_window() {
        let directory = tempfile::tempdir().expect("directory");
        let config = config(directory.path().join("private").join("remote.json"));
        let device = DeviceId::new("dev_crash_window");
        let mut plane = RemoteControlPlane::open(config.clone()).expect("open");
        let mut registration = FakeExchange::ready();
        plane
            .register_device_and_issue_pairing(&mut registration, &device, 1_000, 61_000)
            .expect("registered remote grant");

        assert_eq!(
            plane
                .reconcile_local_revocations(
                    &[DeviceId::new("dev_lan_only"), device.clone()],
                    2_000,
                )
                .expect("reconcile local durable revoke"),
            1
        );
        assert_eq!(
            plane
                .reconcile_local_revocations(std::slice::from_ref(&device), 2_001)
                .expect("idempotent reconcile"),
            0
        );
        assert_eq!(plane.pending_revoke_count(), 1);
        assert!(plane.device_slot(&device).expect("slot").is_revoked());
        drop(plane);

        let mut reopened = RemoteControlPlane::open(config).expect("reopen pending repair");
        assert_eq!(reopened.pending_revoke_count(), 1);
        let mut service = FakeExchange::ready();
        assert_eq!(
            reopened
                .flush_next_revoke(&mut service, 3_000)
                .expect("flush repaired revoke")
                .expect("completion")
                .device_id,
            device
        );
        assert_eq!(reopened.pending_revoke_count(), 0);
    }

    #[test]
    fn ack_lost_revoke_accepts_only_remote_revoked_as_terminal() {
        let directory = tempfile::tempdir().expect("directory");
        let config = config(directory.path().join("private").join("remote.json"));
        let mut plane = RemoteControlPlane::open(config).expect("open");
        let acknowledged = DeviceId::new("dev_ack_lost");
        let rejected = DeviceId::new("dev_rejected");
        let mut registration = FakeExchange::ready();
        plane
            .register_device_and_issue_pairing(&mut registration, &acknowledged, 1_000, 61_000)
            .expect("first grant");
        plane
            .register_device_and_issue_pairing(&mut registration, &rejected, 1_001, 61_001)
            .expect("second grant");
        plane
            .enqueue_revoke_after_local_registry(&acknowledged, 2_000)
            .expect("first revoke");

        let mut lost_ack = FakeExchange::ready();
        lost_ack.fail_next = Some(RemoteControlError::Unavailable);
        assert_eq!(
            plane.flush_next_revoke(&mut lost_ack, 3_000),
            Err(RemoteControlError::Unavailable)
        );
        assert_eq!(plane.pending_revoke_count(), 1);

        let mut already_revoked = FakeExchange::ready();
        already_revoked.fail_next = Some(RemoteControlError::Revoked);
        let completion = plane
            .flush_next_revoke(&mut already_revoked, 3_001)
            .expect("terminal revoked")
            .expect("completion");
        assert_eq!(completion.device_id, acknowledged);
        assert_eq!(plane.pending_revoke_count(), 0);

        plane
            .enqueue_revoke_after_local_registry(&rejected, 4_000)
            .expect("second revoke");
        let mut other_rejection = FakeExchange::ready();
        other_rejection.fail_next = Some(RemoteControlError::Rejected);
        assert_eq!(
            plane.flush_next_revoke(&mut other_rejection, 4_001),
            Err(RemoteControlError::Rejected)
        );
        assert_eq!(plane.pending_revoke_count(), 1);
    }

    #[test]
    fn presence_and_wake_are_fixed_and_bounded() {
        let directory = tempfile::tempdir().expect("directory");
        let mut plane =
            RemoteControlPlane::open(config(directory.path().join("private").join("remote.json")))
                .expect("open");
        let mut exchange = FakeExchange::ready();
        assert_eq!(
            plane
                .refresh_presence(&mut exchange, 1_000)
                .expect("presence"),
            31_000
        );
        let ttl_seconds = exchange.requests.first().and_then(|frame| match frame {
            RemoteControlFrame::RegisterPresence { ttl_seconds, .. } => Some(*ttl_seconds),
            _ => None,
        });
        assert_eq!(ttl_seconds, Some(30));
        assert_eq!(PRESENCE_REFRESH_INTERVAL, Duration::from_secs(10));

        let device = DeviceId::new("dev_wake");
        plane
            .register_device_and_issue_pairing(&mut exchange, &device, 1_001, 61_001)
            .expect("grant");
        for offset in 0..MAX_WAKE_REQUESTS_PER_MINUTE {
            let at_ms = 2_000 + i64::try_from(offset).expect("offset");
            plane
                .wake_device(&mut exchange, &device, at_ms)
                .expect("bounded wake");
        }
        assert_eq!(
            plane.wake_device(&mut exchange, &device, 2_010),
            Err(RemoteControlError::WakeRateLimited)
        );
    }

    #[test]
    fn debug_and_errors_redact_every_remote_secret_and_endpoint() {
        let directory = tempfile::tempdir().expect("directory");
        let config = config(
            directory
                .path()
                .join("private")
                .join("remote-secret-state.json"),
        );
        let mut plane = RemoteControlPlane::open(config.clone()).expect("open");
        let device = DeviceId::new("dev_debug_secret");
        let mut exchange = FakeExchange::ready();
        let uri = plane
            .register_device_and_issue_pairing(&mut exchange, &device, 1_000, 61_000)
            .expect("grant");
        let bootstrap = decode_remote_bootstrap(uri.as_str()).expect("bootstrap");
        let state_path = config.state_file.to_string_lossy().into_owned();
        let samples = [
            format!("{config:?}"),
            format!("{plane:?}"),
            format!("{:?}", plane.host_route()),
            format!("{:?}", plane.device_slot(&device).expect("slot")),
            format!("{uri:?}"),
            RemoteControlError::Rejected.to_string(),
        ];
        for rendered in samples {
            for sensitive in [
                state_path.as_str(),
                config.service_endpoint.as_str(),
                config.service_public_key_pin.as_str(),
                config.host_endpoint_id.as_str(),
                config.relay_url.as_str(),
                plane.host_route().route_id(),
                plane.host_route().admin_token(),
                bootstrap.device_slot_id.as_str(),
                bootstrap.access_token.as_str(),
                uri.as_str(),
            ] {
                assert!(!rendered.contains(sensitive), "leaked sensitive value");
            }
        }
    }

    #[test]
    fn invalid_pin_and_public_relay_are_rejected_before_state_creation() {
        let directory = tempfile::tempdir().expect("directory");
        let mut invalid_pin = config(directory.path().join("invalid-pin").join("state.json"));
        invalid_pin.service_public_key_pin = "sha256:not-a-pin".to_owned();
        assert!(matches!(
            RemoteControlPlane::open(invalid_pin),
            Err(RemoteControlError::Configuration)
        ));

        let mut public = config(directory.path().join("public").join("state.json"));
        public.relay_url = "https://use1-1.relay.n0.iroh.computer".to_owned();
        assert!(matches!(
            RemoteControlPlane::open(public),
            Err(RemoteControlError::Configuration)
        ));
    }

    fn config(state_file: PathBuf) -> RemoteControlConfig {
        RemoteControlConfig {
            state_file,
            service_endpoint: "127.0.0.1:7443".to_owned(),
            service_public_key_pin: format!("sha256:{}", "A".repeat(43)),
            host_endpoint_id: "ABCDEF0123456789".to_owned(),
            relay_url: "https://relay.self-hosted.example.test".to_owned(),
        }
    }
}
