use std::io::{self, BufRead, Write};

use thiserror::Error;

use crate::fixture::{Direction, FixtureError, FixtureSink, Transport};

#[derive(Debug, Error)]
pub enum SseError {
    #[error("failed to read SSE stream: {0}")]
    Io(#[from] io::Error),
    #[error("SSE event did not contain a data field")]
    MissingData,
    #[error(transparent)]
    Fixture(#[from] FixtureError),
}

pub fn record_stream<R: BufRead, W: Write>(
    mut reader: R,
    fixture: &mut FixtureSink<W>,
    maximum_events: Option<usize>,
) -> Result<usize, SseError> {
    let mut data = Vec::new();
    let mut count = 0;
    let mut line = String::new();

    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            if !data.is_empty() {
                record_data(&data, fixture)?;
                count += 1;
            }
            return Ok(count);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if !data.is_empty() {
                record_data(&data, fixture)?;
                count += 1;
                data.clear();
                if maximum_events.is_some_and(|maximum| count >= maximum) {
                    return Ok(count);
                }
            }
        } else if let Some(value) = trimmed.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }
}

fn record_data<W: Write>(data: &[String], fixture: &mut FixtureSink<W>) -> Result<(), SseError> {
    if data.is_empty() {
        return Err(SseError::MissingData);
    }
    fixture.record(Direction::S2c, Transport::Sse, &data.join("\n"))?;
    Ok(())
}
