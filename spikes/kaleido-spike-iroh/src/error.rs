use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpikeError {
    #[error("TicketEncode: {0}")]
    TicketEncode(#[source] serde_json::Error),

    #[error("InvalidTicketBase64: {0}")]
    InvalidTicketBase64(#[source] base64::DecodeError),

    #[error("InvalidTicketPayload: {0}")]
    InvalidTicketPayload(#[source] serde_json::Error),

    #[error("Bind: {0}")]
    Bind(String),

    #[error("ConnectTimeout: connection attempt exceeded {0:?}")]
    ConnectTimeout(Duration),

    #[error("Connect: {0}")]
    Connect(String),

    #[error("Accept: {0}")]
    Accept(String),

    #[error("Stream: {0}")]
    Stream(String),

    #[error("StreamTimeout: {operation} exceeded {duration:?}")]
    StreamTimeout {
        operation: &'static str,
        duration: Duration,
    },

    #[error("Protocol: {0}")]
    Protocol(String),

    #[error("RecordIo: {0}")]
    RecordIo(#[from] std::io::Error),

    #[error("RecordJson: {0}")]
    RecordJson(#[from] serde_json::Error),
}

impl SpikeError {
    pub const fn variant_name(&self) -> &'static str {
        match self {
            Self::TicketEncode(_) => "TicketEncode",
            Self::InvalidTicketBase64(_) => "InvalidTicketBase64",
            Self::InvalidTicketPayload(_) => "InvalidTicketPayload",
            Self::Bind(_) => "Bind",
            Self::ConnectTimeout(_) => "ConnectTimeout",
            Self::Connect(_) => "Connect",
            Self::Accept(_) => "Accept",
            Self::Stream(_) => "Stream",
            Self::StreamTimeout { .. } => "StreamTimeout",
            Self::Protocol(_) => "Protocol",
            Self::RecordIo(_) => "RecordIo",
            Self::RecordJson(_) => "RecordJson",
        }
    }
}
