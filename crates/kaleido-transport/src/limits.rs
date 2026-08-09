use std::collections::BTreeMap;
use std::net::IpAddr;

use kaleido_proto::ids::DeviceId;

use crate::control::{ControlFrame, CorrelationState};
use crate::error::TransportError;
use crate::{
    version_is_compatible, AUTH_TIMEOUT_MS, FRAME_IO_TIMEOUT_MS, HELLO_TIMEOUT_MS,
    IDLE_CLOSE_AFTER_MS, IDLE_PING_AFTER_MS, MAX_CONNECTIONS_PER_DEVICE,
    MAX_CONNECTIONS_PER_SOURCE_IP, MAX_FRAME_LENGTH, MAX_GLOBAL_CONNECTIONS,
    MAX_PRE_AUTH_CONNECTIONS, SESSION_LIFETIME_MS, TLS_HANDSHAKE_TIMEOUT_MS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStage {
    TlsHandshake,
    TransportHello,
    UacpHello,
    Authentication,
    Authenticated,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionAction {
    SendPing,
    CloseTimeout,
    CloseSessionExpired,
}

#[derive(Debug)]
pub struct ConnectionSession {
    stage: ConnectionStage,
    stage_deadline_ms: i64,
    authenticated_device: Option<DeviceId>,
    session_expires_at_ms: Option<i64>,
    last_valid_frame_at_ms: i64,
    ping_sent: bool,
    io_deadline: Option<(IoOperation, i64)>,
    correlation: CorrelationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoOperation {
    FrameRead,
    FrameWrite,
}

impl ConnectionSession {
    pub fn accepted(now_ms: i64) -> Result<Self, TransportError> {
        Ok(Self {
            stage: ConnectionStage::TlsHandshake,
            stage_deadline_ms: add_time(now_ms, TLS_HANDSHAKE_TIMEOUT_MS)?,
            authenticated_device: None,
            session_expires_at_ms: None,
            last_valid_frame_at_ms: now_ms,
            ping_sent: false,
            io_deadline: None,
            correlation: CorrelationState::default(),
        })
    }

    pub fn stage(&self) -> ConnectionStage {
        self.stage
    }

    pub fn authenticated_device(&self) -> Option<&DeviceId> {
        self.authenticated_device.as_ref()
    }

    pub fn correlation(&self) -> &CorrelationState {
        &self.correlation
    }

    pub fn correlation_mut(&mut self) -> &mut CorrelationState {
        &mut self.correlation
    }

    pub fn tls_established(&mut self, now_ms: i64) -> Result<(), TransportError> {
        self.require_stage(ConnectionStage::TlsHandshake)?;
        self.stage = ConnectionStage::TransportHello;
        self.stage_deadline_ms = add_time(now_ms, HELLO_TIMEOUT_MS)?;
        Ok(())
    }

    pub fn accept_transport_hello(
        &mut self,
        frame: &ControlFrame,
        now_ms: i64,
    ) -> Result<u64, TransportError> {
        self.require_stage(ConnectionStage::TransportHello)?;
        let ControlFrame::TransportHello {
            request_id,
            transport_version,
            max_frame_length,
        } = frame
        else {
            return Err(TransportError::AuthenticationFailed);
        };
        if *request_id == 0
            || !version_is_compatible(transport_version)
            || *max_frame_length != MAX_FRAME_LENGTH
        {
            return Err(TransportError::VersionMismatch);
        }
        self.stage = ConnectionStage::UacpHello;
        self.stage_deadline_ms = add_time(now_ms, HELLO_TIMEOUT_MS)?;
        self.last_valid_frame_at_ms = now_ms;
        Ok(*request_id)
    }

    pub fn accept_uacp_hello(
        &mut self,
        frame: &ControlFrame,
        now_ms: i64,
    ) -> Result<u64, TransportError> {
        self.require_stage(ConnectionStage::UacpHello)?;
        let ControlFrame::UacpHello {
            request_id,
            protocol_version,
        } = frame
        else {
            return Err(TransportError::AuthenticationFailed);
        };
        if *request_id == 0 || !kaleido_proto::version_is_compatible(protocol_version) {
            return Err(TransportError::VersionMismatch);
        }
        self.stage = ConnectionStage::Authentication;
        self.stage_deadline_ms = add_time(now_ms, AUTH_TIMEOUT_MS)?;
        self.last_valid_frame_at_ms = now_ms;
        Ok(*request_id)
    }

    pub fn ensure_auth_frame(&self, frame: &ControlFrame) -> Result<(), TransportError> {
        self.require_stage(ConnectionStage::Authentication)?;
        if matches!(
            frame,
            ControlFrame::PairRequest { .. }
                | ControlFrame::ChallengeRequest { .. }
                | ControlFrame::ChallengeProof { .. }
        ) {
            Ok(())
        } else {
            Err(TransportError::AuthenticationFailed)
        }
    }

    pub fn authenticate(
        &mut self,
        device_id: DeviceId,
        now_ms: i64,
    ) -> Result<i64, TransportError> {
        self.require_stage(ConnectionStage::Authentication)?;
        if device_id.is_empty() {
            return Err(TransportError::AuthenticationFailed);
        }
        let expiry = add_time(now_ms, SESSION_LIFETIME_MS)?;
        self.stage = ConnectionStage::Authenticated;
        self.authenticated_device = Some(device_id);
        self.session_expires_at_ms = Some(expiry);
        self.last_valid_frame_at_ms = now_ms;
        self.ping_sent = false;
        Ok(expiry)
    }

    pub fn ensure_business_frame(
        &mut self,
        frame: &ControlFrame,
        now_ms: i64,
    ) -> Result<(), TransportError> {
        self.require_stage(ConnectionStage::Authenticated)?;
        if self
            .session_expires_at_ms
            .is_some_and(|expiry| now_ms >= expiry)
        {
            self.close();
            return Err(TransportError::AuthenticationFailed);
        }
        if !is_business_frame(frame) {
            return Err(TransportError::AuthenticationFailed);
        }
        self.last_valid_frame_at_ms = now_ms;
        self.ping_sent = false;
        Ok(())
    }

    pub fn begin_io(&mut self, operation: IoOperation, now_ms: i64) -> Result<(), TransportError> {
        if self.stage == ConnectionStage::Closed || self.io_deadline.is_some() {
            return Err(TransportError::MalformedFrame);
        }
        self.io_deadline = Some((operation, add_time(now_ms, FRAME_IO_TIMEOUT_MS)?));
        Ok(())
    }

    pub fn complete_io(&mut self, operation: IoOperation) -> Result<(), TransportError> {
        match self.io_deadline.take() {
            Some((active, _)) if active == operation => Ok(()),
            Some(active) => {
                self.io_deadline = Some(active);
                Err(TransportError::MalformedFrame)
            }
            None => Err(TransportError::MalformedFrame),
        }
    }

    pub fn poll(&mut self, now_ms: i64) -> Option<ConnectionAction> {
        if self
            .io_deadline
            .is_some_and(|(_, deadline)| now_ms >= deadline)
        {
            self.close();
            return Some(ConnectionAction::CloseTimeout);
        }
        match self.stage {
            ConnectionStage::Closed => None,
            ConnectionStage::Authenticated => {
                if self
                    .session_expires_at_ms
                    .is_some_and(|expiry| now_ms >= expiry)
                {
                    self.close();
                    return Some(ConnectionAction::CloseSessionExpired);
                }
                let idle = now_ms.saturating_sub(self.last_valid_frame_at_ms);
                if idle >= IDLE_CLOSE_AFTER_MS {
                    self.close();
                    Some(ConnectionAction::CloseTimeout)
                } else if idle >= IDLE_PING_AFTER_MS && !self.ping_sent {
                    self.ping_sent = true;
                    Some(ConnectionAction::SendPing)
                } else {
                    None
                }
            }
            ConnectionStage::TlsHandshake
            | ConnectionStage::TransportHello
            | ConnectionStage::UacpHello
            | ConnectionStage::Authentication => {
                if now_ms >= self.stage_deadline_ms {
                    self.close();
                    Some(ConnectionAction::CloseTimeout)
                } else {
                    None
                }
            }
        }
    }

    pub fn close(&mut self) {
        self.stage = ConnectionStage::Closed;
        self.authenticated_device = None;
        self.session_expires_at_ms = None;
        self.io_deadline = None;
        self.correlation.cancel_all();
    }

    fn require_stage(&self, expected: ConnectionStage) -> Result<(), TransportError> {
        if self.stage == expected {
            Ok(())
        } else {
            Err(TransportError::AuthenticationFailed)
        }
    }
}

fn is_business_frame(frame: &ControlFrame) -> bool {
    matches!(
        frame,
        ControlFrame::ProjectionSubscribeFrame { .. }
            | ControlFrame::ProjectionSubscribeAckFrame { .. }
            | ControlFrame::ProjectionEnvelopeFrame { .. }
            | ControlFrame::ProjectionSubscriptionClosed { .. }
            | ControlFrame::UnsubscribeRequest { .. }
            | ControlFrame::UnsubscribeAck { .. }
            | ControlFrame::ContentWriteHeader { .. }
            | ControlFrame::ContentWriteResult { .. }
            | ControlFrame::ContentReadFrame { .. }
            | ControlFrame::ContentReadResult { .. }
            | ControlFrame::DeviceCommandFrame { .. }
            | ControlFrame::DeviceCommandAck { .. }
            | ControlFrame::Ping { .. }
            | ControlFrame::Pong { .. }
            | ControlFrame::TransportError { .. }
    )
}

#[derive(Debug)]
struct LimitEntry {
    source_ip: IpAddr,
    device_id: Option<DeviceId>,
}

#[derive(Debug, Default)]
pub struct ConnectionLimiter {
    entries: BTreeMap<String, LimitEntry>,
}

impl ConnectionLimiter {
    pub fn accept(&mut self, key: &str, source_ip: IpAddr) -> Result<(), TransportError> {
        if key.is_empty() || self.entries.contains_key(key) {
            return Err(TransportError::MalformedFrame);
        }
        if self.entries.len() >= MAX_GLOBAL_CONNECTIONS {
            return Err(TransportError::TooManyConnections);
        }
        let pre_auth = self
            .entries
            .values()
            .filter(|entry| entry.device_id.is_none())
            .count();
        if pre_auth >= MAX_PRE_AUTH_CONNECTIONS {
            return Err(TransportError::TooManyConnections);
        }
        let source_count = self
            .entries
            .values()
            .filter(|entry| entry.source_ip == source_ip)
            .count();
        if source_count >= MAX_CONNECTIONS_PER_SOURCE_IP {
            return Err(TransportError::TooManyConnections);
        }
        self.entries.insert(
            key.to_owned(),
            LimitEntry {
                source_ip,
                device_id: None,
            },
        );
        Ok(())
    }

    pub fn authenticate(&mut self, key: &str, device_id: &DeviceId) -> Result<(), TransportError> {
        if device_id.is_empty() {
            return Err(TransportError::AuthenticationFailed);
        }
        let device_count = self
            .entries
            .values()
            .filter(|entry| entry.device_id.as_ref() == Some(device_id))
            .count();
        if device_count >= MAX_CONNECTIONS_PER_DEVICE {
            return Err(TransportError::TooManyConnections);
        }
        let entry = self
            .entries
            .get_mut(key)
            .ok_or(TransportError::MalformedFrame)?;
        if entry.device_id.is_some() {
            return Err(TransportError::MalformedFrame);
        }
        entry.device_id = Some(device_id.clone());
        Ok(())
    }

    pub fn close(&mut self, key: &str) -> Result<(), TransportError> {
        if self.entries.remove(key).is_some() {
            Ok(())
        } else {
            Err(TransportError::MalformedFrame)
        }
    }

    pub fn connections_for_device(&self, device_id: &DeviceId) -> Vec<&str> {
        self.entries
            .iter()
            .filter_map(|(key, entry)| {
                (entry.device_id.as_ref() == Some(device_id)).then_some(key.as_str())
            })
            .collect()
    }
}

fn add_time(now_ms: i64, delta_ms: i64) -> Result<i64, TransportError> {
    now_ms
        .checked_add(delta_ms)
        .ok_or(TransportError::TimeOverflow)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::net::{IpAddr, Ipv4Addr};

    use kaleido_proto::ids::DeviceId;

    use super::{ConnectionAction, ConnectionLimiter, ConnectionSession, IoOperation};
    use crate::control::ControlFrame;
    use crate::error::TransportError;
    use crate::{MAX_CONNECTIONS_PER_DEVICE, MAX_CONNECTIONS_PER_SOURCE_IP, MAX_FRAME_LENGTH};

    #[test]
    fn unauthenticated_business_frame_is_rejected() {
        let mut connection = ConnectionSession::accepted(0).expect("connection");
        connection.tls_established(1).expect("TLS");
        let business = ControlFrame::Ping {
            request_id: 1,
            nonce: 2,
        };
        assert_eq!(
            connection.ensure_business_frame(&business, 2),
            Err(TransportError::AuthenticationFailed)
        );
        assert_eq!(
            connection.accept_transport_hello(&business, 2),
            Err(TransportError::AuthenticationFailed)
        );
    }

    #[test]
    fn phase_deadlines_and_session_expiry_cancel_connection_state() {
        let mut slow = ConnectionSession::accepted(0).expect("connection");
        assert_eq!(
            slow.poll(crate::TLS_HANDSHAKE_TIMEOUT_MS),
            Some(ConnectionAction::CloseTimeout)
        );

        let mut authenticated = ConnectionSession::accepted(0).expect("connection");
        authenticated.tls_established(0).expect("TLS");
        authenticated
            .accept_transport_hello(
                &ControlFrame::TransportHello {
                    request_id: 1,
                    transport_version: "0.1.0".to_owned(),
                    max_frame_length: MAX_FRAME_LENGTH,
                },
                0,
            )
            .expect("transport hello");
        authenticated
            .accept_uacp_hello(
                &ControlFrame::UacpHello {
                    request_id: 2,
                    protocol_version: "0.3.0".to_owned(),
                },
                0,
            )
            .expect("UACP hello");
        let expiry = authenticated
            .authenticate(DeviceId::new("device"), 0)
            .expect("authenticate");
        authenticated
            .correlation_mut()
            .begin_incoming_request(1)
            .expect("pending");
        assert_eq!(
            authenticated.poll(expiry),
            Some(ConnectionAction::CloseSessionExpired)
        );
        assert_eq!(authenticated.correlation().pending_count(), 0);
    }

    #[test]
    fn slow_frame_read_or_write_closes_and_cancels_pending_work() {
        for operation in [IoOperation::FrameRead, IoOperation::FrameWrite] {
            let mut connection = ConnectionSession::accepted(0).expect("connection");
            connection
                .correlation_mut()
                .begin_incoming_request(1)
                .expect("pending");
            connection.begin_io(operation, 10).expect("begin IO");
            assert_eq!(
                connection.poll(10 + crate::FRAME_IO_TIMEOUT_MS),
                Some(ConnectionAction::CloseTimeout)
            );
            assert_eq!(connection.correlation().pending_count(), 0);
        }
    }

    #[test]
    fn source_and_device_connection_limits_are_enforced() {
        let mut limiter = ConnectionLimiter::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for index in 0..MAX_CONNECTIONS_PER_SOURCE_IP {
            limiter.accept(&format!("c{index}"), ip).expect("accept");
        }
        assert_eq!(
            limiter.accept("overflow", ip),
            Err(TransportError::TooManyConnections)
        );

        let mut devices = ConnectionLimiter::default();
        let device = DeviceId::new("device");
        for index in 0..MAX_CONNECTIONS_PER_DEVICE {
            let key = format!("d{index}");
            devices
                .accept(&key, IpAddr::V4(Ipv4Addr::new(10, 0, 0, index as u8 + 1)))
                .expect("accept");
            devices.authenticate(&key, &device).expect("authenticate");
        }
        devices
            .accept("extra", IpAddr::V4(Ipv4Addr::new(10, 0, 0, 99)))
            .expect("accept pre-auth");
        assert_eq!(
            devices.authenticate("extra", &device),
            Err(TransportError::TooManyConnections)
        );
    }

    #[test]
    fn preauth_and_global_connection_limits_are_independent() {
        let mut preauth = ConnectionLimiter::default();
        for index in 0..crate::MAX_PRE_AUTH_CONNECTIONS {
            preauth
                .accept(
                    &format!("p{index}"),
                    IpAddr::V4(Ipv4Addr::new(10, 1, 0, index as u8 + 1)),
                )
                .expect("pre-auth connection");
        }
        assert_eq!(
            preauth.accept("preauth-overflow", IpAddr::V4(Ipv4Addr::new(10, 1, 1, 1))),
            Err(TransportError::TooManyConnections)
        );

        let mut global = ConnectionLimiter::default();
        for index in 0..crate::MAX_GLOBAL_CONNECTIONS {
            let key = format!("g{index}");
            global
                .accept(
                    &key,
                    IpAddr::V4(Ipv4Addr::new(
                        10,
                        2,
                        (index / 254) as u8,
                        (index % 254) as u8 + 1,
                    )),
                )
                .expect("global connection");
            global
                .authenticate(&key, &DeviceId::new(format!("device-{index}")))
                .expect("authenticate distinct device");
        }
        assert_eq!(
            global.accept("global-overflow", IpAddr::V4(Ipv4Addr::new(10, 3, 0, 1))),
            Err(TransportError::TooManyConnections)
        );
    }
}
