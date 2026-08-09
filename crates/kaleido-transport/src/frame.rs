use crate::control::ControlFrame;
use crate::error::TransportError;
use crate::{MAX_CONTENT_BODY_BYTES, MAX_CONTROL_BODY_BYTES, MAX_FRAME_LENGTH};

const CONTROL_KIND: u8 = 0x01;
const CONTENT_KIND: u8 = 0x02;
const HEADER_BYTES: usize = 5;

#[derive(Clone, PartialEq, Eq)]
pub enum Frame {
    Control(Vec<u8>),
    Content { request_id: u64, body: Vec<u8> },
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Control(body) => formatter
                .debug_struct("ControlFrameBytes")
                .field("body_len", &body.len())
                .finish(),
            Self::Content { request_id, body } => formatter
                .debug_struct("ContentFrame")
                .field("request_id", request_id)
                .field("body_len", &body.len())
                .finish(),
        }
    }
}

impl Frame {
    pub fn decode_control(&self) -> Result<ControlFrame, TransportError> {
        match self {
            Self::Control(body) => ControlFrame::decode(body),
            Self::Content { .. } => Err(TransportError::MalformedFrame),
        }
    }
}

pub struct FrameDecoder {
    state: DecodeState,
    failed: bool,
}

impl std::fmt::Debug for FrameDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match &self.state {
            DecodeState::Header { .. } => "header",
            DecodeState::Body { .. } => "body",
        };
        formatter
            .debug_struct("FrameDecoder")
            .field("state", &state)
            .field("failed", &self.failed)
            .finish()
    }
}

enum DecodeState {
    Header {
        bytes: [u8; HEADER_BYTES],
        filled: usize,
    },
    Body {
        kind: u8,
        expected: usize,
        bytes: Vec<u8>,
    },
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            state: DecodeState::Header {
                bytes: [0; HEADER_BYTES],
                filled: 0,
            },
            failed: false,
        }
    }

    pub fn push(&mut self, mut input: &[u8]) -> Result<Vec<Frame>, TransportError> {
        if self.failed {
            return Err(TransportError::MalformedFrame);
        }
        let mut frames = Vec::new();
        while !input.is_empty() {
            match &mut self.state {
                DecodeState::Header { bytes, filled } => {
                    let take = (HEADER_BYTES - *filled).min(input.len());
                    let target = bytes
                        .get_mut(*filled..*filled + take)
                        .ok_or(TransportError::MalformedFrame)?;
                    let source = input.get(..take).ok_or(TransportError::MalformedFrame)?;
                    target.copy_from_slice(source);
                    *filled += take;
                    input = input.get(take..).ok_or(TransportError::MalformedFrame)?;
                    if *filled >= 4 {
                        let prefix: [u8; 4] = bytes
                            .get(..4)
                            .ok_or(TransportError::MalformedFrame)?
                            .try_into()
                            .map_err(|_| TransportError::MalformedFrame)?;
                        let length = u32::from_be_bytes(prefix);
                        if length == 0 {
                            self.failed = true;
                            return Err(TransportError::MalformedFrame);
                        }
                        if length > MAX_FRAME_LENGTH {
                            self.failed = true;
                            return Err(TransportError::FrameTooLarge);
                        }
                    }
                    if *filled == HEADER_BYTES {
                        let prefix: [u8; 4] = bytes
                            .get(..4)
                            .ok_or(TransportError::MalformedFrame)?
                            .try_into()
                            .map_err(|_| TransportError::MalformedFrame)?;
                        let length = u32::from_be_bytes(prefix);
                        let kind = *bytes.get(4).ok_or(TransportError::MalformedFrame)?;
                        let body_len = validate_header(length, kind).inspect_err(|_| {
                            self.failed = true;
                        })?;
                        self.state = DecodeState::Body {
                            kind,
                            expected: body_len,
                            bytes: Vec::with_capacity(body_len),
                        };
                    }
                }
                DecodeState::Body {
                    kind,
                    expected,
                    bytes,
                } => {
                    let remaining = expected.saturating_sub(bytes.len());
                    let take = remaining.min(input.len());
                    let source = input.get(..take).ok_or(TransportError::MalformedFrame)?;
                    bytes.extend_from_slice(source);
                    input = input.get(take..).ok_or(TransportError::MalformedFrame)?;
                    if bytes.len() == *expected {
                        let completed = std::mem::take(bytes);
                        let frame = decode_body(*kind, completed).inspect_err(|_| {
                            self.failed = true;
                        })?;
                        frames.push(frame);
                        self.state = DecodeState::Header {
                            bytes: [0; HEADER_BYTES],
                            filled: 0,
                        };
                    }
                }
            }
        }
        Ok(frames)
    }

    pub fn finish(self) -> Result<(), TransportError> {
        match self.state {
            DecodeState::Header { filled: 0, .. } if !self.failed => Ok(()),
            DecodeState::Header { .. } | DecodeState::Body { .. } => {
                Err(TransportError::MalformedFrame)
            }
        }
    }

    #[cfg(test)]
    fn allocated_body_capacity(&self) -> usize {
        match &self.state {
            DecodeState::Header { .. } => 0,
            DecodeState::Body { bytes, .. } => bytes.capacity(),
        }
    }
}

pub fn encode_control(frame: &ControlFrame) -> Result<Vec<u8>, TransportError> {
    let body = frame.encode()?;
    if body.is_empty() || body.len() > MAX_CONTROL_BODY_BYTES {
        return Err(TransportError::FrameTooLarge);
    }
    encode(CONTROL_KIND, &body)
}

pub fn encode_content(request_id: u64, body: &[u8]) -> Result<Vec<u8>, TransportError> {
    if request_id == 0 || body.is_empty() || body.len() > MAX_CONTENT_BODY_BYTES {
        return Err(TransportError::MalformedFrame);
    }
    let mut content = Vec::with_capacity(8 + body.len());
    content.extend_from_slice(&request_id.to_be_bytes());
    content.extend_from_slice(body);
    encode(CONTENT_KIND, &content)
}

fn encode(kind: u8, body: &[u8]) -> Result<Vec<u8>, TransportError> {
    let frame_length = body
        .len()
        .checked_add(1)
        .and_then(|length| u32::try_from(length).ok())
        .ok_or(TransportError::FrameTooLarge)?;
    if frame_length > MAX_FRAME_LENGTH {
        return Err(TransportError::FrameTooLarge);
    }
    let mut encoded = Vec::with_capacity(4 + frame_length as usize);
    encoded.extend_from_slice(&frame_length.to_be_bytes());
    encoded.push(kind);
    encoded.extend_from_slice(body);
    Ok(encoded)
}

fn validate_header(length: u32, kind: u8) -> Result<usize, TransportError> {
    if length == 0 {
        return Err(TransportError::MalformedFrame);
    }
    if length > MAX_FRAME_LENGTH {
        return Err(TransportError::FrameTooLarge);
    }
    let body_len = length
        .checked_sub(1)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(TransportError::MalformedFrame)?;
    match kind {
        CONTROL_KIND if (1..=MAX_CONTROL_BODY_BYTES).contains(&body_len) => Ok(body_len),
        CONTENT_KIND if (9..=8 + MAX_CONTENT_BODY_BYTES).contains(&body_len) => Ok(body_len),
        CONTROL_KIND | CONTENT_KIND => Err(TransportError::MalformedFrame),
        _ => Err(TransportError::MalformedFrame),
    }
}

fn decode_body(kind: u8, body: Vec<u8>) -> Result<Frame, TransportError> {
    match kind {
        CONTROL_KIND => {
            if std::str::from_utf8(&body).is_err() {
                return Err(TransportError::MalformedFrame);
            }
            Ok(Frame::Control(body))
        }
        CONTENT_KIND => {
            let request_bytes: [u8; 8] = body
                .get(..8)
                .ok_or(TransportError::MalformedFrame)?
                .try_into()
                .map_err(|_| TransportError::MalformedFrame)?;
            let request_id = u64::from_be_bytes(request_bytes);
            let content = body
                .get(8..)
                .ok_or(TransportError::MalformedFrame)?
                .to_vec();
            if request_id == 0 || content.is_empty() {
                return Err(TransportError::MalformedFrame);
            }
            Ok(Frame::Content {
                request_id,
                body: content,
            })
        }
        _ => Err(TransportError::MalformedFrame),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{encode_content, Frame, FrameDecoder};
    use crate::error::TransportError;
    use crate::MAX_FRAME_LENGTH;

    #[test]
    fn fragmented_frame_decodes_without_loss() {
        let encoded = encode_content(9, b"payload").expect("encode");
        let mut decoder = FrameDecoder::new();
        let mut frames = Vec::new();
        for byte in encoded {
            frames.extend(decoder.push(&[byte]).expect("decode byte"));
        }
        assert_eq!(
            frames,
            vec![Frame::Content {
                request_id: 9,
                body: b"payload".to_vec()
            }]
        );
        decoder.finish().expect("complete");
    }

    #[test]
    fn oversize_prefix_is_rejected_before_body_allocation() {
        let mut decoder = FrameDecoder::new();
        let prefix = (MAX_FRAME_LENGTH + 1).to_be_bytes();
        assert_eq!(decoder.push(&prefix), Err(TransportError::FrameTooLarge));
        assert_eq!(decoder.allocated_body_capacity(), 0);
    }

    #[test]
    fn unknown_kind_and_truncation_fail_closed() {
        let mut unknown = FrameDecoder::new();
        assert_eq!(
            unknown.push(&[0, 0, 0, 2, 0xff]),
            Err(TransportError::MalformedFrame)
        );
        let encoded = encode_content(2, b"body").expect("encode");
        let mut truncated = FrameDecoder::new();
        truncated
            .push(encoded.get(..encoded.len() - 1).expect("slice"))
            .expect("partial read");
        assert_eq!(truncated.finish(), Err(TransportError::MalformedFrame));
    }
}
