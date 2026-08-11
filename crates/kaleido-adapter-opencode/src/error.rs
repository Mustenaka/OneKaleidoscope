use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpenCodeDecodeError {
    #[error("malformed OpenCode JSON payload")]
    MalformedJson(#[from] serde_json::Error),
    #[error("OpenCode event is missing its type discriminator")]
    MissingEventType,
    #[error("unknown OpenCode event type {0}")]
    UnknownEventType(String),
    #[error("OpenCode event scope does not match the selected session")]
    ScopeMismatch,
    #[error("OpenCode prompt admission does not match the request: {0}")]
    AdmissionMismatch(&'static str),
    #[error("OpenCode payload has an unsupported shape")]
    UnsupportedShape,
    #[error("OpenCode structured reduction failed at {0}")]
    ReductionFailed(String),
}

#[derive(Debug, Error)]
pub enum OpenCodeAdapterError {
    #[error(transparent)]
    Decode(#[from] OpenCodeDecodeError),
    #[error("OpenCode HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("OpenCode server returned HTTP status {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("OpenCode SSE stream ended unexpectedly")]
    SseDisconnected,
    #[error("OpenCode SSE event has no cursor; replay is not lossless")]
    CursorlessEvent,
    #[error("OpenCode SSE reconnect requires a REST snapshot before tailing")]
    SnapshotRequired,
    #[error("OpenCode runtime is not connected")]
    NotConnected,
    #[error("OpenCode runtime is already connected")]
    AlreadyConnected,
    #[error("OpenCode operation is not proven by observed traffic")]
    CapabilityUnavailable,
    #[error("OpenCode canonical effect violates the contract: {0}")]
    Contract(#[from] kaleido_proto::ContractViolation),
    #[error("content access failed: {0}")]
    Content(#[from] kaleido_adapter::content::ContentAccessError),
    #[error("OpenCode transport failed: {0}")]
    Transport(String),
}
