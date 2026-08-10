use std::collections::{BTreeMap, BTreeSet};

use kaleido_proto::ids::{DeviceId, HostId};
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePublicKey, EncodePublicKey};
use p256::PublicKey;
use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

use crate::error::TransportError;
use crate::{version_is_compatible, CHALLENGE_LIFETIME_MS};

const TRANSCRIPT_MAGIC: &[u8] = b"OneKaleidoscope.DeviceAuth.v1";

#[derive(Clone, PartialEq, Eq)]
pub struct DeviceChallenge {
    pub request_id: u64,
    pub challenge_id: Vec<u8>,
    pub nonce: Vec<u8>,
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for DeviceChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceChallenge")
            .field("request_id", &self.request_id)
            .field("challenge_id", &"[redacted]")
            .field("nonce", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ChallengeProof {
    pub request_id: u64,
    pub challenge_id: Vec<u8>,
    pub signature_der: Vec<u8>,
}

impl std::fmt::Debug for ChallengeProof {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChallengeProof")
            .field("request_id", &self.request_id)
            .field("challenge_id", &"[redacted]")
            .field("signature_der", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ChallengeTranscript<'a> {
    pub transport_version: &'a str,
    pub protocol_version: &'a str,
    pub host_id: &'a HostId,
    pub device_id: &'a DeviceId,
    pub tls_exporter: &'a [u8],
    pub challenge_id: &'a [u8],
    pub nonce: &'a [u8],
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for ChallengeTranscript<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChallengeTranscript")
            .field("transport_version", &self.transport_version)
            .field("protocol_version", &self.protocol_version)
            .field("host_id", &self.host_id)
            .field("device_id", &self.device_id)
            .field("tls_exporter", &"[redacted]")
            .field("challenge_id", &"[redacted]")
            .field("nonce", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

pub fn build_transcript(
    input: &ChallengeTranscript<'_>,
) -> Result<Zeroizing<Vec<u8>>, TransportError> {
    // #[allow(kaleido::version_branch)] reason: authentication transcript validation enforces the negotiated wire compatibility boundary, not a product capability
    if !version_is_compatible(input.transport_version)
        || !kaleido_proto::version_is_compatible(input.protocol_version)
        || input.host_id.is_empty()
        || input.device_id.is_empty()
        || input.tls_exporter.len() != 32
        || input.challenge_id.len() != 16
        || input.nonce.len() != 32
    {
        return Err(TransportError::AuthenticationFailed);
    }
    let mut transcript = Zeroizing::new(Vec::new());
    transcript.extend_from_slice(TRANSCRIPT_MAGIC);
    append_sized(&mut transcript, input.transport_version)?;
    append_sized(&mut transcript, input.protocol_version)?;
    append_sized(&mut transcript, input.host_id.as_str())?;
    append_sized(&mut transcript, input.device_id.as_str())?;
    transcript.extend_from_slice(input.tls_exporter);
    transcript.extend_from_slice(input.challenge_id);
    transcript.extend_from_slice(input.nonce);
    transcript.extend_from_slice(&input.expires_at_ms.to_be_bytes());
    Ok(transcript)
}

pub fn sign_transcript(
    signing_key: &SigningKey,
    transcript: &[u8],
) -> Result<Vec<u8>, TransportError> {
    let signature: Signature = signing_key.sign(transcript);
    Ok(signature.to_der().as_bytes().to_vec())
}

pub fn verify_transcript_signature(
    public_key_spki: &[u8],
    transcript: &[u8],
    signature_der: &[u8],
) -> Result<(), TransportError> {
    let key = decode_p256_spki(public_key_spki)?;
    let signature =
        Signature::from_der(signature_der).map_err(|_| TransportError::AuthenticationFailed)?;
    if signature.to_der().as_bytes() != signature_der {
        return Err(TransportError::AuthenticationFailed);
    }
    key.verify(transcript, &signature)
        .map_err(|_| TransportError::AuthenticationFailed)
}

pub fn validate_p256_spki(public_key_spki: &[u8]) -> Result<(), TransportError> {
    decode_p256_spki(public_key_spki).map(|_| ())
}

fn decode_p256_spki(public_key_spki: &[u8]) -> Result<VerifyingKey, TransportError> {
    let public_key = PublicKey::from_public_key_der(public_key_spki)
        .map_err(|_| TransportError::InvalidKeyMaterial)?;
    let canonical = public_key
        .to_public_key_der()
        .map_err(|_| TransportError::InvalidKeyMaterial)?;
    if canonical.as_bytes() != public_key_spki {
        return Err(TransportError::InvalidKeyMaterial);
    }
    Ok(VerifyingKey::from(public_key))
}

fn append_sized(output: &mut Vec<u8>, value: &str) -> Result<(), TransportError> {
    if value.is_empty() {
        return Err(TransportError::AuthenticationFailed);
    }
    let length = u16::try_from(value.len()).map_err(|_| TransportError::AuthenticationFailed)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

#[derive(Default)]
pub struct ChallengeStore {
    active: BTreeMap<Vec<u8>, StoredChallenge>,
    consumed: BTreeSet<(String, Vec<u8>)>,
}

struct StoredChallenge {
    connection_scope: String,
    request_id: u64,
    transport_version: String,
    protocol_version: String,
    host_id: HostId,
    device_id: DeviceId,
    tls_exporter: Zeroizing<Vec<u8>>,
    nonce: Zeroizing<Vec<u8>>,
    expires_at_ms: i64,
}

#[derive(Clone)]
pub struct IssueChallenge<'a> {
    pub connection_scope: &'a str,
    pub request_id: u64,
    pub transport_version: &'a str,
    pub protocol_version: &'a str,
    pub host_id: &'a HostId,
    pub device_id: &'a DeviceId,
    pub tls_exporter: &'a [u8],
    pub now_ms: i64,
}

impl std::fmt::Debug for ChallengeStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChallengeStore")
            .field("active_count", &self.active.len())
            .field("consumed_count", &self.consumed.len())
            .finish()
    }
}

impl std::fmt::Debug for IssueChallenge<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssueChallenge")
            .field("connection_scope", &self.connection_scope)
            .field("request_id", &self.request_id)
            .field("transport_version", &self.transport_version)
            .field("protocol_version", &self.protocol_version)
            .field("host_id", &self.host_id)
            .field("device_id", &self.device_id)
            .field("tls_exporter", &"[redacted]")
            .field("now_ms", &self.now_ms)
            .finish()
    }
}

impl ChallengeStore {
    pub fn issue(&mut self, input: IssueChallenge<'_>) -> Result<DeviceChallenge, TransportError> {
        validate_challenge_request(input.connection_scope, input.request_id)?;
        let expires_at_ms = input
            .now_ms
            .checked_add(CHALLENGE_LIFETIME_MS)
            .ok_or(TransportError::TimeOverflow)?;
        let mut challenge_id = vec![0_u8; 16];
        let mut nonce = vec![0_u8; 32];
        OsRng.fill_bytes(&mut challenge_id);
        OsRng.fill_bytes(&mut nonce);
        let transcript = ChallengeTranscript {
            transport_version: input.transport_version,
            protocol_version: input.protocol_version,
            host_id: input.host_id,
            device_id: input.device_id,
            tls_exporter: input.tls_exporter,
            challenge_id: &challenge_id,
            nonce: &nonce,
            expires_at_ms,
        };
        build_transcript(&transcript)?;
        self.active.insert(
            challenge_id.clone(),
            StoredChallenge {
                connection_scope: input.connection_scope.to_owned(),
                request_id: input.request_id,
                transport_version: input.transport_version.to_owned(),
                protocol_version: input.protocol_version.to_owned(),
                host_id: input.host_id.clone(),
                device_id: input.device_id.clone(),
                tls_exporter: Zeroizing::new(input.tls_exporter.to_vec()),
                nonce: Zeroizing::new(nonce.clone()),
                expires_at_ms,
            },
        );
        Ok(DeviceChallenge {
            request_id: input.request_id,
            challenge_id,
            nonce,
            expires_at_ms,
        })
    }

    pub fn verify(
        &mut self,
        connection_scope: &str,
        proof: &ChallengeProof,
        public_key_spki: &[u8],
        revoked: bool,
        now_ms: i64,
    ) -> Result<DeviceId, TransportError> {
        let presented_tombstone = (connection_scope.to_owned(), proof.challenge_id.clone());
        if self.consumed.contains(&presented_tombstone) {
            return Err(TransportError::ChallengeReplayed);
        }
        let Some(stored) = self.active.remove(&proof.challenge_id) else {
            return Err(TransportError::AuthenticationFailed);
        };
        self.consumed
            .insert((stored.connection_scope.clone(), proof.challenge_id.clone()));
        if stored.connection_scope != connection_scope || stored.request_id != proof.request_id {
            return Err(TransportError::AuthenticationFailed);
        }
        if now_ms >= stored.expires_at_ms {
            return Err(TransportError::ChallengeExpired);
        }
        if revoked {
            return Err(TransportError::AuthenticationFailed);
        }
        let input = ChallengeTranscript {
            transport_version: &stored.transport_version,
            protocol_version: &stored.protocol_version,
            host_id: &stored.host_id,
            device_id: &stored.device_id,
            tls_exporter: &stored.tls_exporter,
            challenge_id: &proof.challenge_id,
            nonce: &stored.nonce,
            expires_at_ms: stored.expires_at_ms,
        };
        let transcript = build_transcript(&input)?;
        verify_transcript_signature(public_key_spki, &transcript, &proof.signature_der)?;
        Ok(stored.device_id)
    }

    pub fn cancel_connection(&mut self, connection_scope: &str) {
        self.active
            .retain(|_, challenge| challenge.connection_scope != connection_scope);
        self.consumed.retain(|(scope, _)| scope != connection_scope);
    }
}

fn validate_challenge_request(
    connection_scope: &str,
    request_id: u64,
) -> Result<(), TransportError> {
    if connection_scope.is_empty() || request_id == 0 {
        Err(TransportError::AuthenticationFailed)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use kaleido_proto::ids::{DeviceId, HostId};
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::EncodePublicKey;
    use rand_core::OsRng;

    use super::{
        build_transcript, sign_transcript, ChallengeProof, ChallengeStore, ChallengeTranscript,
        IssueChallenge,
    };
    use crate::error::TransportError;

    #[test]
    fn transcript_is_exact_and_every_security_field_is_bound() {
        let host = HostId::new("host");
        let device = DeviceId::new("device");
        let input = ChallengeTranscript {
            transport_version: "0.1.0",
            protocol_version: "0.5.0",
            host_id: &host,
            device_id: &device,
            tls_exporter: &[1; 32],
            challenge_id: &[2; 16],
            nonce: &[3; 32],
            expires_at_ms: 4,
        };
        let transcript = build_transcript(&input).expect("transcript");
        assert_eq!(
            URL_SAFE_NO_PAD.encode(&transcript),
            "T25lS2FsZWlkb3Njb3BlLkRldmljZUF1dGgudjEABTAuMS4wAAUwLjUuMAAEaG9zdAAGZGV2aWNlAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQECAgICAgICAgICAgICAgICAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMAAAAAAAAABA"
        );

        let key = SigningKey::random(&mut OsRng);
        let public = p256::PublicKey::from(key.verifying_key())
            .to_public_key_der()
            .expect("SPKI");
        let signature = sign_transcript(&key, &transcript).expect("signature");
        super::verify_transcript_signature(public.as_bytes(), &transcript, &signature)
            .expect("verify");
        let mut trailing_signature = signature.clone();
        trailing_signature.push(0);
        assert!(super::verify_transcript_signature(
            public.as_bytes(),
            &transcript,
            &trailing_signature
        )
        .is_err());
        let mut trailing_spki = public.as_bytes().to_vec();
        trailing_spki.push(0);
        assert!(
            super::verify_transcript_signature(&trailing_spki, &transcript, &signature).is_err()
        );
        let mut changed = transcript.to_vec();
        let last = changed.last_mut().expect("non-empty transcript");
        *last ^= 1;
        assert!(
            super::verify_transcript_signature(public.as_bytes(), &changed, &signature).is_err()
        );
    }

    #[test]
    fn challenge_is_one_time_connection_bound_and_replay_rejected() {
        let host = HostId::new("host");
        let device = DeviceId::new("device");
        let key = SigningKey::random(&mut OsRng);
        let public = p256::PublicKey::from(key.verifying_key())
            .to_public_key_der()
            .expect("SPKI");
        let mut store = ChallengeStore::default();
        let issued = store
            .issue(IssueChallenge {
                connection_scope: "conn-a",
                request_id: 9,
                transport_version: "0.1.0",
                protocol_version: "0.5.0",
                host_id: &host,
                device_id: &device,
                tls_exporter: &[4; 32],
                now_ms: 1_000,
            })
            .expect("issue");
        let transcript = build_transcript(&ChallengeTranscript {
            transport_version: "0.1.0",
            protocol_version: "0.5.0",
            host_id: &host,
            device_id: &device,
            tls_exporter: &[4; 32],
            challenge_id: &issued.challenge_id,
            nonce: &issued.nonce,
            expires_at_ms: issued.expires_at_ms,
        })
        .expect("transcript");
        let proof = ChallengeProof {
            request_id: 9,
            challenge_id: issued.challenge_id,
            signature_der: sign_transcript(&key, &transcript).expect("sign"),
        };
        assert_eq!(
            store
                .verify("conn-a", &proof, public.as_bytes(), false, 2_000)
                .expect("verify"),
            device
        );
        assert_eq!(
            store.verify("conn-a", &proof, public.as_bytes(), false, 2_001),
            Err(TransportError::ChallengeReplayed)
        );
    }

    #[test]
    fn expired_revoked_and_wrong_connection_are_rejected() {
        let host = HostId::new("host");
        let device = DeviceId::new("device");
        let mut store = ChallengeStore::default();
        let issued = store
            .issue(IssueChallenge {
                connection_scope: "conn-a",
                request_id: 1,
                transport_version: "0.1.0",
                protocol_version: "0.5.0",
                host_id: &host,
                device_id: &device,
                tls_exporter: &[0; 32],
                now_ms: 0,
            })
            .expect("issue");
        let proof = ChallengeProof {
            request_id: 1,
            challenge_id: issued.challenge_id,
            signature_der: vec![0],
        };
        assert_eq!(
            store.verify("conn-b", &proof, &[], false, issued.expires_at_ms),
            Err(TransportError::AuthenticationFailed)
        );
        assert_eq!(
            store.verify("conn-a", &proof, &[], false, issued.expires_at_ms),
            Err(TransportError::ChallengeReplayed)
        );
    }

    #[test]
    fn expired_and_revoked_challenges_are_consumed_with_closed_errors() {
        let host = HostId::new("host");
        let device = DeviceId::new("device");
        let mut store = ChallengeStore::default();
        let expired = store
            .issue(IssueChallenge {
                connection_scope: "expired",
                request_id: 1,
                transport_version: "0.1.0",
                protocol_version: "0.5.0",
                host_id: &host,
                device_id: &device,
                tls_exporter: &[0; 32],
                now_ms: 0,
            })
            .expect("issue expired");
        let expired_proof = ChallengeProof {
            request_id: 1,
            challenge_id: expired.challenge_id,
            signature_der: vec![0],
        };
        assert_eq!(
            store.verify("expired", &expired_proof, &[], false, expired.expires_at_ms),
            Err(TransportError::ChallengeExpired)
        );

        let revoked = store
            .issue(IssueChallenge {
                connection_scope: "revoked",
                request_id: 2,
                transport_version: "0.1.0",
                protocol_version: "0.5.0",
                host_id: &host,
                device_id: &device,
                tls_exporter: &[0; 32],
                now_ms: 0,
            })
            .expect("issue revoked");
        let revoked_proof = ChallengeProof {
            request_id: 2,
            challenge_id: revoked.challenge_id,
            signature_der: vec![0],
        };
        assert_eq!(
            store.verify("revoked", &revoked_proof, &[], true, 1),
            Err(TransportError::AuthenticationFailed)
        );
    }
}
