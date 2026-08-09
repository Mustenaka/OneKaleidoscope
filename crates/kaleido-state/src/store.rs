//! The canonical store: validate, transition, assign a cursor, append.
//!
//! The write order is fixed by `docs/ARCHITECTURE.md` section 6 and is the
//! reason a reload converges: nothing in [`CanonicalState::apply`] reads a
//! clock, so replaying the same records in the same order reproduces the same
//! state field for field (`docs/PROTOCOL.md` section 5.4).

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kaleido_proto::attention::{
    AttentionAnswerSource, AttentionItem, AttentionState, ReplyRejection,
};
use kaleido_proto::command::{Command, CommandAck, CommandEnvelope, CommandOutcome};
use kaleido_proto::content::ContentKind;
use kaleido_proto::effect::{Cursor, LogRecord, SessionSnapshot, StateEffect, StreamKey};
use kaleido_proto::error::{CanonicalError, ErrorCode};
use kaleido_proto::ids::{
    CommandId, HostId, ProjectId, ProviderRuntimeId, QueueEntryId, SessionId,
};
use kaleido_proto::projection::{ProjectionKey, ProjectionPayload, PROJECTION_VERSION};
use kaleido_proto::queue::{QueueEntry, QueueIntent, QueueState};
use serde::{Deserialize, Serialize};

use crate::content::{hex_digest, ContentStore};
use crate::error::StateError;
use crate::log::StreamLog;
use crate::projection::{self, DiagnosticProjectionEnvelope, ProjectionName};
use crate::state::CanonicalState;

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
    state: CanonicalState,
    cursors: HashMap<StreamKey, Cursor>,
    last_appended_at_ms: i64,
    clock: ClockSource,
    idempotency: BTreeMap<String, CommandId>,
}

impl CanonicalStore {
    /// Opens an empty store rooted at `root`.
    pub fn open(root: impl AsRef<Path>, clock: ClockSource) -> Result<Self, StateError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|source| StateError::io(&root, source))?;
        Ok(Self {
            log: StreamLog::open(&root)?,
            content: ContentStore::open(&root)?,
            root,
            state: CanonicalState::default(),
            cursors: HashMap::new(),
            last_appended_at_ms: i64::MIN,
            clock,
            idempotency: BTreeMap::new(),
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
        for record in &records {
            store.state.apply(&record.effect)?;
            store.cursors.insert(record.stream.clone(), record.cursor);
            store.last_appended_at_ms = store.last_appended_at_ms.max(record.appended_at_ms);
        }
        store.idempotency = store.read_idempotency()?;
        Ok(store)
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

    /// Validates, transitions, assigns a cursor and appends.
    pub fn apply(&mut self, effect: &StateEffect) -> Result<Vec<LogRecord>, StateError> {
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
        self.apply_trusted(effect)
    }

    /// Applies an effect from a broker-owned path that has already established
    /// its provenance. In particular, only `submit_command` may use this path
    /// to persist `AcceptedLocally`.
    fn apply_trusted(&mut self, effect: &StateEffect) -> Result<Vec<LogRecord>, StateError> {
        effect.validate_for_log()?;
        let stream = self.stream_for(effect)?;
        self.state.apply(effect)?;
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
        self.cursors.insert(stream, cursor);
        self.last_appended_at_ms = appended_at_ms;
        Ok(vec![record])
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
                    view: projection::attention_inbox(&self.state),
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
            Command::RespondAttention { response } => {
                self.decide_attention(&envelope.command_id, response, now_ms)
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
        let entry_id = QueueEntryId::new(format!(
            "que_{}",
            hex_digest(command_id.as_str().as_bytes())
                .get(..16)
                .unwrap_or("0000000000000000")
        ));
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

    fn decide_attention(
        &mut self,
        command_id: &CommandId,
        response: &kaleido_proto::attention::AttentionResponse,
        now_ms: i64,
    ) -> Result<CommandOutcome, StateError> {
        let Some(entry) = self.state.attention(&response.attention_id).cloned() else {
            return Ok(CommandOutcome::Rejected {
                error: canonical_error(ErrorCode::NotFound, false, now_ms),
            });
        };
        if let Err(rejection) = entry.check_reply(response, now_ms) {
            let code = reply_rejection_code(&entry, rejection);
            return Ok(CommandOutcome::Rejected {
                error: canonical_error(code, false, now_ms),
            });
        }
        let mut answered = entry;
        answered.state = AttentionState::Answered {
            option_id: response.option_id.clone(),
            free_form_ref: response.free_form_ref.clone(),
            decided_at_ms: now_ms,
            answer_source: AttentionAnswerSource::LocalCommand {
                command_id: command_id.clone(),
            },
        };
        self.apply(&StateEffect::AttentionUpserted { item: answered })?;
        // The broker has recorded the decision. Whether the runtime accepted it
        // is a separate fact that arrives as upstream traffic.
        Ok(CommandOutcome::AcceptedLocally { note_ref: None })
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
            StateEffect::WorkflowUpserted { .. }
            | StateEffect::StepUpserted { .. }
            | StateEffect::ArtifactUpserted { .. } => {
                return Err(StateError::UnsupportedEffect {
                    detail: "workflow effects are not part of this vertical slice",
                });
            }
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
        file.flush().map_err(|source| StateError::io(&path, source))
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
        | ReplyRejection::DecisionMissing => ErrorCode::InvalidCommand,
    }
}

/// The host stream key for a host identifier.
pub fn host_stream(host_id: &HostId) -> StreamKey {
    StreamKey::Host {
        host_id: host_id.clone(),
    }
}
