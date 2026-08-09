#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Reduction checks driven by the committed Codex recordings.
//!
//! Every input here is either a recorded frame or a recorded frame with one
//! field removed or reordered. Nothing is invented: an "ideal" upstream message
//! would test the test rather than the runtime.

mod support;

use kaleido_adapter_codex::error::CodexAdapterError;
use kaleido_adapter_codex::transcript::Direction;
use kaleido_adapter_codex::{CodexReducer, ReducerConfig, SurfacePurpose, TranscriptFrame};
use kaleido_proto::attention::{AttentionState, AttentionSubject, JoinFailureReason, JoinState};
use kaleido_proto::capability::{Capability, CapabilityState, EvidenceSource};
use kaleido_proto::command::CommandOutcome;
use kaleido_proto::effect::{DiagnosticCode, StateEffect};
use kaleido_proto::error::ErrorCode;
use kaleido_proto::host::{ConnectionFaultReason, ConnectionState, HostPlatform, LaunchSurface};
use kaleido_proto::ids::{CommandId, ItemId, ProviderBindingKind};
use kaleido_proto::session::{LiveBinding, LiveUnboundReason, SessionStatus};
use kaleido_proto::turn::{ItemBody, ItemStatus, MessagePhase, TurnOrigin, TurnStatus};
use serde_json::Value;

use support::{fixture_path, load_transcript, reducer, MemoryContent, BASE_AT_MS};

fn live_reducer() -> CodexReducer {
    CodexReducer::new(ReducerConfig {
        host_display_name: "test-host".to_owned(),
        host_platform: HostPlatform::Windows,
        project_display_name: "test-project".to_owned(),
        identity_salt: "test-host".to_owned(),
        evidence: EvidenceSource::ObservedInTraffic,
        launch_surface: LaunchSurface::BrokerLaunched,
        turn_origin: TurnOrigin::LocalSurface,
        base_at_ms: BASE_AT_MS,
        runtime_version_label: None,
    })
}

/// The last state each item reached, in observation order.
fn items(effects: &[StateEffect]) -> Vec<(ItemId, ItemStatus, String)> {
    let mut order = Vec::new();
    let mut seen = Vec::new();
    for effect in effects {
        if let StateEffect::ItemUpserted { item } = effect {
            let label = match &item.body {
                ItemBody::UserMessage { .. } => "user_message",
                ItemBody::AgentMessage { .. } => "agent_message",
                ItemBody::Reasoning { .. } => "reasoning",
                ItemBody::FileEdit { .. } => "file_edit",
                ItemBody::ToolCall { .. } => "tool_call",
                ItemBody::PlanUpdate { .. } => "plan_update",
                ItemBody::TaskUpdate { .. } => "task_update",
                ItemBody::Diagnostic { .. } => "diagnostic",
            };
            if !seen.contains(&item.id) {
                seen.push(item.id.clone());
                order.push((item.id.clone(), item.status, label.to_owned()));
            } else if let Some(slot) = order.iter_mut().find(|entry| entry.0 == item.id) {
                slot.1 = item.status;
                slot.2 = label.to_owned();
            }
        }
    }
    order
}

fn last_turn(effects: &[StateEffect]) -> kaleido_proto::turn::Turn {
    effects
        .iter()
        .rev()
        .find_map(|effect| match effect {
            StateEffect::TurnUpserted { turn } => Some(turn.clone()),
            _ => None,
        })
        .expect("the recording must contain a turn")
}

fn last_attention(effects: &[StateEffect]) -> kaleido_proto::attention::AttentionItem {
    effects
        .iter()
        .rev()
        .find_map(|effect| match effect {
            StateEffect::AttentionUpserted { item } => Some(item.clone()),
            _ => None,
        })
        .expect("the recording must contain an approval")
}

fn attention_states(effects: &[StateEffect]) -> Vec<JoinState> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            StateEffect::AttentionUpserted { item } => match &item.subject {
                AttentionSubject::Approval { request } => Some(request.join.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn diagnostic_count(effects: &[StateEffect], code: DiagnosticCode) -> u64 {
    effects
        .iter()
        .rev()
        .find_map(|effect| match effect {
            StateEffect::DiagnosticRecorded { diagnostic } if diagnostic.code == code => {
                Some(diagnostic.count)
            }
            _ => None,
        })
        .unwrap_or(0)
}

fn agent_text(effects: &[StateEffect], content: &MemoryContent) -> Option<String> {
    effects.iter().rev().find_map(|effect| match effect {
        StateEffect::ItemUpserted { item } => match &item.body {
            ItemBody::AgentMessage { content: body, .. } => Some(content.text_of(body)),
            _ => None,
        },
        _ => None,
    })
}

#[test]
fn a_simple_turn_reduces_to_one_user_and_one_agent_message() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the recording must reduce cleanly");

    let items = items(&effects);
    assert_eq!(items.len(), 2, "observed items: {items:?}");
    assert_eq!(
        items.first().map(|entry| entry.2.as_str()),
        Some("user_message")
    );
    assert_eq!(
        items.get(1).map(|entry| entry.2.as_str()),
        Some("agent_message")
    );
    assert!(items.iter().all(|entry| entry.1 == ItemStatus::Completed));

    let turn = last_turn(&effects);
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.error, None);
    assert_eq!(
        agent_text(&effects, &content).as_deref(),
        Some("KALEIDO SIMPLE TURN")
    );

    let phase = effects.iter().rev().find_map(|effect| match effect {
        StateEffect::ItemUpserted { item } => match &item.body {
            ItemBody::AgentMessage { phase, .. } => Some(*phase),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(phase, Some(MessagePhase::FinalAnswer));
}

#[test]
fn observed_traffic_proves_the_live_binding_and_its_capability_evidence() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the live recording must reduce cleanly");

    let session = effects.iter().find_map(|effect| match effect {
        StateEffect::SessionUpserted { session } => Some(session),
        _ => None,
    });
    match session.map(|session| &session.live_binding) {
        Some(LiveBinding::Observing {
            runtime_id,
            evidence,
            ..
        }) => {
            assert_eq!(runtime_id, reducer.runtime_id());
            assert_eq!(evidence.source, EvidenceSource::ObservedInTraffic);
        }
        other => panic!("live traffic must produce an observing binding, found {other:?}"),
    }

    let capabilities = effects.iter().rev().find_map(|effect| match effect {
        StateEffect::CapabilitiesUpdated { capabilities } => Some(capabilities),
        _ => None,
    });
    let live_observe = capabilities
        .and_then(|capabilities| {
            capabilities
                .entries
                .iter()
                .find(|entry| entry.capability == Capability::LiveObserve)
        })
        .expect("live observation must have an explicit capability entry");
    assert_eq!(live_observe.state, CapabilityState::Supported);
    assert_eq!(
        live_observe.evidence.source,
        EvidenceSource::ObservedInTraffic
    );
    assert_eq!(
        capabilities
            .expect("live traffic publishes capabilities")
            .state_of(&Capability::LiveControl),
        CapabilityState::NotVerified,
        "an observed response without a local command correlation is not control proof"
    );
    assert!(effects.iter().all(|effect| !matches!(
        effect,
        StateEffect::CommandAcknowledged {
            ack: kaleido_proto::command::CommandAck {
                outcome: CommandOutcome::AcceptedByRuntime { .. },
                ..
            }
        } | StateEffect::SessionUpserted {
            session: kaleido_proto::session::Session {
                live_binding: LiveBinding::Controlling { .. },
                ..
            }
        }
    )));
}

#[test]
fn a_correlated_live_turn_response_proves_control_in_store_safe_order() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    let mut effects = Vec::new();

    // The first six frames are the real handshake. The seventh is the recorded
    // client turn/start with request id 3; correlation is registered before it
    // is ingested, just as the live runtime does before writing the request.
    for frame in transcript.frames().iter().take(6) {
        effects.extend(
            reducer
                .ingest_frame(frame, &mut content)
                .expect("the recorded handshake must reduce"),
        );
    }
    let command_id = CommandId::new("cmd_fixture_prompt");
    assert!(reducer.register_local_turn_start(3, &command_id));
    for frame in transcript.frames().iter().skip(6) {
        effects.extend(
            reducer
                .ingest_frame(frame, &mut content)
                .expect("the recorded turn must reduce"),
        );
    }

    let capability_index = effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                StateEffect::CapabilitiesUpdated { capabilities }
                    if capabilities.state_of(&Capability::LiveControl)
                        == CapabilityState::Supported
            )
        })
        .expect("the matching response must prove live control");
    let ack_index = effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                StateEffect::CommandAcknowledged { ack }
                    if ack.command_id == command_id
                        && matches!(ack.outcome, CommandOutcome::AcceptedByRuntime { .. })
            )
        })
        .expect("the matching response must acknowledge the command");
    let controlling_index = effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                StateEffect::SessionUpserted { session }
                    if matches!(
                        session.live_binding,
                        LiveBinding::Controlling {
                            evidence: kaleido_proto::capability::CapabilityEvidence {
                                source: EvidenceSource::ObservedInTraffic,
                                ..
                            },
                            ..
                        }
                    )
            )
        })
        .expect("the matching response must promote the session");
    assert!(
        ack_index < capability_index && capability_index < controlling_index,
        "runtime ack, live-control capability and controlling binding must be store-safe ordered"
    );
    assert!(effects.iter().any(|effect| matches!(
        effect,
        StateEffect::CommandAcknowledged { ack }
            if ack.command_id == command_id
                && matches!(
                    &ack.outcome,
                    CommandOutcome::AcceptedByRuntime { binding_handle }
                        if binding_handle.kind == ProviderBindingKind::RuntimeAcknowledgement
                            && binding_handle.runtime_id == *reducer.runtime_id()
                )
    )));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        StateEffect::TurnUpserted { turn }
            if matches!(
                &turn.origin,
                TurnOrigin::RemoteCommand { command_id: origin_command_id }
                    if origin_command_id == &command_id
            )
    )));
    assert!(
        effects
            .iter()
            .filter_map(|effect| match effect {
                StateEffect::TurnUpserted { turn } => Some(&turn.origin),
                _ => None,
            })
            .all(|origin| matches!(
                origin,
                TurnOrigin::RemoteCommand { command_id: origin_command_id }
                    if origin_command_id == &command_id
            )),
        "later turn notifications must preserve the correlated command origin"
    );
    let capabilities = reducer.capability_probe().to_capabilities();
    assert_eq!(
        capabilities.state_of(&Capability::LiveControl),
        CapabilityState::Supported
    );
    assert_eq!(
        capabilities.state_of(&Capability::TurnSteer),
        CapabilityState::NotVerified
    );
}

#[test]
fn an_outgoing_turn_without_its_response_never_proves_control() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();

    for frame in transcript.frames().iter().take(6) {
        reducer
            .ingest_frame(frame, &mut content)
            .expect("the recorded handshake must reduce");
    }
    let command_id = CommandId::new("cmd_without_response");
    assert!(reducer.register_local_turn_start(3, &command_id));
    let effects = reducer
        .ingest_frame(
            transcript
                .frames()
                .get(6)
                .expect("the recorded outgoing turn exists"),
            &mut content,
        )
        .expect("the outgoing frame must reduce");

    assert!(effects.iter().all(|effect| !matches!(
        effect,
        StateEffect::CommandAcknowledged { .. }
            | StateEffect::SessionUpserted {
                session: kaleido_proto::session::Session {
                    live_binding: LiveBinding::Controlling { .. },
                    ..
                }
            }
    )));
    assert_eq!(
        reducer
            .capability_probe()
            .to_capabilities()
            .state_of(&Capability::LiveControl),
        CapabilityState::NotVerified
    );
}

#[test]
fn a_live_response_for_another_request_never_proves_control() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    let mut effects = Vec::new();

    for frame in transcript.frames().iter().take(6) {
        effects.extend(
            reducer
                .ingest_frame(frame, &mut content)
                .expect("the recorded handshake must reduce"),
        );
    }
    let command_id = CommandId::new("cmd_wrong_request");
    assert!(reducer.register_local_turn_start(99, &command_id));
    for frame in transcript.frames().iter().skip(6) {
        effects.extend(
            reducer
                .ingest_frame(frame, &mut content)
                .expect("the recorded turn must reduce"),
        );
    }

    assert!(effects.iter().all(|effect| !matches!(
        effect,
        StateEffect::CommandAcknowledged { .. }
            | StateEffect::SessionUpserted {
                session: kaleido_proto::session::Session {
                    live_binding: LiveBinding::Controlling { .. },
                    ..
                }
            }
    )));
    assert_eq!(
        reducer
            .capability_probe()
            .to_capabilities()
            .state_of(&Capability::LiveControl),
        CapabilityState::NotVerified
    );
}

#[test]
fn replay_cannot_register_a_local_control_command() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let command_id = CommandId::new("cmd_replayed_prompt");
    assert!(!reducer.register_local_turn_start(3, &command_id));

    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the real recording must still replay");
    assert!(effects.iter().all(|effect| !matches!(
        effect,
        StateEffect::CommandAcknowledged { .. }
            | StateEffect::SessionUpserted {
                session: kaleido_proto::session::Session {
                    live_binding: LiveBinding::Controlling { .. },
                    ..
                }
            }
    )));
    assert_eq!(
        reducer
            .capability_probe()
            .to_capabilities()
            .state_of(&Capability::LiveControl),
        CapabilityState::NotVerified
    );
}

#[test]
fn a_streaming_increment_updates_the_existing_item_rather_than_adding_one() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the recording must reduce cleanly");

    // Five increments arrive in this recording. If any of them minted an item,
    // the distinct-identifier count would exceed two.
    let distinct = items(&effects).len();
    let upserts = effects
        .iter()
        .filter(|effect| matches!(effect, StateEffect::ItemUpserted { .. }))
        .count();
    assert_eq!(distinct, 2);
    assert!(
        upserts > distinct,
        "increments must produce more upserts ({upserts}) than items ({distinct})"
    );
}

#[test]
fn the_streamed_text_is_complete_before_the_completion_message_arrives() {
    // Frames 1..=34 of this recording stop right after the fifth increment and
    // before `item/completed`, so the text can only come from accumulation.
    let transcript = load_transcript("01-simple-turn.jsonl").prefix(34);
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let mut effects = Vec::new();
    for frame in transcript.frames() {
        effects.extend(
            reducer
                .ingest_frame(frame, &mut content)
                .expect("each frame must reduce cleanly"),
        );
    }
    assert_eq!(
        agent_text(&effects, &content).as_deref(),
        Some("KALEIDO SIMPLE TURN")
    );
}

#[test]
fn a_completion_summary_never_replaces_the_accumulated_item_list() {
    // The recorded completion payload is marked as a summary view and carries
    // only the final message, while the turn actually produced six items.
    let transcript = load_transcript("03-permission-approve.jsonl");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the recording must reduce cleanly");

    let observed = items(&effects);
    assert_eq!(observed.len(), 6, "observed items: {observed:?}");
    let turn = last_turn(&effects);
    assert_eq!(turn.status, TurnStatus::Completed);
    // The reducer contributes no identifiers of its own to the turn; the store
    // accumulates them from item transitions.
    assert!(turn.item_ids.is_empty());
}

#[test]
fn an_approved_file_change_completes_and_its_approval_is_answered() {
    let transcript = load_transcript("03-permission-approve.jsonl");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the recording must reduce cleanly");

    let file_edit = items(&effects)
        .into_iter()
        .find(|entry| entry.2 == "file_edit")
        .expect("the recording contains a file edit");
    assert_eq!(file_edit.1, ItemStatus::Completed);

    let attention = last_attention(&effects);
    assert!(matches!(attention.state, AttentionState::Answered { .. }));
    match &attention.subject {
        AttentionSubject::Approval { request } => {
            assert!(matches!(request.join, JoinState::Joined { .. }));
            assert!(
                request
                    .options
                    .iter()
                    .any(|option| option.option_id == "accept"),
                "the runtime's own vocabulary must be offered"
            );
        }
        other => panic!("expected an approval, found {other:?}"),
    }

    let turn = last_turn(&effects);
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.error, None);
}

#[test]
fn a_declined_file_change_is_terminal_and_the_turn_still_completes() {
    // Rule R-P8, taken directly from the recording: the client answers with a
    // decline, the operation ends `declined`, and the turn still completes.
    let transcript = load_transcript("04-permission-deny.jsonl");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the recording must reduce cleanly");

    let file_edit = items(&effects)
        .into_iter()
        .find(|entry| entry.2 == "file_edit")
        .expect("the recording contains a file edit");
    assert_eq!(
        file_edit.1,
        ItemStatus::Declined,
        "a refusal is a terminal item state, not a failure"
    );
    assert!(!file_edit.1.is_failure());

    let turn = last_turn(&effects);
    assert_eq!(
        turn.status,
        TurnStatus::Completed,
        "a refusal must not fail the enclosing turn"
    );
    assert_eq!(turn.error, None, "a refusal is never a canonical error");

    let attention = last_attention(&effects);
    match &attention.state {
        AttentionState::Answered { option_id, .. } => {
            assert_eq!(option_id.as_deref(), Some("decline"));
        }
        other => panic!("expected an answered approval, found {other:?}"),
    }
}

#[test]
fn a_live_outgoing_reply_never_overwrites_the_stores_real_answer() {
    // Reorder only frames from the real approval recording so the operation
    // arrives *after* the outgoing reply. This exercises the hidden overwrite
    // path where a later join refresh could otherwise republish the replay-only
    // synthetic command ID.
    let transcript = load_transcript("03-permission-approve.jsonl");
    let frames = transcript.frames();
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    for frame in frames.iter().take(47) {
        reducer
            .ingest_frame(frame, &mut content)
            .expect("the recorded preamble must reduce");
    }

    let approval_effects = reducer
        .ingest_frame(
            frames.get(49).expect("recorded approval request exists"),
            &mut content,
        )
        .expect("the recorded approval request must reduce");
    assert!(
        approval_effects.iter().any(|effect| matches!(
            effect,
            StateEffect::AttentionUpserted {
                item: kaleido_proto::attention::AttentionItem {
                    state: AttentionState::Open,
                    ..
                }
            }
        )),
        "the live request itself must still open attention"
    );

    let reply_effects = reducer
        .ingest_frame(
            frames.get(50).expect("recorded outgoing reply exists"),
            &mut content,
        )
        .expect("the recorded outgoing reply must decode and validate");
    assert!(
        reply_effects
            .iter()
            .all(|effect| !matches!(effect, StateEffect::AttentionUpserted { .. })),
        "the reducer must not replace the store's real command ID"
    );
    assert!(
        reducer
            .exercised_purposes()
            .contains(&SurfacePurpose::ApprovalDecision),
        "the suppressed reply must still pass through the pinned reader"
    );

    let later_item_effects = reducer
        .ingest_frame(
            frames.get(47).expect("recorded file-change item exists"),
            &mut content,
        )
        .expect("the delayed recorded item must reduce");
    assert!(
        later_item_effects
            .iter()
            .any(|effect| matches!(effect, StateEffect::ItemUpserted { .. })),
        "the delayed item must still be reduced"
    );
    assert!(
        later_item_effects
            .iter()
            .all(|effect| !matches!(effect, StateEffect::AttentionUpserted { .. })),
        "a later join refresh must not overwrite the store's answered state"
    );
}

#[test]
fn an_approval_that_arrives_first_renders_unjoined_and_is_upgraded_later() {
    // Frames 48 and 50 of this recording are the file-edit announcement and the
    // approval request. Swapping them is the honest way to test the ordering
    // the protocol says must be tolerated.
    let transcript = load_transcript("03-permission-approve.jsonl").with_swapped(47, 49);
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("an out-of-order approval must still reduce");

    let joins = attention_states(&effects);
    assert!(
        matches!(
            joins.first(),
            Some(JoinState::Unjoined {
                reason: JoinFailureReason::ItemNotYetSeen
            })
        ),
        "first join state was {:?}",
        joins.first()
    );
    assert!(
        joins
            .iter()
            .any(|join| matches!(join, JoinState::Joined { .. })),
        "the join must be upgraded once the operation arrives: {joins:?}"
    );
    assert!(
        diagnostic_count(&effects, DiagnosticCode::JoinDeferred) >= 1,
        "a deferred join must be recorded"
    );

    // The intermediate state must be renderable, not a panic and not a fake.
    let intermediate = effects
        .iter()
        .find_map(|effect| match effect {
            StateEffect::AttentionUpserted { item } => Some(item.clone()),
            _ => None,
        })
        .expect("an approval must be emitted");
    assert!(intermediate.validate().is_ok());
}

#[test]
fn an_approval_whose_operation_never_arrives_ends_as_item_unknown() {
    // Same reordering, then truncated before the operation is announced, so the
    // observation window closes with the join still unresolved.
    let transcript = load_transcript("03-permission-approve.jsonl")
        .with_swapped(47, 49)
        .prefix(49);
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("an unresolved approval must still reduce");

    let attention = last_attention(&effects);
    match &attention.subject {
        AttentionSubject::Approval { request } => assert!(
            matches!(
                request.join,
                JoinState::Unjoined {
                    reason: JoinFailureReason::ItemUnknown
                }
            ),
            "join state was {:?}",
            request.join
        ),
        other => panic!("expected an approval, found {other:?}"),
    }
    assert!(
        diagnostic_count(&effects, DiagnosticCode::JoinFailed) >= 1,
        "a failed join must be recorded"
    );
}

#[test]
fn an_early_process_exit_emits_the_runtime_session_and_fault_triad() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    reducer
        .ingest(&transcript, &mut content)
        .expect("the recorded live session must reduce");

    let at_ms = BASE_AT_MS + 100_000;
    let effects = reducer
        .process_exited(Some(23), at_ms)
        .expect("the lifecycle effects must satisfy the canonical contract");
    assert_eq!(effects.len(), 3, "exit effects were {effects:?}");
    assert!(
        effects
            .iter()
            .all(|effect| effect.validate_for_log().is_ok()),
        "every lifecycle effect must pass the real log validator"
    );

    let runtime = match effects.first() {
        Some(StateEffect::RuntimeUpserted { runtime }) => runtime,
        other => panic!("first exit effect must update the runtime, found {other:?}"),
    };
    assert!(matches!(
        runtime.connection,
        ConnectionState::Unavailable {
            reason: ConnectionFaultReason::ProcessExited {
                exit_code: Some(23)
            },
            since_at_ms
        } if since_at_ms == at_ms
    ));
    assert_eq!(
        runtime.capabilities.state_of(&Capability::LiveObserve),
        CapabilityState::Supported,
        "the exit update must retain capabilities proved after bootstrap"
    );

    let session = match effects.get(1) {
        Some(StateEffect::SessionUpserted { session }) => session,
        other => panic!("second exit effect must update the session, found {other:?}"),
    };
    assert_eq!(session.status, SessionStatus::Offline);
    assert!(matches!(
        session.live_binding,
        LiveBinding::NotBound {
            reason: LiveUnboundReason::RuntimeExited
        }
    ));

    let attention = match effects.get(2) {
        Some(StateEffect::AttentionUpserted { item }) => item,
        other => panic!("third exit effect must open attention, found {other:?}"),
    };
    assert_eq!(attention.session_id.as_ref(), Some(&session.id));
    assert_eq!(attention.turn_id, None);
    assert_eq!(attention.state, AttentionState::Open);
    assert!(matches!(
        attention.subject,
        AttentionSubject::ConnectionFault {
            reason: ConnectionFaultReason::ProcessExited {
                exit_code: Some(23)
            },
            ..
        }
    ));
}

#[test]
fn a_process_exit_is_published_only_once() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    reducer
        .ingest(&transcript, &mut content)
        .expect("the recorded live session must reduce");

    let first = reducer
        .process_exited(None, BASE_AT_MS + 100_000)
        .expect("the first exit must reduce");
    let repeated = reducer
        .process_exited(None, BASE_AT_MS + 100_001)
        .expect("a repeated exit is an idempotent lifecycle observation");
    assert_eq!(first.len(), 3);
    assert!(
        repeated.is_empty(),
        "a repeated exit must not duplicate durable state: {repeated:?}"
    );
}

#[test]
fn a_clean_disconnect_has_no_connection_fault_attention() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    reducer
        .ingest(&transcript, &mut content)
        .expect("the recorded live session must reduce");

    let effects = reducer
        .clean_disconnected(BASE_AT_MS + 100_000)
        .expect("clean-close effects must satisfy the canonical contract");
    assert_eq!(effects.len(), 2, "clean-close effects were {effects:?}");
    assert!(effects
        .iter()
        .all(|effect| effect.validate_for_log().is_ok()));
    assert!(matches!(
        effects.first(),
        Some(StateEffect::RuntimeUpserted {
            runtime: kaleido_proto::host::ProviderRuntime {
                connection: ConnectionState::Disconnected,
                ..
            }
        })
    ));
    assert!(matches!(
        effects.get(1),
        Some(StateEffect::SessionUpserted {
            session: kaleido_proto::session::Session {
                status: SessionStatus::Offline,
                live_binding: LiveBinding::NotBound {
                    reason: LiveUnboundReason::RuntimeExited
                },
                ..
            }
        })
    ));
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect, StateEffect::AttentionUpserted { .. })),
        "an intentional close must not page the user"
    );
}

#[test]
fn an_unregistered_method_is_counted_and_produces_no_business_projection() {
    let transcript = load_transcript("01-simple-turn.jsonl");
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    let effects = reducer
        .ingest(&transcript, &mut content)
        .expect("the recording must reduce cleanly");

    // This recording carries fifteen server startup notifications this slice
    // does not model, plus other unmodelled traffic.
    assert!(
        diagnostic_count(&effects, DiagnosticCode::UnknownUpstreamMessage) >= 15,
        "unmodelled traffic must be counted"
    );
    // And none of it became an item, a turn or an approval.
    let from_unmodelled = effects.iter().filter(|effect| {
        matches!(
            effect,
            StateEffect::AttentionUpserted { .. } | StateEffect::QueueEntryUpserted { .. }
        )
    });
    assert_eq!(from_unmodelled.count(), 0);
}

fn frame_with_removed_field(fixture: &str, line: usize, pointer: &str) -> TranscriptFrame {
    let raw = std::fs::read_to_string(fixture_path(fixture)).expect("read fixture");
    let line = raw.lines().nth(line - 1).expect("fixture line exists");
    let mut envelope = serde_json::from_str::<Value>(line).expect("fixture line is JSON");
    let direction = match envelope.get("dir").and_then(Value::as_str) {
        Some("c2s") => Direction::ClientToServer,
        Some("s2c") => Direction::ServerToClient,
        other => panic!("recorded direction must be c2s or s2c, found {other:?}"),
    };
    let payload = envelope
        .get_mut("payload")
        .expect("recorded envelope carries a payload");
    let (parent, leaf) = pointer.rsplit_once('/').expect("pointer has a parent");
    payload
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .expect("parent object exists")
        .remove(leaf)
        .expect("field to remove exists");
    let bytes = serde_json::to_vec(payload).expect("re-encode payload");
    TranscriptFrame::from_wire(direction, 0, &bytes).expect("frame is JSON")
}

fn frame_with_replaced_field(
    fixture: &str,
    line: usize,
    pointer: &str,
    replacement: Value,
) -> TranscriptFrame {
    let raw = std::fs::read_to_string(fixture_path(fixture)).expect("read fixture");
    let line = raw.lines().nth(line - 1).expect("fixture line exists");
    let mut envelope = serde_json::from_str::<Value>(line).expect("fixture line is JSON");
    let direction = match envelope.get("dir").and_then(Value::as_str) {
        Some("c2s") => Direction::ClientToServer,
        Some("s2c") => Direction::ServerToClient,
        other => panic!("recorded direction must be c2s or s2c, found {other:?}"),
    };
    let payload = envelope
        .get_mut("payload")
        .expect("recorded envelope carries a payload");
    *payload.pointer_mut(pointer).expect("pointer resolves") = replacement;
    let bytes = serde_json::to_vec(payload).expect("re-encode payload");
    TranscriptFrame::from_wire(direction, 0, &bytes).expect("frame is JSON")
}

#[test]
fn a_registered_method_missing_a_pinned_field_is_a_protocol_violation() {
    // ADR-0012 D-3: a registered path that stops resolving must fail loudly
    // rather than degrade into a plausible success.
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    for frame in load_transcript("01-simple-turn.jsonl").prefix(26).frames() {
        reducer
            .ingest_frame(frame, &mut content)
            .expect("the preamble must reduce cleanly");
    }
    let damaged = frame_with_removed_field("01-simple-turn.jsonl", 27, "/params/item/id");
    let error = reducer
        .ingest_frame(&damaged, &mut content)
        .expect_err("a missing pinned field must be refused");
    assert!(matches!(error, CodexAdapterError::PointerUnresolved { .. }));
    assert!(error.is_surface_drift());
    assert_eq!(
        error.canonical_error(BASE_AT_MS).code,
        ErrorCode::RuntimeProtocolViolation
    );
}

#[test]
fn a_registered_method_with_a_wrong_value_type_is_a_protocol_violation() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    for frame in load_transcript("01-simple-turn.jsonl").prefix(26).frames() {
        reducer
            .ingest_frame(frame, &mut content)
            .expect("the preamble must reduce cleanly");
    }
    let damaged = frame_with_replaced_field(
        "01-simple-turn.jsonl",
        27,
        "/params/item/id",
        Value::from(42),
    );
    let error = reducer
        .ingest_frame(&damaged, &mut content)
        .expect_err("a wrongly typed pinned field must be refused");
    assert!(matches!(
        error,
        CodexAdapterError::PointerTypeMismatch { .. }
    ));
    assert_eq!(
        error.canonical_error(BASE_AT_MS).code,
        ErrorCode::RuntimeProtocolViolation
    );
}

#[test]
fn a_live_outgoing_reply_still_validates_the_recorded_decision_vocabulary() {
    let mut reducer = live_reducer();
    let mut content = MemoryContent::default();
    for frame in load_transcript("03-permission-approve.jsonl")
        .prefix(50)
        .frames()
    {
        reducer
            .ingest_frame(frame, &mut content)
            .expect("the recorded preamble and approval must reduce");
    }
    let damaged = frame_with_replaced_field(
        "03-permission-approve.jsonl",
        51,
        "/result/decision",
        Value::from("not-a-recorded-decision"),
    );
    let error = reducer
        .ingest_frame(&damaged, &mut content)
        .expect_err("a live reply outside the pinned vocabulary must be refused");
    assert!(matches!(
        error,
        CodexAdapterError::UnmodelledEnumeration {
            purpose: SurfacePurpose::ApprovalDecision
        }
    ));
}

#[test]
fn an_unmodelled_item_kind_becomes_a_diagnostic_rather_than_a_guess() {
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    for frame in load_transcript("01-simple-turn.jsonl").prefix(26).frames() {
        reducer
            .ingest_frame(frame, &mut content)
            .expect("the preamble must reduce cleanly");
    }
    // `commandExecution` is a real Codex item kind this repository has no
    // recording of, so it must not be guessed into a known body.
    let unmodelled = frame_with_replaced_field(
        "01-simple-turn.jsonl",
        27,
        "/params/item/type",
        Value::from("commandExecution"),
    );
    let effects = reducer
        .ingest_frame(&unmodelled, &mut content)
        .expect("an unmodelled kind must not abort the stream");
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect, StateEffect::ItemUpserted { .. })),
        "an unmodelled kind must not become an item"
    );
    assert_eq!(
        diagnostic_count(&effects, DiagnosticCode::UnknownUpstreamLabel),
        1
    );
    // The raw label may be kept, but only behind a sensitive reference.
    let detail = effects.iter().find_map(|effect| match effect {
        StateEffect::DiagnosticRecorded { diagnostic } => diagnostic.detail_ref.clone(),
        _ => None,
    });
    let detail = detail.expect("the label must be retained behind a reference");
    assert_eq!(
        detail.sensitivity,
        kaleido_proto::content::Sensitivity::Sensitive
    );
    assert_eq!(detail.preview, None);
}

#[test]
fn an_approval_pointing_at_another_turn_is_a_scope_mismatch() {
    // Replay through the file-edit announcement, then deliver the recorded
    // approval with its turn identifier swapped for the recording's *thread*
    // identifier. Both values are real; only the pairing is wrong, which is
    // the narrowest way to reach a scope mismatch without inventing a frame.
    let mut reducer = reducer();
    let mut content = MemoryContent::default();
    for frame in load_transcript("03-permission-approve.jsonl")
        .prefix(48)
        .frames()
    {
        reducer
            .ingest_frame(frame, &mut content)
            .expect("the preamble must reduce cleanly");
    }
    let mismatched = frame_with_replaced_field(
        "03-permission-approve.jsonl",
        50,
        "/params/turnId",
        Value::from("019fb1ab-b957-7360-9092-8bbb9c1ae8b4"),
    );
    let effects = reducer
        .ingest_frame(&mismatched, &mut content)
        .expect("a mis-scoped approval must still reduce");

    let attention = last_attention(&effects);
    match &attention.subject {
        AttentionSubject::Approval { request } => assert!(
            matches!(
                request.join,
                JoinState::Unjoined {
                    reason: JoinFailureReason::ScopeMismatch
                }
            ),
            "join state was {:?}",
            request.join
        ),
        other => panic!("expected an approval, found {other:?}"),
    }
    // A mis-scoped approval is still renderable, and still not a fake join.
    assert!(attention.validate().is_ok());
}
