use std::fmt;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::redact::{detect_leaks, Redactor};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    C2s,
    S2c,
}

impl fmt::Display for Direction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::C2s => formatter.write_str("c2s"),
            Self::S2c => formatter.write_str("s2c"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Http,
    Sse,
}

impl fmt::Display for Transport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio => formatter.write_str("stdio"),
            Self::Http => formatter.write_str("http"),
            Self::Sse => formatter.write_str("sse"),
        }
    }
}

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("payload must be a JSON object")]
    PayloadShape,
    #[error("redacted payload still contains {count} prohibited value(s): {categories}")]
    Leak { count: usize, categories: String },
    #[error("recording duration exceeded the fixture timestamp range")]
    TimestampRange,
    #[error("failed to write fixture: {0}")]
    Write(#[from] io::Error),
}

#[derive(Debug)]
pub struct FixtureSink<W> {
    writer: W,
    started: Instant,
    redactor: Redactor,
}

impl<W: Write> FixtureSink<W> {
    pub fn new(writer: W, redactor: Redactor) -> Self {
        Self {
            writer,
            started: Instant::now(),
            redactor,
        }
    }

    pub fn record(
        &mut self,
        direction: Direction,
        transport: Transport,
        payload: &str,
    ) -> Result<(), FixtureError> {
        self.record_at(self.started.elapsed(), direction, transport, payload)
    }

    pub fn record_at(
        &mut self,
        elapsed: Duration,
        direction: Direction,
        transport: Transport,
        payload: &str,
    ) -> Result<(), FixtureError> {
        let redacted = self.redactor.redact(payload);
        let value: Value = serde_json::from_str(&redacted)?;
        if !value.is_object() {
            return Err(FixtureError::PayloadShape);
        }
        let leaks = detect_leaks(&redacted, &value);
        if !leaks.is_empty() {
            let categories = leaks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(FixtureError::Leak {
                count: leaks.len(),
                categories,
            });
        }
        let timestamp =
            u64::try_from(elapsed.as_millis()).map_err(|_| FixtureError::TimestampRange)?;
        writeln!(
            self.writer,
            "{{\"ts_ms\":{},\"dir\":\"{}\",\"transport\":\"{}\",\"payload\":{}}}",
            timestamp, direction, transport, redacted
        )?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

pub fn http_request_payload(
    method: &str,
    path: &str,
    content_type: &str,
    body: &str,
) -> Result<String, FixtureError> {
    http_payload(method, path, None, content_type, body)
}

pub fn http_response_payload(
    method: &str,
    path: &str,
    status: u16,
    content_type: &str,
    body: &str,
) -> Result<String, FixtureError> {
    http_payload(method, path, Some(status), content_type, body)
}

fn http_payload(
    method: &str,
    path: &str,
    status: Option<u16>,
    content_type: &str,
    body: &str,
) -> Result<String, FixtureError> {
    let _: Value = serde_json::from_str(body)?;
    let method = serde_json::to_string(method)?;
    let path = serde_json::to_string(path)?;
    let content_type = serde_json::to_string(content_type)?;
    let status = status.map_or_else(|| "null".to_owned(), |value| value.to_string());
    Ok(format!(
        "{{\"method\":{method},\"path\":{path},\"status\":{status},\"content_type\":{content_type},\"body\":{body}}}"
    ))
}

#[derive(Debug, Deserialize)]
pub struct FixtureRecord {
    pub ts_ms: u64,
    pub dir: Direction,
    pub transport: Transport,
    pub payload: Value,
}
