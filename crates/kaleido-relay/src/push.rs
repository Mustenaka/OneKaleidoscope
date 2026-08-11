use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use google_cloud_auth::credentials::{AccessTokenCredentials, Builder as CredentialsBuilder};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{DeviceSlotId, RouteId};

pub const MAX_PUSH_PAYLOAD_BYTES: usize = 256;
pub const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const FCM_SEND_URL: &str = "https://fcm.googleapis.com/v1/projects";
const OPAQUE_HINT_CHARS: usize = 22;
const MAX_FCM_ERROR_BYTES: usize = 8 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PushProvider {
    FcmFid,
    ApnsToken,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushAddress {
    pub provider: PushProvider,
    pub opaque_address: String,
    pub registered_at_ms: u64,
    pub expires_at_ms: u64,
}

impl PushAddress {
    pub fn fcm_fid(
        opaque_address: String,
        registered_at_ms: u64,
        expires_at_ms: u64,
    ) -> Option<Self> {
        if opaque_address.is_empty()
            || opaque_address.len() > 4_096
            || opaque_address.chars().any(char::is_whitespace)
            || expires_at_ms <= registered_at_ms
        {
            return None;
        }
        Some(Self {
            provider: PushProvider::FcmFid,
            opaque_address,
            registered_at_ms,
            expires_at_ms,
        })
    }
}

impl fmt::Debug for PushAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PushAddress")
            .field("provider", &self.provider)
            .field("opaque_address", &"[redacted]")
            .field("registered_at_ms", &self.registered_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushPayload {
    pub v: String,
    pub kind: String,
    pub route: String,
    pub wake: String,
}

impl PushPayload {
    pub fn wake(route: &RouteId, wake: &DeviceSlotId) -> Option<Self> {
        Self::wake_with_id(route, &wake.opaque())
    }

    pub fn wake_with_id(route: &RouteId, wake: &str) -> Option<Self> {
        let route = route.opaque();
        let wake = wake.to_owned();
        if route.chars().count() != OPAQUE_HINT_CHARS || wake.chars().count() != OPAQUE_HINT_CHARS {
            return None;
        }
        Some(Self {
            v: "1".to_owned(),
            kind: "wake".to_owned(),
            route,
            wake,
        })
    }

    pub fn to_json_bytes(&self) -> Option<Vec<u8>> {
        if !self.is_valid() {
            return None;
        }
        let bytes = serde_json::to_vec(self).ok()?;
        (bytes.len() <= MAX_PUSH_PAYLOAD_BYTES).then_some(bytes)
    }

    pub fn is_valid(&self) -> bool {
        self.v == "1"
            && self.kind == "wake"
            && self.route.chars().count() == OPAQUE_HINT_CHARS
            && self.wake.chars().count() == OPAQUE_HINT_CHARS
            && self
                .route
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            && self
                .wake
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    }
}

/// HTTP v1 request body.  FID is deliberately represented as `message.fid`,
/// not the deprecated registration-token field.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FcmHttpV1Request {
    pub message: FcmMessage,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FcmMessage {
    pub fid: String,
    pub data: PushPayload,
}

impl fmt::Debug for FcmHttpV1Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FcmHttpV1Request")
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Debug for FcmMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FcmMessage")
            .field("fid", &"[redacted]")
            .field("data", &self.data)
            .finish()
    }
}

impl FcmHttpV1Request {
    pub fn new(fid: String, payload: PushPayload) -> Option<Self> {
        PushAddress::fcm_fid(fid.clone(), 0, 1)?;
        if !payload.is_valid() {
            return None;
        }
        Some(Self {
            message: FcmMessage { fid, data: payload },
        })
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum FcmSendError {
    #[error("FCM credentials unavailable")]
    Credentials,
    #[error("FCM address is invalid or expired")]
    InvalidAddress,
    #[error("FCM payload is invalid")]
    InvalidPayload,
    #[error("FCM request transport failed")]
    Transport,
    #[error("FCM credentials rejected")]
    AuthRejected,
    #[error("FCM address is no longer registered")]
    DeleteAddress,
    #[error("FCM service rejected the request")]
    Rejected,
    #[error("FCM service is temporarily unavailable")]
    Retryable,
}

/// FCM HTTP v1 sender using ADC and the current FID target field.
///
/// The access token, FID, and response body are kept out of this type's Debug
/// implementation and are never logged.  A `DeleteAddress` result is the only
/// signal consumers may use to remove a persisted FID.
pub struct FcmSender {
    endpoint: String,
    credentials: AccessTokenCredentials,
    client: reqwest::Client,
}

impl fmt::Debug for FcmSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FcmSender")
            .field("endpoint", &"[redacted]")
            .finish()
    }
}

impl FcmSender {
    pub fn from_adc(project_id: String) -> Result<Self, FcmSendError> {
        if !valid_project_id(&project_id) {
            return Err(FcmSendError::Credentials);
        }
        let credentials = CredentialsBuilder::default()
            .with_scopes([FCM_SCOPE])
            .build_access_token_credentials()
            .map_err(|_| FcmSendError::Credentials)?;
        let client = reqwest::Client::builder()
            .https_only(true)
            .build()
            .map_err(|_| FcmSendError::Transport)?;
        let endpoint = format!("{FCM_SEND_URL}/{project_id}/messages:send");
        Ok(Self {
            endpoint,
            credentials,
            client,
        })
    }

    pub async fn send_wake(
        &self,
        address: &PushAddress,
        payload: &PushPayload,
    ) -> Result<(), FcmSendError> {
        if address.provider != PushProvider::FcmFid {
            return Err(FcmSendError::InvalidAddress);
        }
        if address.expires_at_ms <= now_ms() {
            return Err(FcmSendError::InvalidAddress);
        }
        if !payload.is_valid() || payload.to_json_bytes().is_none() {
            return Err(FcmSendError::InvalidPayload);
        }
        let request = FcmHttpV1Request::new(address.opaque_address.clone(), payload.clone())
            .ok_or(FcmSendError::InvalidPayload)?;
        let token = self
            .credentials
            .access_token()
            .await
            .map_err(|_| FcmSendError::Credentials)?;
        let mut response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(token.token)
            .json(&request)
            .send()
            .await
            .map_err(|_| FcmSendError::Transport)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let mut body = Vec::new();
        while body.len() < MAX_FCM_ERROR_BYTES {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) | Err(_) => break,
            };
            let remaining = MAX_FCM_ERROR_BYTES.saturating_sub(body.len());
            body.extend(chunk.iter().copied().take(remaining));
        }
        Err(classify_response(status, &String::from_utf8_lossy(&body)))
    }
}

fn valid_project_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn classify_response(status: StatusCode, body: &str) -> FcmSendError {
    if status == StatusCode::NOT_FOUND || body.contains("UNREGISTERED") {
        FcmSendError::DeleteAddress
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        FcmSendError::AuthRejected
    } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        FcmSendError::Retryable
    } else {
        FcmSendError::Rejected
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn fcm_statuses_close_over_unregistered_address_without_body_leak() {
        assert_eq!(
            classify_response(StatusCode::NOT_FOUND, "UNREGISTERED fid-secret"),
            FcmSendError::DeleteAddress
        );
        assert_eq!(
            classify_response(StatusCode::TOO_MANY_REQUESTS, "retry-after=1"),
            FcmSendError::Retryable
        );
        assert_eq!(
            classify_response(StatusCode::BAD_REQUEST, "invalid"),
            FcmSendError::Rejected
        );
        let payload = PushPayload::wake(
            &RouteId::from_bytes([1; 16]),
            &DeviceSlotId::from_bytes([2; 16]),
        )
        .unwrap();
        let request = FcmHttpV1Request::new("fid-secret".to_owned(), payload).unwrap();
        assert!(!format!("{request:?}").contains("fid-secret"));
    }
}
