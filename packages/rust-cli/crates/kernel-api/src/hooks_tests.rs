//! hook 类型的序列化往返（docs/specs/rust-cli/README.md §6 规则 8）与事件名。

use serde_json::json;

use super::*;
use crate::ids::{EnvironmentId, MessageId};
use crate::message::AttachmentMessage;

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    }
}

#[test]
fn event_names_round_trip_all_33() {
    assert_eq!(HookEventName::ALL.len(), 33);
    for event in HookEventName::ALL {
        assert_eq!(HookEventName::parse(event.as_str()), Some(*event));
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!(event.as_str()),
            "serde 名与 as_str 相同"
        );
    }
    assert_eq!(HookEventName::parse("Unknown"), None);
}

#[test]
fn hook_event_round_trips_and_old_payloads_get_defaults() {
    let event = HookEvent {
        name: HookEventName::Stop,
        session: SessionId::new("sess_1"),
        workspace: workspace(),
        payload: json!({"stop_hook_active": false}),
        context: HookContext {
            mode: Some(PermissionMode::AcceptEdits),
            agent: Some(AgentId::new("agent_1")),
            agent_type: Some("Explore".to_owned()),
            cwd: Some(EnvPath::new("/w/sub")),
            run: Some(RunId::new("run_1")),
            selection: Some(crate::model::ModelSelection {
                provider: "anthropic".to_owned(),
                model: "claude-opus-5-5".to_owned(),
                reasoning: None,
                connection: None,
            }),
        },
    };
    let text = serde_json::to_string(&event).unwrap();
    assert_eq!(serde_json::from_str::<HookEvent>(&text).unwrap(), event);
    let old: HookEvent = serde_json::from_value(json!({
        "name": "PreToolUse",
        "session": "sess_1",
        "workspace": {"environment": "local", "root": "/w"},
        "payload": {}
    }))
    .unwrap();
    assert_eq!(old.context, HookContext::default());
}

#[test]
fn hook_outcome_round_trips_and_old_outcomes_get_defaults() {
    let outcome = HookOutcome {
        decision: Some(PermissionDecision::Deny {
            message: "no".to_owned(),
        }),
        blocking_error: Some("blocked".to_owned()),
        additional_context: vec!["ctx".to_owned()],
        updated_input: Some(json!({"command": "ls"})),
        prevent_continuation: true,
        stop_reason: Some("stop".to_owned()),
        messages: vec![Message::Attachment(AttachmentMessage {
            id: MessageId::new("msg_1"),
            source: "hook_additional_context".to_owned(),
            text: "<system-reminder>\nx\n</system-reminder>".to_owned(),
            data: json!({"hookName": "Stop"}),
        })],
        after_result: Vec::new(),
        notices: vec!["notice".to_owned()],
        updated_tool_output: Some(json!("replaced")),
        initial_user_message: Some("hi".to_owned()),
        session_title: Some("title".to_owned()),
        watch_paths: vec!["/w/a".to_owned()],
        custom_instructions: Some("focus".to_owned()),
        retry: true,
    };
    let text = serde_json::to_string(&outcome).unwrap();
    assert_eq!(serde_json::from_str::<HookOutcome>(&text).unwrap(), outcome);
    let old: HookOutcome = serde_json::from_value(json!({"prevent_continuation": false})).unwrap();
    assert_eq!(old, HookOutcome::default());
}

#[test]
fn http_hook_types_round_trip() {
    let request = HookHttpRequest {
        url: "https://hooks.example.com/pre".to_owned(),
        headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
        body: "{}".to_owned(),
        timeout_ms: 600_000,
    };
    let text = serde_json::to_string(&request).unwrap();
    assert_eq!(
        serde_json::from_str::<HookHttpRequest>(&text).unwrap(),
        request
    );
    for error in [
        HookHttpError::Cancelled,
        HookHttpError::TimedOut,
        HookHttpError::Failed {
            message: "refused".to_owned(),
        },
    ] {
        let text = serde_json::to_string(&error).unwrap();
        assert_eq!(serde_json::from_str::<HookHttpError>(&text).unwrap(), error);
    }
}

#[test]
fn absorbed_defaults_to_prompts_before_notices() {
    let items = Absorbed::ordered(
        vec![Steered::plain("s1".to_owned())],
        vec!["n1".to_owned(), "n2".to_owned()],
    );
    assert_eq!(
        items,
        vec![
            Absorbed::Prompt(Steered::plain("s1".to_owned())),
            Absorbed::Notice("n1".to_owned()),
            Absorbed::Notice("n2".to_owned()),
        ]
    );
}

#[test]
fn node_events_are_the_seven_node_names_in_node_order() {
    let names: Vec<&str> = NODE_EVENTS.iter().map(|event| event.as_str()).collect();
    assert_eq!(
        names,
        [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "PostToolUseFailure",
            "Stop",
        ]
    );
}
