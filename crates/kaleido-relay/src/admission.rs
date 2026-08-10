use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::{RemoteErrorCode, RemoteResult};
use crate::ids::{DeviceSlotId, RouteId};
use crate::protocol::rejected;
use crate::registry::{BearerFailure, Registry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLimits {
    pub global_connections: usize,
    pub route_connections: usize,
    pub device_connections: usize,
    pub preauth_connections: usize,
    pub bytes_per_second: NonZeroU64,
    pub burst_bytes: NonZeroU64,
    pub idle_timeout_ms: u64,
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            global_connections: 1_024,
            route_connections: 8,
            device_connections: 2,
            preauth_connections: 4,
            bytes_per_second: NonZeroU64::new(1_048_576).unwrap_or(NonZeroU64::MIN),
            burst_bytes: NonZeroU64::new(262_144).unwrap_or(NonZeroU64::MIN),
            idle_timeout_ms: 90_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionPrincipal {
    pub route_id: RouteId,
    pub slot_id: Option<DeviceSlotId>,
}

#[derive(Debug, Clone, Copy)]
struct ConnectionRecord {
    principal: Option<AdmissionPrincipal>,
}

#[derive(Debug, Default)]
struct AdmissionState {
    connections: HashMap<u64, ConnectionRecord>,
}

pub struct RelayAdmission {
    registry: Arc<Registry>,
    limits: AdmissionLimits,
    state: Arc<Mutex<AdmissionState>>,
    next_id: AtomicU64,
}

impl fmt::Debug for RelayAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active = self
            .state
            .lock()
            .map(|state| state.connections.len())
            .unwrap_or(0);
        formatter
            .debug_struct("RelayAdmission")
            .field("active_connections", &active)
            .field("limits", &self.limits)
            .finish()
    }
}

impl RelayAdmission {
    pub fn new(registry: Arc<Registry>, limits: AdmissionLimits) -> Self {
        Self {
            registry,
            limits,
            state: Arc::new(Mutex::new(AdmissionState::default())),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub const fn limits(&self) -> AdmissionLimits {
        self.limits
    }

    pub fn admit(&self, principal: AdmissionPrincipal) -> RemoteResult<ConnectionLease> {
        self.admit_inner(Some(principal))
    }

    pub fn admit_preauthenticated(&self) -> RemoteResult<ConnectionLease> {
        self.admit_inner(None)
    }

    pub fn admit_bearer(&self, bearer: &str) -> Result<ConnectionLease, RemoteErrorCode> {
        self.admit_bearer_for_endpoint(bearer, None)
    }

    pub fn admit_bearer_for_endpoint(
        &self,
        bearer: &str,
        endpoint_id: Option<&[u8; 32]>,
    ) -> Result<ConnectionLease, RemoteErrorCode> {
        let identity =
            self.registry
                .authenticate_bearer(bearer)
                .map_err(|failure| match failure {
                    BearerFailure::Invalid => RemoteErrorCode::AuthenticationFailed,
                    BearerFailure::Revoked => RemoteErrorCode::Revoked,
                })?;
        if identity
            .expected_host_endpoint
            .is_some_and(|expected| endpoint_id != Some(expected.as_bytes()))
        {
            return Err(RemoteErrorCode::AuthenticationFailed);
        }
        self.admit(AdmissionPrincipal {
            route_id: identity.route_id,
            slot_id: identity.slot_id,
        })
        .map_err(|error| error.code())
    }

    fn admit_inner(&self, principal: Option<AdmissionPrincipal>) -> RemoteResult<ConnectionLease> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| rejected(RemoteErrorCode::Internal))?;
        if state.connections.len() >= self.limits.global_connections {
            return Err(rejected(RemoteErrorCode::LimitExceeded));
        }
        let preauth = state
            .connections
            .values()
            .filter(|record| record.principal.is_none())
            .count();
        if principal.is_none() && preauth >= self.limits.preauth_connections {
            return Err(rejected(RemoteErrorCode::LimitExceeded));
        }
        if let Some(principal) = principal {
            let route_count = state
                .connections
                .values()
                .filter(|record| {
                    record
                        .principal
                        .map(|value| value.route_id == principal.route_id)
                        .unwrap_or(false)
                })
                .count();
            if route_count >= self.limits.route_connections {
                return Err(rejected(RemoteErrorCode::LimitExceeded));
            }
            if let Some(slot_id) = principal.slot_id {
                let device_count = state
                    .connections
                    .values()
                    .filter(|record| {
                        record
                            .principal
                            .map(|value| {
                                value.route_id == principal.route_id
                                    && value.slot_id == Some(slot_id)
                            })
                            .unwrap_or(false)
                    })
                    .count();
                if device_count >= self.limits.device_connections {
                    return Err(rejected(RemoteErrorCode::LimitExceeded));
                }
            }
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        state.connections.insert(id, ConnectionRecord { principal });
        Ok(ConnectionLease {
            id,
            principal,
            admission: Arc::clone(&self.state),
            limits: self.limits,
            bucket: Mutex::new(TokenBucket::new(self.limits.burst_bytes.get())),
        })
    }
}

/// A lease held for the lifetime of one opaque relay byte pipe.
pub struct ConnectionLease {
    id: u64,
    principal: Option<AdmissionPrincipal>,
    admission: Arc<Mutex<AdmissionState>>,
    limits: AdmissionLimits,
    bucket: Mutex<TokenBucket>,
}

impl fmt::Debug for ConnectionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionLease")
            .field("id", &self.id)
            .field("principal", &self.principal)
            .finish()
    }
}

impl ConnectionLease {
    pub const fn id(&self) -> u64 {
        self.id
    }

    pub const fn principal(&self) -> Option<AdmissionPrincipal> {
        self.principal
    }

    /// Account bytes for the per-connection byte budget.  The relay adapter
    /// calls this before forwarding each opaque frame; no frame is inspected.
    pub fn consume(&self, bytes: u64, now_ms: u64) -> RemoteResult<()> {
        if bytes > self.limits.burst_bytes.get() {
            return Err(rejected(RemoteErrorCode::RateLimited));
        }
        let mut bucket = self
            .bucket
            .lock()
            .map_err(|_| rejected(RemoteErrorCode::Internal))?;
        if now_ms.saturating_sub(bucket.last_ms) > self.limits.idle_timeout_ms {
            return Err(rejected(RemoteErrorCode::Expired));
        }
        let elapsed = now_ms.saturating_sub(bucket.last_ms);
        let refill = elapsed
            .saturating_mul(self.limits.bytes_per_second.get())
            .saturating_div(1_000);
        bucket.tokens = bucket
            .tokens
            .saturating_add(refill)
            .min(self.limits.burst_bytes.get());
        bucket.last_ms = now_ms;
        if bytes > bucket.tokens {
            return Err(rejected(RemoteErrorCode::RateLimited));
        }
        bucket.tokens -= bytes;
        Ok(())
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.admission.lock() {
            state.connections.remove(&self.id);
        }
    }
}

#[derive(Debug)]
struct TokenBucket {
    tokens: u64,
    last_ms: u64,
}

impl TokenBucket {
    const fn new(capacity: u64) -> Self {
        Self {
            tokens: capacity,
            last_ms: 0,
        }
    }
}
