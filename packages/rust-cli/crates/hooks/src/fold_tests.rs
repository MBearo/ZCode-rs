//! 工具类事件的合并（spec §4.3、§6）。

use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::testing::SequentialIds;
use serde_json::json;

use super::*;
use crate::result::Blocking;

fn fold<'a>(ids: &'a SequentialIds, event: &'a str, hook_name: &'a str) -> Fold<'a> {
    Fold {
        ids,
        event,
        hook_name,
    }
}

fn decided(behavior: Behavior, reason: Option<&str>, input: Option<Value>) -> HookResult {
    HookResult {
        behavior: Some(behavior),
        reason: reason.map(str::to_owned),
        updated_input: input,
        ..HookResult::default()
    }
}

fn blocking(error: &str) -> HookResult {
    HookResult::blocking(error.to_owned(), "g".to_owned())
}

fn text(message: &Message) -> String {
    match message {
        Message::Attachment(attachment) => attachment.text.clone(),
        other => format!("{other:?}"),
    }
}

#[test]
fn pre_tool_use_precedence_and_texts() {
    let ids = SequentialIds::default();
    let f = fold(&ids, "PreToolUse", "PreToolUse:Bash");
    // deny 优先；之后的 allow 不交出。
    let out = pre_tool_use(
        &f,
        &[
            decided(Behavior::Deny, None, None),
            decided(Behavior::Allow, None, None),
        ],
    );
    assert_eq!(
        out.decision,
        Some(PermissionDecision::Deny {
            message: "Hook PreToolUse:Bash denied this tool".to_owned()
        })
    );
    // ask 盖过 allow，带上 ask 的 updatedInput。
    let out = pre_tool_use(
        &f,
        &[
            decided(Behavior::Allow, None, Some(json!({"command": "a"}))),
            decided(Behavior::Ask, Some("check"), Some(json!({"command": "b"}))),
        ],
    );
    assert_eq!(
        out.decision,
        Some(PermissionDecision::Ask {
            message: "check".to_owned(),
            suggestions: Vec::new()
        })
    );
    assert_eq!(out.updated_input, Some(json!({"command": "b"})));
    let out = pre_tool_use(&f, &[decided(Behavior::Ask, None, None)]);
    assert_eq!(
        out.decision,
        Some(PermissionDecision::Ask {
            message: "Hook PreToolUse:Bash asked for confirmation for this tool".to_owned(),
            suggestions: Vec::new()
        })
    );
    // 退出码 2 的阻塞：PreToolUse 的拒绝文本。
    let out = pre_tool_use(&f, &[blocking("[g]: nope")]);
    assert_eq!(
        out.decision,
        Some(PermissionDecision::Deny {
            message: "PreToolUse:Bash hook error: [g]: nope".to_owned()
        })
    );
    // JSON deny：保留阻塞文本。
    let json_deny = HookResult {
        blocking: Some(Blocking {
            error: "no rm".to_owned(),
            command: "g".to_owned(),
        }),
        ..decided(Behavior::Deny, Some("no rm"), None)
    };
    let out = pre_tool_use(&f, &[json_deny]);
    assert_eq!(
        out.decision,
        Some(PermissionDecision::Deny {
            message: "PreToolUse:Bash hook error: no rm".to_owned()
        })
    );
}

#[test]
fn pre_tool_use_passthrough_context_and_stop() {
    let ids = SequentialIds::default();
    let f = fold(&ids, "PreToolUse", "PreToolUse:Bash");
    let out = pre_tool_use(
        &f,
        &[
            HookResult {
                updated_input: Some(json!({"command": "x"})),
                additional_context: Some("one".to_owned()),
                ..HookResult::default()
            },
            HookResult {
                additional_context: Some("two".to_owned()),
                prevent: true,
                ..HookResult::default()
            },
        ],
    );
    assert_eq!(out.decision, None);
    assert_eq!(out.updated_input, Some(json!({"command": "x"})));
    assert_eq!(out.messages.len(), 2, "每个 hook 一条附加上下文");
    assert!(text(&out.messages[1]).contains("PreToolUse:Bash hook additional context: two"));
    assert!(out.prevent_continuation);
    assert_eq!(out.after_result.len(), 1);
    assert!(
        text(&out.after_result[0])
            .contains("PreToolUse:Bash hook stopped continuation: Execution stopped by hook")
    );
}

#[test]
fn post_tool_use_stops_at_prevent() {
    let ids = SequentialIds::default();
    let f = fold(&ids, "PostToolUse", "PostToolUse:Bash");
    let out = post_tool_use(
        &f,
        &[
            HookResult {
                updated_tool_output: Some(json!("new")),
                ..blocking("[g]: bad")
            },
            HookResult {
                prevent: true,
                stop_reason: Some("enough".to_owned()),
                ..HookResult::default()
            },
            HookResult {
                additional_context: Some("dropped".to_owned()),
                ..HookResult::default()
            },
        ],
    );
    assert_eq!(out.updated_tool_output, Some(json!("new")));
    assert_eq!(out.messages.len(), 2);
    assert!(
        text(&out.messages[0])
            .contains("PostToolUse:Bash hook blocking error from command: \"g\": [g]: bad")
    );
    assert!(text(&out.messages[1]).contains("PostToolUse:Bash hook stopped continuation: enough"));
    assert!(out.prevent_continuation && out.additional_context.is_empty());
    let default_stop = post_tool_use(
        &f,
        &[HookResult {
            prevent: true,
            ..HookResult::default()
        }],
    );
    assert!(text(&default_stop.messages[0]).contains("Execution stopped by PostToolUse hook"));
}

#[test]
fn failure_and_permission_request() {
    let ids = SequentialIds::default();
    let f = fold(&ids, "PostToolUseFailure", "PostToolUseFailure:Bash");
    let out = post_tool_use_failure(
        &f,
        &[HookResult {
            additional_context: Some("retry with -f".to_owned()),
            ..blocking("[g]: x")
        }],
    );
    assert_eq!(out.messages.len(), 2);
    let deny = HookResult {
        permission_request: Some(PermissionRequestDecision::Deny { message: None }),
        ..HookResult::default()
    };
    let allow = HookResult {
        permission_request: Some(PermissionRequestDecision::Allow {
            updated_input: Some(json!({"a": 1})),
        }),
        ..HookResult::default()
    };
    let out = permission_request(&[deny, allow.clone()]);
    assert_eq!(
        out.decision,
        Some(PermissionDecision::Deny {
            message: "Permission denied by hook".to_owned()
        })
    );
    let out = permission_request(&[HookResult::default(), allow]);
    assert_eq!(
        out.decision,
        Some(PermissionDecision::Allow {
            updated_input: None
        })
    );
    assert_eq!(out.updated_input, Some(json!({"a": 1})));
    assert!(
        permission_denied(&[HookResult {
            retry: true,
            ..HookResult::default()
        }])
        .retry
    );
}
