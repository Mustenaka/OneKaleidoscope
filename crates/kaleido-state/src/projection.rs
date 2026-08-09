//! The read models this slice implements.
//!
//! Section 8 allows a projection to select, sort, aggregate and truncate, and
//! nothing else. In particular it may not invent a state canonical state does
//! not carry, and it must keep "unsupported", "unverified" and "blocked"
//! visible rather than hiding the control.

use kaleido_proto::capability::Capability;
use kaleido_proto::effect::{Cursor, StreamKey};
use kaleido_proto::ids::{ItemId, ProjectId, ProviderRuntimeId, SessionId};
use kaleido_proto::projection::{
    AttentionInboxView, InputQueueView, LiveActivityView, ProjectionPayload, RuntimeCapabilityView,
    SessionIndexView, SessionSummary, TranscriptTurn, TranscriptView,
};
use kaleido_proto::session::{Session, SessionStatus};
use kaleido_proto::turn::{ItemBody, ItemStatus};
use serde::{Deserialize, Serialize};

use crate::error::StateError;
use crate::state::CanonicalState;

/// One-shot local diagnostic view built from a canonical stream head.
///
/// It is intentionally not a UACP projection envelope and is never exported
/// through UniFFI. T-107's projection journal is the only component allowed to
/// construct mobile [`kaleido_proto::projection::ProjectionEnvelope`] values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticProjectionEnvelope {
    pub projection_version: u32,
    pub stream: StreamKey,
    pub cursor: Cursor,
    pub payload: ProjectionPayload,
}

/// The six read models this slice produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionName {
    SessionIndex,
    Transcript,
    LiveActivity,
    InputQueue,
    AttentionInbox,
    RuntimeCapability,
}

impl ProjectionName {
    pub const ALL: [ProjectionName; 6] = [
        ProjectionName::SessionIndex,
        ProjectionName::Transcript,
        ProjectionName::LiveActivity,
        ProjectionName::InputQueue,
        ProjectionName::AttentionInbox,
        ProjectionName::RuntimeCapability,
    ];

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "session-index" => Some(ProjectionName::SessionIndex),
            "transcript" => Some(ProjectionName::Transcript),
            "live-activity" => Some(ProjectionName::LiveActivity),
            "input-queue" => Some(ProjectionName::InputQueue),
            "attention-inbox" => Some(ProjectionName::AttentionInbox),
            "runtime-capability" => Some(ProjectionName::RuntimeCapability),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectionName::SessionIndex => "session-index",
            ProjectionName::Transcript => "transcript",
            ProjectionName::LiveActivity => "live-activity",
            ProjectionName::InputQueue => "input-queue",
            ProjectionName::AttentionInbox => "attention-inbox",
            ProjectionName::RuntimeCapability => "runtime-capability",
        }
    }
}

/// Sessions of one project, split into active, history and archived.
pub fn session_index(state: &CanonicalState, project_id: &ProjectId) -> SessionIndexView {
    let mut active = Vec::new();
    let mut history = Vec::new();
    let mut archived = Vec::new();
    for session in state.sessions() {
        if &session.project_id != project_id {
            continue;
        }
        let summary = summarise(session);
        if session.archived {
            archived.push(summary);
        } else if session.status.is_active() || session.status.waits_for_human() {
            active.push(summary);
        } else {
            history.push(summary);
        }
    }
    SessionIndexView {
        project_id: project_id.clone(),
        active,
        history,
        archived,
    }
}

fn summarise(session: &Session) -> SessionSummary {
    SessionSummary {
        session_id: session.id.clone(),
        project_binding_id: session.project_binding_id.clone(),
        title: session.title.clone(),
        status: session.status,
        ownership: session.ownership,
        live_binding: session.live_binding.clone(),
        queue_depth: session.queue_depth,
        open_attention_count: session.open_attention_count,
        last_activity_at_ms: session.last_activity_at_ms,
    }
}

/// Every observed turn of a session with its accumulated items.
pub fn transcript(
    state: &CanonicalState,
    session_id: &SessionId,
) -> Result<TranscriptView, StateError> {
    require_session(state, session_id)?;
    let turns = state
        .turns_of(session_id)
        .into_iter()
        .map(|turn| {
            // Section 4.4: the item list comes from the accumulated identifiers,
            // never from a provider's completion summary.
            let items = turn
                .item_ids
                .iter()
                .filter_map(|item_id| state.item(item_id))
                .cloned()
                .collect();
            TranscriptTurn {
                turn: turn.clone(),
                items,
            }
        })
        .collect();
    Ok(TranscriptView {
        session_id: session_id.clone(),
        turns,
        has_earlier: false,
    })
}

/// What is happening in the session right now.
pub fn live_activity(
    state: &CanonicalState,
    session_id: &SessionId,
) -> Result<LiveActivityView, StateError> {
    let session = require_session(state, session_id)?;
    let streaming_item_ids: Vec<ItemId> = match &session.active_turn_id {
        Some(active_turn_id) => state
            .items_of(session_id)
            .into_iter()
            .filter(|item| &item.turn_id == active_turn_id)
            .filter(|item| item.status == ItemStatus::InProgress)
            .map(|item| item.id.clone())
            .collect(),
        None => Vec::new(),
    };
    let mut plan = Vec::new();
    let mut tasks = Vec::new();
    for item in state.items_of(session_id) {
        match &item.body {
            ItemBody::PlanUpdate { entries } => plan = entries.clone(),
            ItemBody::TaskUpdate { tasks: updated } => tasks = updated.clone(),
            _ => {}
        }
    }
    Ok(LiveActivityView {
        session_id: session_id.clone(),
        active_turn_id: session.active_turn_id.clone(),
        streaming_item_ids,
        plan,
        tasks,
        updated_at_ms: session.last_activity_at_ms,
    })
}

/// The user's pending input, and whether steering is actually available.
pub fn input_queue(
    state: &CanonicalState,
    session_id: &SessionId,
) -> Result<InputQueueView, StateError> {
    let session = require_session(state, session_id)?;
    // Rule R-P9 shows up here: a steering intent may only be presented as
    // injected when the runtime proved it. `steer_supported` therefore reads
    // the negotiated capability and never the provider's name.
    let steer_supported = state
        .capabilities_of(session)
        .is_some_and(|capabilities| capabilities.permits(&Capability::TurnSteer));
    Ok(InputQueueView {
        session_id: session_id.clone(),
        entries: state
            .queue_of(session_id)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        // The queue belongs to the broker, so it accepts writes whenever the
        // session is reachable. Delivery is a separate promise the queue state
        // makes, not this flag.
        writable: !session.archived && session.status != SessionStatus::Offline,
        steer_supported,
    })
}

/// Everything waiting on a human, soonest expiry first.
pub fn attention_inbox(state: &CanonicalState) -> AttentionInboxView {
    let mut entries = state
        .attention_entries()
        .into_iter()
        .filter(|entry| entry.state.is_open())
        .cloned()
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.expires_at_ms.unwrap_or(i64::MAX));
    AttentionInboxView { entries }
}

/// What one runtime connection was actually observed to support.
pub fn runtime_capability(
    state: &CanonicalState,
    runtime_id: &ProviderRuntimeId,
) -> Result<RuntimeCapabilityView, StateError> {
    let runtime = state
        .runtime(runtime_id)
        .ok_or_else(|| StateError::UnknownRuntime {
            runtime_id: runtime_id.clone(),
        })?;
    Ok(RuntimeCapabilityView::from_capabilities(
        runtime.host_id.clone(),
        &runtime.capabilities,
    ))
}

fn require_session<'state>(
    state: &'state CanonicalState,
    session_id: &SessionId,
) -> Result<&'state Session, StateError> {
    state
        .session(session_id)
        .ok_or_else(|| StateError::UnknownSession {
            session_id: session_id.clone(),
        })
}
