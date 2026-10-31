//! hook 输出的解析（docs/specs/rust-cli/hooks.md §4.1、§4.2）。
//!
//! command hook 的 stdout、http hook 的响应体与已校验的 JSON 输出（解析、校验、转成单个 hook 的结果）。
//! schema 只校验已知字段的类型，
//! 未知字段忽略（zod object 的默认行为）。

use serde_json::{Map, Value};

use crate::result::{Behavior, Blocking, HookResult, Outcome, PermissionRequestDecision};

/// 一次输出的解析结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Parsed {
    Json(Map<String, Value>),
    /// `{"async": true}`：hook 声明自己在后台继续。
    Async,
    Plain,
    /// 校验失败或看起来像 JSON 但不合法；文本是给用户的说明。
    Invalid(String),
}

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Str,
    Object,
    Strings,
    Any,
    OneOf(&'static [&'static str]),
}

const TOP: &[(&str, Kind)] = &[
    ("continue", Kind::Bool),
    ("suppressOutput", Kind::Bool),
    ("stopReason", Kind::Str),
    ("decision", Kind::OneOf(&["approve", "block"])),
    ("systemMessage", Kind::Str),
    ("terminalSequence", Kind::Str),
    ("reason", Kind::Str),
];

const CONTEXT: &[(&str, Kind)] = &[("additionalContext", Kind::Str)];
/// 至少这么多行、每行都是 JSON 时算 JSON lines。
const MIN_JSON_LINES: usize = 2;
/// HTTP hook 的正文不是 JSON 时，说明里最多给出这么多个字符。
const MAX_SHOWN_BODY_CHARS: usize = 200;

/// 各事件的 `hookSpecificOutput` 字段。
fn specific(event: &str) -> Option<&'static [(&'static str, Kind)]> {
    Some(match event {
        "PreToolUse" => &[
            (
                "permissionDecision",
                Kind::OneOf(&["allow", "deny", "ask", "defer"]),
            ),
            ("permissionDecisionReason", Kind::Str),
            ("updatedInput", Kind::Object),
            ("additionalContext", Kind::Str),
        ],
        "UserPromptSubmit" => &[
            ("additionalContext", Kind::Str),
            ("sessionTitle", Kind::Str),
            ("suppressOriginalPrompt", Kind::Bool),
        ],
        "UserPromptExpansion" => &[
            ("additionalContext", Kind::Str),
            ("suppressOriginalPrompt", Kind::Bool),
        ],
        "SessionStart" => &[
            ("additionalContext", Kind::Str),
            ("initialUserMessage", Kind::Str),
            ("sessionTitle", Kind::Str),
            ("watchPaths", Kind::Strings),
            ("reloadSkills", Kind::Bool),
        ],
        "PreModelSwitch" => &[
            ("permissionDecision", Kind::OneOf(&["allow", "deny", "ask"])),
            ("permissionDecisionReason", Kind::Str),
        ],
        "PostToolUse" => &[
            ("additionalContext", Kind::Str),
            ("classifierContext", Kind::Str),
            ("updatedToolOutput", Kind::Any),
            ("updatedMCPToolOutput", Kind::Any),
        ],
        "Setup" | "PostModelSwitch" | "SubagentStart" | "PostToolBatch" | "PostToolUseFailure"
        | "Stop" | "SubagentStop" | "Notification" => CONTEXT,
        "PermissionDenied" => &[("retry", Kind::Bool)],
        "PermissionRequest" => &[("decision", Kind::Object)],
        "Elicitation" | "ElicitationResult" => &[
            ("action", Kind::OneOf(&["accept", "decline", "cancel"])),
            ("content", Kind::Object),
        ],
        "CwdChanged" | "FileChanged" => &[("watchPaths", Kind::Strings)],
        "WorktreeCreate" => &[("worktreePath", Kind::Str)],
        "MessageDisplay" => &[("displayContent", Kind::Str)],
        _ => return None,
    })
}

fn fits(kind: Kind, value: &Value) -> bool {
    match kind {
        Kind::Bool => value.is_boolean(),
        Kind::Str => value.is_string(),
        Kind::Object => value.is_object(),
        Kind::Strings => value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string)),
        Kind::Any => true,
        Kind::OneOf(allowed) => value.as_str().is_some_and(|text| allowed.contains(&text)),
    }
}

fn check(object: &Map<String, Value>, fields: &[(&str, Kind)], path: &str) -> Result<(), String> {
    for (name, kind) in fields {
        if let Some(value) = object.get(*name)
            && !fits(*kind, value)
        {
            return Err(format!("{path}{name}: invalid value"));
        }
    }
    Ok(())
}

/// hook JSON 输出的 schema 校验。
fn validate(object: &Map<String, Value>) -> Result<(), String> {
    check(object, TOP, "")?;
    let Some(output) = object.get("hookSpecificOutput") else {
        return Ok(());
    };
    let output = output
        .as_object()
        .ok_or("hookSpecificOutput: expected object")?;
    let event = output
        .get("hookEventName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let fields = specific(event).ok_or("hookSpecificOutput.hookEventName: invalid value")?;
    check(output, fields, "hookSpecificOutput.")?;
    if event == "PermissionRequest" {
        let behavior = output
            .get("decision")
            .and_then(|decision| decision.get("behavior"))
            .and_then(Value::as_str);
        if !matches!(behavior, Some("allow" | "deny")) {
            return Err("hookSpecificOutput.decision: invalid value".to_owned());
        }
    }
    if event == "WorktreeCreate" && !output.contains_key("worktreePath") {
        return Err("hookSpecificOutput.worktreePath: required".to_owned());
    }
    Ok(())
}

fn validation_error(detail: &str) -> String {
    format!("Hook JSON output validation failed \u{2014} {detail}")
}

fn is_async(value: &Value) -> bool {
    value.get("async") == Some(&Value::Bool(true))
}

/// 多行、每行都是不含有效字段的 JSON：按纯文本处理。
fn json_lines(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines.len() >= MIN_JSON_LINES
        && lines.iter().all(|line| {
            serde_json::from_str::<Value>(line.trim()).is_ok_and(|value| {
                value
                    .as_object()
                    .is_none_or(|object| validate(object).is_err() || object.is_empty())
            })
        })
}

/// command hook 的 stdout。
pub(crate) fn parse(stdout: &str) -> Parsed {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('{') {
        return Parsed::Plain;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) if is_async(&value) => Parsed::Async,
        Ok(Value::Object(object)) => match validate(&object) {
            Ok(()) => Parsed::Json(object),
            Err(detail) => Parsed::Invalid(validation_error(&detail)),
        },
        Ok(_) => Parsed::Invalid(validation_error("expected object")),
        Err(_) if !trimmed.ends_with('}') || json_lines(trimmed) => Parsed::Plain,
        Err(error) => Parsed::Invalid(format!(
            "Hook output looks like a JSON object but is not valid JSON \u{2014} {error}. Emit the payload with a JSON encoder (jq, ConvertTo-Json, json.dumps) rather than string concatenation so backslashes and quotes inside strings are escaped."
        )),
    }
}

/// http hook 的响应体：必须是 JSON，空体按 `{}`。
pub(crate) fn parse_http(body: &str) -> Parsed {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Parsed::Json(Map::new());
    }
    if !trimmed.starts_with('{') {
        let shown: String = if trimmed.chars().count() > MAX_SHOWN_BODY_CHARS {
            format!(
                "{}\u{2026}",
                trimmed
                    .chars()
                    .take(MAX_SHOWN_BODY_CHARS)
                    .collect::<String>()
            )
        } else {
            trimmed.to_owned()
        };
        return Parsed::Invalid(format!(
            "HTTP hook must return JSON, but got non-JSON response body: {shown}"
        ));
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) if is_async(&value) => Parsed::Async,
        Ok(Value::Object(object)) => match validate(&object) {
            Ok(()) => Parsed::Json(object),
            Err(detail) => Parsed::Invalid(validation_error(&detail)),
        },
        Ok(_) => Parsed::Invalid(validation_error("expected object")),
        Err(error) => Parsed::Invalid(format!(
            "HTTP hook must return valid JSON, but parsing failed: {error}"
        )),
    }
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn behavior(text: &str) -> Option<Behavior> {
    match text {
        "allow" => Some(Behavior::Allow),
        "deny" => Some(Behavior::Deny),
        "ask" => Some(Behavior::Ask),
        "defer" => Some(Behavior::Defer),
        _ => None,
    }
}

/// 已通过校验的 JSON 输出 → 单个 hook 的结果。错误（事件名不符）按 hook
/// 运行失败处理。`blocking` 与 `outcome` 由调用方按退出码补充。
pub(crate) fn process(
    json: &Map<String, Value>,
    command: &str,
    event: &str,
) -> Result<HookResult, String> {
    let mut result = HookResult {
        success_content: Some(String::new()),
        ..HookResult::default()
    };
    let reason = string(json, "reason");
    let block = |text: Option<String>| Blocking {
        error: text.unwrap_or_else(|| "Blocked by hook".to_owned()),
        command: command.to_owned(),
    };
    if json.get("continue") == Some(&Value::Bool(false)) {
        result.prevent = true;
        result.stop_reason = string(json, "stopReason");
    }
    match json.get("decision").and_then(Value::as_str) {
        Some("approve") => result.behavior = Some(Behavior::Allow),
        Some("block") => {
            result.behavior = Some(Behavior::Deny);
            result.blocking = Some(block(reason.clone()));
        }
        _ => {}
    }
    result.system_message = string(json, "systemMessage");
    let output = json.get("hookSpecificOutput").and_then(Value::as_object);
    let name = output
        .and_then(|output| output.get("hookEventName"))
        .and_then(Value::as_str);
    let decision = output
        .and_then(|output| output.get("permissionDecision"))
        .and_then(Value::as_str)
        .and_then(behavior);
    if name == Some("PreToolUse")
        && let Some(decision) = decision
    {
        result.behavior = Some(decision);
        if decision == Behavior::Deny {
            result.blocking = Some(block(reason.clone()));
        }
    }
    if result.behavior.is_some() && reason.is_some() {
        result.reason.clone_from(&reason);
    }
    let Some(output) = output else {
        return Ok(finish(result));
    };
    let name = name.unwrap_or_default();
    if name != event {
        return Err(format!(
            "Hook returned incorrect event name: expected '{event}' but got '{name}'. Full stdout: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        ));
    }
    let context = string(output, "additionalContext");
    match name {
        "PreToolUse" => {
            if let Some(decision) = decision {
                result.behavior = Some(decision);
                if decision == Behavior::Deny {
                    let why = string(output, "permissionDecisionReason").or(reason);
                    result.blocking = Some(block(why));
                }
            }
            result.reason = string(output, "permissionDecisionReason");
            result.updated_input = output.get("updatedInput").cloned();
            result.additional_context = context;
        }
        "UserPromptSubmit" | "UserPromptExpansion" => {
            result.additional_context = context;
            result.session_title = string(output, "sessionTitle");
            result.suppress_original_prompt =
                output.get("suppressOriginalPrompt") == Some(&Value::Bool(true));
        }
        "SessionStart" => {
            result.additional_context = context;
            result.initial_user_message = string(output, "initialUserMessage");
            result.session_title = string(output, "sessionTitle");
            if let Some(paths) = output.get("watchPaths").and_then(Value::as_array) {
                result.watch_paths = paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
            }
        }
        "PostToolUse" => {
            result.additional_context = context;
            result.updated_tool_output = output.get("updatedToolOutput").cloned();
        }
        "Setup" | "PostModelSwitch" | "SubagentStart" | "PostToolBatch" | "PostToolUseFailure"
        | "Stop" | "SubagentStop" | "Notification" => result.additional_context = context,
        "PermissionDenied" => result.retry = output.get("retry") == Some(&Value::Bool(true)),
        "PermissionRequest" => {
            if let Some(decision) = output.get("decision").and_then(Value::as_object) {
                let allow = decision.get("behavior").and_then(Value::as_str) == Some("allow");
                result.behavior = Some(if allow {
                    Behavior::Allow
                } else {
                    Behavior::Deny
                });
                result.permission_request = Some(if allow {
                    PermissionRequestDecision::Allow {
                        updated_input: decision.get("updatedInput").cloned(),
                    }
                } else {
                    PermissionRequestDecision::Deny {
                        message: string(decision, "message"),
                    }
                });
                if allow {
                    result.updated_input = decision.get("updatedInput").cloned();
                }
            }
        }
        _ => {}
    }
    Ok(finish(result))
}

fn finish(mut result: HookResult) -> HookResult {
    if result.blocking.is_some() {
        result.outcome = Outcome::Blocking;
        result.success_content = None;
    }
    result
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
