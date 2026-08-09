//! Durable at-most-once mobile command admission and dispatch claims.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use kaleido_proto::command::{
    Actor, CommandAck, CommandEnvelope, CommandOutcome, DeviceCommandRequest,
};
use kaleido_proto::ids::{CommandId, ContentId, DeviceId};
use kaleido_proto::projection::ProjectionEnvelope;
use serde::{Deserialize, Serialize};

use crate::content::hex_digest;
use crate::error::StateError;

const OUTBOX_FILE: &str = "device-command-outbox.jsonl";
const OUTBOX_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchTicket {
    command_id: CommandId,
    token_digest: String,
}

impl DispatchTicket {
    pub fn command_id(&self) -> &CommandId {
        &self.command_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCommandAdmission {
    pub ack: CommandAck,
    pub dispatch_ticket: Option<DispatchTicket>,
    pub projections: Vec<ProjectionEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchClaim {
    pub envelope: CommandEnvelope,
    pub ticket: DispatchTicket,
    pub projections: Vec<ProjectionEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDispatch {
    pub ticket: DispatchTicket,
    pub envelope: CommandEnvelope,
}

#[derive(Debug, Clone)]
pub(crate) struct AdmissionRecovery {
    pub(crate) ticket: DispatchTicket,
    pub(crate) envelope: CommandEnvelope,
    pub(crate) admission_ack: CommandAck,
    pub(crate) complete_after_recovery: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutboxStatus {
    Ready,
    Claimed,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutboxRecord {
    format_version: u32,
    key_digest: String,
    request_digest: String,
    envelope: CommandEnvelope,
    admission_ack: CommandAck,
    status: OutboxStatus,
}

#[derive(Debug)]
pub(crate) struct DeviceCommandOutbox {
    path: PathBuf,
    by_key: BTreeMap<String, OutboxRecord>,
}

impl DeviceCommandOutbox {
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = root.as_ref().join(OUTBOX_FILE);
        let by_key = read_records(&path)?;
        Ok(Self { path, by_key })
    }

    pub(crate) fn existing(&self, key_digest: &str) -> Option<(&str, &CommandAck)> {
        self.by_key
            .get(key_digest)
            .map(|record| (record.request_digest.as_str(), &record.admission_ack))
    }

    pub(crate) fn insert_ready(
        &mut self,
        key_digest: String,
        request_digest: String,
        envelope: CommandEnvelope,
        admission_ack: CommandAck,
    ) -> Result<DispatchTicket, StateError> {
        let ticket = ticket_for(&key_digest, &request_digest, &envelope.command_id);
        self.insert_initial(OutboxRecord {
            format_version: OUTBOX_FORMAT_VERSION,
            key_digest,
            request_digest,
            envelope,
            admission_ack,
            status: OutboxStatus::Ready,
        })?;
        Ok(ticket)
    }

    pub(crate) fn insert_claimed(
        &mut self,
        key_digest: String,
        request_digest: String,
        envelope: CommandEnvelope,
        admission_ack: CommandAck,
    ) -> Result<DispatchTicket, StateError> {
        let ticket = ticket_for(&key_digest, &request_digest, &envelope.command_id);
        self.insert_initial(OutboxRecord {
            format_version: OUTBOX_FORMAT_VERSION,
            key_digest,
            request_digest,
            envelope,
            admission_ack,
            status: OutboxStatus::Claimed,
        })?;
        Ok(ticket)
    }

    pub(crate) fn claim(&mut self, ticket: &DispatchTicket) -> Result<CommandEnvelope, StateError> {
        let Some((key, current)) = self.by_key.iter().find(|(_, record)| {
            record.envelope.command_id == ticket.command_id
                && ticket_for(
                    &record.key_digest,
                    &record.request_digest,
                    &record.envelope.command_id,
                ) == *ticket
        }) else {
            return Err(StateError::DispatchNotAvailable {
                command_id: ticket.command_id.clone(),
            });
        };
        if current.status != OutboxStatus::Ready {
            return Err(StateError::DispatchNotAvailable {
                command_id: ticket.command_id.clone(),
            });
        }
        let key = key.clone();
        let mut claimed = current.clone();
        claimed.status = OutboxStatus::Claimed;
        self.append(&claimed)?;
        let envelope = claimed.envelope.clone();
        self.by_key.insert(key, claimed);
        Ok(envelope)
    }

    pub(crate) fn complete(&mut self, ticket: &DispatchTicket) -> Result<(), StateError> {
        let Some((key, current)) = self.by_key.iter().find(|(_, record)| {
            record.envelope.command_id == ticket.command_id
                && ticket_for(
                    &record.key_digest,
                    &record.request_digest,
                    &record.envelope.command_id,
                ) == *ticket
        }) else {
            return Err(StateError::DispatchNotAvailable {
                command_id: ticket.command_id.clone(),
            });
        };
        if current.status != OutboxStatus::Claimed {
            return Err(StateError::DispatchNotAvailable {
                command_id: ticket.command_id.clone(),
            });
        }
        let key = key.clone();
        let mut completed = current.clone();
        completed.status = OutboxStatus::Completed;
        self.append(&completed)?;
        self.by_key.insert(key, completed);
        Ok(())
    }

    pub(crate) fn ensure_claimed(
        &self,
        ticket: &DispatchTicket,
    ) -> Result<&CommandEnvelope, StateError> {
        self.by_key
            .values()
            .find(|record| {
                record.status == OutboxStatus::Claimed
                    && record.envelope.command_id == ticket.command_id
                    && ticket_for(
                        &record.key_digest,
                        &record.request_digest,
                        &record.envelope.command_id,
                    ) == *ticket
            })
            .map(|record| &record.envelope)
            .ok_or_else(|| StateError::DispatchNotAvailable {
                command_id: ticket.command_id.clone(),
            })
    }

    pub(crate) fn pending(&self) -> Vec<PendingDispatch> {
        self.by_key
            .values()
            .filter(|record| record.status == OutboxStatus::Ready)
            .map(|record| PendingDispatch {
                ticket: ticket_for(
                    &record.key_digest,
                    &record.request_digest,
                    &record.envelope.command_id,
                ),
                envelope: record.envelope.clone(),
            })
            .collect()
    }

    pub(crate) fn uncertain(&self) -> Vec<CommandId> {
        self.by_key
            .values()
            .filter(|record| {
                record.status == OutboxStatus::Claimed
                    && matches!(
                        record.admission_ack.outcome,
                        kaleido_proto::command::CommandOutcome::AcceptedLocally { .. }
                    )
            })
            .map(|record| record.envelope.command_id.clone())
            .collect()
    }

    pub(crate) fn referenced_content_ids(&self) -> BTreeSet<ContentId> {
        let mut ids = BTreeSet::new();
        for record in self
            .by_key
            .values()
            .filter(|record| record.status != OutboxStatus::Completed)
        {
            match &record.envelope.body {
                kaleido_proto::command::Command::SubmitPrompt { body, .. }
                | kaleido_proto::command::Command::EnqueueInput { body, .. }
                | kaleido_proto::command::Command::EditQueueEntry { body, .. } => {
                    ids.insert(body.content_id.clone());
                }
                kaleido_proto::command::Command::RespondAttention { response } => {
                    ids.extend(
                        response
                            .free_form_ref
                            .iter()
                            .map(|reference| reference.content_id.clone()),
                    );
                }
                kaleido_proto::command::Command::ReworkStep { reason_ref, .. }
                | kaleido_proto::command::Command::SkipStep { reason_ref, .. } => {
                    ids.extend(
                        reason_ref
                            .iter()
                            .map(|reference| reference.content_id.clone()),
                    );
                }
                kaleido_proto::command::Command::ReorderQueue { .. }
                | kaleido_proto::command::Command::CancelQueueEntry { .. }
                | kaleido_proto::command::Command::InterruptTurn { .. }
                | kaleido_proto::command::Command::RetryTurn { .. }
                | kaleido_proto::command::Command::OpenSession { .. }
                | kaleido_proto::command::Command::ResumeSession { .. }
                | kaleido_proto::command::Command::CloseSession { .. }
                | kaleido_proto::command::Command::AdvanceStep { .. }
                | kaleido_proto::command::Command::RetryStep { .. }
                | kaleido_proto::command::Command::CancelStep { .. }
                | kaleido_proto::command::Command::ReassignStep { .. }
                | kaleido_proto::command::Command::CancelWorkflow { .. } => {}
            }
        }
        ids
    }

    /// Every non-completed record is a write-ahead admission. Startup may
    /// repair its canonical local decision because this cannot repeat a
    /// runtime side effect. Accepted runtime routes remain Ready/Claimed;
    /// local-only outcomes become Completed after repair.
    pub(crate) fn admission_recoveries(&self) -> Vec<AdmissionRecovery> {
        self.by_key
            .values()
            .filter(|record| record.status != OutboxStatus::Completed)
            .map(|record| AdmissionRecovery {
                ticket: ticket_for(
                    &record.key_digest,
                    &record.request_digest,
                    &record.envelope.command_id,
                ),
                envelope: record.envelope.clone(),
                admission_ack: record.admission_ack.clone(),
                complete_after_recovery: !matches!(
                    record.admission_ack.outcome,
                    CommandOutcome::AcceptedLocally { .. }
                ),
            })
            .collect()
    }

    fn insert_initial(&mut self, record: OutboxRecord) -> Result<(), StateError> {
        if self.by_key.contains_key(&record.key_digest)
            || self
                .by_key
                .values()
                .any(|existing| existing.envelope.command_id == record.envelope.command_id)
        {
            return Err(StateError::CommandOutboxDiverged);
        }
        self.append(&record)?;
        self.by_key.insert(record.key_digest.clone(), record);
        Ok(())
    }

    fn append(&self, record: &OutboxRecord) -> Result<(), StateError> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        let created = !self.path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| StateError::io(&self.path, source))?;
        file.write_all(line.as_bytes())
            .map_err(|source| StateError::io(&self.path, source))?;
        file.flush()
            .map_err(|source| StateError::io(&self.path, source))?;
        file.sync_data()
            .map_err(|source| StateError::io(&self.path, source))?;
        if created {
            let parent = self
                .path
                .parent()
                .ok_or(StateError::CommandOutboxDiverged)?;
            crate::platform::sync_parent_directory(parent)
                .map_err(|source| StateError::io(parent, source))?;
        }
        Ok(())
    }
}

fn read_records(path: &Path) -> Result<BTreeMap<String, OutboxRecord>, StateError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(source) => return Err(StateError::io(path, source)),
    };
    let mut by_key: BTreeMap<String, OutboxRecord> = BTreeMap::new();
    let mut command_ids = BTreeSet::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| StateError::io(path, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<OutboxRecord>(&line).map_err(|_| {
            StateError::MalformedRecord {
                path: path.to_path_buf(),
                line: index + 1,
            }
        })?;
        if !valid_record(&record) {
            return Err(StateError::MalformedRecord {
                path: path.to_path_buf(),
                line: index + 1,
            });
        }
        record.envelope.validate()?;
        record.admission_ack.validate()?;
        let Some(request) = request_from_envelope(&record.envelope) else {
            return Err(StateError::MalformedRecord {
                path: path.to_path_buf(),
                line: index + 1,
            });
        };
        let Actor::Human { device_id } = &record.envelope.actor else {
            return Err(StateError::MalformedRecord {
                path: path.to_path_buf(),
                line: index + 1,
            });
        };
        let digests_match = device_command_key_digest(device_id, &request.idempotency_key)
            == record.key_digest
            && device_request_digest(&request).is_ok_and(|digest| digest == record.request_digest);
        if record.admission_ack.command_id != record.envelope.command_id
            || record.admission_ack.acked_at_ms != record.envelope.issued_at_ms
            || request.validate().is_err()
            || !digests_match
        {
            return Err(StateError::MalformedRecord {
                path: path.to_path_buf(),
                line: index + 1,
            });
        }
        match by_key.get(&record.key_digest) {
            None => {
                if !command_ids.insert(record.envelope.command_id.clone()) {
                    return Err(StateError::MalformedRecord {
                        path: path.to_path_buf(),
                        line: index + 1,
                    });
                }
            }
            Some(previous)
                if previous.request_digest != record.request_digest
                    || previous.envelope != record.envelope
                    || previous.admission_ack != record.admission_ack
                    || !valid_transition(previous.status, record.status) =>
            {
                return Err(StateError::MalformedRecord {
                    path: path.to_path_buf(),
                    line: index + 1,
                });
            }
            Some(_) => {}
        }
        by_key.insert(record.key_digest.clone(), record);
    }
    Ok(by_key)
}

fn valid_record(record: &OutboxRecord) -> bool {
    record.format_version == OUTBOX_FORMAT_VERSION
        && valid_digest(&record.key_digest)
        && valid_digest(&record.request_digest)
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_transition(previous: OutboxStatus, next: OutboxStatus) -> bool {
    matches!(
        (previous, next),
        (OutboxStatus::Ready, OutboxStatus::Claimed)
            | (OutboxStatus::Claimed, OutboxStatus::Completed)
    )
}

fn request_from_envelope(envelope: &CommandEnvelope) -> Option<DeviceCommandRequest> {
    let ttl_ms = match envelope.expires_at_ms {
        Some(expires_at_ms) => {
            let relative = expires_at_ms.checked_sub(envelope.issued_at_ms)?;
            Some(u64::try_from(relative).ok()?)
        }
        None => None,
    };
    Some(DeviceCommandRequest {
        idempotency_key: envelope.idempotency_key.clone(),
        ttl_ms,
        body: envelope.body.clone(),
    })
}

pub(crate) fn device_command_key_digest(device_id: &DeviceId, idempotency_key: &str) -> String {
    let mut material = Vec::new();
    extend_field(&mut material, b"human");
    extend_field(&mut material, device_id.as_str().as_bytes());
    extend_field(&mut material, idempotency_key.as_bytes());
    hex_digest(&material)
}

pub(crate) fn device_request_digest(request: &DeviceCommandRequest) -> Result<String, StateError> {
    let encoded = serde_json::to_vec(&(request.ttl_ms, &request.body))?;
    let mut material = Vec::new();
    extend_field(&mut material, b"device-request-v1");
    extend_field(&mut material, &encoded);
    Ok(hex_digest(&material))
}

fn ticket_for(key_digest: &str, request_digest: &str, command_id: &CommandId) -> DispatchTicket {
    let mut material = Vec::new();
    extend_field(&mut material, key_digest.as_bytes());
    extend_field(&mut material, request_digest.as_bytes());
    extend_field(&mut material, command_id.as_str().as_bytes());
    DispatchTicket {
        command_id: command_id.clone(),
        token_digest: hex_digest(&material),
    }
}

fn extend_field(target: &mut Vec<u8>, bytes: &[u8]) {
    target.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    target.extend_from_slice(bytes);
}
