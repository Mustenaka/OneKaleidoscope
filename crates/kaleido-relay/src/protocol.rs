use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{RemoteError, RemoteErrorCode, RemoteResult};
use crate::ids::{digest, OperationId};

pub const REMOTE_CONTROL_VERSION: &str = "0.1.0";
pub const REQUEST_SKEW_MS: u64 = 60_000;
pub const REPLAY_TTL_MS: u64 = 120_000;
pub const REPLAY_CAPACITY: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteHello {
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteHelloAck {
    pub version: String,
}

impl RemoteHello {
    pub fn new() -> Self {
        Self {
            version: REMOTE_CONTROL_VERSION.to_owned(),
        }
    }

    pub fn accept(&self) -> RemoteResult<RemoteHelloAck> {
        // #[allow(kaleido::version_branch)] reason: this is the protocol negotiation boundary itself, not product feature selection
        if self.version != REMOTE_CONTROL_VERSION {
            return Err(RemoteError::Rejected {
                code: RemoteErrorCode::VersionMismatch,
            });
        }
        Ok(RemoteHelloAck {
            version: REMOTE_CONTROL_VERSION.to_owned(),
        })
    }
}

impl Default for RemoteHello {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteRequest<T> {
    pub version: String,
    pub request_id: u64,
    pub operation_id: OperationId,
    pub issued_at_ms: u64,
    pub body: T,
}

impl<T> RemoteRequest<T> {
    pub fn new(request_id: u64, body: T, now_ms: u64) -> Self {
        Self {
            version: REMOTE_CONTROL_VERSION.to_owned(),
            request_id,
            operation_id: OperationId::random(),
            issued_at_ms: now_ms,
            body,
        }
    }
}

#[derive(Debug)]
pub struct RequestTracker {
    next_request_id: u64,
    replay: ReplayCache,
}

impl RequestTracker {
    pub fn new() -> Self {
        Self {
            next_request_id: 1,
            replay: ReplayCache::new(REPLAY_CAPACITY, REPLAY_TTL_MS),
        }
    }

    pub fn accept<T>(
        &mut self,
        request: &RemoteRequest<T>,
        credential: &[u8],
        now_ms: u64,
    ) -> RemoteResult<()> {
        // #[allow(kaleido::version_branch)] reason: every independently decoded versioned request must fail closed before replay or dispatch
        if request.version != REMOTE_CONTROL_VERSION {
            return Err(rejected(RemoteErrorCode::VersionMismatch));
        }
        if request.request_id != self.next_request_id {
            return Err(rejected(RemoteErrorCode::MalformedFrame));
        }
        if request.issued_at_ms.abs_diff(now_ms) > REQUEST_SKEW_MS {
            return Err(rejected(RemoteErrorCode::Expired));
        }
        self.replay
            .check_and_record(credential, request.operation_id, now_ms)?;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| rejected(RemoteErrorCode::MalformedFrame))?;
        Ok(())
    }

    pub const fn next_request_id(&self) -> u64 {
        self.next_request_id
    }
}

impl Default for RequestTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct ReplayCache {
    capacity: usize,
    ttl_ms: u64,
    entries: HashMap<ReplayKey, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ReplayKey {
    credential_digest: [u8; 32],
    operation_id: OperationId,
}

impl ReplayCache {
    fn new(capacity: usize, ttl_ms: u64) -> Self {
        Self {
            capacity,
            ttl_ms,
            entries: HashMap::new(),
        }
    }

    fn check_and_record(
        &mut self,
        credential: &[u8],
        operation_id: OperationId,
        now_ms: u64,
    ) -> RemoteResult<()> {
        self.entries
            .retain(|_, seen_at| now_ms.saturating_sub(*seen_at) <= self.ttl_ms);
        let key = ReplayKey {
            credential_digest: digest(credential),
            operation_id,
        };
        if self.entries.contains_key(&key) {
            return Err(rejected(RemoteErrorCode::Replay));
        }
        if self.entries.len() >= self.capacity {
            return Err(rejected(RemoteErrorCode::LimitExceeded));
        }
        self.entries.insert(key, now_ms);
        Ok(())
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(crate) fn rejected(code: RemoteErrorCode) -> RemoteError {
    RemoteError::Rejected { code }
}
