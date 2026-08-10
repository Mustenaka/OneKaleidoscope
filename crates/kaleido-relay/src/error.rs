use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Closed error vocabulary for REMOTE_CONTROL 0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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

impl RemoteErrorCode {
    pub const fn is_retriable(self) -> bool {
        matches!(
            self,
            Self::RouteUnavailable | Self::Expired | Self::RateLimited | Self::Internal
        )
    }
}

/// Error returned by the service kernel.  The wire frame intentionally omits
/// the string/source so a remote peer cannot use it as an oracle.
#[derive(Debug, Error)]
pub enum RemoteError {
    #[error("remote request rejected: {code:?}")]
    Rejected { code: RemoteErrorCode },
    #[error("registry storage is not safe")]
    UnsafeStorage,
    #[error("registry storage failed")]
    Storage(#[source] std::io::Error),
    #[error("registry encoding failed")]
    Encoding(#[source] serde_json::Error),
}

impl RemoteError {
    pub const fn code(&self) -> RemoteErrorCode {
        match self {
            Self::Rejected { code } => *code,
            Self::UnsafeStorage | Self::Storage(_) | Self::Encoding(_) => RemoteErrorCode::Internal,
        }
    }
}

pub type RemoteResult<T> = Result<T, RemoteError>;

/// The only error frame emitted by the remote-control endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteErrorFrame {
    pub request_id: Option<u64>,
    pub code: RemoteErrorCode,
    pub retriable: bool,
}

impl RemoteErrorFrame {
    pub const fn new(request_id: Option<u64>, code: RemoteErrorCode) -> Self {
        Self {
            request_id,
            code,
            retriable: code.is_retriable(),
        }
    }

    pub const fn from_error(request_id: Option<u64>, error: &RemoteError) -> Self {
        Self::new(request_id, error.code())
    }
}
