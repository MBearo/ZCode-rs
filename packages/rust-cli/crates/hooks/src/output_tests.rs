//! 输出解析与 JSON 字段（spec §4.2）。

use serde_json::json;

use super::*;

fn json(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

#[test]
fn classifies_stdout() {
    assert_eq!(parse("plain text\n"), Parsed::Plain);
    assert_eq!(parse("  {\"async\": true}\n"), Parsed::Async);
    assert_eq!(
        parse("{\"continue\": false}"),
        Parsed::Json(json(json!({"continue": false})))
    );
    assert!(
        matches!(parse("{\"continue\": \"no\"}"), Parsed::Invalid(text) if text.contains("continue"))
    );
    assert!(matches!(
        parse("{\"decision\": \"maybe\"}"),
        Parsed::Invalid(_)
    ));
    assert!(matches!(
        parse("{\"hookSpecificOutput\": {\"hookEventName\": \"Nope\"}}"),
        Parsed::Invalid(_)
    ));
    // 以 { 开头但不以 } 结尾：纯文本；多行空对象：纯文本；像 JSON 但不合法：校验错误。
    assert_eq!(parse("{ not json"), Parsed::Plain);
    assert_eq!(parse("{}\n{}"), Parsed::Plain);
    assert!(
        matches!(parse("{\"a\": \"\\q\"}"), Parsed::Invalid(text) if text.contains("JSON encoder"))
    );
}

#[test]
fn http_bodies_must_be_json() {
    assert_eq!(parse_http("  "), Parsed::Json(Map::new()));
    assert!(matches!(parse_http("ok"), Parsed::Invalid(text)
        if text == "HTTP hook must return JSON, but got non-JSON response body: ok"));
    let long = "x".repeat(300);
    assert!(matches!(parse_http(&long), Parsed::Invalid(text) if text.ends_with("x\u{2026}")));
    assert!(
        matches!(parse_http("{bad}"), Parsed::Invalid(text) if text.starts_with("HTTP hook must return valid JSON"))
    );
}

#[test]
fn pre_tool_use_decisions() {
    let deny = process(
        &json(json!({"reason": "top", "hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "deny",
            "permissionDecisionReason": "no rm"}})),
        "guard.sh",
        "PreToolUse",
    )
    .unwrap();
    assert_eq!(deny.behavior, Some(Behavior::Deny));
    assert_eq!(deny.outcome, Outcome::Blocking);
    assert_eq!(deny.blocking.as_ref().unwrap().error, "no rm");
    assert_eq!(deny.reason.as_deref(), Some("no rm"));
    // 只有顶层 reason：阻塞用它，原因被 permissionDecisionReason（缺省）覆盖为空。
    let top_only = process(
        &json(json!({"reason": "top", "hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "deny"}})),
        "guard.sh",
        "PreToolUse",
    )
    .unwrap();
    assert_eq!(top_only.blocking.unwrap().error, "top");
    assert_eq!(top_only.reason, None);
    let ask = process(
        &json(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "ask", "updatedInput": {"command": "ls"},
            "additionalContext": "ctx"}})),
        "g",
        "PreToolUse",
    )
    .unwrap();
    assert_eq!(ask.behavior, Some(Behavior::Ask));
    assert_eq!(ask.updated_input, Some(json!({"command": "ls"})));
    assert_eq!(ask.additional_context.as_deref(), Some("ctx"));
    assert_eq!(ask.success_content.as_deref(), Some(""));
}

#[test]
fn common_fields() {
    let result = process(
        &json(
            json!({"continue": false, "stopReason": "halt", "decision": "block",
            "reason": "because", "systemMessage": "note"}),
        ),
        "c",
        "Stop",
    )
    .unwrap();
    assert!(result.prevent);
    assert_eq!(result.stop_reason.as_deref(), Some("halt"));
    assert_eq!(result.blocking.unwrap().error, "because");
    assert_eq!(result.system_message.as_deref(), Some("note"));
    let approve = process(&json(json!({"decision": "approve"})), "c", "PreToolUse").unwrap();
    assert_eq!(approve.behavior, Some(Behavior::Allow));
    let block = process(&json(json!({"decision": "block"})), "c", "Stop").unwrap();
    assert_eq!(block.blocking.unwrap().error, "Blocked by hook");
}

#[test]
fn event_specific_fields() {
    let start = process(
        &json(
            json!({"hookSpecificOutput": {"hookEventName": "SessionStart",
            "additionalContext": "a", "initialUserMessage": "hi", "sessionTitle": "t",
            "watchPaths": ["/w/x"]}}),
        ),
        "c",
        "SessionStart",
    )
    .unwrap();
    assert_eq!(start.initial_user_message.as_deref(), Some("hi"));
    assert_eq!(start.watch_paths, ["/w/x"]);
    let post = process(
        &json(
            json!({"hookSpecificOutput": {"hookEventName": "PostToolUse",
            "updatedToolOutput": "new"}}),
        ),
        "c",
        "PostToolUse",
    )
    .unwrap();
    assert_eq!(post.updated_tool_output, Some(json!("new")));
    let request = process(
        &json(
            json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": "nope"}}}),
        ),
        "c",
        "PermissionRequest",
    )
    .unwrap();
    assert_eq!(
        request.permission_request,
        Some(PermissionRequestDecision::Deny {
            message: Some("nope".to_owned())
        })
    );
    let wrong = process(
        &json(json!({"hookSpecificOutput": {"hookEventName": "Stop"}})),
        "c",
        "PreToolUse",
    )
    .unwrap_err();
    assert!(wrong.starts_with(
        "Hook returned incorrect event name: expected 'PreToolUse' but got 'Stop'. Full stdout: {"
    ));
}
