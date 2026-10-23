//! hook 输入 JSON（docs/specs/rust-cli/hooks.md §3.1）。
//!
//! 公共字段的键序：`session_id`、
//! `transcript_path`、`cwd`、`permission_mode`、`agent_id`、`agent_type`，之后是 `hook_event_name`
//! 与调用方给出的事件字段（键序保持）。

use rust_cli_kernel_api::hooks::HookEvent;
use serde_json::{Map, Value};

/// 组装输入 JSON。`cwd` 由引擎解析好（`context.cwd`、会话 shell cwd 或工作区根）。
pub(crate) fn build(event: &HookEvent, cwd: &str) -> Value {
    let mut input = Map::new();
    input.insert("session_id".to_owned(), Value::from(event.session.as_str()));
    // zcode 没有 JSONL 记录（D2）。
    input.insert("transcript_path".to_owned(), Value::from(""));
    input.insert("cwd".to_owned(), Value::from(cwd));
    let context = &event.context;
    if let Some(mode) = context.mode {
        input.insert(
            "permission_mode".to_owned(),
            serde_json::to_value(mode).unwrap_or(Value::Null),
        );
    }
    if let Some(agent) = &context.agent {
        input.insert("agent_id".to_owned(), Value::from(agent.as_str()));
    }
    if let Some(agent_type) = &context.agent_type {
        input.insert("agent_type".to_owned(), Value::from(agent_type.as_str()));
    }
    input.insert(
        "hook_event_name".to_owned(),
        Value::from(event.name.as_str()),
    );
    if let Value::Object(fields) = &event.payload {
        for (key, value) in fields {
            input.insert(key.clone(), value.clone());
        }
    }
    Value::Object(input)
}

/// `hookName`：有 match query 时是 `<事件>:<query>`。
pub(crate) fn hook_name(event: &str, query: Option<&str>) -> String {
    match query {
        Some(query) => format!("{event}:{query}"),
        None => event.to_owned(),
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
