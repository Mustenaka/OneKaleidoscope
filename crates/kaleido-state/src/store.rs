//! The canonical store: validate, transition, assign a cursor, append.
//!
//! The write order is fixed by `docs/ARCHITECTURE.md` section 6 and is the
//! reason a reload converges: nothing in [`CanonicalState::apply`] reads a
//! clock, so replaying the same records in the same order reproduces the same
//! state field for field (`docs/PROTOCOL.md` section 5.4).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kaleido_proto::attention::{
    AttentionAnswerSource, AttentionItem, AttentionState, ReplyRejection,
};
use kaleido_proto::capability::Capability;
use kaleido_proto::command::{
    Actor, Command, CommandAck, CommandEnvelope, CommandOutcome, DeviceCommandRequest,
    RuntimeAcceptanceKind,
};
use kaleido_proto::content::{
    ContentKind, ContentReadRequest, ContentReadResponse, ContentRef, ContentWriteRequest,
    ContentWriteResponse,
};
use kaleido_proto::effect::{Cursor, LogRecord, SessionSnapshot, StateEffect, StreamKey};
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::ids::{
    CommandId, DeviceId, HostId, ProjectId, ProviderRuntimeId, QueueEntryId, SessionId,
};
use kaleido_proto::projection::{
    ProjectionEnvelope, ProjectionKey, ProjectionPayload, ProjectionSubscribe, PROJECTION_VERSION,
};
use kaleido_proto::queue::{QueueEntry, QueueIntent, QueueState};
use serde::{Deserialize, Serialize};

use crate::command_outbox::{
    device_command_key_digest, device_request_digest, AdmissionRecovery, DeviceCommandAdmission,
    DeviceCommandOutbox, DispatchClaim, DispatchTicket, PendingDispatch,
};
use crate::content::{hex_digest, ContentStore};
use crate::error::StateError;
use crate::log::StreamLog;
use crate::projection::{self, DiagnosticProjectionEnvelope, ProjectionName};
use crate::projection_journal::{ProjectionJournal, ProjectionReplay};
use crate::state::CanonicalState;

/// One canonical append and the full projection entries it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCommit {
    pub records: Vec<LogRecord>,
    pub projections: Vec<ProjectionEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueDeliveryClaim {
    pub entry: QueueEntry,
    pub command_id: CommandId,
    pub projections: Vec<ProjectionEnvelope>,
}

/// File holding the idempotency side table.
///
/// This is store bookkeeping rather than canonical state, so it is not a
/// `LogRecord`. Only the digest of the `(actor, key)` pair is written, so a
/// device identifier never lands on disk.
const IDEMPOTENCY_FILE: &str = "idempotency.jsonl";
const IDEMPOTENCY_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdempotencyRecord {
    format_version: u32,
    key_digest: String,
    command_id: CommandId,
}

/// Where append timestamps come from.
#[derive(Debug, Clone, Copy)]
pub enum ClockSource {
    /// Wall clock, for a live host.
    System,
    /// A fixed base instant, for deterministic replay.
    Fixed { at_ms: i64 },
}

impl ClockSource {
    fn now_ms(&self) -> i64 {
        match self {
            ClockSource::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
                .unwrap_or(0),
            ClockSource::Fixed { at_ms } => *at_ms,
        }
    }
}

/// Canonical state plus its durable log and content-addressed bodies.
#[derive(Debug)]
pub struct CanonicalStore {
    root: PathBuf,
    log: StreamLog,
    content: ContentStore,
    projections: ProjectionJournal,
    device_outbox: DeviceCommandOutbox,
    state: CanonicalState,
    cursors: HashMap<StreamKey, Cursor>,
    last_appended_at_ms: i64,
    clock: ClockSource,
    idempotency: BTreeMap<String, CommandId>,
    recovery_required: bool,
}

impl CanonicalStore {
    /// Opens an empty store rooted at `root`.
    pub fn open(root: impl AsRef<Path>, clock: ClockSource) -> Result<Self, StateError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|source| StateError::io(&root, source))?;
        Ok(Self {
            log: StreamLog::open(&root)?,
            content: ContentStore::open(&root)?,
            projections: ProjectionJournal::open(&root)?,
            device_outbox: DeviceCommandOutbox::open(&root)?,
            root,
            state: CanonicalState::default(),
            cursors: HashMap::new(),
            last_appended_at_ms: i64::MIN,
            clock,
            idempotency: BTreeMap::new(),
            recovery_required: false,
        })
    }

    /// Rebuilds a store from its durable log.
    ///
    /// Reading verifies every stream is contiguous first, so a tampered or
    /// truncated log fails here rather than producing a plausible-looking but
    /// wrong state.
    pub fn load(root: impl AsRef<Path>, clock: ClockSource) -> Result<Self, StateError> {
        let mut store = Self::open(root, clock)?;
        let records = store.log.read_all()?;
        let mut expected = ProjectionJournal::memory(store.projections.retention_entries())?;
        for record in &records {
            let before = store.state.clone();
            store.state.apply(&record.effect)?;
            for key in projection::affected_keys(&before, &store.state, &record.effect) {
                let payload = projection::build(&store.state, &key)?;
                expected.record(key, payload)?;
            }
            store.cursors.insert(record.stream.clone(), record.cursor);
            store.last_appended_at_ms = store.last_appended_at_ms.max(record.appended_at_ms);
        }
        store.projections.reconcile(&expected)?;
        store.idempotency = store.read_idempotency()?;
        store.recover_admissions()?;
        let mut protected_content = store
            .state
            .content_refs()
            .into_iter()
            .map(|reference| reference.content_id.clone())
            .collect::<BTreeSet<_>>();
        protected_content.extend(store.device_outbox.referenced_content_ids());
        store
            .content
            .cleanup_after_replay(store.clock.now_ms(), &protected_content)?;
        Ok(store)
    }

    fn ensure_writable(&self) -> Result<(), StateError> {
        if self.recovery_required {
            Err(StateError::RecoveryRequired)
        } else {
            Ok(())
        }
    }

    fn recover_admissions(&mut self) -> Result<(), StateError> {
        for recovery in self.device_outbox.admission_recoveries() {
            let terminal_already_committed = self.recover_admission(&recovery)?;
            if recovery.complete_after_recovery || terminal_already_committed {
                self.device_outbox.complete(&recovery.ticket)?;
            }
        }
        Ok(())
    }

    fn recover_admission(&mut self, recovery: &AdmissionRecovery) -> Result<bool, StateError> {
        let matching_acks = self
            .state
            .acknowledgements()
            .iter()
            .filter(|ack| ack.command_id == recovery.envelope.command_id)
            .collect::<Vec<_>>();
        if let Some(existing) = matching_acks.first() {
            if *existing != &recovery.admission_ack {
                return Err(StateError::CommandOutboxDiverged);
            }
            let terminal_already_committed = match matching_acks.as_slice() {
                [_] => false,
                [_, terminal]
                    if matches!(
                        (&recovery.envelope.body, &terminal.outcome),
                        (
                            Command::SubmitPrompt { .. },
                            CommandOutcome::AcceptedByRuntime { .. }
                                | CommandOutcome::Rejected { .. }
                        ) | (
                            Command::RespondAttention { .. },
                            CommandOutcome::Rejected { .. }
                        )
                    ) =>
                {
                    true
                }
                _ => return Err(StateError::CommandOutboxDiverged),
            };
            if device_route_uses_submit(&recovery.envelope.body) {
                self.ensure_command_idempotency(&recovery.envelope)?;
            }
            return Ok(terminal_already_committed);
        }

        if let CommandOutcome::Enqueued { entry_id } = &recovery.admission_ack.outcome {
            let Command::EnqueueInput {
                session_id,
                body,
                intent,
            } = &recovery.envelope.body
            else {
                return Err(StateError::CommandOutboxDiverged);
            };
            if entry_id != &queue_entry_id(&recovery.envelope.command_id) {
                return Err(StateError::CommandOutboxDiverged);
            }
            if let Some(existing) = self.state.queue_entry(entry_id) {
                let position = u32::try_from(
                    self.state
                        .queue_of(session_id)
                        .iter()
                        .filter(|entry| entry.id != *entry_id && entry.state.is_pending())
                        .count(),
                )
                .unwrap_or(u32::MAX);
                let expected = QueueEntry {
                    id: entry_id.clone(),
                    session_id: session_id.clone(),
                    position,
                    intent: *intent,
                    body: body.clone(),
                    state: QueueState::Pending,
                    editable: true,
                    created_at_ms: recovery.envelope.issued_at_ms,
                    updated_at_ms: recovery.envelope.issued_at_ms,
                };
                if existing != &expected {
                    return Err(StateError::CommandOutboxDiverged);
                }
                self.apply_trusted(&StateEffect::CommandAcknowledged {
                    ack: recovery.admission_ack.clone(),
                })?;
                self.ensure_command_idempotency(&recovery.envelope)?;
                return Ok(false);
            }
        }

        if matches!(
            recovery.admission_ack.outcome,
            CommandOutcome::AcceptedLocally { .. }
        ) {
            if let Command::RespondAttention { response } = &recovery.envelope.body {
                let Some(attention) = self.state.attention(&response.attention_id) else {
                    return Err(StateError::CommandOutboxDiverged);
                };
                if matches!(
                    &attention.state,
                    AttentionState::Answered {
                        option_id,
                        free_form_ref,
                        question_answers,
                        decided_at_ms,
                        answer_source: AttentionAnswerSource::LocalCommand { command_id },
                    } if option_id == &response.option_id
                        && free_form_ref == &response.free_form_ref
                        && question_answers == &response.question_answers
                        && *decided_at_ms == recovery.envelope.issued_at_ms
                        && command_id == &recovery.envelope.command_id
                ) {
                    self.apply_trusted(&StateEffect::CommandAcknowledged {
                        ack: recovery.admission_ack.clone(),
                    })?;
                    self.ensure_command_idempotency(&recovery.envelope)?;
                    return Ok(false);
                }
                if !attention.state.is_open() {
                    return Err(StateError::CommandOutboxDiverged);
                }
            }
        }

        let actual_ack = if device_route_uses_submit(&recovery.envelope.body) {
            self.submit_command(&recovery.envelope, recovery.envelope.issued_at_ms)?
        } else {
            self.apply_trusted(&StateEffect::CommandAcknowledged {
                ack: recovery.admission_ack.clone(),
            })?;
            recovery.admission_ack.clone()
        };
        if actual_ack != recovery.admission_ack {
            return Err(StateError::CommandOutboxDiverged);
        }
        Ok(false)
    }

    fn ensure_command_idempotency(&mut self, envelope: &CommandEnvelope) -> Result<(), StateError> {
        let key = hex_digest(envelope.dedupe_key().as_bytes());
        match self.idempotency.get(&key) {
            Some(command_id) if command_id == &envelope.command_id => Ok(()),
            Some(_) => Err(StateError::CommandOutboxDiverged),
            None => self.record_idempotency(&key, &envelope.command_id),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state(&self) -> &CanonicalState {
        &self.state
    }

    pub fn content(&self) -> &ContentStore {
        &self.content
    }

    pub fn log(&self) -> &StreamLog {
        &self.log
    }

    pub fn projection_journal(&self) -> &ProjectionJournal {
        &self.projections
    }

    pub fn projection_replay(
        &self,
        request: &ProjectionSubscribe,
        at_ms: i64,
    ) -> Result<ProjectionReplay, StateError> {
        self.projections.replay(request, at_ms)
    }

    pub fn write_content_for_device(
        &mut self,
        device_id: &DeviceId,
        request: &ContentWriteRequest,
        bytes: &[u8],
        now_ms: i64,
    ) -> Result<ContentWriteResponse, StateError> {
        self.content
            .write_for_device(device_id, request, bytes, now_ms)
    }

    pub fn read_content_for_device(
        &self,
        device_id: &DeviceId,
        request: &ContentReadRequest,
        now_ms: i64,
    ) -> Result<ContentReadResponse, StateError> {
        if let Some(reference) = self.state.content_ref(&request.content_id) {
            return self.content.read_reference(reference, request);
        }
        self.content.read_for_device(device_id, request, now_ms)
    }

    /// Durably admits one authenticated mobile request. A dispatch ticket is
    /// returned only for the two R3 commands with a real runtime route.
    pub fn admit_device_command(
        &mut self,
        authenticated_device: &DeviceId,
        envelope: &CommandEnvelope,
        request: &DeviceCommandRequest,
        now_ms: i64,
    ) -> Result<DeviceCommandAdmission, StateError> {
        self.ensure_writable()?;
        request.validate()?;
        envelope.validate()?;
        validate_device_envelope(authenticated_device, envelope, request, now_ms)?;
        let key_digest = device_command_key_digest(authenticated_device, &request.idempotency_key);
        let request_digest = device_request_digest(request)?;
        if let Some((original_digest, original_ack)) = self.device_outbox.existing(&key_digest) {
            let outcome = if original_digest == request_digest {
                CommandOutcome::Duplicate {
                    original_command_id: original_ack.command_id.clone(),
                }
            } else {
                CommandOutcome::Rejected {
                    error: canonical_error(ErrorCode::IdempotencyConflict, false, now_ms),
                }
            };
            return Ok(DeviceCommandAdmission {
                ack: CommandAck {
                    command_id: envelope.command_id.clone(),
                    outcome,
                    acked_at_ms: now_ms,
                },
                dispatch_ticket: None,
                projections: Vec::new(),
            });
        }
        self.validate_device_command_content(authenticated_device, &request.body, now_ms)?;
        let (admission_ack, runtime_dispatch) = self.preview_device_admission(envelope, now_ms);
        if runtime_dispatch {
            let ticket = self.device_outbox.insert_ready(
                key_digest,
                request_digest,
                envelope.clone(),
                admission_ack.clone(),
            )?;
            let checkpoint = self.projections.checkpoint();
            let actual_ack = match self.submit_command(envelope, now_ms) {
                Ok(ack) => ack,
                Err(error) => {
                    self.recovery_required = true;
                    return Err(error);
                }
            };
            if actual_ack != admission_ack {
                self.recovery_required = true;
                return Err(StateError::CommandOutboxDiverged);
            }
            return Ok(DeviceCommandAdmission {
                ack: actual_ack,
                dispatch_ticket: Some(ticket),
                projections: self.projections.changes_since(&checkpoint)?,
            });
        }

        let ticket = self.device_outbox.insert_claimed(
            key_digest,
            request_digest,
            envelope.clone(),
            admission_ack.clone(),
        )?;
        let checkpoint = self.projections.checkpoint();
        let actual_ack = if device_route_uses_submit(&envelope.body)
            && matches!(
                admission_ack.outcome,
                CommandOutcome::AcceptedLocally { .. } | CommandOutcome::Enqueued { .. }
            ) {
            match self.submit_command(envelope, now_ms) {
                Ok(ack) => ack,
                Err(error) => {
                    self.recovery_required = true;
                    return Err(error);
                }
            }
        } else {
            if let Err(error) = self.apply_trusted(&StateEffect::CommandAcknowledged {
                ack: admission_ack.clone(),
            }) {
                self.recovery_required = true;
                return Err(error);
            }
            admission_ack.clone()
        };
        if actual_ack != admission_ack {
            self.recovery_required = true;
            return Err(StateError::CommandOutboxDiverged);
        }
        if let Err(error) = self.device_outbox.complete(&ticket) {
            self.recovery_required = true;
            return Err(error);
        }
        Ok(DeviceCommandAdmission {
            ack: actual_ack,
            dispatch_ticket: None,
            projections: self.projections.changes_since(&checkpoint)?,
        })
    }

    /// Durably claims before returning bytes to a runtime worker. A crash
    /// after the claim is deliberately uncertain and is never auto-replayed.
    pub fn claim_dispatch(&mut self, ticket: &DispatchTicket) -> Result<DispatchClaim, StateError> {
        self.ensure_writable()?;
        let envelope = self.device_outbox.claim(ticket)?;
        Ok(DispatchClaim {
            envelope,
            ticket: ticket.clone(),
            projections: Vec::new(),
        })
    }

    /// Closes a durable Ready route that cannot exist after runtime recovery.
    ///
    /// The original command and target session are never rewritten. Claiming
    /// first preserves at-most-once delivery; the explicit rejection then
    /// gives the device a durable terminal result instead of leaving an old
    /// ephemeral provider session permanently pending.
    pub fn reject_ready_dispatch(
        &mut self,
        ticket: &DispatchTicket,
        at_ms: i64,
    ) -> Result<Vec<ProjectionEnvelope>, StateError> {
        self.ensure_writable()?;
        let pending = self
            .device_outbox
            .pending()
            .into_iter()
            .find(|pending| &pending.ticket == ticket)
            .ok_or_else(|| StateError::DispatchNotAvailable {
                command_id: ticket.command_id().clone(),
            })?;
        if !matches!(
            pending.envelope.body,
            Command::SubmitPrompt { .. }
                | Command::RespondAttention { .. }
                | Command::InterruptTurn { .. }
                | Command::ResumeSession { .. }
        ) {
            return Err(StateError::DeviceCommandMismatch {
                detail: "only a concrete R3 runtime route can be rejected during recovery",
            });
        }
        let envelope = self.device_outbox.claim(ticket)?;
        let checkpoint = self.projections.checkpoint();
        self.apply(&StateEffect::CommandAcknowledged {
            ack: CommandAck {
                command_id: envelope.command_id,
                outcome: CommandOutcome::Rejected {
                    error: CanonicalError {
                        code: ErrorCode::RuntimeUnavailable,
                        retriable: true,
                        detail_ref: None,
                        at_ms,
                    },
                },
                acked_at_ms: at_ms,
            },
        })?;
        self.device_outbox.complete(ticket)?;
        self.projections.changes_since(&checkpoint)
    }

    /// Records structured runtime results and only then completes the claim.
    /// If any append fails the claim remains uncertain and cannot be dispatched
    /// again automatically.
    pub fn finish_dispatch(
        &mut self,
        ticket: &DispatchTicket,
        effects: &[StateEffect],
    ) -> Result<Vec<ProjectionEnvelope>, StateError> {
        self.ensure_writable()?;
        let claimed = self.device_outbox.ensure_claimed(ticket)?.clone();
        let command_id = claimed.command_id.clone();
        let matching_terminal_ack_count = effects
            .iter()
            .filter(|effect| {
                matches!(
                    effect,
                    StateEffect::CommandAcknowledged { ack }
                        if ack.command_id == command_id
                            && matches!(
                                ack.outcome,
                                CommandOutcome::AcceptedByRuntime { .. }
                                    | CommandOutcome::Rejected { .. }
                            )
                )
            })
            .count();
        let has_invalid_matching_ack = effects.iter().any(|effect| {
            matches!(
                effect,
                StateEffect::CommandAcknowledged { ack }
                    if ack.command_id == command_id
                        && !matches!(
                            ack.outcome,
                            CommandOutcome::AcceptedByRuntime { .. }
                                | CommandOutcome::Rejected { .. }
                        )
            )
        });
        let has_foreign_ack = effects.iter().any(|effect| {
            matches!(
                effect,
                StateEffect::CommandAcknowledged { ack } if ack.command_id != command_id
            )
        });
        let terminal_outcome = effects.iter().find_map(|effect| match effect {
            StateEffect::CommandAcknowledged { ack } if ack.command_id == command_id => {
                Some(&ack.outcome)
            }
            _ => None,
        });
        let common_terminal =
            matching_terminal_ack_count == 1 && !has_invalid_matching_ack && !has_foreign_ack;
        let valid_terminal = match &claimed.body {
            Command::SubmitPrompt { session_id, .. } => {
                common_terminal
                    && match terminal_outcome {
                        Some(CommandOutcome::AcceptedByRuntime {
                            session_id: acknowledged_session,
                            acceptance_kind: RuntimeAcceptanceKind::PromptTurn,
                            ..
                        }) if acknowledged_session == session_id => effects.iter().any(|effect| {
                            matches!(
                                effect,
                                StateEffect::TurnUpserted { turn }
                                    if &turn.session_id == session_id
                                        && matches!(
                                            &turn.origin,
                                            kaleido_proto::turn::TurnOrigin::RemoteCommand {
                                                command_id: turn_command
                                            } if turn_command == &command_id
                                        )
                            )
                        }),
                        Some(CommandOutcome::Rejected { .. }) => true,
                        _ => false,
                    }
            }
            Command::InterruptTurn { session_id, .. } => {
                common_terminal
                    && (matches!(
                        terminal_outcome,
                            Some(CommandOutcome::AcceptedByRuntime {
                                session_id: acknowledged_session,
                                acceptance_kind: RuntimeAcceptanceKind::SessionControl,
                                ..
                        }) if acknowledged_session == session_id
                    ) || matches!(terminal_outcome, Some(CommandOutcome::Rejected { .. })))
            }
            Command::ResumeSession { session_id } => {
                matching_terminal_ack_count == 0
                    && !has_invalid_matching_ack
                    && !has_foreign_ack
                    && effects.iter().any(|effect| {
                        matches!(
                            effect,
                            StateEffect::SessionUpserted { session }
                                if &session.id == session_id
                                    && self.state.runtime_of(session).is_some()
                        )
                    })
            }
            // A structured provider result closes the route, but is not an
            // AcceptedByRuntime command receipt. Require the exact Answered
            // transition correlated to this local command instead.
            Command::RespondAttention { response } => {
                matching_terminal_ack_count == 0
                    && !has_invalid_matching_ack
                    && !has_foreign_ack
                    && effects.iter().any(|effect| {
                        matches!(
                            effect,
                            StateEffect::AttentionUpserted { item }
                                if item.id == response.attention_id
                                    && item.session_id == response.session_id
                                    && matches!(
                                        &item.state,
                                        AttentionState::Answered {
                                            option_id,
                                            free_form_ref,
                                            question_answers,
                                            answer_source:
                                                AttentionAnswerSource::LocalCommand {
                                                    command_id: answered_command,
                                                },
                                            ..
                                        } if option_id == &response.option_id
                                            && free_form_ref == &response.free_form_ref
                                            && question_answers == &response.question_answers
                                            && answered_command == &command_id
                                    )
                        )
                    })
            }
            _ => false,
        };
        if !valid_terminal {
            return Err(StateError::DeviceCommandMismatch {
                detail:
                    "dispatch result does not close the claimed command's concrete runtime route",
            });
        }
        let checkpoint = self.projections.checkpoint();
        self.apply_all(effects)?;
        self.device_outbox.complete(ticket)?;
        self.projections.changes_since(&checkpoint)
    }

    pub fn pending_dispatches(&self) -> Vec<PendingDispatch> {
        self.device_outbox.pending()
    }

    pub fn pending_queue_deliveries(&self) -> Vec<(QueueEntry, CommandId)> {
        self.state
            .sessions()
            .filter(|session| session.active_turn_id.is_none())
            .filter_map(|session| {
                self.state.queue_of(&session.id).into_iter().find(|entry| {
                    entry.intent == QueueIntent::NewTurn
                        && matches!(entry.state, QueueState::Pending)
                })
            })
            .filter_map(|entry| {
                self.state
                    .queue_command_id(&entry.id)
                    .cloned()
                    .map(|command_id| (entry.clone(), command_id))
            })
            .collect()
    }

    /// Durably moves a pending entry to Submitting before provider bytes can
    /// leave the process. A crash afterwards is intentionally uncertain and
    /// is never auto-replayed.
    pub fn claim_queue_delivery(
        &mut self,
        entry_id: &QueueEntryId,
        at_ms: i64,
    ) -> Result<QueueDeliveryClaim, StateError> {
        self.ensure_writable()?;
        let mut entry = self
            .state
            .queue_entry(entry_id)
            .filter(|entry| {
                entry.intent == QueueIntent::NewTurn && matches!(entry.state, QueueState::Pending)
            })
            .cloned()
            .ok_or(StateError::DeviceCommandMismatch {
                detail: "queue entry is not a pending new-turn delivery",
            })?;
        let command_id = self
            .state
            .queue_command_id(entry_id)
            .cloned()
            .ok_or(StateError::CommandOutboxDiverged)?;
        entry.state = QueueState::Submitting {
            command_id: command_id.clone(),
        };
        entry.editable = false;
        entry.updated_at_ms = at_ms;
        let checkpoint = self.projections.checkpoint();
        self.apply(&StateEffect::QueueEntryUpserted {
            entry: entry.clone(),
        })?;
        Ok(QueueDeliveryClaim {
            entry,
            command_id,
            projections: self.projections.changes_since(&checkpoint)?,
        })
    }

    pub fn finish_queue_delivery(
        &mut self,
        entry_id: &QueueEntryId,
        effects: &[StateEffect],
    ) -> Result<Vec<ProjectionEnvelope>, StateError> {
        self.ensure_writable()?;
        let current =
            self.state
                .queue_entry(entry_id)
                .cloned()
                .ok_or(StateError::DeviceCommandMismatch {
                    detail: "queue delivery has no canonical entry",
                })?;
        let QueueState::Submitting { command_id } = &current.state else {
            return Err(StateError::DeviceCommandMismatch {
                detail: "queue delivery was not durably claimed",
            });
        };
        if effects
            .iter()
            .any(|effect| matches!(effect, StateEffect::CommandAcknowledged { .. }))
        {
            return Err(StateError::DeviceCommandMismatch {
                detail: "queue delivery cannot fabricate a command acknowledgement",
            });
        }
        let delivered = effects
            .iter()
            .filter_map(|effect| match effect {
                StateEffect::QueueEntryUpserted { entry } if &entry.id == entry_id => Some(entry),
                _ => None,
            })
            .collect::<Vec<_>>();
        let Some(delivered) = delivered.first().filter(|_| delivered.len() == 1) else {
            return Err(StateError::DeviceCommandMismatch {
                detail: "queue delivery requires one exact terminal entry",
            });
        };
        if delivered.session_id != current.session_id
            || delivered.intent != current.intent
            || delivered.body != current.body
            || delivered.created_at_ms != current.created_at_ms
            || delivered.editable
        {
            return Err(StateError::DeviceCommandMismatch {
                detail: "queue delivery changed immutable entry fields",
            });
        }
        let QueueState::DeliveredAsNewTurn { turn_id, .. } = &delivered.state else {
            return Err(StateError::DeviceCommandMismatch {
                detail: "new-turn queue delivery lacks a structured terminal receipt",
            });
        };
        let has_correlated_turn = effects.iter().any(|effect| {
            matches!(
                effect,
                StateEffect::TurnUpserted { turn }
                    if turn.id == *turn_id
                        && turn.session_id == current.session_id
                        && matches!(
                            &turn.origin,
                            kaleido_proto::turn::TurnOrigin::RemoteCommand {
                                command_id: turn_command,
                            } if turn_command == command_id
                        )
            )
        });
        if !has_correlated_turn {
            return Err(StateError::DeviceCommandMismatch {
                detail: "queue delivery has no correlated provider turn",
            });
        }
        let checkpoint = self.projections.checkpoint();
        self.apply_all(effects)?;
        self.projections.changes_since(&checkpoint)
    }

    /// Claimed commands are intentionally surfaced as uncertain after restart;
    /// they require a new user decision and are never returned as pending.
    pub fn uncertain_dispatches(&self) -> Vec<CommandId> {
        self.device_outbox.uncertain()
    }

    /// Validates, transitions, assigns a cursor and appends.
    pub fn apply(&mut self, effect: &StateEffect) -> Result<Vec<LogRecord>, StateError> {
        Ok(self.apply_commit(effect)?.records)
    }

    /// Applies one effect and returns the exact full projection entries hostd
    /// should publish after this commit.
    pub fn apply_commit(&mut self, effect: &StateEffect) -> Result<StateCommit, StateError> {
        if matches!(
            effect,
            StateEffect::CommandAcknowledged {
                ack: CommandAck {
                    outcome: CommandOutcome::AcceptedLocally { .. },
                    ..
                }
            }
        ) {
            return Err(StateError::UntrustedLocalAcknowledgement);
        }
        self.apply_trusted_commit(effect)
    }

    /// Applies an effect from a broker-owned path that has already established
    /// its provenance. In particular, only `submit_command` may use this path
    /// to persist `AcceptedLocally`.
    fn apply_trusted(&mut self, effect: &StateEffect) -> Result<Vec<LogRecord>, StateError> {
        Ok(self.apply_trusted_commit(effect)?.records)
    }

    fn apply_trusted_commit(&mut self, effect: &StateEffect) -> Result<StateCommit, StateError> {
        if self.recovery_required {
            return Err(StateError::RecoveryRequired);
        }
        effect.validate_for_log()?;
        let stream = self.stream_for(effect)?;
        let before = self.state.clone();
        let mut next_state = before.clone();
        next_state.apply(effect)?;
        let projection_candidates = projection::affected_keys(&before, &next_state, effect)
            .into_iter()
            .map(|key| {
                let payload = projection::build(&next_state, &key)?;
                Ok((key, payload))
            })
            .collect::<Result<Vec<_>, StateError>>()?;
        let cursor = match self.cursors.get(&stream) {
            Some(previous) => previous.next()?,
            None => Cursor::START,
        };
        let appended_at_ms = self.next_timestamp()?;
        let record = LogRecord {
            cursor,
            stream: stream.clone(),
            appended_at_ms,
            effect: effect.clone(),
        };
        self.log.append(&record)?;
        // Section 10 fixes what may appear in an ordinary log line: canonical
        // identifiers, enumeration names, counts, digests, byte lengths,
        // timestamps and error codes. Everything else stays behind a reference.
        tracing::trace!(
            target: "kaleido.state",
            stream = crate::log::stream_file_name(&stream),
            cursor = cursor.seq,
            effect = effect_label(effect),
            appended_at_ms,
            "appended a state transition"
        );
        self.state = next_state;
        self.cursors.insert(stream, cursor);
        self.last_appended_at_ms = appended_at_ms;
        let mut projection_entries = Vec::new();
        for (key, payload) in projection_candidates {
            match self.projections.record(key, payload) {
                Ok(Some(envelope)) => projection_entries.push(envelope),
                Ok(None) => {}
                Err(error) => {
                    self.recovery_required = true;
                    return Err(error);
                }
            }
        }
        Ok(StateCommit {
            records: vec![record],
            projections: projection_entries,
        })
    }

    /// Applies a batch, stopping at the first refusal.
    pub fn apply_all(&mut self, effects: &[StateEffect]) -> Result<Vec<LogRecord>, StateError> {
        let mut records = Vec::new();
        for effect in effects {
            records.extend(self.apply(effect)?);
        }
        Ok(records)
    }

    pub fn session_snapshot(&self, session_id: &SessionId) -> Result<SessionSnapshot, StateError> {
        self.state.session_snapshot(session_id)
    }

    /// The current cursor of a stream, or the start position if it has none.
    pub fn cursor_of(&self, stream: &StreamKey) -> Cursor {
        self.cursors.get(stream).copied().unwrap_or(Cursor::START)
    }

    /// Builds one read model as a local diagnostic envelope.
    ///
    /// This existing one-shot path retains its canonical stream cursor for
    /// diagnostics only. It deliberately returns a state-local type so that a
    /// canonical stream head cannot masquerade as a mobile projection cursor.
    pub fn projection(
        &self,
        name: ProjectionName,
        session_id: Option<&SessionId>,
    ) -> Result<DiagnosticProjectionEnvelope, StateError> {
        let host_id = self
            .state
            .hosts()
            .next()
            .map(|host| host.id.clone())
            .ok_or(StateError::UnknownHost)?;
        let (source_stream, key, payload) = match name {
            ProjectionName::ProjectIndex => (
                StreamKey::Host {
                    host_id: host_id.clone(),
                },
                ProjectionKey::ProjectIndex {
                    host_id: host_id.clone(),
                },
                ProjectionPayload::ProjectIndex {
                    view: projection::project_index(&self.state, &host_id)?,
                },
            ),
            ProjectionName::SessionIndex => {
                let project_id = self.scoped_project(session_id)?;
                (
                    StreamKey::Project {
                        project_id: project_id.clone(),
                    },
                    ProjectionKey::SessionIndex {
                        project_id: project_id.clone(),
                    },
                    ProjectionPayload::SessionIndex {
                        view: projection::session_index(&self.state, &project_id),
                    },
                )
            }
            ProjectionName::Transcript => {
                let session_id = self.scoped_session(session_id)?;
                (
                    StreamKey::Session {
                        session_id: session_id.clone(),
                    },
                    ProjectionKey::Transcript {
                        session_id: session_id.clone(),
                    },
                    ProjectionPayload::Transcript {
                        view: projection::transcript(&self.state, &session_id)?,
                    },
                )
            }
            ProjectionName::LiveActivity => {
                let session_id = self.scoped_session(session_id)?;
                (
                    StreamKey::Session {
                        session_id: session_id.clone(),
                    },
                    ProjectionKey::LiveActivity {
                        session_id: session_id.clone(),
                    },
                    ProjectionPayload::LiveActivity {
                        view: projection::live_activity(&self.state, &session_id)?,
                    },
                )
            }
            ProjectionName::InputQueue => {
                let session_id = self.scoped_session(session_id)?;
                (
                    StreamKey::Session {
                        session_id: session_id.clone(),
                    },
                    ProjectionKey::InputQueue {
                        session_id: session_id.clone(),
                    },
                    ProjectionPayload::InputQueue {
                        view: projection::input_queue(&self.state, &session_id)?,
                    },
                )
            }
            ProjectionName::AttentionInbox => (
                StreamKey::Host {
                    host_id: host_id.clone(),
                },
                ProjectionKey::AttentionInbox {
                    host_id: host_id.clone(),
                },
                ProjectionPayload::AttentionInbox {
                    view: projection::attention_inbox(&self.state, &host_id),
                },
            ),
            ProjectionName::RuntimeCapability => {
                let runtime_id = self.scoped_runtime(session_id)?;
                (
                    StreamKey::Host {
                        host_id: host_id.clone(),
                    },
                    ProjectionKey::RuntimeCapability {
                        host_id: host_id.clone(),
                        runtime_id: runtime_id.clone(),
                    },
                    ProjectionPayload::RuntimeCapability {
                        view: projection::runtime_capability(&self.state, &runtime_id)?,
                    },
                )
            }
            ProjectionName::WorkflowBoard => {
                let workflow_id = self
                    .state
                    .workflows()
                    .next()
                    .map(|workflow| workflow.id.clone())
                    .ok_or(StateError::AmbiguousScope {
                        detail: "the store holds no workflow",
                    })?;
                (
                    StreamKey::Workflow {
                        workflow_id: workflow_id.clone(),
                    },
                    ProjectionKey::WorkflowBoard {
                        workflow_id: workflow_id.clone(),
                    },
                    ProjectionPayload::WorkflowBoard {
                        view: projection::workflow_board(&self.state, &workflow_id)?,
                    },
                )
            }
        };
        payload.validate_for_key(&key)?;
        Ok(DiagnosticProjectionEnvelope {
            projection_version: PROJECTION_VERSION,
            cursor: self.cursor_of(&source_stream),
            stream: source_stream,
            payload,
        })
    }

    fn scoped_session(&self, session_id: Option<&SessionId>) -> Result<SessionId, StateError> {
        if let Some(session_id) = session_id {
            return Ok(session_id.clone());
        }
        let mut sessions = self.state.sessions();
        match (sessions.next(), sessions.next()) {
            (Some(only), None) => Ok(only.id.clone()),
            (None, _) => Err(StateError::AmbiguousScope {
                detail: "the store holds no session",
            }),
            _ => Err(StateError::AmbiguousScope {
                detail: "the store holds more than one session; name one",
            }),
        }
    }

    fn scoped_project(&self, session_id: Option<&SessionId>) -> Result<ProjectId, StateError> {
        if let Some(session_id) = session_id {
            let session =
                self.state
                    .session(session_id)
                    .ok_or_else(|| StateError::UnknownSession {
                        session_id: session_id.clone(),
                    })?;
            return Ok(session.project_id.clone());
        }
        let mut projects = self.state.projects();
        match (projects.next(), projects.next()) {
            (Some(only), None) => Ok(only.id.clone()),
            (None, _) => Err(StateError::AmbiguousScope {
                detail: "the store holds no project",
            }),
            _ => Err(StateError::AmbiguousScope {
                detail: "the store holds more than one project; name a session",
            }),
        }
    }

    fn scoped_runtime(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<ProviderRuntimeId, StateError> {
        if let Some(session_id) = session_id {
            let session =
                self.state
                    .session(session_id)
                    .ok_or_else(|| StateError::UnknownSession {
                        session_id: session_id.clone(),
                    })?;
            if let Some(runtime) = self.state.runtime_of(session) {
                return Ok(runtime.id.clone());
            }
        }
        let mut runtimes = self.state.runtimes();
        match (runtimes.next(), runtimes.next()) {
            (Some(only), None) => Ok(only.id.clone()),
            (None, _) => Err(StateError::AmbiguousScope {
                detail: "the store holds no runtime",
            }),
            _ => Err(StateError::AmbiguousScope {
                detail: "the store holds more than one runtime; name a session",
            }),
        }
    }

    fn preview_device_admission(
        &self,
        envelope: &CommandEnvelope,
        now_ms: i64,
    ) -> (CommandAck, bool) {
        let (outcome, dispatch) = if envelope.expired_at(now_ms) {
            (
                CommandOutcome::Rejected {
                    error: canonical_error(ErrorCode::CommandExpired, false, now_ms),
                },
                false,
            )
        } else {
            match &envelope.body {
                Command::SubmitPrompt { session_id, .. } => {
                    if self.state.session(session_id).is_some() {
                        (CommandOutcome::AcceptedLocally { note_ref: None }, true)
                    } else {
                        (
                            CommandOutcome::Rejected {
                                error: canonical_error(ErrorCode::NotFound, false, now_ms),
                            },
                            false,
                        )
                    }
                }
                Command::RespondAttention { response } => {
                    if self
                        .device_outbox
                        .has_unfinished_attention(&response.attention_id)
                    {
                        (
                            CommandOutcome::Rejected {
                                error: canonical_error(
                                    ErrorCode::ApprovalAlreadyAnswered,
                                    false,
                                    now_ms,
                                ),
                            },
                            false,
                        )
                    } else {
                        match self.state.attention(&response.attention_id) {
                            None => (
                                CommandOutcome::Rejected {
                                    error: canonical_error(ErrorCode::NotFound, false, now_ms),
                                },
                                false,
                            ),
                            Some(entry) => match entry.check_reply(response, now_ms) {
                                Ok(()) => {
                                    (CommandOutcome::AcceptedLocally { note_ref: None }, true)
                                }
                                Err(rejection) => (
                                    CommandOutcome::Rejected {
                                        error: canonical_error(
                                            reply_rejection_code(entry, rejection),
                                            false,
                                            now_ms,
                                        ),
                                    },
                                    false,
                                ),
                            },
                        }
                    }
                }
                Command::InterruptTurn { session_id, .. } => {
                    if self.state.session(session_id).is_some() {
                        (CommandOutcome::AcceptedLocally { note_ref: None }, true)
                    } else {
                        (
                            CommandOutcome::Rejected {
                                error: canonical_error(ErrorCode::NotFound, false, now_ms),
                            },
                            false,
                        )
                    }
                }
                Command::ResumeSession { session_id } => match self.state.session(session_id) {
                    None => (
                        CommandOutcome::Rejected {
                            error: canonical_error(ErrorCode::NotFound, false, now_ms),
                        },
                        false,
                    ),
                    Some(session)
                        if self.state.runtime_of(session).is_some_and(|runtime| {
                            runtime.capabilities.permits(&Capability::HistoryResume)
                        }) =>
                    {
                        (CommandOutcome::AcceptedLocally { note_ref: None }, true)
                    }
                    Some(_) => (
                        CommandOutcome::Rejected {
                            error: canonical_error(ErrorCode::CapabilityUnavailable, false, now_ms),
                        },
                        false,
                    ),
                },
                Command::EnqueueInput { session_id, .. } => {
                    if self.state.session(session_id).is_some() {
                        (
                            CommandOutcome::Enqueued {
                                entry_id: queue_entry_id(&envelope.command_id),
                            },
                            false,
                        )
                    } else {
                        (
                            CommandOutcome::Rejected {
                                error: canonical_error(ErrorCode::NotFound, false, now_ms),
                            },
                            false,
                        )
                    }
                }
                _ => (
                    CommandOutcome::Rejected {
                        error: canonical_error(ErrorCode::InvalidCommand, false, now_ms),
                    },
                    false,
                ),
            }
        };
        (
            CommandAck {
                command_id: envelope.command_id.clone(),
                outcome,
                acked_at_ms: now_ms,
            },
            dispatch,
        )
    }

    fn validate_device_command_content(
        &self,
        device_id: &DeviceId,
        command: &Command,
        now_ms: i64,
    ) -> Result<(), StateError> {
        let references: Vec<&ContentRef> = match command {
            Command::SubmitPrompt { body, .. }
            | Command::EnqueueInput { body, .. }
            | Command::EditQueueEntry { body, .. } => vec![body],
            Command::RespondAttention { response } => response
                .question_answers
                .iter()
                .filter_map(|answer| answer.free_form_ref.as_ref())
                .chain(response.free_form_ref.iter())
                .collect::<Vec<_>>(),
            Command::ReworkStep { reason_ref, .. } | Command::SkipStep { reason_ref, .. } => {
                reason_ref.iter().collect::<Vec<_>>()
            }
            Command::ReorderQueue { .. }
            | Command::CancelQueueEntry { .. }
            | Command::InterruptTurn { .. }
            | Command::RetryTurn { .. }
            | Command::OpenSession { .. }
            | Command::ResumeSession { .. }
            | Command::CloseSession { .. }
            | Command::AdvanceStep { .. }
            | Command::RetryStep { .. }
            | Command::CancelStep { .. }
            | Command::ReassignStep { .. }
            | Command::CancelWorkflow { .. } => Vec::new(),
        };
        for reference in references {
            self.content
                .validate_device_reference(device_id, reference, now_ms)?;
        }
        Ok(())
    }

    /// Accepts a command, honouring `(actor, idempotency_key)` exactly once.
    ///
    /// Rule R-P10: the returned outcome distinguishes local acceptance from
    /// runtime acceptance, and a repeat submission is a `Duplicate` that
    /// appends nothing.
    pub fn submit_command(
        &mut self,
        envelope: &CommandEnvelope,
        now_ms: i64,
    ) -> Result<CommandAck, StateError> {
        envelope.validate()?;
        let key = hex_digest(envelope.dedupe_key().as_bytes());
        if let Some(original_command_id) = self.idempotency.get(&key) {
            // Deliberately no log record: section 6.1 requires the repeat to be
            // reported, not re-executed.
            return Ok(CommandAck {
                command_id: envelope.command_id.clone(),
                outcome: CommandOutcome::Duplicate {
                    original_command_id: original_command_id.clone(),
                },
                acked_at_ms: now_ms,
            });
        }

        let outcome = if envelope.expired_at(now_ms) {
            CommandOutcome::Rejected {
                error: canonical_error(ErrorCode::CommandExpired, false, now_ms),
            }
        } else {
            self.dispatch(envelope, now_ms)?
        };

        let ack = CommandAck {
            command_id: envelope.command_id.clone(),
            outcome,
            acked_at_ms: now_ms,
        };
        self.apply_trusted(&StateEffect::CommandAcknowledged { ack: ack.clone() })?;
        self.record_idempotency(&key, &envelope.command_id)?;
        Ok(ack)
    }

    fn dispatch(
        &mut self,
        envelope: &CommandEnvelope,
        now_ms: i64,
    ) -> Result<CommandOutcome, StateError> {
        match &envelope.body {
            Command::SubmitPrompt { session_id, .. } => {
                if self.state.session(session_id).is_some() {
                    Ok(CommandOutcome::AcceptedLocally { note_ref: None })
                } else {
                    Ok(CommandOutcome::Rejected {
                        error: canonical_error(ErrorCode::NotFound, false, now_ms),
                    })
                }
            }
            Command::RespondAttention { response } => {
                let Some(entry) = self.state.attention(&response.attention_id) else {
                    return Ok(CommandOutcome::Rejected {
                        error: canonical_error(ErrorCode::NotFound, false, now_ms),
                    });
                };
                if let Err(rejection) = entry.check_reply(response, now_ms) {
                    return Ok(CommandOutcome::Rejected {
                        error: canonical_error(
                            reply_rejection_code(entry, rejection),
                            false,
                            now_ms,
                        ),
                    });
                }
                // Admission proves only that the Broker durably owns the
                // reply. The attention remains open until the adapter observes
                // the provider's structured result.
                Ok(CommandOutcome::AcceptedLocally { note_ref: None })
            }
            Command::EnqueueInput {
                session_id,
                body,
                intent,
            } => self.enqueue(&envelope.command_id, session_id, body, *intent, now_ms),
            // Every other command in this slice is recorded locally only. It
            // has not reached a runtime, and section 6.1 forbids showing local
            // acceptance as runtime acceptance.
            _ => Ok(CommandOutcome::AcceptedLocally { note_ref: None }),
        }
    }

    fn enqueue(
        &mut self,
        command_id: &CommandId,
        session_id: &SessionId,
        body: &kaleido_proto::content::ContentRef,
        intent: QueueIntent,
        now_ms: i64,
    ) -> Result<CommandOutcome, StateError> {
        if self.state.session(session_id).is_none() {
            return Ok(CommandOutcome::Rejected {
                error: canonical_error(ErrorCode::NotFound, false, now_ms),
            });
        }
        let position = u32::try_from(
            self.state
                .queue_of(session_id)
                .iter()
                .filter(|entry| entry.state.is_pending())
                .count(),
        )
        .unwrap_or(u32::MAX);
        let entry_id = queue_entry_id(command_id);
        let entry = QueueEntry {
            id: entry_id.clone(),
            session_id: session_id.clone(),
            position,
            intent,
            body: body.clone(),
            // Section 4.6: a queued input starts pending and only leaves that
            // state on proof. Nothing here can shortcut it to a delivered
            // steer, because the proof lives in `SteerAcknowledgement`.
            state: QueueState::Pending,
            editable: true,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        self.apply(&StateEffect::QueueEntryUpserted { entry })?;
        Ok(CommandOutcome::Enqueued { entry_id })
    }

    /// Stores a body and returns the reference canonical state will carry.
    pub fn store_content(
        &self,
        kind: ContentKind,
        sensitivity: kaleido_proto::content::Sensitivity,
        bytes: &[u8],
    ) -> Result<kaleido_proto::content::ContentRef, StateError> {
        self.content.store(kind, sensitivity, bytes)
    }

    fn next_timestamp(&self) -> Result<i64, StateError> {
        let now = self.clock.now_ms();
        if now > self.last_appended_at_ms {
            return Ok(now);
        }
        self.last_appended_at_ms
            .checked_add(1)
            .ok_or(StateError::TimestampOverflow)
    }

    /// Routes an effect to the one stream that owns it.
    ///
    /// Section 5.2 does not guarantee ordering across streams, so an effect
    /// belongs to exactly one: duplicating it would mean applying it twice on
    /// reload for no read-model benefit at this stage.
    fn stream_for(&self, effect: &StateEffect) -> Result<StreamKey, StateError> {
        let host_stream = || -> Result<StreamKey, StateError> {
            self.state
                .hosts()
                .next()
                .map(|host| StreamKey::Host {
                    host_id: host.id.clone(),
                })
                .ok_or(StateError::UnknownHost)
        };
        let session_stream = |session_id: &SessionId| StreamKey::Session {
            session_id: session_id.clone(),
        };
        Ok(match effect {
            StateEffect::HostUpserted { host } => StreamKey::Host {
                host_id: host.id.clone(),
            },
            StateEffect::RuntimeUpserted { runtime } => StreamKey::Host {
                host_id: runtime.host_id.clone(),
            },
            StateEffect::SessionUpserted { session } => session_stream(&session.id),
            StateEffect::SessionStatusChanged { session_id, .. } => session_stream(session_id),
            StateEffect::TurnUpserted { turn } => session_stream(&turn.session_id),
            StateEffect::ItemUpserted { item } => session_stream(&item.session_id),
            StateEffect::QueueEntryUpserted { entry } => session_stream(&entry.session_id),
            StateEffect::QueueReordered { session_id, .. } => session_stream(session_id),
            StateEffect::AttentionUpserted { item } => match &item.session_id {
                Some(session_id) => session_stream(session_id),
                None => host_stream()?,
            },
            StateEffect::DiagnosticRecorded { diagnostic } => match &diagnostic.session_id {
                Some(session_id) => session_stream(session_id),
                None => host_stream()?,
            },
            StateEffect::CapabilitiesUpdated { .. }
            | StateEffect::ProjectUpserted { .. }
            | StateEffect::CommandAcknowledged { .. } => host_stream()?,
            StateEffect::WorkflowUpserted { workflow } => StreamKey::Workflow {
                workflow_id: workflow.id.clone(),
            },
            StateEffect::StepUpserted { step } => StreamKey::Workflow {
                workflow_id: step.workflow_id.clone(),
            },
            StateEffect::ArtifactUpserted { artifact } => StreamKey::Workflow {
                workflow_id: artifact.workflow_id.clone(),
            },
        })
    }

    fn idempotency_path(&self) -> PathBuf {
        self.root.join(IDEMPOTENCY_FILE)
    }

    fn record_idempotency(&mut self, key: &str, command_id: &CommandId) -> Result<(), StateError> {
        self.idempotency.insert(key.to_owned(), command_id.clone());
        let path = self.idempotency_path();
        let record = IdempotencyRecord {
            format_version: IDEMPOTENCY_FORMAT_VERSION,
            key_digest: key.to_owned(),
            command_id: command_id.clone(),
        };
        let line = format!("{}\n", serde_json::to_string(&record)?);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| StateError::io(&path, source))?;
        file.write_all(line.as_bytes())
            .map_err(|source| StateError::io(&path, source))?;
        file.flush()
            .map_err(|source| StateError::io(&path, source))?;
        file.sync_data()
            .map_err(|source| StateError::io(&path, source))
    }

    fn read_idempotency(&self) -> Result<BTreeMap<String, CommandId>, StateError> {
        let path = self.idempotency_path();
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(source) => return Err(StateError::io(&path, source)),
        };
        let mut table = BTreeMap::new();
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|source| StateError::io(&path, source))?;
            if line.trim().is_empty() {
                continue;
            }
            let record = serde_json::from_str::<IdempotencyRecord>(&line).map_err(|_| {
                StateError::MalformedRecord {
                    path: path.clone(),
                    line: index + 1,
                }
            })?;
            let digest_is_valid = record.key_digest.len() == 64
                && record
                    .key_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
            // #[allow(kaleido::version_branch)] reason: durable side-table format validation must reject incompatible on-disk records before replay
            if record.format_version != IDEMPOTENCY_FORMAT_VERSION
                || !digest_is_valid
                || record.command_id.is_empty()
            {
                return Err(StateError::MalformedRecord {
                    path: path.clone(),
                    line: index + 1,
                });
            }
            if table
                .insert(record.key_digest, record.command_id.clone())
                .is_some_and(|existing| existing != record.command_id)
            {
                return Err(StateError::MalformedRecord {
                    path: path.clone(),
                    line: index + 1,
                });
            }
        }
        Ok(table)
    }
}

fn validate_device_envelope(
    authenticated_device: &DeviceId,
    envelope: &CommandEnvelope,
    request: &DeviceCommandRequest,
    now_ms: i64,
) -> Result<(), StateError> {
    if !matches!(
        &envelope.actor,
        Actor::Human { device_id } if device_id == authenticated_device
    ) {
        return Err(StateError::DeviceCommandMismatch {
            detail: "actor is not the authenticated device",
        });
    }
    if envelope.idempotency_key != request.idempotency_key {
        return Err(StateError::DeviceCommandMismatch {
            detail: "idempotency key differs from the authenticated request",
        });
    }
    if envelope.body != request.body {
        return Err(StateError::DeviceCommandMismatch {
            detail: "command body differs from the authenticated request",
        });
    }
    if envelope.issued_at_ms != now_ms {
        return Err(StateError::DeviceCommandMismatch {
            detail: "issued timestamp was not injected from host receive time",
        });
    }
    let expected_expiry = match request.ttl_ms {
        Some(ttl_ms) => Some(
            i64::try_from(ttl_ms)
                .ok()
                .and_then(|ttl_ms| now_ms.checked_add(ttl_ms))
                .ok_or(StateError::DeviceCommandMismatch {
                    detail: "ttl cannot be represented as an absolute expiry",
                })?,
        ),
        None => None,
    };
    if envelope.expires_at_ms != expected_expiry {
        return Err(StateError::DeviceCommandMismatch {
            detail: "absolute expiry does not match the request ttl",
        });
    }
    Ok(())
}

fn queue_entry_id(command_id: &CommandId) -> QueueEntryId {
    let prefix = hex_digest(command_id.as_str().as_bytes())
        .chars()
        .take(16)
        .collect::<String>();
    QueueEntryId::new(format!("que_{prefix}"))
}

fn device_route_uses_submit(command: &Command) -> bool {
    matches!(
        command,
        Command::SubmitPrompt { .. }
            | Command::RespondAttention { .. }
            | Command::EnqueueInput { .. }
    )
}

/// The variant name of an effect, which section 10 permits in a log line.
fn effect_label(effect: &StateEffect) -> &'static str {
    match effect {
        StateEffect::HostUpserted { .. } => "host_upserted",
        StateEffect::RuntimeUpserted { .. } => "runtime_upserted",
        StateEffect::CapabilitiesUpdated { .. } => "capabilities_updated",
        StateEffect::ProjectUpserted { .. } => "project_upserted",
        StateEffect::SessionUpserted { .. } => "session_upserted",
        StateEffect::SessionStatusChanged { .. } => "session_status_changed",
        StateEffect::TurnUpserted { .. } => "turn_upserted",
        StateEffect::ItemUpserted { .. } => "item_upserted",
        StateEffect::QueueEntryUpserted { .. } => "queue_entry_upserted",
        StateEffect::QueueReordered { .. } => "queue_reordered",
        StateEffect::AttentionUpserted { .. } => "attention_upserted",
        StateEffect::WorkflowUpserted { .. } => "workflow_upserted",
        StateEffect::StepUpserted { .. } => "step_upserted",
        StateEffect::ArtifactUpserted { .. } => "artifact_upserted",
        StateEffect::CommandAcknowledged { .. } => "command_acknowledged",
        StateEffect::DiagnosticRecorded { .. } => "diagnostic_recorded",
    }
}

fn canonical_error(code: ErrorCode, retriable: bool, at_ms: i64) -> CanonicalError {
    CanonicalError {
        code,
        retriable,
        // Section 7 keeps upstream text out of canonical errors; there is no
        // detail here to leak.
        detail_ref: None,
        at_ms,
    }
}

/// Maps a reply refusal to the closed error code a reader can localise.
///
/// Rule R-P8 is why a refused approval is absent from this mapping: declining
/// is a decision, and a decision is never an error.
fn reply_rejection_code(entry: &AttentionItem, rejection: ReplyRejection) -> ErrorCode {
    match rejection {
        ReplyRejection::Expired => ErrorCode::ApprovalExpired,
        ReplyRejection::NotOpen => {
            if matches!(entry.state, AttentionState::Answered { .. }) {
                ErrorCode::ApprovalAlreadyAnswered
            } else {
                ErrorCode::InvalidCommand
            }
        }
        ReplyRejection::AttentionMismatch | ReplyRejection::NotReplyable => ErrorCode::NotFound,
        ReplyRejection::SessionRequired
        | ReplyRejection::SessionMismatch
        | ReplyRejection::RequestKeyMismatch
        | ReplyRejection::ExpiryMismatch
        | ReplyRejection::UnknownOption
        | ReplyRejection::FreeFormNotAllowed
        | ReplyRejection::InvalidFreeForm
        | ReplyRejection::DecisionMissing
        | ReplyRejection::QuestionAnswersRequired
        | ReplyRejection::QuestionAnswersUnexpected
        | ReplyRejection::QuestionTopLevelDecision
        | ReplyRejection::QuestionAnswerEmpty
        | ReplyRejection::QuestionAnswerDuplicateKey
        | ReplyRejection::QuestionAnswerDuplicateOption
        | ReplyRejection::QuestionAnswerUnknownKey
        | ReplyRejection::QuestionAnswerUnknownOption
        | ReplyRejection::QuestionAnswerTooManyOptions
        | ReplyRejection::QuestionAnswerMissing => ErrorCode::InvalidCommand,
    }
}

/// The host stream key for a host identifier.
pub fn host_stream(host_id: &HostId) -> StreamKey {
    StreamKey::Host {
        host_id: host_id.clone(),
    }
}
