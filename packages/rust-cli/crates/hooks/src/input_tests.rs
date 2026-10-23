//! 输入 JSON 的字段与键序（spec §3.1）。

use rust_cli_kernel_api::hooks::{HookContext, HookEventName};
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::permission::PermissionMode;
use serde_json::json;

use super::*;
use crate::test_support::workspace;

#[test]
fn base_fields_come_first_in_order() {
    let event = HookEvent {
        name: HookEventName::PreToolUse,
        session: SessionId::new("sess_1"),
        workspace: workspace(),
        payload: json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "toolu_1"}),
        context: HookContext {
            mode: Some(PermissionMode::AcceptEdits),
            agent: Some(AgentId::new("agent_1")),
            agent_type: Some("Explore".to_owned()),
            ..HookContext::default()
        },
    };
    assert_eq!(
        build(&event, "/Users/dev/project/sub").to_string(),
        r#"{"session_id":"sess_1","transcript_path":"","cwd":"/Users/dev/project/sub","permission_mode":"acceptEdits","agent_id":"agent_1","agent_type":"Explore","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_1"}"#
    );
}

#[test]
fn absent_context_fields_are_omitted() {
    let event = HookEvent {
        name: HookEventName::Notification,
        session: SessionId::new("sess_1"),
        workspace: workspace(),
        payload: json!({"message": "m", "notification_type": "idle_prompt"}),
        context: HookContext::default(),
    };
    assert_eq!(
        build(&event, "/w").to_string(),
        r#"{"session_id":"sess_1","transcript_path":"","cwd":"/w","hook_event_name":"Notification","message":"m","notification_type":"idle_prompt"}"#
    );
    assert_eq!(hook_name("PreToolUse", Some("Bash")), "PreToolUse:Bash");
    assert_eq!(hook_name("Stop", None), "Stop");
}
