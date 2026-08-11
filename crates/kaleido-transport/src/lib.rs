//! Security and framing kernel for OneKaleidoscope TRANSPORT 0.1.
//!
//! This crate deliberately does not accept sockets or spawn connection tasks.
//! It provides the bounded parsers, TLS configuration, authentication state,
//! durable device registry and correlation rules used by the host and mobile
//! composition roots.

pub mod auth;
pub mod bootstrap;
pub mod control;
pub mod error;
pub mod frame;
pub mod limits;
pub mod private_file;
pub mod registry;
pub mod remote;
pub mod remote_client;
pub mod tls;

mod platform;

pub const TRANSPORT_VERSION: &str = "0.1.0";
pub const MAX_FRAME_LENGTH: u32 = 65_545;
pub const MAX_CONTROL_BODY_BYTES: usize = 65_536;
pub const MAX_CONTENT_BODY_BYTES: usize = 65_536;
pub const MAX_BOOTSTRAP_JSON_BYTES: usize = 2_048;
pub const MAX_PENDING_REQUESTS: usize = 32;
pub const MAX_ACTIVE_SUBSCRIPTIONS: usize = 16;
pub const MAX_GLOBAL_CONNECTIONS: usize = 64;
pub const MAX_PRE_AUTH_CONNECTIONS: usize = 16;
pub const MAX_CONNECTIONS_PER_SOURCE_IP: usize = 4;
pub const MAX_CONNECTIONS_PER_DEVICE: usize = 2;

pub const TLS_HANDSHAKE_TIMEOUT_MS: i64 = 5_000;
pub const HELLO_TIMEOUT_MS: i64 = 5_000;
pub const AUTH_TIMEOUT_MS: i64 = 30_000;
pub const FRAME_IO_TIMEOUT_MS: i64 = 10_000;
pub const IDLE_PING_AFTER_MS: i64 = 30_000;
pub const IDLE_CLOSE_AFTER_MS: i64 = 90_000;
pub const PAIRING_LIFETIME_MS: i64 = 300_000;
pub const CHALLENGE_LIFETIME_MS: i64 = 30_000;
pub const SESSION_LIFETIME_MS: i64 = 900_000;

/// TRANSPORT 0.1 accepts only the pre-1.0 `0.1.x` compatibility line.
pub fn version_is_compatible(peer_version: &str) -> bool {
    parse_version(peer_version).is_some_and(|(major, minor, _)| major == 0 && minor == 1)
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
    use super::version_is_compatible;

    #[test]
    fn transport_version_has_a_closed_compatibility_line() {
        assert!(version_is_compatible("0.1.99"));
        for invalid in ["0.2.0", "1.1.0", "0.1", "0.1.0-extra", "00.1.0"] {
            assert!(!version_is_compatible(invalid), "accepted {invalid}");
        }
    }
}
