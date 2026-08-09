use std::io;

use thiserror::Error;

/// Errors emitted by the local transport security kernel.
///
/// Variants intentionally carry no secret, body, signature, endpoint, key or
/// filesystem path. Callers may safely count or log only the variant.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport version is incompatible")]
    VersionMismatch,
    #[error("transport frame is malformed")]
    MalformedFrame,
    #[error("transport frame is too large")]
    FrameTooLarge,
    #[error("transport resource limit was reached")]
    RateLimited,
    #[error("pairing credentials are invalid")]
    PairingInvalid,
    #[error("device authentication failed")]
    AuthenticationFailed,
    #[error("device challenge expired")]
    ChallengeExpired,
    #[error("device challenge was already consumed")]
    ChallengeReplayed,
    #[error("device is revoked")]
    DeviceRevoked,
    #[error("connection limit was reached")]
    TooManyConnections,
    #[error("subscription limit was reached")]
    TooManySubscriptions,
    #[error("secure persistence failed")]
    Persistence,
    #[error("cryptographic material is invalid")]
    InvalidKeyMaterial,
    #[error("secure filesystem permissions are invalid")]
    InsecurePermissions,
    #[error("internal transport failure")]
    Internal,
    #[error("clock arithmetic overflowed")]
    TimeOverflow,
}

impl PartialEq for TransportError {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl Eq for TransportError {}

impl From<io::Error> for TransportError {
    fn from(_error: io::Error) -> Self {
        Self::Persistence
    }
}
