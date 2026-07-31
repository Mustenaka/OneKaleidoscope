//! Upstream frames, and the recorded transcripts this slice replays.
//!
//! A frame's payload stays private. ADR-0012 D-1 keeps untyped JSON inside this
//! crate, so the composition root receives frames it can hand back but never
//! inspect, and the only way a value leaves is as a canonical type.

use serde_json::Value;

use crate::error::CodexAdapterError;

/// Which side of the connection sent a frame.
///
/// A response carries no method, so the two request identifier spaces — the
/// client's and the server's — can only be told apart by direction. The
/// recorded fixtures use 1, 2, 3 for client requests and 0 for the server's
/// approval request, which would collide in a single table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

/// One JSON-RPC frame.
#[derive(Debug, Clone)]
pub struct TranscriptFrame {
    direction: Direction,
    recorded_offset_ms: i64,
    payload: Value,
}

impl TranscriptFrame {
    /// Builds a frame from raw wire bytes.
    pub fn from_wire(
        direction: Direction,
        recorded_offset_ms: i64,
        raw: &[u8],
    ) -> Result<Self, CodexAdapterError> {
        let payload = serde_json::from_slice(raw).map_err(|_| CodexAdapterError::MalformedFrame)?;
        Ok(Self {
            direction,
            recorded_offset_ms,
            payload,
        })
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn recorded_offset_ms(&self) -> i64 {
        self.recorded_offset_ms
    }

    pub(crate) fn payload(&self) -> &Value {
        &self.payload
    }

    pub(crate) fn method(&self) -> Option<&str> {
        self.payload.get("method").and_then(Value::as_str)
    }

    pub(crate) fn request_id(&self) -> Option<i64> {
        self.payload.get("id").and_then(Value::as_i64)
    }
}

/// A recorded conversation.
#[derive(Debug, Clone)]
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

    /// Returns a prefix of the transcript, for testing mid-stream state.
    pub fn prefix(&self, frames: usize) -> Self {
        Self {
            frames: self.frames.iter().take(frames).cloned().collect(),
        }
    }

    /// Returns the transcript with two frames swapped.
    ///
    /// The reducer must survive an approval arriving before the operation it
    /// refers to, and the only honest way to test that is to reorder recorded
    /// frames rather than to invent one.
    pub fn with_swapped(&self, left: usize, right: usize) -> Self {
        let mut frames = self.frames.clone();
        if left < frames.len() && right < frames.len() {
            frames.swap(left, right);
        }
        Self { frames }
    }
}

/// Parses a recorded transcript in the recorder's line format.
///
/// Each line wraps one frame with the direction and a relative offset:
/// `{"ts_ms": .., "dir": "s2c" | "c2s", "payload": { .. }}`.
pub fn parse_transcript(raw: &str) -> Result<Transcript, CodexAdapterError> {
    let mut frames = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let envelope = serde_json::from_str::<Value>(line)
            .map_err(|_| CodexAdapterError::MalformedTranscriptLine { line: line_number })?;
        let direction = match envelope.get("dir").and_then(Value::as_str) {
            Some("c2s") => Direction::ClientToServer,
            Some("s2c") => Direction::ServerToClient,
            _ => {
                return Err(CodexAdapterError::MalformedTranscriptEnvelope {
                    line: line_number,
                    field: "dir",
                });
            }
        };
        let recorded_offset_ms = envelope.get("ts_ms").and_then(Value::as_i64).ok_or(
            CodexAdapterError::MalformedTranscriptEnvelope {
                line: line_number,
                field: "ts_ms",
            },
        )?;
        let payload = envelope.get("payload").cloned().ok_or(
            CodexAdapterError::MalformedTranscriptEnvelope {
                line: line_number,
                field: "payload",
            },
        )?;
        frames.push(TranscriptFrame {
            direction,
            recorded_offset_ms,
            payload,
        });
    }
    Ok(Transcript { frames })
}
