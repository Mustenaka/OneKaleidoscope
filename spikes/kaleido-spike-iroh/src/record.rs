use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::SpikeError;

pub const RECORD_SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeRecord {
    pub schema: u32,
    pub run_id: String,
    pub started_at: String,
    pub role: String,
    pub label: String,
    pub iroh_version: String,
    pub os: String,
    pub remote_endpoint_id: Option<String>,
    pub connect_ok: bool,
    pub connect_ms: Option<u64>,
    pub direct_path_opened_ms: Option<u64>,
    pub direct_path_selected_ms: Option<u64>,
    pub ended_direct: bool,
    pub selected_path_at_end: Option<String>,
    pub relay_url: Option<String>,
    pub local_direct_addrs: Vec<String>,
    pub rtt_relay_ms: Option<f64>,
    pub rtt_direct_ms: Option<f64>,
    pub pings_sent: u64,
    pub pongs_recv: u64,
    pub window_secs: u64,
    pub error: Option<String>,
}

pub fn append_record(path: impl AsRef<Path>, record: &ProbeRecord) -> Result<(), SpikeError> {
    let mut encoded = serde_json::to_vec(record).map_err(SpikeError::RecordJson)?;
    encoded.push(b'\n');

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(SpikeError::RecordIo)?;
    file.write_all(&encoded).map_err(SpikeError::RecordIo)?;
    file.flush().map_err(SpikeError::RecordIo)
}

pub fn read_records(
    path: impl AsRef<Path>,
    warnings: &mut impl Write,
) -> Result<Vec<ProbeRecord>, SpikeError> {
    let file = File::open(path).map_err(SpikeError::RecordIo)?;
    read_records_from(BufReader::new(file), warnings)
}

pub fn read_records_from(
    reader: impl BufRead,
    warnings: &mut impl Write,
) -> Result<Vec<ProbeRecord>, SpikeError> {
    let mut records = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(SpikeError::RecordIo)?;
        match serde_json::from_str(&line) {
            Ok(record) => records.push(record),
            Err(error) => {
                writeln!(
                    warnings,
                    "warning: skipped malformed JSONL line {}: {error}",
                    index + 1
                )
                .map_err(SpikeError::RecordIo)?;
            }
        }
    }

    Ok(records)
}

pub fn summarize_file(
    path: impl AsRef<Path>,
    warnings: &mut impl Write,
) -> Result<String, SpikeError> {
    let records = read_records(path, warnings)?;
    Ok(summarize(&records))
}

pub fn summarize(records: &[ProbeRecord]) -> String {
    let records = unique_runs(records);
    let mut by_label: BTreeMap<&str, Vec<&ProbeRecord>> = BTreeMap::new();
    for record in &records {
        by_label.entry(&record.label).or_default().push(record);
    }

    let mut lines = Vec::with_capacity(by_label.len() + 1);
    for (label, group) in &by_label {
        lines.push(format_group(label, group));
    }

    let g0_records: Vec<&ProbeRecord> = records
        .iter()
        .copied()
        .filter(|record| record.label.starts_with("4g"))
        .collect();
    if !g0_records.is_empty() {
        lines.push(format_g0_verdict(&g0_records));
    }

    lines.join("\n")
}

fn unique_runs(records: &[ProbeRecord]) -> Vec<&ProbeRecord> {
    let mut run_indexes: BTreeMap<&str, usize> = BTreeMap::new();
    let mut unique: Vec<&ProbeRecord> = Vec::with_capacity(records.len());

    for record in records {
        if record.run_id.is_empty() {
            unique.push(record);
            continue;
        }

        if let Some(index) = run_indexes.get(record.run_id.as_str()).copied() {
            if let Some(existing) = unique.get_mut(index) {
                if existing.role != "dial" && record.role == "dial" {
                    *existing = record;
                }
            }
        } else {
            run_indexes.insert(record.run_id.as_str(), unique.len());
            unique.push(record);
        }
    }

    unique
}

fn format_group(label: &str, records: &[&ProbeRecord]) -> String {
    let runs = records.len();
    let connect_ok = records.iter().filter(|record| record.connect_ok).count();
    let direct = records
        .iter()
        .filter(|record| record.connect_ok && record.ended_direct)
        .count();
    let relay_only = records
        .iter()
        .filter(|record| {
            record.connect_ok && record.selected_path_at_end.as_deref() == Some("relay")
        })
        .count();
    let direct_rate = percentage(direct, runs);
    let median = median_selected_ms(records)
        .map(|milliseconds| format!("{:.2}s", milliseconds / 1_000.0))
        .unwrap_or_else(|| "n/a".to_owned());

    format!(
        "label={label:<3}  runs={runs}  connect_ok={connect_ok}  direct={direct} \
         ({direct_rate:.1}%)  relay_only={relay_only}  median_time_to_direct={median}"
    )
}

fn format_g0_verdict(records: &[&ProbeRecord]) -> String {
    let runs = records.len();
    let direct = records
        .iter()
        .filter(|record| record.connect_ok && record.ended_direct)
        .count();
    let direct_rate = percentage(direct, runs);

    if (direct as u128) * 100 >= (runs as u128) * 60 {
        format!("G0 VERDICT: direct rate {direct_rate:.1}% >= 60.0% -> L2 relay stays OPTIONAL")
    } else {
        format!(
            "G0 VERDICT: direct rate {direct_rate:.1}% < 60.0% -> L2 relay becomes MANDATORY for v1"
        )
    }
}

fn percentage(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}

fn median_selected_ms(records: &[&ProbeRecord]) -> Option<f64> {
    let mut values: Vec<u64> = records
        .iter()
        .filter_map(|record| record.direct_path_selected_ms)
        .collect();
    values.sort_unstable();

    let midpoint = values.len() / 2;
    if values.is_empty() {
        None
    } else if values.len().is_multiple_of(2) {
        let lower_index = midpoint.checked_sub(1)?;
        let lower = *values.get(lower_index)?;
        let upper = *values.get(midpoint)?;
        Some((lower as f64 + upper as f64) / 2.0)
    } else {
        values.get(midpoint).map(|value| *value as f64)
    }
}
