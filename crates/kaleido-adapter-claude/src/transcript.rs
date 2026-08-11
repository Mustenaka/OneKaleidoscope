//! Versioned OneKaleidoscope sidecar envelopes.
//!
//! This is the only wire shape Rust accepts from the Node bridge. `Value` is
//! used only to dispatch the local envelope by kind; every owned payload,
//! event and block is decoded through a `deny_unknown_fields` struct before
//! reduction. Claude's upstream unions are exhausted and converted on the
//! TypeScript side, never re-created in Rust.

use serde::Deserialize;
use serde_json::Value;

use crate::error::ClaudeAdapterError;

pub const SIDECAR_PROTOCOL: &str = "onekaleidoscope.claude.sidecar";
pub const SIDECAR_VERSION: u64 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    v: u64,
    protocol: String,
    kind: String,
    payload: Value,
}

macro_rules! closed_payload {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct $name { $( $field: $ty ),* }
    };
}

closed_payload!(ReadyPayload {
    sdk_version: String,
    cwd: String,
    resume_session_id: Option<String>,
});
closed_payload!(SessionPayload {
    session_id: String,
    cwd: String,
});
closed_payload!(SessionListPayload {
    cwd: String,
    sessions: Vec<SessionListEntry>,
});
closed_payload!(SessionListEntry {
    session_id: String,
    summary: String,
    last_modified: i64,
});
closed_payload!(SessionMessagesPayload {
    cwd: String,
    session_id: String,
    offset: u64,
    limit: u64,
    next_offset: Option<u64>,
    messages: Vec<SessionMessageEntry>,
});
closed_payload!(SessionMessageEntry {
    role: String,
    message_id: String,
    session_id: String,
    parent_tool_use_id: Option<String>,
    parent_agent_id: Option<String>,
    message_json: String,
});
closed_payload!(PromptAcceptedPayload { turn_id: String });
closed_payload!(PermissionRequestPayload {
    request_id: String,
    tool_name: String,
    input_json: String,
    tool_use_id: Option<String>,
    title: Option<String>,
});
closed_payload!(PermissionResultPayload {
    request_id: String,
    decision: String,
});
closed_payload!(QuestionRequestPayload {
    request_id: String,
    tool_name: String,
    questions: Vec<QuestionEntry>,
});
closed_payload!(QuestionEntry {
    question: String,
    header: String,
    multi_select: bool,
    options: Vec<QuestionOptionEntry>,
});
closed_payload!(QuestionOptionEntry {
    label: String,
    description: String,
});
closed_payload!(QuestionResultPayload {
    request_id: String,
    answers: Vec<QuestionAnswerEntry>,
});
closed_payload!(QuestionAnswerEntry {
    question_index: u64,
    values: Vec<String>,
});
closed_payload!(InterruptResultPayload {
    cancelled: bool,
    still_queued: Vec<String>,
});
closed_payload!(SdkEventPayload {
    session_id: String,
    turn_id: Option<String>,
    event: Value,
});
closed_payload!(ErrorPayload { code: String });
closed_payload!(EmptyPayload {});

closed_payload!(InitEvent {
    event: String,
    cwd: String,
    capabilities: Vec<String>,
});
closed_payload!(AssistantEvent {
    event: String,
    message_id: String,
    error: Option<String>,
    blocks: Vec<Value>,
});
closed_payload!(UserEvent {
    event: String,
    message_id: String,
    blocks: Vec<Value>,
});
closed_payload!(StreamTextEvent {
    event: String,
    block_index: u64,
    text: String,
});
closed_payload!(ToolProgressEvent {
    event: String,
    tool_use_id: String,
});
closed_payload!(ToolSummaryEvent {
    event: String,
    tool_use_ids: Vec<String>,
    summary: String,
});
closed_payload!(ResultEvent {
    event: String,
    subtype: String,
    is_error: bool,
    stop_reason: Option<String>,
    errors: Vec<String>,
});
closed_payload!(IgnoredEvent {
    event: String,
    label: String,
});
closed_payload!(AssistantTextBlock {
    kind: String,
    item_id: String,
    text: String,
});
closed_payload!(AssistantToolBlock {
    kind: String,
    item_id: String,
    name: String,
    input_json: String,
});
closed_payload!(AssistantIgnoredBlock {
    kind: String,
    item_id: String,
    label: String,
});
closed_payload!(UserTextBlock {
    kind: String,
    text: String,
});
closed_payload!(UserToolResultBlock {
    kind: String,
    tool_use_id: String,
    content_json: String,
    is_error: bool,
});
closed_payload!(UserIgnoredBlock {
    kind: String,
    label: String,
});

fn decode<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, ClaudeAdapterError> {
    serde_json::from_value(value.clone()).map_err(|_| ClaudeAdapterError::MalformedFrame)
}

fn validate_block(value: &Value, assistant: bool) -> Result<(), ClaudeAdapterError> {
    let tag = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(ClaudeAdapterError::MalformedFrame)?;
    if assistant {
        match tag {
            "text" | "thinking" => drop(decode::<AssistantTextBlock>(value)?),
            "tool_use" => drop(decode::<AssistantToolBlock>(value)?),
            "ignored" => drop(decode::<AssistantIgnoredBlock>(value)?),
            _ => return Err(ClaudeAdapterError::UnknownFrameKind),
        }
    } else {
        match tag {
            "text" => drop(decode::<UserTextBlock>(value)?),
            "tool_result" => drop(decode::<UserToolResultBlock>(value)?),
            "ignored" => drop(decode::<UserIgnoredBlock>(value)?),
            _ => return Err(ClaudeAdapterError::UnknownFrameKind),
        }
    }
    Ok(())
}

fn validate_sdk_event(payload: &SdkEventPayload) -> Result<(), ClaudeAdapterError> {
    let event = payload
        .event
        .get("event")
        .and_then(Value::as_str)
        .ok_or(ClaudeAdapterError::MalformedFrame)?;
    match event {
        "init" => drop(decode::<InitEvent>(&payload.event)?),
        "assistant" => {
            let decoded = decode::<AssistantEvent>(&payload.event)?;
            if decoded.error.as_deref().is_some_and(|error| {
                !matches!(
                    error,
                    "authentication_failed"
                        | "oauth_org_not_allowed"
                        | "billing_error"
                        | "rate_limit"
                        | "overloaded"
                        | "invalid_request"
                        | "model_not_found"
                        | "server_error"
                        | "unknown"
                        | "max_output_tokens"
                )
            }) {
                return Err(ClaudeAdapterError::MalformedFrame);
            }
            for block in &decoded.blocks {
                validate_block(block, true)?;
            }
        }
        "user" => {
            let decoded = decode::<UserEvent>(&payload.event)?;
            for block in &decoded.blocks {
                validate_block(block, false)?;
            }
        }
        "stream_text" => drop(decode::<StreamTextEvent>(&payload.event)?),
        "tool_progress" => drop(decode::<ToolProgressEvent>(&payload.event)?),
        "tool_summary" => drop(decode::<ToolSummaryEvent>(&payload.event)?),
        "result" => {
            let decoded = decode::<ResultEvent>(&payload.event)?;
            if !matches!(
                decoded.subtype.as_str(),
                "success"
                    | "error_during_execution"
                    | "error_max_turns"
                    | "error_max_budget_usd"
                    | "error_max_structured_output_retries"
            ) {
                return Err(ClaudeAdapterError::MalformedFrame);
            }
        }
        "ignored" => drop(decode::<IgnoredEvent>(&payload.event)?),
        _ => return Err(ClaudeAdapterError::UnknownFrameKind),
    }
    Ok(())
}

fn validate_payload(kind: &str, payload: &Value) -> Result<(), ClaudeAdapterError> {
    match kind {
        "ready" => {
            let decoded = decode::<ReadyPayload>(payload)?;
            // #[allow(kaleido::version_branch)] reason: this validates the pinned local sidecar contract and never selects provider features by version
            if decoded.sdk_version != "0.3.226" || decoded.cwd.is_empty() {
                return Err(ClaudeAdapterError::MalformedFrame);
            }
        }
        "session_started" | "session_resumed" => drop(decode::<SessionPayload>(payload)?),
        "session_list" => drop(decode::<SessionListPayload>(payload)?),
        "session_messages" => {
            let decoded = decode::<SessionMessagesPayload>(payload)?;
            if decoded
                .messages
                .iter()
                .any(|message| !matches!(message.role.as_str(), "user" | "assistant" | "system"))
            {
                return Err(ClaudeAdapterError::MalformedFrame);
            }
        }
        "prompt_accepted" => drop(decode::<PromptAcceptedPayload>(payload)?),
        "permission_request" => drop(decode::<PermissionRequestPayload>(payload)?),
        "permission_result" => drop(decode::<PermissionResultPayload>(payload)?),
        "question_request" => drop(decode::<QuestionRequestPayload>(payload)?),
        "question_result" => drop(decode::<QuestionResultPayload>(payload)?),
        "interrupt_result" => drop(decode::<InterruptResultPayload>(payload)?),
        "sdk_event" => {
            let decoded = decode::<SdkEventPayload>(payload)?;
            validate_sdk_event(&decoded)?;
        }
        "closed" => {
            let _: EmptyPayload = decode(payload)?;
        }
        "error" => drop(decode::<ErrorPayload>(payload)?),
        _ => return Err(ClaudeAdapterError::UnknownFrameKind),
    }
    Ok(())
}

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
        let envelope: Envelope =
            serde_json::from_value(envelope).map_err(|_| ClaudeAdapterError::MalformedFrame)?;
        let version = envelope.v;
        // #[allow(kaleido::version_branch)] reason: the closed local sidecar envelope must be decoded before any capability negotiation is possible
        if version != SIDECAR_VERSION {
            return Err(ClaudeAdapterError::ProtocolVersion {
                found: version,
                expected: SIDECAR_VERSION,
            });
        }
        if envelope.protocol != SIDECAR_PROTOCOL {
            return Err(ClaudeAdapterError::ProtocolViolation {
                detail: "sidecar protocol identifier mismatch",
            });
        }
        validate_payload(&envelope.kind, &envelope.payload)?;
        Ok(Self {
            direction,
            recorded_offset_ms,
            kind: envelope.kind,
            payload: envelope.payload,
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
