use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use iroh_relay::server::{Access, AccessControl, ClientRequest, ConnectionId, ServerConfig};

use crate::admission::RelayAdmission;
use crate::error::RemoteErrorCode;

/// URL supplied to an iroh client through `RelayMode::Custom`.
///
/// The parser intentionally rejects n0 public and staging endpoints.  A URL
/// is only a location; authorization is still supplied by the bearer token.
#[derive(Clone, PartialEq, Eq)]
pub struct SelfHostedRelayUrl(String);

impl SelfHostedRelayUrl {
    pub fn parse(value: String) -> Result<Self, RemoteErrorCode> {
        let lower = value.to_ascii_lowercase();
        let authority = value.strip_prefix("https://").unwrap_or_default();
        if !lower.starts_with("https://")
            || lower.contains("n0.computer")
            || lower.contains("staging")
            || value.chars().any(char::is_whitespace)
            || authority.is_empty()
            || authority.starts_with('/')
        {
            return Err(RemoteErrorCode::MalformedFrame);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn relay_map_with_auth_token(
        &self,
        auth_token: &str,
    ) -> Result<iroh::RelayMap, RemoteErrorCode> {
        if crate::AccessToken::from_opaque(auth_token).is_none() {
            return Err(RemoteErrorCode::AuthenticationFailed);
        }
        let url = self
            .0
            .parse::<iroh::RelayUrl>()
            .map_err(|_| RemoteErrorCode::MalformedFrame)?;
        Ok(iroh::RelayMap::from(url).with_auth_token(auth_token.to_owned()))
    }
}

impl fmt::Debug for SelfHostedRelayUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SelfHostedRelayUrl([redacted])")
    }
}

/// iroh's server hook is intentionally the only code here that sees an
/// EndpointId.  It admits only credentials from the durable route registry and
/// forwards encrypted packets without interpreting their contents.
#[derive(Clone)]
pub struct IrohAccessControl {
    admission: Arc<RelayAdmission>,
    leases: Arc<Mutex<HashMap<ConnectionId, ActiveConnection>>>,
}

struct ActiveConnection {
    endpoint_id: iroh_base::EndpointId,
    lease: crate::ConnectionLease,
}

impl IrohAccessControl {
    pub fn new(admission: Arc<RelayAdmission>) -> Self {
        Self {
            admission,
            leases: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn admission(&self) -> &Arc<RelayAdmission> {
        &self.admission
    }

    /// Install this policy into an iroh server config.  The caller must still
    /// supply its own TLS certificate; no public/default relay is selected.
    pub fn install(&self, mut config: ServerConfig) -> Result<ServerConfig, RemoteErrorCode> {
        let relay = config
            .relay
            .as_mut()
            .ok_or(RemoteErrorCode::MalformedFrame)?;
        relay.access = Arc::new(self.clone_for_config());
        Ok(config)
    }

    fn clone_for_config(&self) -> Self {
        self.clone()
    }

    pub fn endpoints_for_device(
        &self,
        route_id: crate::RouteId,
        slot_id: crate::DeviceSlotId,
    ) -> Result<Vec<iroh_base::EndpointId>, RemoteErrorCode> {
        let leases = self.leases.lock().map_err(|_| RemoteErrorCode::Internal)?;
        Ok(leases
            .values()
            .filter_map(|active| {
                (active.lease.principal()
                    == Some(crate::AdmissionPrincipal {
                        route_id,
                        slot_id: Some(slot_id),
                    }))
                .then_some(active.endpoint_id)
            })
            .collect())
    }
}

impl fmt::Debug for IrohAccessControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active = self.leases.lock().map(|leases| leases.len()).unwrap_or(0);
        formatter
            .debug_struct("IrohAccessControl")
            .field("active_connections", &active)
            .finish()
    }
}

impl AccessControl for IrohAccessControl {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        let Some(token) = request.auth_token() else {
            return Access::Deny { reason: None };
        };
        let endpoint_id = request.endpoint_id();
        let Ok(lease) = self
            .admission
            .admit_bearer_for_endpoint(&token, Some(endpoint_id.as_bytes()))
        else {
            return Access::Deny { reason: None };
        };
        let mut leases = match self.leases.lock() {
            Ok(leases) => leases,
            Err(_) => return Access::Deny { reason: None },
        };
        leases.insert(
            request.connection_id(),
            ActiveConnection { endpoint_id, lease },
        );
        Access::Allow
    }

    fn on_disconnect(&self, _endpoint_id: iroh_base::EndpointId, connection_id: ConnectionId) {
        if let Ok(mut leases) = self.leases.lock() {
            leases.remove(&connection_id);
        }
    }
}
