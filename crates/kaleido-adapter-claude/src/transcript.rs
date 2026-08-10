//! Versioned OneKaleidoscope sidecar envelopes.
//!
//! This is the only wire shape Rust accepts from the Node bridge.  The
//! `payload` remains an untyped JSON value on purpose: Claude's upstream
//! message union is owned and checked by the official TypeScript SDK, not
//! re-created in Rust.

use serde_json::Value;

use crate::error::ClaudeAdapterError;

pub const SIDECAR_PROTOCOL: &str = "onekaleidoscope.claude.sidecar";
pub const SIDECAR_VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    BridgeToHost,
    HostToBridge,
}

#[derive(Debug, Clone)]
pub struct TranscriptFrame {
    direction: Direction,
    recorded_offset_ms: i64,
    kind: String,
    payload: Value,
}

impl TranscriptFrame {
    pub fn from_wire(
        direction: Direction,
        recorded_offset_ms: i64,
        raw: &[u8],
    ) -> Result<Self, ClaudeAdapterError> {
        let envelope =
            serde_json::from_slice::<Value>(raw).map_err(|_| ClaudeAdapterError::MalformedFrame)?;
        Self::from_value(direction, recorded_offset_ms, envelope)
    }

    pub fn from_value(
        direction: Direction,
        recorded_offset_ms: i64,
        envelope: Value,
    ) -> Result<Self, ClaudeAdapterError> {
        let version = envelope
            .get("v")
            .and_then(Value::as_u64)
            .ok_or(ClaudeAdapterError::MalformedFrame)?;
        // #[allow(kaleido::version_branch)] reason: the closed local sidecar envelope must be decoded before any capability negotiation is possible
        if version != SIDECAR_VERSION {
            return Err(ClaudeAdapterError::ProtocolVersion {
                found: version,
                expected: SIDECAR_VERSION,
            });
        }
        let protocol = envelope
            .get("protocol")
            .and_then(Value::as_str)
            .ok_or(ClaudeAdapterError::MalformedFrame)?;
        if protocol != SIDECAR_PROTOCOL {
            return Err(ClaudeAdapterError::ProtocolViolation {
                detail: "sidecar protocol identifier mismatch",
            });
        }
        let kind = envelope
            .get("kind")
            .and_then(Value::as_str)
            .ok_or(ClaudeAdapterError::MalformedFrame)?
            .to_owned();
        let payload = envelope
            .get("payload")
            .cloned()
            .ok_or(ClaudeAdapterError::MalformedFrame)?;
        Ok(Self {
            direction,
            recorded_offset_ms,
            kind,
            payload,
        })
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn recorded_offset_ms(&self) -> i64 {
        self.recorded_offset_ms
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub(crate) fn payload(&self) -> &Value {
        &self.payload
    }
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    frames: Vec<TranscriptFrame>,
}

impl Transcript {
    pub fn frames(&self) -> &[TranscriptFrame] {
        &self.frames
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn prefix(&self, frames: usize) -> Self {
        Self {
            frames: self.frames.iter().take(frames).cloned().collect(),
        }
    }
}

/// Parses recorded bridge output.  Empty lines are ignored so a transport can
/// use a conventional line-buffered writer without creating phantom frames.
pub fn parse_transcript(raw: &str) -> Result<Transcript, ClaudeAdapterError> {
    let mut frames = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let envelope = serde_json::from_str::<Value>(line)
            .map_err(|_| ClaudeAdapterError::MalformedTranscriptLine { line: line_number })?;
        // Captures taken directly from the bridge are already sidecar
        // envelopes.  Keep accepting the recorder wrapper below for replay
        // tooling that annotates direction and absolute time.
        if envelope.get("frame").is_none()
            && envelope.get("protocol").and_then(Value::as_str) == Some(SIDECAR_PROTOCOL)
        {
            frames.push(TranscriptFrame::from_value(
                Direction::BridgeToHost,
                i64::try_from(line_number.saturating_sub(1)).unwrap_or(i64::MAX),
                envelope,
            )?);
            continue;
        }
        let at_ms = envelope.get("at_ms").and_then(Value::as_i64).ok_or(
            ClaudeAdapterError::MalformedTranscriptEnvelope {
                line: line_number,
                field: "at_ms",
            },
        )?;
        let direction = match envelope.get("dir").and_then(Value::as_str) {
            Some("bridge_to_host") => Direction::BridgeToHost,
            Some("host_to_bridge") => Direction::HostToBridge,
            _ => {
                return Err(ClaudeAdapterError::MalformedTranscriptEnvelope {
                    line: line_number,
                    field: "dir",
                });
            }
        };
        let sidecar = envelope.get("frame").cloned().ok_or(
            ClaudeAdapterError::MalformedTranscriptEnvelope {
                line: line_number,
                field: "frame",
            },
        )?;
        frames.push(TranscriptFrame::from_value(direction, at_ms, sidecar)?);
    }
    Ok(Transcript { frames })
}
