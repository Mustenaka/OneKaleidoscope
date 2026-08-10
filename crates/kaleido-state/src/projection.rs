//! The read models this slice implements.
//!
//! Section 8 allows a projection to select, sort, aggregate and truncate, and
//! nothing else. In particular it may not invent a state canonical state does
//! not carry, and it must keep "unsupported", "unverified" and "blocked"
//! visible rather than hiding the control.

use kaleido_proto::capability::Capability;
use kaleido_proto::effect::{Cursor, StateEffect, StreamKey};
use kaleido_proto::host::ProviderFamily;
use kaleido_proto::ids::{HostId, ItemId, ProjectId, ProviderRuntimeId, SessionId, WorkflowId};
use kaleido_proto::projection::{
    AttentionInboxView, InputQueueView, LiveActivityView, ProjectBindingSummary, ProjectIndexView,
    ProjectSummary, ProjectionKey, ProjectionPayload, ProviderGroup, RuntimeCapabilityView,
    SessionIndexView, SessionSummary, TranscriptTurn, TranscriptView, WorkflowBoardStep,
    WorkflowBoardView,
};
use kaleido_proto::session::{Session, SessionStatus};
use kaleido_proto::turn::{ItemBody, ItemStatus};
use kaleido_proto::workflow::{StepBlocker, StepState};
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

/// The eight product read-model classes exposed by the v0.4 projection
/// contract. Presence in this list does not imply that a provider invents an
/// instance; for example, Codex has no WorkflowBoard until canonical workflow
/// state actually exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionName {
    ProjectIndex,
    SessionIndex,
    Transcript,
    LiveActivity,
    InputQueue,
    AttentionInbox,
    RuntimeCapability,
    WorkflowBoard,
}

impl ProjectionName {
    pub const ALL: [ProjectionName; 8] = [
        ProjectionName::ProjectIndex,
        ProjectionName::SessionIndex,
        ProjectionName::Transcript,
        ProjectionName::LiveActivity,
        ProjectionName::InputQueue,
        ProjectionName::AttentionInbox,
        ProjectionName::RuntimeCapability,
        ProjectionName::WorkflowBoard,
    ];

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "project-index" => Some(ProjectionName::ProjectIndex),
            "session-index" => Some(ProjectionName::SessionIndex),
            "transcript" => Some(ProjectionName::Transcript),
            "live-activity" => Some(ProjectionName::LiveActivity),
            "input-queue" => Some(ProjectionName::InputQueue),
            "attention-inbox" => Some(ProjectionName::AttentionInbox),
            "runtime-capability" => Some(ProjectionName::RuntimeCapability),
            "workflow-board" => Some(ProjectionName::WorkflowBoard),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectionName::ProjectIndex => "project-index",
            ProjectionName::SessionIndex => "session-index",
            ProjectionName::Transcript => "transcript",
            ProjectionName::LiveActivity => "live-activity",
            ProjectionName::InputQueue => "input-queue",
            ProjectionName::AttentionInbox => "attention-inbox",
            ProjectionName::RuntimeCapability => "runtime-capability",
            ProjectionName::WorkflowBoard => "workflow-board",
        }
    }
}

/// Every runtime and project visible through one host, grouped only for
/// presentation by provider family. Capability decisions never read this
/// grouping label.
pub fn project_index(
    state: &CanonicalState,
    host_id: &HostId,
) -> Result<ProjectIndexView, StateError> {
    let host = state
        .hosts()
        .find(|host| &host.id == host_id)
        .ok_or_else(|| StateError::UnknownHostId {
            host_id: host_id.clone(),
        })?;
    let families = [
        ProviderFamily::Codex,
        ProviderFamily::ClaudeCode,
        ProviderFamily::OpenCode,
        ProviderFamily::Acp,
    ];
    let mut groups = Vec::new();
    for family in families {
        let runtime_ids = state
            .runtimes()
            .filter(|runtime| runtime.host_id == *host_id && runtime.family == family)
            .map(|runtime| runtime.id.clone())
            .collect::<Vec<_>>();
        if runtime_ids.is_empty() {
            continue;
        }
        let mut projects = Vec::new();
        for project in state.projects() {
            let bindings = project
                .bindings
                .iter()
                .filter(|binding| runtime_ids.contains(&binding.runtime_id))
                .map(|binding| ProjectBindingSummary {
                    binding_id: binding.id.clone(),
                    runtime_id: binding.runtime_id.clone(),
                })
                .collect::<Vec<_>>();
            if bindings.is_empty() {
                continue;
            }
            projects.push(ProjectSummary {
                project_id: project.id.clone(),
                display_name: project.display_name.clone(),
                bindings,
                session_counts: project.session_counts,
                attention_count: project.attention_count,
                workflow_count: project.workflow_count,
                last_activity_at_ms: project.last_activity_at_ms,
            });
        }
        groups.push(ProviderGroup {
            family,
            runtime_ids,
            projects,
        });
    }
    Ok(ProjectIndexView {
        host_id: host.id.clone(),
        reachability: host.reachability.clone(),
        groups,
    })
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
pub fn attention_inbox(state: &CanonicalState, host_id: &HostId) -> AttentionInboxView {
    let mut entries = state
        .attention_entries()
        .into_iter()
        .filter(|entry| &entry.host_id == host_id)
        .filter(|entry| entry.state.is_open())
        .cloned()
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.expires_at_ms.unwrap_or(i64::MAX));
    AttentionInboxView { entries }
}

/// The complete workflow board, including blockers derived from the selected
/// runtime's observed capabilities and the canonical dependency/gate state.
pub fn workflow_board(
    state: &CanonicalState,
    workflow_id: &WorkflowId,
) -> Result<WorkflowBoardView, StateError> {
    let workflow = state
        .workflow(workflow_id)
        .ok_or_else(|| StateError::UnknownWorkflow {
            workflow_id: workflow_id.clone(),
        })?;
    let project = state.project(&workflow.project_id);
    let mut steps = Vec::new();
    for step_id in &workflow.step_ids {
        let Some(step) = state.step(step_id) else {
            continue;
        };
        let dependency_states = step
            .depends_on
            .iter()
            .filter_map(|dependency_id| {
                state
                    .step(dependency_id)
                    .map(|dependency| (dependency.id.clone(), dependency.state))
            })
            .collect::<Vec<_>>();
        let runtime = project
            .and_then(|project| {
                project
                    .bindings
                    .iter()
                    .find(|binding| binding.id == step.assignment.project_binding_id)
            })
            .and_then(|binding| state.runtime(&binding.runtime_id));
        let gate_open = step
            .human_gate
            .as_ref()
            .and_then(|attention_id| state.attention(attention_id))
            .is_some_and(|attention| attention.state.is_open());
        let blockers = if let Some(runtime) = runtime {
            step.blockers(&dependency_states, &runtime.capabilities, gate_open)
        } else {
            unavailable_runtime_blockers(step, &dependency_states, gate_open)
        };
        steps.push(WorkflowBoardStep {
            step_id: step.id.clone(),
            title: step.title.clone(),
            state: step.state,
            depends_on: step.depends_on.clone(),
            assignment: step.assignment.clone(),
            session_id: step.session_id.clone(),
            blockers,
        });
    }
    Ok(WorkflowBoardView {
        workflow_id: workflow.id.clone(),
        state: workflow.state,
        steps,
        artifacts: state
            .artifacts_of(workflow_id)
            .into_iter()
            .cloned()
            .collect(),
    })
}

fn unavailable_runtime_blockers(
    step: &kaleido_proto::workflow::Step,
    dependency_states: &[(kaleido_proto::ids::StepId, StepState)],
    gate_open: bool,
) -> Vec<StepBlocker> {
    let mut blockers = Vec::new();
    if step.state != StepState::Ready {
        blockers.push(StepBlocker::NotSchedulable { state: step.state });
    }
    for dependency in &step.depends_on {
        let complete = dependency_states.iter().any(|(id, state)| {
            id == dependency && matches!(state, StepState::Completed | StepState::Skipped)
        });
        if !complete {
            blockers.push(StepBlocker::DependencyIncomplete {
                step_id: dependency.clone(),
            });
        }
    }
    blockers.extend(
        step.assignment
            .selector
            .required
            .iter()
            .copied()
            .map(|capability| StepBlocker::CapabilityNotSupported { capability }),
    );
    if gate_open {
        if let Some(attention_id) = &step.human_gate {
            blockers.push(StepBlocker::HumanGateOpen {
                attention_id: attention_id.clone(),
            });
        }
    }
    blockers
}

/// Builds one complete payload for its unique projection key.
pub fn build(state: &CanonicalState, key: &ProjectionKey) -> Result<ProjectionPayload, StateError> {
    let payload = match key {
        ProjectionKey::ProjectIndex { host_id } => ProjectionPayload::ProjectIndex {
            view: project_index(state, host_id)?,
        },
        ProjectionKey::SessionIndex { project_id } => ProjectionPayload::SessionIndex {
            view: session_index(state, project_id),
        },
        ProjectionKey::Transcript { session_id } => ProjectionPayload::Transcript {
            view: transcript(state, session_id)?,
        },
        ProjectionKey::LiveActivity { session_id } => ProjectionPayload::LiveActivity {
            view: live_activity(state, session_id)?,
        },
        ProjectionKey::InputQueue { session_id } => ProjectionPayload::InputQueue {
            view: input_queue(state, session_id)?,
        },
        ProjectionKey::AttentionInbox { host_id } => ProjectionPayload::AttentionInbox {
            view: attention_inbox(state, host_id),
        },
        ProjectionKey::WorkflowBoard { workflow_id } => ProjectionPayload::WorkflowBoard {
            view: workflow_board(state, workflow_id)?,
        },
        ProjectionKey::RuntimeCapability {
            host_id: _,
            runtime_id,
        } => ProjectionPayload::RuntimeCapability {
            view: runtime_capability(state, runtime_id)?,
        },
    };
    payload.validate_for_key(key)?;
    Ok(payload)
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

/// The explicit StateEffect-to-projection fanout matrix required by ADR-0020.
///
/// Both the pre-transition and post-transition states are provided so an
/// upsert that changes scope refreshes the old key as well as the new one.
/// The returned order is deterministic and contains no duplicates.
pub fn affected_keys(
    before: &CanonicalState,
    after: &CanonicalState,
    effect: &StateEffect,
) -> Vec<ProjectionKey> {
    let mut keys = Vec::new();
    match effect {
        StateEffect::HostUpserted { host } => {
            push_host_roots(&mut keys, &host.id);
        }
        StateEffect::RuntimeUpserted { runtime } => {
            if let Some(previous) = before.runtime(&runtime.id) {
                push_unique(
                    &mut keys,
                    ProjectionKey::ProjectIndex {
                        host_id: previous.host_id.clone(),
                    },
                );
            }
            push_unique(
                &mut keys,
                ProjectionKey::ProjectIndex {
                    host_id: runtime.host_id.clone(),
                },
            );
            push_unique(
                &mut keys,
                ProjectionKey::RuntimeCapability {
                    host_id: runtime.host_id.clone(),
                    runtime_id: runtime.id.clone(),
                },
            );
            push_runtime_sessions(&mut keys, before, &runtime.id);
            push_runtime_sessions(&mut keys, after, &runtime.id);
            push_runtime_workflows(&mut keys, before, &runtime.id);
            push_runtime_workflows(&mut keys, after, &runtime.id);
        }
        StateEffect::CapabilitiesUpdated { capabilities } => {
            if let Some(runtime) = after.runtime(&capabilities.runtime_id) {
                push_unique(
                    &mut keys,
                    ProjectionKey::RuntimeCapability {
                        host_id: runtime.host_id.clone(),
                        runtime_id: runtime.id.clone(),
                    },
                );
            }
            push_runtime_sessions(&mut keys, after, &capabilities.runtime_id);
            push_runtime_workflows(&mut keys, after, &capabilities.runtime_id);
        }
        StateEffect::ProjectUpserted { project } => {
            push_project(&mut keys, before, &project.id);
            push_project(&mut keys, after, &project.id);
            push_unique(
                &mut keys,
                ProjectionKey::SessionIndex {
                    project_id: project.id.clone(),
                },
            );
            push_project_workflows(&mut keys, before, &project.id);
            push_project_workflows(&mut keys, after, &project.id);
        }
        StateEffect::SessionUpserted { session } => {
            if let Some(previous) = before.session(&session.id) {
                push_session(&mut keys, before, previous);
            }
            if let Some(current) = after.session(&session.id) {
                push_session(&mut keys, after, current);
            }
        }
        StateEffect::SessionStatusChanged { session_id, .. } => {
            if let Some(session) = after.session(session_id) {
                push_session(&mut keys, after, session);
            }
        }
        StateEffect::TurnUpserted { turn } => {
            if let Some(session) = after.session(&turn.session_id) {
                push_session(&mut keys, after, session);
            }
        }
        StateEffect::ItemUpserted { item } => {
            if let Some(previous) = before.item(&item.id) {
                if let Some(session) = before.session(&previous.session_id) {
                    push_session(&mut keys, before, session);
                }
            }
            if let Some(session) = after.session(&item.session_id) {
                push_session(&mut keys, after, session);
            }
        }
        StateEffect::QueueEntryUpserted { entry } => {
            if let Some(previous) = before.queue_entry(&entry.id) {
                if let Some(session) = before.session(&previous.session_id) {
                    push_session(&mut keys, before, session);
                }
            }
            if let Some(session) = after.session(&entry.session_id) {
                push_session(&mut keys, after, session);
            }
        }
        StateEffect::QueueReordered { session_id, .. } => {
            if let Some(session) = after.session(session_id) {
                push_session(&mut keys, after, session);
            }
        }
        StateEffect::AttentionUpserted { item } => {
            if let Some(previous) = before.attention(&item.id) {
                push_attention(&mut keys, before, previous);
            }
            push_attention(&mut keys, after, item);
            push_all_workflows(&mut keys, after);
        }
        StateEffect::WorkflowUpserted { workflow } => {
            if let Some(previous) = before.workflow(&workflow.id) {
                push_project(&mut keys, before, &previous.project_id);
            }
            push_project(&mut keys, after, &workflow.project_id);
            push_unique(
                &mut keys,
                ProjectionKey::WorkflowBoard {
                    workflow_id: workflow.id.clone(),
                },
            );
        }
        StateEffect::StepUpserted { step } => {
            if let Some(previous) = before.step(&step.id) {
                push_unique(
                    &mut keys,
                    ProjectionKey::WorkflowBoard {
                        workflow_id: previous.workflow_id.clone(),
                    },
                );
            }
            push_unique(
                &mut keys,
                ProjectionKey::WorkflowBoard {
                    workflow_id: step.workflow_id.clone(),
                },
            );
        }
        StateEffect::ArtifactUpserted { artifact } => {
            for workflow in before.workflows() {
                if before
                    .artifacts_of(&workflow.id)
                    .iter()
                    .any(|existing| existing.id == artifact.id)
                {
                    push_unique(
                        &mut keys,
                        ProjectionKey::WorkflowBoard {
                            workflow_id: workflow.id.clone(),
                        },
                    );
                }
            }
            push_unique(
                &mut keys,
                ProjectionKey::WorkflowBoard {
                    workflow_id: artifact.workflow_id.clone(),
                },
            );
        }
        StateEffect::CommandAcknowledged { .. } | StateEffect::DiagnosticRecorded { .. } => {}
    }
    keys
}

fn push_host_roots(keys: &mut Vec<ProjectionKey>, host_id: &HostId) {
    push_unique(
        keys,
        ProjectionKey::ProjectIndex {
            host_id: host_id.clone(),
        },
    );
    push_unique(
        keys,
        ProjectionKey::AttentionInbox {
            host_id: host_id.clone(),
        },
    );
}

fn push_runtime_sessions(
    keys: &mut Vec<ProjectionKey>,
    state: &CanonicalState,
    runtime_id: &ProviderRuntimeId,
) {
    for session in state.sessions() {
        if state
            .runtime_of(session)
            .is_some_and(|runtime| &runtime.id == runtime_id)
        {
            push_session(keys, state, session);
        }
    }
}

fn push_runtime_workflows(
    keys: &mut Vec<ProjectionKey>,
    state: &CanonicalState,
    runtime_id: &ProviderRuntimeId,
) {
    for project in state.projects() {
        if project
            .bindings
            .iter()
            .any(|binding| &binding.runtime_id == runtime_id)
        {
            push_project_workflows(keys, state, &project.id);
        }
    }
}

fn push_project_workflows(
    keys: &mut Vec<ProjectionKey>,
    state: &CanonicalState,
    project_id: &ProjectId,
) {
    for workflow in state
        .workflows()
        .filter(|workflow| &workflow.project_id == project_id)
    {
        push_unique(
            keys,
            ProjectionKey::WorkflowBoard {
                workflow_id: workflow.id.clone(),
            },
        );
    }
}

fn push_session(keys: &mut Vec<ProjectionKey>, state: &CanonicalState, session: &Session) {
    push_unique(
        keys,
        ProjectionKey::SessionIndex {
            project_id: session.project_id.clone(),
        },
    );
    push_unique(
        keys,
        ProjectionKey::Transcript {
            session_id: session.id.clone(),
        },
    );
    push_unique(
        keys,
        ProjectionKey::LiveActivity {
            session_id: session.id.clone(),
        },
    );
    push_unique(
        keys,
        ProjectionKey::InputQueue {
            session_id: session.id.clone(),
        },
    );
    push_project(keys, state, &session.project_id);
}

fn push_attention(
    keys: &mut Vec<ProjectionKey>,
    state: &CanonicalState,
    item: &kaleido_proto::attention::AttentionItem,
) {
    push_unique(
        keys,
        ProjectionKey::AttentionInbox {
            host_id: item.host_id.clone(),
        },
    );
    push_project(keys, state, &item.project_id);
    if let Some(session_id) = &item.session_id {
        if let Some(session) = state.session(session_id) {
            push_session(keys, state, session);
        }
    }
}

fn push_project(keys: &mut Vec<ProjectionKey>, state: &CanonicalState, project_id: &ProjectId) {
    for host_id in project_host_ids(state, project_id) {
        push_unique(keys, ProjectionKey::ProjectIndex { host_id });
    }
}

fn project_host_ids(state: &CanonicalState, project_id: &ProjectId) -> Vec<HostId> {
    let Some(project) = state.project(project_id) else {
        return Vec::new();
    };
    let mut host_ids = Vec::new();
    for binding in &project.bindings {
        if let Some(runtime) = state.runtime(&binding.runtime_id) {
            if !host_ids.contains(&runtime.host_id) {
                host_ids.push(runtime.host_id.clone());
            }
        }
    }
    host_ids
}

fn push_all_workflows(keys: &mut Vec<ProjectionKey>, state: &CanonicalState) {
    for workflow in state.workflows() {
        push_unique(
            keys,
            ProjectionKey::WorkflowBoard {
                workflow_id: workflow.id.clone(),
            },
        );
    }
}

fn push_unique(keys: &mut Vec<ProjectionKey>, key: ProjectionKey) {
    if !keys.contains(&key) {
        keys.push(key);
    }
}
