use std::collections::{BTreeMap, BTreeSet};

use kaleido_proto::command::{CommandAck, DeviceCommandRequest};
use kaleido_proto::content::{
    ContentReadRequest, ContentReadResponse, ContentWriteRequest, ContentWriteResponse,
};
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::ids::{DeviceId, HostId};
use kaleido_proto::projection::{ProjectionEnvelope, ProjectionSubscribe, ProjectionSubscribeAck};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::error::TransportError;
use crate::frame::Frame;
use crate::{MAX_ACTIVE_SUBSCRIPTIONS, MAX_PENDING_REQUESTS};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportErrorCode {
    VersionMismatch,
    MalformedFrame,
    FrameTooLarge,
    RateLimited,
    PairingInvalid,
    AuthenticationFailed,
    ChallengeExpired,
    ChallengeReplayed,
    DeviceRevoked,
    TooManyConnections,
    TooManySubscriptions,
    Internal,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub request_id: u64,
    pub secret: Vec<u8>,
    pub device_public_key_spki: Vec<u8>,
    pub device_label: String,
}

impl std::fmt::Debug for PairRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairRequest")
            .field("request_id", &self.request_id)
            .field("secret", &"[redacted]")
            .field("device_public_key_spki", &"[redacted]")
            .field("device_label", &"[redacted]")
            .finish()
    }
}

impl Drop for PairRequest {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairResponse {
    pub request_id: u64,
    pub device_id: DeviceId,
    pub host_id: HostId,
    pub transport_version: String,
    pub protocol_version: String,
    pub connection_id: String,
    pub session_expires_at_ms: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlFrame {
    TransportHello {
        request_id: u64,
        transport_version: String,
        max_frame_length: u32,
    },
    TransportHelloAck {
        request_id: u64,
        transport_version: String,
        max_frame_length: u32,
    },
    UacpHello {
        request_id: u64,
        protocol_version: String,
    },
    UacpHelloAck {
        request_id: u64,
        protocol_version: String,
    },
    PairRequest {
        #[serde(flatten)]
        request: PairRequest,
    },
    PairResponse {
        #[serde(flatten)]
        response: PairResponse,
    },
    ChallengeRequest {
        request_id: u64,
        device_id: DeviceId,
    },
    DeviceChallenge {
        request_id: u64,
        challenge_id: Vec<u8>,
        nonce: Vec<u8>,
        expires_at_ms: i64,
    },
    ChallengeProof {
        request_id: u64,
        challenge_id: Vec<u8>,
        signature_der: Vec<u8>,
    },
    AuthAccepted {
        request_id: u64,
        connection_id: String,
        expires_at_ms: i64,
    },
    ProjectionSubscribeFrame {
        request_id: u64,
        subscription_id: u64,
        subscribe: ProjectionSubscribe,
    },
    ProjectionSubscribeAckFrame {
        request_id: u64,
        subscription_id: u64,
        ack: ProjectionSubscribeAck,
    },
    ProjectionEnvelopeFrame {
        subscription_id: u64,
        envelope: ProjectionEnvelope,
    },
    ProjectionSubscriptionClosed {
        subscription_id: u64,
        error: CanonicalError,
    },
    UnsubscribeRequest {
        request_id: u64,
        subscription_id: u64,
    },
    UnsubscribeAck {
        request_id: u64,
        subscription_id: u64,
    },
    ContentWriteHeader {
        request_id: u64,
        request: ContentWriteRequest,
    },
    ContentWriteResult {
        request_id: u64,
        response: ContentWriteResponse,
    },
    ContentReadFrame {
        request_id: u64,
        request: ContentReadRequest,
    },
    ContentReadResult {
        request_id: u64,
        response: ContentReadResponse,
    },
    DeviceCommandFrame {
        request_id: u64,
        request: DeviceCommandRequest,
    },
    DeviceCommandAck {
        request_id: u64,
        ack: CommandAck,
    },
    Ping {
        request_id: u64,
        nonce: u64,
    },
    Pong {
        request_id: u64,
        nonce: u64,
    },
    TransportError {
        request_id: Option<u64>,
        code: TransportErrorCode,
        retriable: bool,
    },
}

impl std::fmt::Debug for ControlFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlFrame")
            .field("kind", &control_kind(self))
            .finish()
    }
}

fn control_kind(frame: &ControlFrame) -> &'static str {
    match frame {
        ControlFrame::TransportHello { .. } => "transport_hello",
        ControlFrame::TransportHelloAck { .. } => "transport_hello_ack",
        ControlFrame::UacpHello { .. } => "uacp_hello",
        ControlFrame::UacpHelloAck { .. } => "uacp_hello_ack",
        ControlFrame::PairRequest { .. } => "pair_request",
        ControlFrame::PairResponse { .. } => "pair_response",
        ControlFrame::ChallengeRequest { .. } => "challenge_request",
        ControlFrame::DeviceChallenge { .. } => "device_challenge",
        ControlFrame::ChallengeProof { .. } => "challenge_proof",
        ControlFrame::AuthAccepted { .. } => "auth_accepted",
        ControlFrame::ProjectionSubscribeFrame { .. } => "projection_subscribe_frame",
        ControlFrame::ProjectionSubscribeAckFrame { .. } => "projection_subscribe_ack_frame",
        ControlFrame::ProjectionEnvelopeFrame { .. } => "projection_envelope_frame",
        ControlFrame::ProjectionSubscriptionClosed { .. } => "projection_subscription_closed",
        ControlFrame::UnsubscribeRequest { .. } => "unsubscribe_request",
        ControlFrame::UnsubscribeAck { .. } => "unsubscribe_ack",
        ControlFrame::ContentWriteHeader { .. } => "content_write_header",
        ControlFrame::ContentWriteResult { .. } => "content_write_result",
        ControlFrame::ContentReadFrame { .. } => "content_read_frame",
        ControlFrame::ContentReadResult { .. } => "content_read_result",
        ControlFrame::DeviceCommandFrame { .. } => "device_command_frame",
        ControlFrame::DeviceCommandAck { .. } => "device_command_ack",
        ControlFrame::Ping { .. } => "ping",
        ControlFrame::Pong { .. } => "pong",
        ControlFrame::TransportError { .. } => "transport_error",
    }
}

impl ControlFrame {
    pub fn decode(bytes: &[u8]) -> Result<Self, TransportError> {
        if bytes.is_empty() || std::str::from_utf8(bytes).is_err() {
            return Err(TransportError::MalformedFrame);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let mut ignored = false;
        let frame = serde_ignored::deserialize(&mut deserializer, |_| ignored = true)
            .map_err(|_| TransportError::MalformedFrame)?;
        deserializer
            .end()
            .map_err(|_| TransportError::MalformedFrame)?;
        if ignored {
            Err(TransportError::MalformedFrame)
        } else {
            Ok(frame)
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, TransportError> {
        serde_json::to_vec(self).map_err(|_| TransportError::MalformedFrame)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedResponse {
    ProjectionSubscribeAck { subscription_id: u64 },
    UnsubscribeAck { subscription_id: u64 },
    ContentWriteResult,
    ContentReadResult,
    DeviceCommandAck,
    Pong { nonce: u64 },
}

#[derive(Debug, Default)]
pub struct CorrelationState {
    last_incoming_request_id: u64,
    last_outgoing_request_id: u64,
    incoming_pending: BTreeSet<u64>,
    outgoing_pending: BTreeMap<u64, ExpectedResponse>,
    subscriptions: BTreeSet<u64>,
    subscription_tombstones: BTreeMap<u64, bool>,
    awaiting_content: BTreeMap<u64, ExpectedContent>,
}

#[derive(Debug, Clone)]
struct ExpectedContent {
    byte_len: u64,
    digest: String,
}

impl CorrelationState {
    pub fn begin_incoming_request(&mut self, request_id: u64) -> Result<(), TransportError> {
        self.check_pending_limit()?;
        if request_id == 0 || request_id <= self.last_incoming_request_id {
            return Err(TransportError::MalformedFrame);
        }
        self.last_incoming_request_id = request_id;
        if !self.incoming_pending.insert(request_id) {
            return Err(TransportError::MalformedFrame);
        }
        Ok(())
    }

    pub fn complete_incoming_request(&mut self, request_id: u64) -> Result<(), TransportError> {
        if request_id == 0 || !self.incoming_pending.remove(&request_id) {
            return Err(TransportError::MalformedFrame);
        }
        self.awaiting_content.remove(&request_id);
        Ok(())
    }

    pub fn begin_outgoing_request(
        &mut self,
        request_id: u64,
        expected: ExpectedResponse,
    ) -> Result<(), TransportError> {
        self.check_pending_limit()?;
        if request_id == 0 || request_id <= self.last_outgoing_request_id {
            return Err(TransportError::MalformedFrame);
        }
        self.last_outgoing_request_id = request_id;
        if self.outgoing_pending.insert(request_id, expected).is_some() {
            return Err(TransportError::MalformedFrame);
        }
        Ok(())
    }

    pub fn accept_response(&mut self, frame: &ControlFrame) -> Result<(), TransportError> {
        let (request_id, actual) = response_shape(frame)?;
        let expected = self
            .outgoing_pending
            .remove(&request_id)
            .ok_or(TransportError::MalformedFrame)?;
        if expected != actual {
            return Err(TransportError::MalformedFrame);
        }
        Ok(())
    }

    pub fn open_subscription(&mut self, subscription_id: u64) -> Result<(), TransportError> {
        if subscription_id == 0
            || self.subscriptions.contains(&subscription_id)
            || self.subscription_tombstones.contains_key(&subscription_id)
        {
            return Err(TransportError::MalformedFrame);
        }
        if self.subscriptions.len() >= MAX_ACTIVE_SUBSCRIPTIONS {
            return Err(TransportError::TooManySubscriptions);
        }
        self.subscriptions.insert(subscription_id);
        Ok(())
    }

    pub fn require_subscription(&self, subscription_id: u64) -> Result<(), TransportError> {
        if subscription_id != 0 && self.subscriptions.contains(&subscription_id) {
            Ok(())
        } else {
            Err(TransportError::MalformedFrame)
        }
    }

    pub fn close_subscription(&mut self, subscription_id: u64) -> Result<(), TransportError> {
        if subscription_id == 0 || !self.subscriptions.remove(&subscription_id) {
            return Err(TransportError::MalformedFrame);
        }
        self.subscription_tombstones.insert(subscription_id, true);
        Ok(())
    }

    pub fn close_subscription_for_gap(
        &mut self,
        subscription_id: u64,
        error: &CanonicalError,
    ) -> Result<(), TransportError> {
        validate_subscription_gap(error)?;
        if subscription_id == 0
            || !self.subscriptions.remove(&subscription_id)
            || self
                .subscription_tombstones
                .insert(subscription_id, false)
                .is_some()
        {
            return Err(TransportError::MalformedFrame);
        }
        Ok(())
    }

    pub fn unsubscribe(
        &mut self,
        subscription_id: u64,
    ) -> Result<UnsubscribeDisposition, TransportError> {
        if subscription_id == 0 {
            return Err(TransportError::MalformedFrame);
        }
        if self.subscriptions.remove(&subscription_id) {
            self.subscription_tombstones.insert(subscription_id, true);
            return Ok(UnsubscribeDisposition::Active);
        }
        let acknowledged = self
            .subscription_tombstones
            .get_mut(&subscription_id)
            .ok_or(TransportError::MalformedFrame)?;
        if *acknowledged {
            return Err(TransportError::MalformedFrame);
        }
        *acknowledged = true;
        Ok(UnsubscribeDisposition::TombstonedAfterGap)
    }

    pub fn expect_content(
        &mut self,
        request_id: u64,
        request: &ContentWriteRequest,
    ) -> Result<(), TransportError> {
        request
            .validate()
            .map_err(|_| TransportError::MalformedFrame)?;
        if !self.incoming_pending.contains(&request_id)
            || self
                .awaiting_content
                .insert(
                    request_id,
                    ExpectedContent {
                        byte_len: request.byte_len,
                        digest: request.digest.clone(),
                    },
                )
                .is_some()
        {
            return Err(TransportError::MalformedFrame);
        }
        Ok(())
    }

    pub fn bind_content(&mut self, frame: Frame) -> Result<(u64, Vec<u8>), TransportError> {
        let Frame::Content { request_id, body } = frame else {
            return Err(TransportError::MalformedFrame);
        };
        let expected = self
            .awaiting_content
            .remove(&request_id)
            .ok_or(TransportError::MalformedFrame)?;
        let actual_digest = format!("sha256:{:x}", Sha256::digest(&body));
        if body.len() as u64 != expected.byte_len || actual_digest != expected.digest {
            self.incoming_pending.remove(&request_id);
            return Err(TransportError::MalformedFrame);
        }
        Ok((request_id, body))
    }

    pub fn cancel_all(&mut self) {
        self.incoming_pending.clear();
        self.outgoing_pending.clear();
        self.subscriptions.clear();
        self.subscription_tombstones.clear();
        self.awaiting_content.clear();
    }

    pub fn pending_count(&self) -> usize {
        self.incoming_pending.len() + self.outgoing_pending.len()
    }

    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    fn check_pending_limit(&self) -> Result<(), TransportError> {
        if self.pending_count() >= MAX_PENDING_REQUESTS {
            Err(TransportError::RateLimited)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsubscribeDisposition {
    Active,
    TombstonedAfterGap,
}

fn validate_subscription_gap(error: &CanonicalError) -> Result<(), TransportError> {
    if matches!(error.code, ErrorCode::CursorGap) && error.retriable && error.detail_ref.is_none() {
        Ok(())
    } else {
        Err(TransportError::MalformedFrame)
    }
}

fn response_shape(frame: &ControlFrame) -> Result<(u64, ExpectedResponse), TransportError> {
    match frame {
        ControlFrame::ProjectionSubscribeAckFrame {
            request_id,
            subscription_id,
            ..
        } => Ok((
            *request_id,
            ExpectedResponse::ProjectionSubscribeAck {
                subscription_id: *subscription_id,
            },
        )),
        ControlFrame::UnsubscribeAck {
            request_id,
            subscription_id,
        } => Ok((
            *request_id,
            ExpectedResponse::UnsubscribeAck {
                subscription_id: *subscription_id,
            },
        )),
        ControlFrame::ContentWriteResult { request_id, .. } => {
            Ok((*request_id, ExpectedResponse::ContentWriteResult))
        }
        ControlFrame::ContentReadResult { request_id, .. } => {
            Ok((*request_id, ExpectedResponse::ContentReadResult))
        }
        ControlFrame::DeviceCommandAck { request_id, .. } => {
            Ok((*request_id, ExpectedResponse::DeviceCommandAck))
        }
        ControlFrame::Pong { request_id, nonce } => {
            Ok((*request_id, ExpectedResponse::Pong { nonce: *nonce }))
        }
        _ => Err(TransportError::MalformedFrame),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use kaleido_proto::content::{ContentKind, ContentWriteRequest};
    use kaleido_proto::error::{CanonicalError, ErrorCode};
    use sha2::Digest;

    use super::{
        ControlFrame, CorrelationState, ExpectedResponse, PairRequest, UnsubscribeDisposition,
    };
    use crate::error::TransportError;
    use crate::frame::Frame;
    use crate::{MAX_ACTIVE_SUBSCRIPTIONS, MAX_PENDING_REQUESTS};

    #[test]
    fn control_json_is_closed_and_rejects_unknown_fields() {
        let valid = br#"{"kind":"ping","request_id":1,"nonce":2}"#;
        assert_eq!(
            ControlFrame::decode(valid).expect("decode"),
            ControlFrame::Ping {
                request_id: 1,
                nonce: 2
            }
        );
        for invalid in [
            br#"{"kind":"future","request_id":1}"#.as_slice(),
            br#"{"kind":"ping","request_id":1,"nonce":2,"detail":"leak"}"#.as_slice(),
            br#"{"kind":"transport_error","request_id":1,"code":"internal","retriable":false,"source":"leak"}"#.as_slice(),
        ] {
            assert!(ControlFrame::decode(invalid).is_err());
        }
    }

    #[test]
    fn pairing_control_record_is_flat_and_unknown_nested_data_is_rejected() {
        let frame = ControlFrame::PairRequest {
            request: PairRequest {
                request_id: 1,
                secret: vec![0; 32],
                device_public_key_spki: vec![1, 2],
                device_label: "phone".to_owned(),
            },
        };
        let encoded = frame.encode().expect("encode pairing");
        let text = std::str::from_utf8(&encoded).expect("UTF-8");
        assert!(text.starts_with(r#"{"kind":"pair_request","request_id":1,"secret":"#));
        assert!(!text.contains(r#""request":{"#));
        assert_eq!(
            ControlFrame::decode(&encoded).expect("decode pairing"),
            frame
        );
        assert!(ControlFrame::decode(
            br#"{"kind":"pair_request","request_id":1,"secret":[],"device_public_key_spki":[],"device_label":"p","extra":"rejected"}"#
        )
        .is_err());
    }

    #[test]
    fn request_ids_are_monotonic_bounded_and_correlated() {
        let mut state = CorrelationState::default();
        state
            .begin_outgoing_request(7, ExpectedResponse::Pong { nonce: 4 })
            .expect("request");
        assert_eq!(
            state.accept_response(&ControlFrame::Pong {
                request_id: 7,
                nonce: 5
            }),
            Err(TransportError::MalformedFrame)
        );
        assert_eq!(
            state.begin_outgoing_request(7, ExpectedResponse::Pong { nonce: 4 }),
            Err(TransportError::MalformedFrame)
        );

        let mut bounded = CorrelationState::default();
        for id in 1..=MAX_PENDING_REQUESTS as u64 {
            bounded.begin_incoming_request(id).expect("pending");
        }
        assert_eq!(
            bounded.begin_incoming_request(MAX_PENDING_REQUESTS as u64 + 1),
            Err(TransportError::RateLimited)
        );
    }

    #[test]
    fn active_subscriptions_are_bounded_and_ids_never_reused() {
        let mut state = CorrelationState::default();
        for id in 1..=MAX_ACTIVE_SUBSCRIPTIONS as u64 {
            state.open_subscription(id).expect("subscription");
        }
        assert_eq!(
            state.open_subscription(MAX_ACTIVE_SUBSCRIPTIONS as u64 + 1),
            Err(TransportError::TooManySubscriptions)
        );
        state.close_subscription(1).expect("close");
        assert_eq!(
            state.open_subscription(1),
            Err(TransportError::MalformedFrame)
        );
    }

    #[test]
    fn content_requires_one_matching_header_body_pair() {
        let body = b"secret body".to_vec();
        let request = ContentWriteRequest {
            content_kind: ContentKind::PlainText,
            byte_len: body.len() as u64,
            digest: format!("sha256:{:x}", sha2::Sha256::digest(&body)),
        };
        let mut state = CorrelationState::default();
        state.begin_incoming_request(3).expect("request");
        state.expect_content(3, &request).expect("header");
        assert_eq!(
            state
                .bind_content(Frame::Content {
                    request_id: 3,
                    body: body.clone()
                })
                .expect("body"),
            (3, body)
        );
        assert!(state
            .bind_content(Frame::Content {
                request_id: 3,
                body: b"duplicate".to_vec()
            })
            .is_err());
    }

    #[test]
    fn gap_terminal_closes_only_target_and_tombstone_never_reopens() {
        let mut state = CorrelationState::default();
        state.open_subscription(10).expect("first subscription");
        state.open_subscription(11).expect("second subscription");
        let gap = CanonicalError {
            code: ErrorCode::CursorGap,
            retriable: true,
            detail_ref: None,
            at_ms: 5,
        };
        state
            .close_subscription_for_gap(10, &gap)
            .expect("terminal");
        assert_eq!(state.subscription_count(), 1);
        state
            .require_subscription(11)
            .expect("other remains active");
        assert_eq!(
            state.require_subscription(10),
            Err(TransportError::MalformedFrame)
        );
        assert_eq!(
            state.close_subscription_for_gap(10, &gap),
            Err(TransportError::MalformedFrame)
        );
        assert_eq!(
            state.unsubscribe(10).expect("in-flight unsubscribe"),
            UnsubscribeDisposition::TombstonedAfterGap
        );
        assert_eq!(state.unsubscribe(10), Err(TransportError::MalformedFrame));
        assert_eq!(
            state.open_subscription(10),
            Err(TransportError::MalformedFrame)
        );
    }

    #[test]
    fn subscription_terminal_accepts_only_canonical_retriable_cursor_gap() {
        let invalid = [
            CanonicalError {
                code: ErrorCode::Internal,
                retriable: true,
                detail_ref: None,
                at_ms: 1,
            },
            CanonicalError {
                code: ErrorCode::CursorGap,
                retriable: false,
                detail_ref: None,
                at_ms: 1,
            },
        ];
        for error in invalid {
            let mut state = CorrelationState::default();
            state.open_subscription(1).expect("subscription");
            assert_eq!(
                state.close_subscription_for_gap(1, &error),
                Err(TransportError::MalformedFrame)
            );
        }
    }
}
