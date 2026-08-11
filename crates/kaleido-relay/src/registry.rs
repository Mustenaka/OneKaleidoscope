use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::error::{RemoteError, RemoteErrorCode, RemoteResult};
use crate::ids::{
    admin_digest, token_digest, AccessToken, DeviceSlotId, HostEndpointId, RouteAdminToken, RouteId,
};
use crate::protocol::{now_ms, rejected};
use crate::push::PushAddress;

pub const MIN_PRESENCE_TTL_SECS: u64 = 15;
pub const MAX_PRESENCE_TTL_SECS: u64 = 90;
pub const MAX_ROUTES: usize = 1_024;
pub const MAX_GRANTS_PER_ROUTE: usize = 128;

#[derive(Debug, Clone)]
pub struct RegistryConfig {
    path: Option<PathBuf>,
}

impl RegistryConfig {
    pub fn ephemeral() -> Self {
        Self { path: None }
    }

    pub fn durable(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }
}

#[derive(Clone)]
pub struct Registry {
    state: Arc<RwLock<RegistryState>>,
    path: Option<PathBuf>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self
            .state
            .read()
            .map(|value| value.routes.len())
            .unwrap_or(0);
        formatter
            .debug_struct("Registry")
            .field("durable", &self.path.is_some())
            .field("route_count", &state)
            .finish()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryState {
    routes: BTreeMap<RouteId, RouteRecord>,
    pushes: BTreeMap<RouteId, BTreeMap<DeviceSlotId, PushAddress>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteRecord {
    route_hint: RouteId,
    host_endpoint: HostEndpointId,
    relay_url: String,
    admin_digest: [u8; 32],
    grants: BTreeMap<DeviceSlotId, GrantRecord>,
    revoked: BTreeMap<DeviceSlotId, [u8; 32]>,
    #[serde(skip)]
    presence: Option<PresenceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantRecord {
    token_digest: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresenceRecord {
    host_endpoint: HostEndpointId,
    relay_url: String,
    expires_at_ms: u64,
}

#[derive(Clone)]
pub struct RouteBootstrap {
    pub route_id: RouteId,
    pub admin_token: RouteAdminToken,
    pub host_endpoint: HostEndpointId,
    pub relay_url: String,
}

impl fmt::Debug for RouteBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouteBootstrap")
            .field("route_id", &self.route_id)
            .field("admin_token", &"[redacted]")
            .field("host_endpoint", &self.host_endpoint)
            .field("relay_url", &self.relay_url)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct RouteRegistration {
    pub route_id: RouteId,
    pub route_hint: RouteId,
    pub admin_token: RouteAdminToken,
    pub host_endpoint: HostEndpointId,
    pub relay_url: String,
}

#[derive(Debug, Clone)]
pub struct PresenceRegistration {
    pub route_id: RouteId,
    pub admin_token: RouteAdminToken,
    pub host_endpoint: HostEndpointId,
    pub relay_url: String,
    pub ttl_secs: u64,
}

#[derive(Clone)]
pub struct DeviceGrant {
    pub route_id: RouteId,
    pub slot_id: DeviceSlotId,
    pub access_token: AccessToken,
}

#[derive(Clone)]
pub struct DeviceGrantRegistration {
    pub route_id: RouteId,
    pub slot_id: DeviceSlotId,
    pub access_token: AccessToken,
    pub admin_token: RouteAdminToken,
}

impl fmt::Debug for DeviceGrantRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceGrantRegistration")
            .field("route_id", &self.route_id)
            .field("slot_id", &self.slot_id)
            .field("access_token", &"[redacted]")
            .field("admin_token", &"[redacted]")
            .finish()
    }
}

impl fmt::Debug for DeviceGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceGrant")
            .field("route_id", &self.route_id)
            .field("slot_id", &self.slot_id)
            .field("access_token", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveResponse {
    pub host_endpoint: HostEndpointId,
    pub relay_url: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BearerIdentity {
    pub route_id: RouteId,
    pub slot_id: Option<DeviceSlotId>,
    pub expected_host_endpoint: Option<HostEndpointId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BearerFailure {
    Invalid,
    Revoked,
}

impl Registry {
    pub fn open(config: RegistryConfig) -> RemoteResult<Self> {
        let state = match config.path.as_deref() {
            Some(path) if path.exists() => {
                ensure_owner_only(path, false)?;
                let mut bytes = Vec::new();
                File::open(path)
                    .and_then(|mut file| file.read_to_end(&mut bytes))
                    .map_err(RemoteError::Storage)?;
                serde_json::from_slice(&bytes).map_err(RemoteError::Encoding)?
            }
            Some(path) => {
                if let Some(parent) = path.parent() {
                    prepare_private_parent(parent)?;
                }
                let state = RegistryState::default();
                write_state(path, &state)?;
                state
            }
            None => RegistryState::default(),
        };
        validate_state(&state)?;
        Ok(Self {
            state: Arc::new(RwLock::new(state)),
            path: config.path,
        })
    }

    pub fn new_ephemeral() -> Self {
        Self::open(RegistryConfig::ephemeral()).unwrap_or_else(|_| Self {
            state: Arc::new(RwLock::new(RegistryState::default())),
            path: None,
        })
    }

    pub fn create_route(
        &self,
        host_endpoint: HostEndpointId,
        relay_url: String,
    ) -> RemoteResult<RouteBootstrap> {
        validate_relay_url(&relay_url)?;
        let route_id = RouteId::random();
        let admin_token = RouteAdminToken::random();
        let record = RouteRecord {
            route_hint: RouteId::random(),
            host_endpoint,
            relay_url: relay_url.clone(),
            admin_digest: admin_digest(&admin_token),
            grants: BTreeMap::new(),
            revoked: BTreeMap::new(),
            presence: None,
        };
        self.commit(|state| {
            if state.routes.contains_key(&route_id) {
                return Err(rejected(RemoteErrorCode::Internal));
            }
            if state.routes.len() >= MAX_ROUTES {
                return Err(rejected(RemoteErrorCode::LimitExceeded));
            }
            state.routes.insert(route_id, record);
            Ok(())
        })?;
        Ok(RouteBootstrap {
            route_id,
            admin_token,
            host_endpoint,
            relay_url,
        })
    }

    pub fn register_presence(&self, registration: PresenceRegistration) -> RemoteResult<()> {
        validate_ttl(registration.ttl_secs)?;
        validate_relay_url(&registration.relay_url)?;
        self.commit(|state| {
            let route = state
                .routes
                .get_mut(&registration.route_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            let presented_admin = admin_digest(&registration.admin_token);
            if !digest_matches(&route.admin_digest, &presented_admin) {
                return Err(rejected(RemoteErrorCode::AuthenticationFailed));
            }
            if route.host_endpoint != registration.host_endpoint
                || route.relay_url != registration.relay_url
            {
                return Err(rejected(RemoteErrorCode::AuthenticationFailed));
            }
            route.presence = Some(PresenceRecord {
                host_endpoint: registration.host_endpoint,
                relay_url: registration.relay_url.clone(),
                expires_at_ms: now_ms().saturating_add(registration.ttl_secs.saturating_mul(1_000)),
            });
            Ok(())
        })
    }

    pub fn register_route(&self, registration: RouteRegistration) -> RemoteResult<()> {
        validate_relay_url(&registration.relay_url)?;
        self.commit(|state| {
            let presented_admin = admin_digest(&registration.admin_token);
            if let Some(route) = state.routes.get(&registration.route_id) {
                if !digest_matches(&route.admin_digest, &presented_admin)
                    || route.route_hint != registration.route_hint
                    || route.host_endpoint != registration.host_endpoint
                    || route.relay_url != registration.relay_url
                {
                    return Err(rejected(RemoteErrorCode::AuthenticationFailed));
                }
                return Ok(());
            }
            if state.routes.len() >= MAX_ROUTES {
                return Err(rejected(RemoteErrorCode::LimitExceeded));
            }
            state.routes.insert(
                registration.route_id,
                RouteRecord {
                    route_hint: registration.route_hint,
                    host_endpoint: registration.host_endpoint,
                    relay_url: registration.relay_url,
                    admin_digest: presented_admin,
                    grants: BTreeMap::new(),
                    revoked: BTreeMap::new(),
                    presence: None,
                },
            );
            Ok(())
        })
    }

    pub fn grant_device(
        &self,
        route_id: RouteId,
        admin_token: &RouteAdminToken,
    ) -> RemoteResult<DeviceGrant> {
        let slot_id = DeviceSlotId::random();
        let access_token = AccessToken::random();
        self.commit(|state| {
            let route = state
                .routes
                .get_mut(&route_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            if !digest_matches(&route.admin_digest, &admin_digest(admin_token)) {
                return Err(rejected(RemoteErrorCode::AuthenticationFailed));
            }
            if route.grants.len().saturating_add(route.revoked.len()) >= MAX_GRANTS_PER_ROUTE {
                return Err(rejected(RemoteErrorCode::LimitExceeded));
            }
            route.revoked.remove(&slot_id);
            route.grants.insert(
                slot_id,
                GrantRecord {
                    token_digest: token_digest(&access_token),
                },
            );
            Ok(())
        })?;
        Ok(DeviceGrant {
            route_id,
            slot_id,
            access_token,
        })
    }

    pub fn register_device_grant(&self, registration: DeviceGrantRegistration) -> RemoteResult<()> {
        self.commit(|state| {
            let route = state
                .routes
                .get_mut(&registration.route_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            if !digest_matches(
                &route.admin_digest,
                &admin_digest(&registration.admin_token),
            ) {
                return Err(rejected(RemoteErrorCode::AuthenticationFailed));
            }
            if route.revoked.contains_key(&registration.slot_id) {
                return Err(rejected(RemoteErrorCode::Revoked));
            }
            let presented = token_digest(&registration.access_token);
            if let Some(existing) = route.grants.get(&registration.slot_id) {
                if !digest_matches(&existing.token_digest, &presented) {
                    return Err(rejected(RemoteErrorCode::AuthenticationFailed));
                }
                return Ok(());
            }
            if route.grants.len().saturating_add(route.revoked.len()) >= MAX_GRANTS_PER_ROUTE {
                return Err(rejected(RemoteErrorCode::LimitExceeded));
            }
            route.grants.insert(
                registration.slot_id,
                GrantRecord {
                    token_digest: presented,
                },
            );
            Ok(())
        })
    }

    pub fn revoke_device(
        &self,
        route_id: RouteId,
        admin_token: &RouteAdminToken,
        slot_id: DeviceSlotId,
    ) -> RemoteResult<()> {
        self.commit(|state| {
            let route = state
                .routes
                .get_mut(&route_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            if !digest_matches(&route.admin_digest, &admin_digest(admin_token)) {
                return Err(rejected(RemoteErrorCode::AuthenticationFailed));
            }
            if let Some(grant) = route.grants.remove(&slot_id) {
                route.revoked.insert(slot_id, grant.token_digest);
            } else {
                return Err(rejected(RemoteErrorCode::Revoked));
            }
            remove_push(state, route_id, slot_id);
            Ok(())
        })
    }

    pub fn register_push(
        &self,
        route_id: RouteId,
        slot_id: DeviceSlotId,
        access_token: &AccessToken,
        address: PushAddress,
    ) -> RemoteResult<()> {
        if address.provider != crate::push::PushProvider::FcmFid {
            return Err(rejected(RemoteErrorCode::MalformedFrame));
        }
        let now = now_ms();
        if address.expires_at_ms <= now {
            return Err(rejected(RemoteErrorCode::Expired));
        }
        self.commit(|state| {
            let route = state
                .routes
                .get(&route_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            let grant = route
                .grants
                .get(&slot_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            if !digest_matches(&grant.token_digest, &token_digest(access_token)) {
                return Err(rejected(RemoteErrorCode::AuthenticationFailed));
            }
            state
                .pushes
                .entry(route_id)
                .or_default()
                .insert(slot_id, address);
            Ok(())
        })
    }

    pub fn delete_push(
        &self,
        route_id: RouteId,
        slot_id: DeviceSlotId,
        access_token: &AccessToken,
    ) -> RemoteResult<()> {
        self.commit(|state| {
            let route = state
                .routes
                .get(&route_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            let grant = route
                .grants
                .get(&slot_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            if !digest_matches(&grant.token_digest, &token_digest(access_token)) {
                return Err(rejected(RemoteErrorCode::RouteUnavailable));
            }
            remove_push(state, route_id, slot_id);
            Ok(())
        })
    }

    pub fn push_address(
        &self,
        route_id: RouteId,
        slot_id: DeviceSlotId,
        admin_token: &RouteAdminToken,
    ) -> RemoteResult<Option<(PushAddress, RouteId)>> {
        let state = self
            .state
            .read()
            .map_err(|_| rejected(RemoteErrorCode::Internal))?;
        let route = state
            .routes
            .get(&route_id)
            .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
        if !digest_matches(&route.admin_digest, &admin_digest(admin_token))
            || !route.grants.contains_key(&slot_id)
            || route.revoked.contains_key(&slot_id)
        {
            return Err(rejected(RemoteErrorCode::RouteUnavailable));
        }
        Ok(state
            .pushes
            .get(&route_id)
            .and_then(|pushes| pushes.get(&slot_id))
            .cloned()
            .map(|address| (address, route.route_hint)))
    }

    pub(crate) fn delete_push_for_service(
        &self,
        route_id: RouteId,
        slot_id: DeviceSlotId,
    ) -> RemoteResult<()> {
        self.commit(|state| {
            let route = state
                .routes
                .get(&route_id)
                .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
            if !route.grants.contains_key(&slot_id) || route.revoked.contains_key(&slot_id) {
                return Err(rejected(RemoteErrorCode::RouteUnavailable));
            }
            remove_push(state, route_id, slot_id);
            Ok(())
        })
    }

    pub fn resolve(
        &self,
        route_id: RouteId,
        slot_id: DeviceSlotId,
        access_token: &AccessToken,
        at_ms: u64,
    ) -> RemoteResult<ResolveResponse> {
        let state = self
            .state
            .read()
            .map_err(|_| rejected(RemoteErrorCode::Internal))?;
        let route = state
            .routes
            .get(&route_id)
            .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
        if route.revoked.contains_key(&slot_id) {
            return Err(rejected(RemoteErrorCode::RouteUnavailable));
        }
        let grant = route
            .grants
            .get(&slot_id)
            .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
        if !digest_matches(&grant.token_digest, &token_digest(access_token)) {
            return Err(rejected(RemoteErrorCode::RouteUnavailable));
        }
        let presence = route
            .presence
            .as_ref()
            .filter(|presence| presence.expires_at_ms >= at_ms)
            .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
        Ok(ResolveResponse {
            host_endpoint: presence.host_endpoint,
            relay_url: presence.relay_url.clone(),
            expires_at_ms: presence.expires_at_ms,
        })
    }

    pub(crate) fn authenticate_bearer(
        &self,
        bearer: &str,
    ) -> Result<BearerIdentity, BearerFailure> {
        let bearer = AccessToken::from_opaque(bearer).ok_or(BearerFailure::Invalid)?;
        let digest = token_digest(&bearer);
        let state = self.state.read().map_err(|_| BearerFailure::Invalid)?;
        let mut was_revoked = false;
        for (route_id, route) in &state.routes {
            if digest_matches(&route.admin_digest, &digest) {
                return Ok(BearerIdentity {
                    route_id: *route_id,
                    slot_id: None,
                    expected_host_endpoint: Some(route.host_endpoint),
                });
            }
            for (slot_id, grant) in &route.grants {
                if digest_matches(&grant.token_digest, &digest) {
                    return Ok(BearerIdentity {
                        route_id: *route_id,
                        slot_id: Some(*slot_id),
                        expected_host_endpoint: None,
                    });
                }
            }
            was_revoked |= route
                .revoked
                .values()
                .any(|revoked| digest_matches(revoked, &digest));
        }
        if was_revoked {
            Err(BearerFailure::Revoked)
        } else {
            Err(BearerFailure::Invalid)
        }
    }

    pub(crate) fn authorize_admin(
        &self,
        route_id: RouteId,
        admin_token: &RouteAdminToken,
    ) -> RemoteResult<()> {
        let state = self
            .state
            .read()
            .map_err(|_| rejected(RemoteErrorCode::Internal))?;
        let route = state
            .routes
            .get(&route_id)
            .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
        if !digest_matches(&route.admin_digest, &admin_digest(admin_token)) {
            return Err(rejected(RemoteErrorCode::AuthenticationFailed));
        }
        Ok(())
    }

    pub(crate) fn authorize_device(
        &self,
        route_id: RouteId,
        slot_id: DeviceSlotId,
        access_token: &AccessToken,
    ) -> RemoteResult<()> {
        let state = self
            .state
            .read()
            .map_err(|_| rejected(RemoteErrorCode::Internal))?;
        let route = state
            .routes
            .get(&route_id)
            .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
        if route.revoked.contains_key(&slot_id) {
            return Err(rejected(RemoteErrorCode::Revoked));
        }
        let grant = route
            .grants
            .get(&slot_id)
            .ok_or_else(|| rejected(RemoteErrorCode::RouteUnavailable))?;
        if !digest_matches(&grant.token_digest, &token_digest(access_token)) {
            return Err(rejected(RemoteErrorCode::AuthenticationFailed));
        }
        Ok(())
    }

    fn commit<F>(&self, mutate: F) -> RemoteResult<()>
    where
        F: FnOnce(&mut RegistryState) -> RemoteResult<()>,
    {
        let mut guard = self
            .state
            .write()
            .map_err(|_| rejected(RemoteErrorCode::Internal))?;
        let mut next = guard.clone();
        mutate(&mut next)?;
        if let Some(path) = self.path.as_deref() {
            write_state(path, &next)?;
        }
        *guard = next;
        Ok(())
    }
}

fn remove_push(state: &mut RegistryState, route_id: RouteId, slot_id: DeviceSlotId) {
    let should_remove_route = state.pushes.get_mut(&route_id).is_some_and(|pushes| {
        pushes.remove(&slot_id);
        pushes.is_empty()
    });
    if should_remove_route {
        state.pushes.remove(&route_id);
    }
}

fn digest_matches(left: &[u8; 32], right: &[u8; 32]) -> bool {
    bool::from(left.ct_eq(right))
}

fn validate_ttl(ttl_secs: u64) -> RemoteResult<()> {
    if (MIN_PRESENCE_TTL_SECS..=MAX_PRESENCE_TTL_SECS).contains(&ttl_secs) {
        Ok(())
    } else {
        Err(rejected(RemoteErrorCode::MalformedFrame))
    }
}

fn validate_relay_url(value: &str) -> RemoteResult<()> {
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("https://")
        || lower.contains("n0.computer")
        || lower.contains("staging")
        || value.chars().any(char::is_whitespace)
        || value.len() > 2_048
    {
        return Err(rejected(RemoteErrorCode::MalformedFrame));
    }
    let authority = value.strip_prefix("https://").unwrap_or_default();
    if authority.is_empty() || authority.starts_with('/') {
        return Err(rejected(RemoteErrorCode::MalformedFrame));
    }
    Ok(())
}

fn validate_state(state: &RegistryState) -> RemoteResult<()> {
    if state.routes.len() > MAX_ROUTES {
        return Err(rejected(RemoteErrorCode::LimitExceeded));
    }
    for route in state.routes.values() {
        validate_relay_url(&route.relay_url)?;
        if route.grants.len().saturating_add(route.revoked.len()) > MAX_GRANTS_PER_ROUTE
            || route.admin_digest == [0_u8; 32]
        {
            return Err(rejected(RemoteErrorCode::Internal));
        }
        for grant in route.grants.values() {
            if grant.token_digest == [0_u8; 32] {
                return Err(rejected(RemoteErrorCode::Internal));
            }
        }
    }
    for (route_id, pushes) in &state.pushes {
        let route = state
            .routes
            .get(route_id)
            .ok_or_else(|| rejected(RemoteErrorCode::Internal))?;
        for (slot_id, address) in pushes {
            if !route.grants.contains_key(slot_id)
                || route.revoked.contains_key(slot_id)
                || PushAddress::fcm_fid(
                    address.opaque_address.clone(),
                    address.registered_at_ms,
                    address.expires_at_ms,
                )
                .as_ref()
                    != Some(address)
            {
                return Err(rejected(RemoteErrorCode::Internal));
            }
        }
    }
    Ok(())
}

fn write_state(path: &Path, state: &RegistryState) -> RemoteResult<()> {
    let parent = path.parent().ok_or(RemoteError::UnsafeStorage)?;
    prepare_private_parent(parent)?;
    let bytes = serde_json::to_vec(state).map_err(RemoteError::Encoding)?;
    let temporary = parent.join(format!(
        ".relay-registry-{}.tmp",
        RouteId::random().opaque()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(RemoteError::Storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(RemoteError::Storage)?;
    }
    ensure_owner_only(&temporary, false)?;
    let result = (|| {
        file.write_all(&bytes).map_err(RemoteError::Storage)?;
        file.sync_all().map_err(RemoteError::Storage)?;
        fs::rename(&temporary, path).map_err(RemoteError::Storage)?;
        sync_parent(parent).map_err(RemoteError::Storage)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn prepare_private_parent(parent: &Path) -> RemoteResult<()> {
    if parent.exists() {
        return ensure_owner_only(parent, true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent).map_err(RemoteError::Storage)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(parent).map_err(RemoteError::Storage)?;
    ensure_owner_only(parent, true)
}

fn sync_parent(parent: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

fn ensure_owner_only(path: &Path, directory: bool) -> RemoteResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .map_err(RemoteError::Storage)?
            .permissions()
            .mode();
        let expected = if directory { 0o700 } else { 0o600 };
        if mode & 0o077 != 0 {
            return Err(RemoteError::UnsafeStorage);
        }
        if mode & 0o700 != expected {
            fs::set_permissions(path, fs::Permissions::from_mode(expected))
                .map_err(RemoteError::Storage)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, directory);
        // The Ubuntu service is Unix-only.  Non-Unix callers must supply an
        // owner-only deployment ACL before opening a durable registry.
        Ok(())
    }
}
