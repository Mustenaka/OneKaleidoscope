use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub use kaleido_recorder::agents::{
    validate_exact_permission_cwd, validate_exact_permission_path, validate_permission_argv,
    validate_permission_argv_as, validate_permission_command, validate_permission_command_as,
    validate_permission_path, PermissionCommand, PermissionScopeError,
};

pub mod fixture {
    pub use kaleido_recorder::fixture::*;
}

pub mod platform {
    pub use kaleido_recorder::platform::*;
}

pub mod stdio_tee {
    pub use kaleido_recorder::stdio_tee::*;
}

#[path = "../src/agents/acp.rs"]
mod acp;

use acp::{
    AcpError, AcpScenario, AcpStateMachine, AuthenticationStage, MachineStep, ScenarioOutcome,
    UnsupportedReason, CLAUDE_ACP_INSTALL_COMMAND, CLAUDE_ACP_PACKAGE, CLAUDE_ACP_PACKAGE_NAME,
    CLAUDE_ACP_VERSION,
};

fn sandbox() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("sandbox")
}

fn sandbox_file_permission_path(filename: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(platform::permission_path_pattern(
        &sandbox().join(filename),
    )?)
}

fn one_message(step: MachineStep) -> Result<Value, Box<dyn std::error::Error>> {
    let MachineStep::Send(messages) = step else {
        return Err("expected outbound message".into());
    };
    let mut messages = messages.into_iter();
    let raw = messages.next().ok_or("outbound message was missing")?;
    if messages.next().is_some() {
        return Err("expected exactly one outbound message".into());
    }
    Ok(serde_json::from_str(&raw)?)
}

fn initialize(
    machine: &mut AcpStateMachine,
    capabilities: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let initialize = one_message(machine.start()?)?;
    assert_eq!(
        initialize.get("method").and_then(Value::as_str),
        Some("initialize")
    );
    let step = machine.accept_raw(&format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":1,"agentCapabilities":{capabilities}}}}}"#
    ))?;
    one_message(step)
}

fn create_session(machine: &mut AcpStateMachine) -> Result<Value, Box<dyn std::error::Error>> {
    let new_session = initialize(machine, serde_json::json!({}))?;
    assert_eq!(
        new_session.get("method").and_then(Value::as_str),
        Some("session/new")
    );
    let prompt = one_message(
        machine.accept_raw(r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"session-1"}}"#)?,
    )?;
    Ok(prompt)
}

#[test]
fn simple_turn_runs_initialize_new_and_prompt() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    let prompt = create_session(&mut machine)?;
    assert_eq!(
        prompt.get("method").and_then(Value::as_str),
        Some("session/prompt")
    );
    assert_eq!(
        prompt
            .get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str),
        Some("session-1")
    );

    let outcome =
        machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    let MachineStep::Complete(ScenarioOutcome::Completed { stop_reason, .. }) = outcome else {
        return Err("expected completed scenario".into());
    };
    assert_eq!(stop_reason, "end_turn");
    Ok(())
}

#[test]
fn simple_turn_counts_only_nonempty_chunks_from_the_active_session(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut empty = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    let _ = create_session(&mut empty)?;
    let _ = empty.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""}}}}"#,
    )?;
    let outcome =
        empty.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        outcome,
        MachineStep::Complete(ScenarioOutcome::Completed { observations, .. })
            if !observations.session_update_kinds.iter().any(|kind| kind == "agent_message_chunk")
    ));

    let mut nonempty = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    let _ = create_session(&mut nonempty)?;
    let _ = nonempty.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"real reply"}}}}"#,
    )?;
    let outcome =
        nonempty.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        outcome,
        MachineStep::Complete(ScenarioOutcome::Completed { observations, .. })
            if observations.session_update_kinds.iter().any(|kind| kind == "agent_message_chunk")
    ));

    let mut foreign = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    let _ = create_session(&mut foreign)?;
    assert!(matches!(
        foreign.accept_raw(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-foreign","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"foreign"}}}}"#
        ),
        Err(AcpError::SessionIdMismatch)
    ));
    Ok(())
}

#[test]
fn approve_selects_offered_allow_once_option() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::PermissionApprove);
    let _ = create_session(&mut machine)?;
    let reply = one_message(machine.accept_raw(
        r#"{"jsonrpc":"2.0","id":"permission-7","method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"deny","name":"Deny","kind":"reject_once"},{"optionId":"approve-exact","name":"Allow","kind":"allow_once"}]}}"#,
    )?)?;
    assert_eq!(
        reply
            .get("result")
            .and_then(|result| result.get("outcome"))
            .and_then(|outcome| outcome.get("optionId"))
            .and_then(Value::as_str),
        Some("approve-exact")
    );
    assert_eq!(
        reply.get("id").and_then(Value::as_str),
        Some("permission-7")
    );
    Ok(())
}

#[test]
fn deny_selects_offered_reject_once_option() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::PermissionDeny);
    let _ = create_session(&mut machine)?;
    let reply = one_message(machine.accept_raw(
        r#"{"jsonrpc":"2.0","id":77,"method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"deny-exact","name":"Deny","kind":"reject_once"}]}}"#,
    )?)?;
    assert_eq!(
        reply
            .get("result")
            .and_then(|result| result.get("outcome"))
            .and_then(|outcome| outcome.get("optionId"))
            .and_then(Value::as_str),
        Some("deny-exact")
    );
    Ok(())
}

#[test]
fn read_and_edit_permissions_accept_only_the_scenario_target(
) -> Result<(), Box<dyn std::error::Error>> {
    for (scenario, kind, filename) in [
        (AcpScenario::ToolCall, "read", "notes.txt"),
        (AcpScenario::FileChange, "edit", "editable.txt"),
    ] {
        let path = sandbox_file_permission_path(filename)?;
        let mut request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": format!("permission-{kind}"),
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1",
                "toolCall": {
                    "toolCallId": format!("tool-{kind}"),
                    "title": format!("{kind} fixture file"),
                    "kind": kind,
                    "locations": [{"path": path}],
                    "rawInput": {"path": path}
                },
                "options": [{
                    "optionId": "allow-once",
                    "name": "Allow once",
                    "kind": "allow_once"
                }]
            }
        });
        let tool_call = request
            .pointer_mut("/params/toolCall")
            .and_then(Value::as_object_mut)
            .ok_or("permission tool call was not an object")?;
        if kind == "read" {
            tool_call.remove("locations");
        } else {
            tool_call.remove("rawInput");
        }
        let request = request.to_string();
        let mut machine = AcpStateMachine::new(sandbox(), scenario);
        let _ = create_session(&mut machine)?;

        let reply = one_message(machine.accept_raw(&request)?)?;

        assert_eq!(
            reply
                .pointer("/result/outcome/optionId")
                .and_then(Value::as_str),
            Some("allow-once")
        );
    }
    Ok(())
}

#[test]
fn read_permission_requires_path_evidence_and_validates_all_present_evidence(
) -> Result<(), Box<dyn std::error::Error>> {
    let notes = sandbox_file_permission_path("notes.txt")?;
    let editable = sandbox_file_permission_path("editable.txt")?;
    for tool_call in [
        serde_json::json!({
            "toolCallId": "tool-read",
            "title": "Missing target",
            "kind": "read",
            "locations": [],
            "rawInput": {"offset": 0}
        }),
        serde_json::json!({
            "toolCallId": "tool-read",
            "title": "Conflicting target",
            "kind": "read",
            "locations": [{"path": editable}],
            "rawInput": {"path": notes}
        }),
    ] {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "permission-read",
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1",
                "toolCall": tool_call,
                "options": [{
                    "optionId": "allow-once",
                    "name": "Allow once",
                    "kind": "allow_once"
                }]
            }
        })
        .to_string();
        let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
        let _ = create_session(&mut machine)?;

        assert!(matches!(
            machine.accept_raw(&request),
            Err(AcpError::UnsafePermissionScope)
        ));
    }
    Ok(())
}

#[test]
fn read_permission_for_a_different_sandbox_file_is_rejected(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = sandbox_file_permission_path("editable.txt")?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "permission-read",
        "method": "session/request_permission",
        "params": {
            "sessionId": "session-1",
            "toolCall": {
                "toolCallId": "tool-read",
                "title": "Read unrelated file",
                "kind": "read",
                "locations": [{"path": path}],
                "rawInput": {"path": path}
            },
            "options": [{
                "optionId": "allow-once",
                "name": "Allow once",
                "kind": "allow_once"
            }]
        }
    })
    .to_string();
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut machine)?;

    assert!(matches!(
        machine.accept_raw(&request),
        Err(AcpError::UnsafePermissionScope)
    ));
    Ok(())
}

#[test]
fn permission_scenario_fails_closed_when_no_request_arrives(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::PermissionApprove);
    let _ = create_session(&mut machine)?;
    let step =
        machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::PermissionRequestNotObserved,
            ..
        })
    ));
    Ok(())
}

#[test]
fn tool_call_requires_matching_start_update_and_successful_end(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut incomplete = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut incomplete)?;
    let _ = incomplete.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Read notes","kind":"read","status":"in_progress"}}}"#,
    )?;
    let step =
        incomplete.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            ..
        })
    ));

    let mut mismatched = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut mismatched)?;
    let _ = mismatched.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Read notes","kind":"read","status":"in_progress"}}}"#,
    )?;
    let _ = mismatched.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-2","status":"completed"}}}"#,
    )?;
    let step =
        mismatched.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            ..
        })
    ));

    let mut complete = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut complete)?;
    let notes = sandbox_file_permission_path("notes.txt")?;
    let start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tool-1",
                "title": "Read notes",
                "kind": "read",
                "status": "in_progress",
                "locations": [{"path": notes}],
                "rawInput": {"path": notes}
            }
        }
    })
    .to_string();
    let _ = complete.accept_raw(&start)?;
    let _ = complete.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed","rawOutput":"sandbox contents"}}}"#,
    )?;
    let step =
        complete.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Completed {
            observations,
            ..
        }) if observations.completed_tool_lifecycle
    ));
    Ok(())
}

#[test]
fn orphan_tool_update_permanently_taints_an_otherwise_valid_lifecycle(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut machine)?;
    let _ = machine.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"in_progress","rawOutput":"orphan update"}}}"#,
    )?;

    let notes = sandbox_file_permission_path("notes.txt")?;
    let start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tool-1",
                "title": "Read notes",
                "kind": "read",
                "status": "in_progress",
                "locations": [{"path": notes}],
                "rawInput": {"path": notes}
            }
        }
    })
    .to_string();
    let _ = machine.accept_raw(&start)?;
    let _ = machine.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed","rawOutput":"sandbox contents"}}}"#,
    )?;
    let step =
        machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            observations,
            ..
        }) if !observations.completed_tool_lifecycle
    ));
    Ok(())
}

#[test]
fn a_second_valid_tool_lifecycle_invalidates_the_scenario() -> Result<(), Box<dyn std::error::Error>>
{
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut machine)?;
    let notes = sandbox_file_permission_path("notes.txt")?;

    for tool_call_id in ["tool-1", "tool-2"] {
        let start = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": tool_call_id,
                    "title": "Read notes",
                    "kind": "read",
                    "status": "in_progress",
                    "locations": [{"path": notes}],
                    "rawInput": {"path": notes}
                }
            }
        })
        .to_string();
        let _ = machine.accept_raw(&start)?;
        let terminal = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": tool_call_id,
                    "status": "completed",
                    "rawOutput": "sandbox contents"
                }
            }
        })
        .to_string();
        let _ = machine.accept_raw(&terminal)?;
    }

    let step =
        machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            observations,
            ..
        }) if !observations.completed_tool_lifecycle
    ));
    Ok(())
}

#[test]
fn orphan_tool_updates_cannot_hide_in_simple_or_cancel_scenarios(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut simple = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    let _ = create_session(&mut simple)?;
    let _ = simple.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"orphan","status":"in_progress","rawOutput":"unexpected"}}}"#,
    )?;
    let simple_step =
        simple.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        simple_step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            ..
        })
    ));

    let mut cancel = AcpStateMachine::new(sandbox(), AcpScenario::Cancel);
    let _ = create_session(&mut cancel)?;
    let cancel_step = cancel.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"orphan","status":"in_progress","rawOutput":"unexpected"}}}"#,
    )?;
    assert!(matches!(cancel_step, MachineStep::Send(_)));
    let cancel_outcome =
        cancel.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}"#)?;
    assert!(matches!(
        cancel_outcome,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            ..
        })
    ));
    Ok(())
}

#[test]
fn conflicting_terminal_updates_permanently_invalidate_the_lifecycle(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut machine)?;
    let notes = sandbox_file_permission_path("notes.txt")?;
    let start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "tool-1",
                "title": "Read notes",
                "kind": "read",
                "status": "in_progress",
                "locations": [{"path": notes}],
                "rawInput": {"path": notes}
            }
        }
    })
    .to_string();
    let _ = machine.accept_raw(&start)?;
    let _ = machine.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"failed","rawOutput":"first terminal"}}}"#,
    )?;
    let _ = machine.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed","rawOutput":"conflicting terminal"}}}"#,
    )?;
    let step =
        machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            ..
        })
    ));
    Ok(())
}

#[test]
fn complete_tool_lifecycles_still_require_the_exact_scenario_scope(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut unscoped_read = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut unscoped_read)?;
    let _ = unscoped_read.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"read-unscoped","title":"Read without target","kind":"read","status":"in_progress"}}}"#,
    )?;
    let _ = unscoped_read.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"read-unscoped","status":"completed","rawOutput":"read completed"}}}"#,
    )?;
    let step = unscoped_read
        .accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            observations,
            ..
        }) if !observations.completed_tool_lifecycle
    ));

    let editable = sandbox_file_permission_path("editable.txt")?;
    let mut wrong_read = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
    let _ = create_session(&mut wrong_read)?;
    let wrong_read_start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "read-wrong",
                "title": "Read wrong file",
                "kind": "read",
                "status": "in_progress",
                "locations": [{"path": editable}],
                "rawInput": {"path": editable}
            }
        }
    })
    .to_string();
    let _ = wrong_read.accept_raw(&wrong_read_start)?;
    let _ = wrong_read.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"read-wrong","status":"completed","rawOutput":"read completed"}}}"#,
    )?;
    let step =
        wrong_read.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            observations,
            ..
        }) if !observations.completed_tool_lifecycle
    ));

    let mut exact_error = AcpStateMachine::new(sandbox(), AcpScenario::Error);
    let _ = create_session(&mut exact_error)?;
    let _ = exact_error.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"fail-exact","title":"Run forced failure","kind":"execute","status":"in_progress","rawInput":{"command":"cargo run -- fail"}}}}"#,
    )?;
    let _ = exact_error.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"fail-exact","status":"failed","rawOutput":"forced failure"}}}"#,
    )?;
    let step =
        exact_error.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Completed {
            observations,
            ..
        }) if observations.failed_tool_lifecycle
    ));

    let mut wrong_error = AcpStateMachine::new(sandbox(), AcpScenario::Error);
    let _ = create_session(&mut wrong_error)?;
    let _ = wrong_error.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"fail-wrong","title":"Run wrong command","kind":"execute","status":"in_progress","rawInput":{"command":"cargo run -- wait"}}}}"#,
    )?;
    let _ = wrong_error.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"fail-wrong","status":"failed","rawOutput":"failed"}}}"#,
    )?;
    let step =
        wrong_error.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            observations,
            ..
        }) if !observations.failed_tool_lifecycle
    ));
    Ok(())
}

#[test]
fn pure_terminal_status_is_not_a_meaningful_tool_update() -> Result<(), Box<dyn std::error::Error>>
{
    for (scenario, tool_kind, terminal_status, start_content) in [
        (AcpScenario::ToolCall, "read", "completed", ""),
        (AcpScenario::Error, "execute", "failed", ""),
        (
            AcpScenario::FileChange,
            "edit",
            "completed",
            r#","content":[{"type":"diff","path":"/fixture/tests/fixtures/sandbox/editable.txt","oldText":"ORIGINAL","newText":"CHANGED"}]"#,
        ),
    ] {
        let mut machine = AcpStateMachine::new(sandbox(), scenario);
        let _ = create_session(&mut machine)?;
        let start = format!(
            r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"session-1","update":{{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Probe","kind":"{tool_kind}","status":"in_progress"{start_content}}}}}}}"#
        );
        let _ = machine.accept_raw(&start)?;
        let terminal = format!(
            r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"session-1","update":{{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"{terminal_status}"}}}}}}"#
        );
        let _ = machine.accept_raw(&terminal)?;
        let step =
            machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
        assert!(matches!(
            step,
            MachineStep::Complete(ScenarioOutcome::Unsupported { .. })
        ));
    }
    Ok(())
}

#[test]
fn file_change_requires_changed_diff_and_completed_tool() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::tempdir()?;
    let sandbox = temporary.path().join("sandbox");
    std::fs::create_dir(&sandbox)?;
    std::fs::write(sandbox.join("editable.txt"), b"ORIGINAL\n")?;
    std::fs::write(sandbox.join("notes.txt"), b"notes\n")?;
    let editable = platform::permission_path_pattern(&sandbox.join("editable.txt"))?;

    let mut unchanged = AcpStateMachine::new(sandbox.clone(), AcpScenario::FileChange);
    let _ = create_session(&mut unchanged)?;
    let unchanged_start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "edit-1",
                "title": "Edit file",
                "kind": "edit",
                "status": "in_progress",
                "locations": [{"path": editable}],
                "content": [{
                    "type": "diff",
                    "path": editable,
                    "oldText": "ORIGINAL\n",
                    "newText": "ORIGINAL\n"
                }]
            }
        }
    })
    .to_string();
    let _ = unchanged.accept_raw(&unchanged_start)?;
    let _ = unchanged.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"edit-1","status":"completed","rawOutput":"unchanged"}}}"#,
    )?;
    let step =
        unchanged.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::FileDiffNotObserved,
            ..
        })
    ));

    let mut reported_only = AcpStateMachine::new(sandbox.clone(), AcpScenario::FileChange);
    let _ = create_session(&mut reported_only)?;
    let reported_start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "edit-reported",
                "title": "Report edit without changing bytes",
                "kind": "edit",
                "status": "in_progress",
                "locations": [{"path": editable}],
                "content": [{
                    "type": "diff",
                    "path": editable,
                    "oldText": "ORIGINAL\n",
                    "newText": "CHANGED\n"
                }]
            }
        }
    })
    .to_string();
    let _ = reported_only.accept_raw(&reported_start)?;
    let _ = reported_only.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"edit-reported","status":"completed","rawOutput":"reported only"}}}"#,
    )?;
    let step = reported_only
        .accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::FileDiffNotObserved,
            observations,
            ..
        }) if observations.nonempty_file_diff && !observations.file_changed_on_disk
    ));

    let mut changed = AcpStateMachine::new(sandbox.clone(), AcpScenario::FileChange);
    let _ = create_session(&mut changed)?;
    let changed_start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "edit-1",
                "title": "Edit file",
                "kind": "edit",
                "status": "in_progress",
                "locations": [{"path": editable}],
                "content": [{
                    "type": "diff",
                    "path": editable,
                    "oldText": "ORIGINAL\n",
                    "newText": "CHANGED\n"
                }]
            }
        }
    })
    .to_string();
    let _ = changed.accept_raw(&changed_start)?;
    std::fs::write(sandbox.join("editable.txt"), b"CHANGED\n")?;
    let changed_terminal = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "edit-1",
                "status": "completed",
                "content": [{
                    "type": "diff",
                    "path": editable,
                    "oldText": "ORIGINAL\n",
                    "newText": "CHANGED\n"
                }]
            }
        }
    })
    .to_string();
    let _ = changed.accept_raw(&changed_terminal)?;
    let step =
        changed.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Completed {
            observations,
            ..
        }) if observations.nonempty_file_diff && observations.file_changed_on_disk
    ));

    std::fs::write(sandbox.join("editable.txt"), b"ORIGINAL\n")?;
    let mut wrong_target = AcpStateMachine::new(sandbox.clone(), AcpScenario::FileChange);
    let _ = create_session(&mut wrong_target)?;
    let notes = platform::permission_path_pattern(&sandbox.join("notes.txt"))?;
    let wrong_start = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "edit-wrong",
                "title": "Edit wrong file",
                "kind": "edit",
                "status": "in_progress",
                "locations": [{"path": notes}],
                "content": [{
                    "type": "diff",
                    "path": notes,
                    "oldText": "notes\n",
                    "newText": "changed notes\n"
                }]
            }
        }
    })
    .to_string();
    let _ = wrong_target.accept_raw(&wrong_start)?;
    std::fs::write(sandbox.join("editable.txt"), b"CHANGED\n")?;
    let _ = wrong_target.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"edit-wrong","status":"completed","rawOutput":"completed"}}}"#,
    )?;
    let step = wrong_target
        .accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ToolCallLifecycleIncomplete,
            observations,
            ..
        }) if !observations.nonempty_file_diff
    ));
    Ok(())
}

#[test]
fn approved_permission_requires_same_tool_to_complete_then_end_turn(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut incomplete = AcpStateMachine::new(sandbox(), AcpScenario::PermissionApprove);
    let _ = create_session(&mut incomplete)?;
    let _ = incomplete.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Run cargo","kind":"execute","status":"pending","rawInput":{"command":"cargo run --"}}}}"#,
    )?;
    let _ = incomplete.accept_raw(
        r#"{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]}}"#,
    )?;
    let step =
        incomplete.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::PermissionApprovalDidNotComplete,
            ..
        })
    ));

    let mut continued = AcpStateMachine::new(sandbox(), AcpScenario::PermissionApprove);
    let _ = create_session(&mut continued)?;
    let _ = continued.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Run cargo","kind":"execute","status":"pending","rawInput":{"command":"cargo run --"}}}}"#,
    )?;
    let _ = continued.accept_raw(
        r#"{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]}}"#,
    )?;
    let _ = continued.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed"}}}"#,
    )?;
    let step =
        continued.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Completed {
            observations,
            ..
        }) if observations.approved_permission_flow
    ));

    let mut nonterminal = AcpStateMachine::new(sandbox(), AcpScenario::PermissionApprove);
    let _ = create_session(&mut nonterminal)?;
    let _ = nonterminal.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Run cargo","kind":"execute","status":"pending","rawInput":{"command":"cargo run --"}}}}"#,
    )?;
    let _ = nonterminal.accept_raw(
        r#"{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]}}"#,
    )?;
    let _ = nonterminal.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed"}}}"#,
    )?;
    let step = nonterminal
        .accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"max_tokens"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::UnexpectedTerminalStopReason { .. },
            ..
        })
    ));
    Ok(())
}

#[test]
fn denied_permission_requires_same_tool_failure_and_terminal_stop(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut wrong_status = AcpStateMachine::new(sandbox(), AcpScenario::PermissionDeny);
    let _ = create_session(&mut wrong_status)?;
    let _ = wrong_status.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Run cargo","kind":"execute","status":"pending","rawInput":{"command":"cargo run --"}}}}"#,
    )?;
    let _ = wrong_status.accept_raw(
        r#"{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"deny","name":"Deny","kind":"reject_once"}]}}"#,
    )?;
    let _ = wrong_status.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed"}}}"#,
    )?;
    let step = wrong_status
        .accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::PermissionDenialDidNotReachFailure,
            ..
        })
    ));

    let mut denied = AcpStateMachine::new(sandbox(), AcpScenario::PermissionDeny);
    let _ = create_session(&mut denied)?;
    let _ = denied.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Run cargo","kind":"execute","status":"pending","rawInput":{"command":"cargo run --"}}}}"#,
    )?;
    let _ = denied.accept_raw(
        r#"{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"deny","name":"Deny","kind":"reject_once"}]}}"#,
    )?;
    let _ = denied.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"failed"}}}"#,
    )?;
    let step =
        denied.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Completed {
            observations,
            ..
        }) if observations.denied_permission_flow
    ));

    let mut nonterminal = AcpStateMachine::new(sandbox(), AcpScenario::PermissionDeny);
    let _ = create_session(&mut nonterminal)?;
    let _ = nonterminal.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Run cargo","kind":"execute","status":"pending","rawInput":{"command":"cargo run --"}}}}"#,
    )?;
    let _ = nonterminal.accept_raw(
        r#"{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"deny","name":"Deny","kind":"reject_once"}]}}"#,
    )?;
    let _ = nonterminal.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"failed"}}}"#,
    )?;
    let step = nonterminal
        .accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"max_tokens"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::UnexpectedTerminalStopReason { .. },
            ..
        })
    ));
    Ok(())
}

#[test]
fn cancellation_is_sent_after_real_update_and_must_be_confirmed(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::Cancel);
    let _ = create_session(&mut machine)?;
    let cancel = one_message(machine.accept_raw(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"working"}}}}"#,
    )?)?;
    assert_eq!(
        cancel.get("method").and_then(Value::as_str),
        Some("session/cancel")
    );
    let step =
        machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Completed {
            stop_reason,
            ..
        }) if stop_reason == "cancelled"
    ));
    Ok(())
}

#[test]
fn session_load_checks_capabilities_before_sending_request(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::for_session_load(sandbox(), "seed-session".to_owned());
    let _ = one_message(machine.start()?)?;
    let step = machine.accept_raw(
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"sessionCapabilities":{}}}}"#,
    )?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::SessionListNotAdvertised,
            ..
        })
    ));
    Ok(())
}

#[test]
fn session_load_lists_then_loads_matching_sandbox_session() -> Result<(), Box<dyn std::error::Error>>
{
    let mut machine = AcpStateMachine::for_session_load(sandbox(), "old-session".to_owned());
    let list = initialize(
        &mut machine,
        serde_json::json!({
            "loadSession": true,
            "sessionCapabilities": {
                "list": {},
                "resume": {}
            }
        }),
    )?;
    assert_eq!(
        list.get("method").and_then(Value::as_str),
        Some("session/list")
    );
    let sandbox = sandbox();
    let cwd = serde_json::to_string(&sandbox.to_string_lossy())?;
    let additional = serde_json::to_string(&sandbox.join("src").to_string_lossy())?;
    let load = one_message(machine.accept_raw(&format!(
        r#"{{"jsonrpc":"2.0","id":2,"result":{{"sessions":[{{"sessionId":"old-session","cwd":{cwd},"additionalDirectories":[{additional}]}}]}}}}"#
    ))?)?;
    assert_eq!(
        load.get("method").and_then(Value::as_str),
        Some("session/load")
    );
    assert_eq!(
        load.get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str),
        Some("old-session")
    );
    let step = machine.accept_raw(r#"{"jsonrpc":"2.0","id":3,"result":{"configOptions":[]}}"#)?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::SessionLoaded {
            session_id,
            ..
        }) if session_id == "old-session"
    ));
    Ok(())
}

#[test]
fn session_load_finishes_pagination_before_loading() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::for_session_load(sandbox(), "second-session".to_owned());
    let _ = initialize(
        &mut machine,
        serde_json::json!({
            "loadSession": true,
            "sessionCapabilities": {"list": {}, "resume": {}}
        }),
    )?;
    let cwd = serde_json::to_string(&sandbox().to_string_lossy())?;

    let next_page = one_message(machine.accept_raw(&format!(
        r#"{{"jsonrpc":"2.0","id":2,"result":{{"sessions":[{{"sessionId":"first-session","cwd":{cwd}}}],"nextCursor":"page-2"}}}}"#
    ))?)?;
    assert_eq!(
        next_page.get("method").and_then(Value::as_str),
        Some("session/list")
    );
    assert_eq!(
        next_page.pointer("/params/cursor").and_then(Value::as_str),
        Some("page-2")
    );

    let load = one_message(machine.accept_raw(&format!(
        r#"{{"jsonrpc":"2.0","id":3,"result":{{"sessions":[{{"sessionId":"second-session","cwd":{cwd}}}]}}}}"#
    ))?)?;
    assert_eq!(
        load.get("method").and_then(Value::as_str),
        Some("session/load")
    );
    assert_eq!(
        load.pointer("/params/sessionId").and_then(Value::as_str),
        Some("second-session")
    );
    Ok(())
}

#[test]
fn session_load_without_explicit_id_fails_before_sending_any_wire_message(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::SessionLoad);

    let step = machine.start()?;

    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::SessionIdRequired,
            ..
        })
    ));
    Ok(())
}

#[test]
fn session_load_does_not_fall_back_to_an_unrequested_sandbox_session(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::for_session_load(sandbox(), "requested-session".to_owned());
    let _ = initialize(
        &mut machine,
        serde_json::json!({
            "loadSession": true,
            "sessionCapabilities": {"list": {}, "resume": {}}
        }),
    )?;
    let cwd = serde_json::to_string(&sandbox().to_string_lossy())?;

    let step = machine.accept_raw(&format!(
        r#"{{"jsonrpc":"2.0","id":2,"result":{{"sessions":[{{"sessionId":"other-session","cwd":{cwd}}}]}}}}"#
    ))?;

    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::NoSessionForSandbox,
            ..
        })
    ));
    Ok(())
}

#[test]
fn unsafe_session_list_marker_is_never_written() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let sandbox = temporary.path().join("sandbox");
    let outside = temporary.path().join("real-project");
    std::fs::create_dir_all(&sandbox)?;
    std::fs::create_dir_all(&outside)?;

    for cwd in [
        Some(outside.to_string_lossy().into_owned()),
        None,
        Some(sandbox.join("missing").to_string_lossy().into_owned()),
    ] {
        let mut machine =
            AcpStateMachine::for_session_load(sandbox.clone(), "outside-session".to_owned());
        let _ = initialize(
            &mut machine,
            serde_json::json!({
                "loadSession": true,
                "sessionCapabilities": {"list": {}, "resume": {}}
            }),
        )?;
        let session = match cwd {
            Some(cwd) => serde_json::json!({
                "sessionId": "outside-session",
                "cwd": cwd,
                "marker": "SESSION_LIST_SENSITIVE_MARKER"
            }),
            None => serde_json::json!({
                "sessionId": "missing-cwd",
                "marker": "SESSION_LIST_SENSITIVE_MARKER"
            }),
        };
        let raw = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"sessions": [session]}
        }))?;
        let mut fixture = fixture::FixtureSink::new(
            Vec::new(),
            kaleido_recorder::redact::Redactor::from_pairs([]),
        );

        let result = acp::accept_recorded_message(&mut machine, &raw, &mut fixture);
        assert!(matches!(result, Err(AcpError::UnsafeSessionList)));
        let bytes = fixture.into_inner();
        assert!(bytes.is_empty());
        assert!(!bytes
            .windows(b"SESSION_LIST_SENSITIVE_MARKER".len())
            .any(|window| window == b"SESSION_LIST_SENSITIVE_MARKER"));
    }
    Ok(())
}

#[test]
fn unsafe_additional_directory_marker_is_never_written() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let sandbox = temporary.path().join("sandbox");
    let outside = temporary.path().join("real-project");
    std::fs::create_dir_all(&sandbox)?;
    std::fs::create_dir_all(&outside)?;

    for additional_directories in [
        serde_json::json!([outside]),
        serde_json::json!([sandbox.join("missing")]),
        serde_json::json!(["relative"]),
        serde_json::json!([7]),
        serde_json::json!(null),
        serde_json::json!({"root": sandbox}),
    ] {
        let mut machine =
            AcpStateMachine::for_session_load(sandbox.clone(), "seed-session".to_owned());
        let _ = initialize(
            &mut machine,
            serde_json::json!({
                "loadSession": true,
                "sessionCapabilities": {"list": {}, "resume": {}}
            }),
        )?;
        let raw = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "sessions": [{
                    "sessionId": "seed-session",
                    "cwd": sandbox,
                    "additionalDirectories": additional_directories,
                    "marker": "SESSION_LIST_SENSITIVE_MARKER"
                }]
            }
        }))?;
        let mut fixture = fixture::FixtureSink::new(
            Vec::new(),
            kaleido_recorder::redact::Redactor::from_pairs([]),
        );

        let result = acp::accept_recorded_message(&mut machine, &raw, &mut fixture);
        assert!(matches!(result, Err(AcpError::UnsafeSessionList)));
        let bytes = fixture.into_inner();
        assert!(bytes.is_empty());
        assert!(!bytes
            .windows(b"SESSION_LIST_SENSITIVE_MARKER".len())
            .any(|window| window == b"SESSION_LIST_SENSITIVE_MARKER"));
    }
    Ok(())
}

#[test]
fn unsafe_later_session_list_page_is_not_written_or_loaded(
) -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let sandbox = temporary.path().join("sandbox");
    let outside = temporary.path().join("real-project");
    std::fs::create_dir_all(&sandbox)?;
    std::fs::create_dir_all(&outside)?;
    let mut machine = AcpStateMachine::for_session_load(sandbox.clone(), "safe-session".to_owned());
    let _ = initialize(
        &mut machine,
        serde_json::json!({
            "loadSession": true,
            "sessionCapabilities": {"list": {}, "resume": {}}
        }),
    )?;
    let mut fixture = fixture::FixtureSink::new(
        Vec::new(),
        kaleido_recorder::redact::Redactor::from_pairs([]),
    );
    let safe_page = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "sessions": [{"sessionId": "safe-session", "cwd": sandbox}],
            "nextCursor": "page-2"
        }
    }))?;
    let next = acp::accept_recorded_message(&mut machine, &safe_page, &mut fixture)?;
    assert_eq!(
        one_message(next)?.get("method").and_then(Value::as_str),
        Some("session/list")
    );
    let unsafe_page = serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "result": {
            "sessions": [{
                "sessionId": "outside-session",
                "cwd": sandbox,
                "additionalDirectories": [outside],
                "marker": "SESSION_LIST_SENSITIVE_MARKER"
            }]
        }
    }))?;

    let result = acp::accept_recorded_message(&mut machine, &unsafe_page, &mut fixture);
    assert!(matches!(result, Err(AcpError::UnsafeSessionList)));
    let bytes = fixture.into_inner();
    assert!(!bytes
        .windows(b"SESSION_LIST_SENSITIVE_MARKER".len())
        .any(|window| window == b"SESSION_LIST_SENSITIVE_MARKER"));
    Ok(())
}

#[test]
fn elicitation_never_synthesizes_a_wire_message() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::Elicitation);
    let step = machine.start()?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::Unsupported {
            reason: UnsupportedReason::ElicitationAbsentFromPinnedSchema,
            ..
        })
    ));
    Ok(())
}

#[test]
fn authentication_required_is_reported_not_mislabeled_as_missing_install(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    let _ = one_message(machine.start()?)?;
    let _ = one_message(machine.accept_raw(
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"login","name":"Login"}],"agentCapabilities":{}}}"#,
    )?)?;
    let step = machine.accept_raw(
        r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Authentication required"}}"#,
    )?;
    assert!(matches!(
        step,
        MachineStep::Complete(ScenarioOutcome::AuthenticationRequired {
            stage: AuthenticationStage::NewSession,
            advertised_methods: 1,
            ..
        })
    ));
    Ok(())
}

#[test]
fn response_id_mismatch_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    let _ = one_message(machine.start()?)?;
    let error = machine.accept_raw(
        r#"{"jsonrpc":"2.0","id":99,"result":{"protocolVersion":1,"agentCapabilities":{}}}"#,
    );
    assert!(matches!(error, Err(AcpError::ResponseId { expected: 1 })));
    Ok(())
}

#[test]
fn permission_for_another_session_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::PermissionApprove);
    let _ = create_session(&mut machine)?;
    let error = machine.accept_raw(
        r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"sessionId":"other-session","toolCall":{"toolCallId":"tool-1","rawInput":{"command":"cargo run --"}},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]}}"#,
    );
    assert!(matches!(error, Err(AcpError::SessionIdMismatch)));
    Ok(())
}

#[test]
fn unsafe_permission_request_is_rejected_before_fixture_write(
) -> Result<(), Box<dyn std::error::Error>> {
    const OUTSIDE_MARKER: &str = "PRIVATE OUTSIDE PERMISSION";
    let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::PermissionDeny);
    let _ = create_session(&mut machine)?;
    let raw = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{{"sessionId":"session-1","toolCall":{{"toolCallId":"tool-1","rawInput":{{"command":"cargo run -- fail","cwd":"..\\{OUTSIDE_MARKER}"}}}},"options":[{{"optionId":"deny","name":"Deny","kind":"reject_once"}}]}}}}"#
    );
    let mut fixture = fixture::FixtureSink::new(
        Vec::new(),
        kaleido_recorder::redact::Redactor::from_pairs([]),
    );

    let result = acp::accept_recorded_message(&mut machine, &raw, &mut fixture);

    assert!(matches!(result, Err(AcpError::UnsafePermissionScope)));
    let output = String::from_utf8(fixture.into_inner())?;
    assert!(output.is_empty());
    assert!(!output.contains(OUTSIDE_MARKER));
    Ok(())
}

#[test]
fn unsafe_read_permission_is_rejected_before_fixture_write(
) -> Result<(), Box<dyn std::error::Error>> {
    const OUTSIDE_MARKER: &str = "PRIVATE OUTSIDE READ";
    for path in [
        format!("..\\{OUTSIDE_MARKER}.txt"),
        sandbox()
            .parent()
            .ok_or("sandbox did not have a parent")?
            .join(format!("{OUTSIDE_MARKER}.txt"))
            .to_string_lossy()
            .into_owned(),
    ] {
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "permission-read",
            "method": "session/request_permission",
            "params": {
                "sessionId": "session-1",
                "toolCall": {
                    "toolCallId": "tool-read",
                    "title": "Read outside",
                    "kind": "read",
                    "locations": [{"path": path}],
                    "rawInput": {"path": path}
                },
                "options": [{
                    "optionId": "allow-once",
                    "name": "Allow once",
                    "kind": "allow_once"
                }]
            }
        })
        .to_string();
        let mut machine = AcpStateMachine::new(sandbox(), AcpScenario::ToolCall);
        let _ = create_session(&mut machine)?;
        let mut fixture = fixture::FixtureSink::new(
            Vec::new(),
            kaleido_recorder::redact::Redactor::from_pairs([]),
        );

        let result = acp::accept_recorded_message(&mut machine, &raw, &mut fixture);

        assert!(matches!(result, Err(AcpError::UnsafePermissionScope)));
        let output = String::from_utf8(fixture.into_inner())?;
        assert!(output.is_empty());
        assert!(!output.contains(OUTSIDE_MARKER));
    }
    Ok(())
}

#[test]
fn public_runner_surface_uses_only_the_preinstalled_pinned_launcher() {
    assert!(acp::pinned_launcher_arguments().is_empty());
    assert_eq!(
        CLAUDE_ACP_PACKAGE,
        format!("{CLAUDE_ACP_PACKAGE_NAME}@{CLAUDE_ACP_VERSION}")
    );
    assert_eq!(
        CLAUDE_ACP_INSTALL_COMMAND,
        "npm install --global @agentclientprotocol/claude-agent-acp@0.63.0"
    );
    assert!(!CLAUDE_ACP_INSTALL_COMMAND.contains("--yes"));
    assert!(!CLAUDE_ACP_INSTALL_COMMAND.contains("npx"));
    assert!(acp::is_pinned_launcher_version("0.63.0\n"));
    assert!(!acp::is_pinned_launcher_version("0.62.0\n"));
    let _credential_names = acp::explicit_credential_variables_present();
    let scenarios = [
        AcpScenario::SimpleTurn,
        AcpScenario::ToolCall,
        AcpScenario::PermissionApprove,
        AcpScenario::PermissionDeny,
        AcpScenario::FileChange,
        AcpScenario::Cancel,
        AcpScenario::Error,
        AcpScenario::SessionLoad,
        AcpScenario::Elicitation,
    ];
    assert_eq!(scenarios.len(), 9);

    let machine = AcpStateMachine::new(sandbox(), AcpScenario::SimpleTurn);
    assert!(machine.capabilities().is_none());

    type Sink = fixture::FixtureSink<Vec<u8>>;
    type Runner = fn(
        &platform::ResolvedExecutable,
        &[OsString],
        &Path,
        AcpScenario,
        &mut Sink,
    ) -> Result<ScenarioOutcome, AcpError>;
    let _runner: Runner = acp::record_scenario::<Vec<u8>>;
    type SessionLoadRunner = fn(
        &platform::ResolvedExecutable,
        &[OsString],
        &Path,
        String,
        &mut Sink,
        std::time::Duration,
    ) -> Result<ScenarioOutcome, AcpError>;
    let _session_load_runner: SessionLoadRunner = acp::record_session_load_with_timeout::<Vec<u8>>;
}

#[test]
fn runner_rejects_every_directory_except_the_repository_fixture_sandbox(
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(matches!(
        acp::validate_fixture_sandbox(manifest),
        Err(AcpError::InvalidSandbox)
    ));

    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or("workspace root is missing")?;
    let exact = workspace.join("tests").join("fixtures").join("sandbox");
    let validated = acp::validate_fixture_sandbox(&exact)?;
    assert_eq!(validated, exact.canonicalize()?);
    Ok(())
}
