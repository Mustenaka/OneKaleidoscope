use std::net::{Ipv4Addr, Ipv6Addr};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use kaleido_proto::ids::HostId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::TransportError;
use crate::tls::SpkiPin;
use crate::MAX_BOOTSTRAP_JSON_BYTES;

const URI_PREFIX: &str = "onekaleidoscope://pair/v1?data=";
const SECRET_LENGTH: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub struct PairingBootstrap {
    pub host_id: HostId,
    pub endpoint: String,
    pub host_public_key_pin: String,
    pub secret: Vec<u8>,
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for PairingBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingBootstrap")
            .field("host_id", &self.host_id)
            .field("endpoint", &"[redacted]")
            .field("host_public_key_pin", &"[redacted]")
            .field("secret", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapWire {
    #[serde(rename = "version")]
    revision: u64,
    host_id: String,
    endpoint: String,
    host_public_key_pin: String,
    secret: String,
    expires_at_ms: i64,
}

pub fn encode_uri(bootstrap: &PairingBootstrap) -> Result<String, TransportError> {
    validate_bootstrap(bootstrap)?;
    let wire = BootstrapWire {
        revision: 1,
        host_id: bootstrap.host_id.value.clone(),
        endpoint: bootstrap.endpoint.clone(),
        host_public_key_pin: bootstrap.host_public_key_pin.clone(),
        secret: URL_SAFE_NO_PAD.encode(&bootstrap.secret),
        expires_at_ms: bootstrap.expires_at_ms,
    };
    let json = serde_json::to_vec(&wire).map_err(|_| TransportError::MalformedFrame)?;
    if json.len() > MAX_BOOTSTRAP_JSON_BYTES {
        return Err(TransportError::FrameTooLarge);
    }
    Ok(format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(json)))
}

pub fn decode_uri(uri: &str) -> Result<PairingBootstrap, TransportError> {
    let encoded = uri
        .strip_prefix(URI_PREFIX)
        .ok_or(TransportError::MalformedFrame)?;
    if encoded.is_empty()
        || encoded.contains('=')
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(TransportError::MalformedFrame);
    }
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| TransportError::MalformedFrame)?,
    );
    if decoded.len() > MAX_BOOTSTRAP_JSON_BYTES
        || URL_SAFE_NO_PAD.encode(decoded.as_slice()) != encoded
    {
        return Err(TransportError::MalformedFrame);
    }
    let wire: BootstrapWire =
        serde_json::from_slice(&decoded).map_err(|_| TransportError::MalformedFrame)?;
    let canonical = serde_json::to_vec(&wire).map_err(|_| TransportError::MalformedFrame)?;
    if canonical.as_slice() != decoded.as_slice() || wire.revision != 1 {
        return Err(TransportError::MalformedFrame);
    }
    let secret = decode_secret(&wire.secret)?;
    let bootstrap = PairingBootstrap {
        host_id: HostId::new(wire.host_id),
        endpoint: wire.endpoint,
        host_public_key_pin: wire.host_public_key_pin,
        secret,
        expires_at_ms: wire.expires_at_ms,
    };
    validate_bootstrap(&bootstrap)?;
    Ok(bootstrap)
}

pub fn validate_endpoint(endpoint: &str) -> Result<(), TransportError> {
    if endpoint.is_empty() || !endpoint.is_ascii() || endpoint.contains(['/', '?', '#', '@']) {
        return Err(TransportError::MalformedFrame);
    }
    let (host, port) = if let Some(rest) = endpoint.strip_prefix('[') {
        let close = rest.find(']').ok_or(TransportError::MalformedFrame)?;
        let host = rest.get(..close).ok_or(TransportError::MalformedFrame)?;
        let suffix = rest
            .get(close + 1..)
            .and_then(|value| value.strip_prefix(':'))
            .ok_or(TransportError::MalformedFrame)?;
        if host.contains('%') || host.parse::<Ipv6Addr>().is_err() {
            return Err(TransportError::MalformedFrame);
        }
        (host, suffix)
    } else {
        if endpoint.bytes().filter(|byte| *byte == b':').count() != 1 {
            return Err(TransportError::MalformedFrame);
        }
        let (host, port) = endpoint
            .split_once(':')
            .ok_or(TransportError::MalformedFrame)?;
        if !valid_dns_or_ipv4(host) {
            return Err(TransportError::MalformedFrame);
        }
        (host, port)
    };
    if host.is_empty() || !valid_port(port) {
        return Err(TransportError::MalformedFrame);
    }
    Ok(())
}

fn validate_bootstrap(bootstrap: &PairingBootstrap) -> Result<(), TransportError> {
    if bootstrap.host_id.is_empty() || bootstrap.secret.len() != SECRET_LENGTH {
        return Err(TransportError::MalformedFrame);
    }
    validate_endpoint(&bootstrap.endpoint)?;
    SpkiPin::parse(&bootstrap.host_public_key_pin)?;
    Ok(())
}

fn decode_secret(encoded: &str) -> Result<Vec<u8>, TransportError> {
    if encoded.len() != 43 || encoded.contains('=') {
        return Err(TransportError::MalformedFrame);
    }
    let secret = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| TransportError::MalformedFrame)?;
    if secret.len() != SECRET_LENGTH || URL_SAFE_NO_PAD.encode(&secret) != encoded {
        return Err(TransportError::MalformedFrame);
    }
    Ok(secret)
}

fn valid_dns_or_ipv4(host: &str) -> bool {
    if host.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    if host.is_empty() || host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && (port.len() == 1 || !port.starts_with('0'))
        && port
            .parse::<u16>()
            .is_ok_and(|number| (1..=u16::MAX).contains(&number))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use kaleido_proto::ids::HostId;

    use super::{decode_uri, encode_uri, validate_endpoint, PairingBootstrap};

    fn bootstrap() -> PairingBootstrap {
        PairingBootstrap {
            host_id: HostId::new("host-1"),
            endpoint: "[2001:db8::1]:443".to_owned(),
            host_public_key_pin: format!("sha256:{}", URL_SAFE_NO_PAD.encode([7_u8; 32])),
            secret: vec![8_u8; 32],
            expires_at_ms: 123_456,
        }
    }

    #[test]
    fn qr_codec_is_canonical_and_round_trips() {
        let expected_json = r#"{"version":1,"host_id":"host-1","endpoint":"[2001:db8::1]:443","host_public_key_pin":"sha256:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc","secret":"CAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAg","expires_at_ms":123456}"#;
        let uri = encode_uri(&bootstrap()).expect("encode");
        assert_eq!(
            uri,
            format!(
                "onekaleidoscope://pair/v1?data={}",
                URL_SAFE_NO_PAD.encode(expected_json)
            )
        );
        assert_eq!(decode_uri(&uri).expect("decode"), bootstrap());
    }

    #[test]
    fn qr_rejects_unknown_duplicate_padded_and_noncanonical_data() {
        let cases = [
            r#"{"version":1,"host_id":"h","endpoint":"host:1","host_public_key_pin":"sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","secret":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","expires_at_ms":1,"extra":1}"#,
            r#"{"version":1,"version":1,"host_id":"h","endpoint":"host:1","host_public_key_pin":"sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","secret":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","expires_at_ms":1}"#,
            r#"{ "version":1,"host_id":"h","endpoint":"host:1","host_public_key_pin":"sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","secret":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","expires_at_ms":1}"#,
        ];
        for json in cases {
            let uri = format!(
                "onekaleidoscope://pair/v1?data={}",
                URL_SAFE_NO_PAD.encode(json)
            );
            assert!(decode_uri(&uri).is_err(), "accepted {json}");
        }
        let padded = format!("{}=", encode_uri(&bootstrap()).expect("encode"));
        assert!(decode_uri(&padded).is_err());
    }

    #[test]
    fn endpoint_grammar_is_closed() {
        for valid in ["host.local:1", "127.0.0.1:65535", "[::1]:443"] {
            validate_endpoint(valid).expect("valid endpoint");
        }
        for invalid in [
            "host:01",
            "host:+1",
            "::1:443",
            "[fe80::1%3]:443",
            "user@host:1",
            "host:0",
            "høst:443",
            "host:443/path",
        ] {
            assert!(validate_endpoint(invalid).is_err(), "accepted {invalid}");
        }
    }
}
