//! Hook inputs (Node `HookInput` built by `runtime/methods/hooks.ts` and
//! `tool/executor/hook-flow.ts`), the stdin compatibility object, transcript
//! and `${VAR}` expansion (`configured-runner-input.ts`).
use super::{HookEvent, Plugin, truncate_utf16};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Fields every input carries. Node builds each input with sorted keys, so
/// the builders below insert through a sorted map to keep the same order.
#[derive(Clone, Debug)]
pub struct Base<'a> {
    /// Only subagent-like runtimes have one; tool events never carry it.
    pub agent_name: Option<&'a str>,
    pub cwd: &'a str,
    pub mode: &'a str,
    pub session_id: &'a str,
    pub timestamp: &'a str,
    pub trace_id: &'a str,
    pub turn_id: Option<&'a str>,
}

/// The tool call a tool event is about.
#[derive(Clone, Copy, Debug)]
pub struct Call<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub input: &'a Value,
}

fn object(event: HookEvent, base: &Base, agent: bool, fields: Vec<(&str, Value)>) -> Value {
    let mut sorted: BTreeMap<&str, Value> = fields.into_iter().collect();
    if agent && let Some(name) = base.agent_name {
        sorted.insert("agentName", name.into());
    }
    sorted.insert("cwd", base.cwd.into());
    sorted.insert("hookEventName", event.as_str().into());
    sorted.insert("mode", base.mode.into());
    sorted.insert("sessionId", base.session_id.into());
    sorted.insert("timestamp", base.timestamp.into());
    sorted.insert("traceId", base.trace_id.into());
    if let Some(turn) = base.turn_id {
        sorted.insert("turnId", turn.into());
    }
    Value::Object(
        sorted
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn call_fields<'a>(call: &Call<'a>) -> Vec<(&'a str, Value)> {
    vec![
        ("toolCallId", call.id.into()),
        ("toolInput", call.input.clone()),
        ("toolName", call.name.into()),
    ]
}

pub fn pre_tool_use(base: &Base, call: &Call, risk: &str, scope: Option<&str>) -> Value {
    let mut fields = call_fields(call);
    fields.push(("riskLevel", risk.into()));
    if let Some(scope) = scope {
        fields.push(("sideEffectScope", scope.into()));
    }
    object(HookEvent::PreToolUse, base, false, fields)
}

pub fn permission_request(
    base: &Base,
    call: &Call,
    reason: &str,
    request_id: &str,
    risk: &str,
    scope: Option<&str>,
) -> Value {
    let mut fields = call_fields(call);
    fields.push(("reason", reason.into()));
    fields.push(("requestId", request_id.into()));
    fields.push(("riskLevel", risk.into()));
    if let Some(scope) = scope {
        fields.push(("sideEffectScope", scope.into()));
    }
    object(HookEvent::PermissionRequest, base, false, fields)
}

pub fn post_tool_use(base: &Base, call: &Call, response: &Value, artifact: Option<&str>) -> Value {
    let mut fields = call_fields(call);
    if let Some(path) = artifact {
        fields.push(("artifactRefs", json!([path])));
    }
    fields.push(("toolResponse", response.clone()));
    fields.push(("toolResultPreview", preview(response).into()));
    object(HookEvent::PostToolUse, base, false, fields)
}

/// `kind` is the CoreError type (`tool_cancelled` marks an interrupt) or the
/// JS error name.
pub fn post_tool_use_failure(base: &Base, call: &Call, message: &str, kind: &str) -> Value {
    let mut fields = call_fields(call);
    fields.push(("error", json!({"message":message,"type":kind})));
    fields.push(("isInterrupt", (kind == "tool_cancelled").into()));
    object(HookEvent::PostToolUseFailure, base, false, fields)
}

pub fn session_start(base: &Base, model: Option<&str>, source: &str) -> Value {
    let mut fields = vec![("source", source.into())];
    if let Some(model) = model {
        fields.push(("model", model.into()));
    }
    object(HookEvent::SessionStart, base, true, fields)
}

pub fn user_prompt_submit(base: &Base, prompt: &str, attachments: Option<String>) -> Value {
    let mut fields = vec![("prompt", prompt.into())];
    if let Some(summary) = attachments {
        fields.push(("attachmentsSummary", summary.into()));
    }
    object(HookEvent::UserPromptSubmit, base, true, fields)
}

pub fn stop(base: &Base, response: &str, tool_calls: usize, active: bool) -> Value {
    let fields = vec![
        (
            "responsePreview",
            truncate_utf16(response, 4000, "...").into(),
        ),
        ("responseText", response.into()),
        ("stopHookActive", active.into()),
        ("toolCallCount", tool_calls.into()),
    ];
    object(HookEvent::Stop, base, true, fields)
}

/// One attachment as Node `summarizeTurnAttachments` sees it.
pub struct Attachment<'a> {
    pub kind: &'a str,
    pub path: Option<&'a str>,
    /// Inline content length in UTF-16 units.
    pub inline: Option<usize>,
}

pub fn attachments_summary(items: &[Attachment]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let lines: Vec<String> = items
        .iter()
        .enumerate()
        .map(
            |(index, a)| match (a.path.filter(|p| !p.is_empty()), a.inline) {
                (Some(path), _) => format!("{}:{}:{path}", index + 1, a.kind),
                (None, Some(n)) if n > 0 => format!("{}:{}:inline:{n} chars", index + 1, a.kind),
                _ => format!("{}:{}", index + 1, a.kind),
            },
        )
        .collect();
    Some(lines.join("\n"))
}

/// Node `previewHookValue`: the string itself or its JSON, cut at 4000 units.
pub fn preview(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    truncate_utf16(&text, 4000, "...[truncated]")
}

/// Node `createCompatibleHookStdin`: the input plus the snake_case aliases,
/// top-level keys in Node's order (nested objects keep serde's order).
pub fn stdin(input: &Value, transcript_path: &str) -> String {
    let mut out: Vec<(&str, Value)> = input
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.as_str(), v.clone())).collect())
        .unwrap_or_default();
    // JSON.stringify 省略 undefined：只有输入里存在的字段才会带上别名；
    // 已存在的键原位覆盖（JS 对象保持首次插入的位置）。
    let mut set = |key: &'static str, value: Option<&Value>| {
        let Some(value) = value else {
            return;
        };
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value.clone(),
            None => out.push((key, value.clone())),
        }
    };
    let path = Value::from(transcript_path);
    set("agent_type", input.get("agentName"));
    set("hook_event_name", input.get("hookEventName"));
    set("permission_mode", input.get("mode"));
    set("session_id", input.get("sessionId"));
    set("transcript_path", Some(&path));
    set("transcriptPath", Some(&path));
    if input.get("toolName").is_some() {
        set("tool_name", input.get("toolName"));
        set("tool_input", input.get("toolInput"));
        set("tool_use_id", input.get("toolCallId"));
    }
    match input["hookEventName"].as_str().and_then(HookEvent::parse) {
        Some(HookEvent::PostToolUse) => set("tool_response", input.get("toolResponse")),
        Some(HookEvent::PostToolUseFailure) => {
            set("error_details", input.get("error"));
            // 与 Node 一致：snake_case 的 error 是消息字符串，覆盖原位置的对象。
            set("error", input["error"].get("message"));
            set("is_interrupt", input.get("isInterrupt"));
        }
        Some(HookEvent::Stop) => {
            set("last_assistant_message", last_message(input));
            set("stop_hook_active", input.get("stopHookActive"));
        }
        // PermissionRequest 的 permission_suggestions 在 Node 中从未填充。
        _ => {}
    }
    let body: Vec<String> = out
        .iter()
        .map(|(key, value)| format!("{}:{value}", Value::from(*key)))
        .collect();
    format!("{{{}}}\n", body.join(","))
}

/// Node `responseText ?? responsePreview`.
fn last_message(input: &Value) -> Option<&Value> {
    input
        .get("responseText")
        .filter(|v| !v.is_null())
        .or_else(|| input.get("responsePreview"))
}

/// Node `formatTranscript`: the one message Stop and UserPromptSubmit carry.
pub fn transcript(input: &Value) -> String {
    let (role, text) = match input["hookEventName"].as_str().and_then(HookEvent::parse) {
        Some(HookEvent::Stop) => ("assistant", last_message(input).unwrap_or(&Value::Null)),
        Some(HookEvent::UserPromptSubmit) => ("user", &input["prompt"]),
        _ => return String::new(),
    };
    format!(
        "{}\n",
        json!({"message":{"content":[{"text":text,"type":"text"}],"role":role}})
    )
}

const VARIABLES: [&str; 11] = [
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PLUGIN_DATA",
    "CLAUDE_PLUGIN_ROOT",
    "CLAUDE_PROJECT_DIR",
    "CLAUDE_SESSION_ID",
    "CLAUDE_SKILL_DIR",
    "ZCODE_PLUGIN_DATA",
    "ZCODE_PLUGIN_ROOT",
    "ZCODE_PROJECT_DIR",
    "ZCODE_SESSION_ID",
    "ZCODE_SKILL_DIR",
];

fn project_dir<'a>(input: &'a Value, cwd: &'a str) -> &'a str {
    input["cwd"]
        .as_str()
        .filter(|c| !c.is_empty())
        .unwrap_or(cwd)
}

/// Node `expandPluginVariables`. Plugin variables stay literal without a
/// plugin; skill variables are a configuration error.
pub fn expand(
    value: &str,
    plugin: Option<&Plugin>,
    input: &Value,
    cwd: &str,
) -> Result<String, String> {
    let session = input["sessionId"].as_str().unwrap_or("");
    let project = project_dir(input, cwd);
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let name = tail
            .find('}')
            .map(|end| &tail[..end])
            .filter(|name| VARIABLES.contains(name));
        let Some(name) = name else {
            out.push_str("${");
            rest = tail;
            continue;
        };
        let replacement = match (name, plugin) {
            ("CLAUDE_SKILL_DIR" | "ZCODE_SKILL_DIR", _) => {
                return Err(format!("Hook variable requires a skill context: {name}"));
            }
            ("CLAUDE_CODE_SESSION_ID" | "CLAUDE_SESSION_ID" | "ZCODE_SESSION_ID", _) => session,
            ("CLAUDE_PROJECT_DIR" | "ZCODE_PROJECT_DIR", _) => project,
            ("CLAUDE_PLUGIN_DATA" | "ZCODE_PLUGIN_DATA", Some(p)) => p.data_path.as_str(),
            ("CLAUDE_PLUGIN_ROOT" | "ZCODE_PLUGIN_ROOT", Some(p)) => p.root_path.as_str(),
            _ => &rest[start..start + 3 + name.len()],
        };
        out.push_str(replacement);
        rest = &tail[name.len() + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Node `createPluginEnvOverlay`, in its insertion order.
pub fn env(plugin: Option<&Plugin>, input: &Value, cwd: &str) -> Vec<(String, String)> {
    let session = input["sessionId"].as_str().unwrap_or("").to_owned();
    let project = project_dir(input, cwd).to_owned();
    let mut out = vec![
        ("CLAUDE_CODE_SESSION_ID", session.clone()),
        ("CLAUDE_PROJECT_DIR", project.clone()),
        ("CLAUDE_SESSION_ID", session.clone()),
        ("ZCODE_PROJECT_DIR", project),
        ("ZCODE_SESSION_ID", session),
    ];
    if let Some(p) = plugin {
        out.extend([
            ("CLAUDE_PLUGIN_DATA", p.data_path.clone()),
            ("CLAUDE_PLUGIN_ROOT", p.root_path.clone()),
            ("ZCODE_PLUGIN_DATA", p.data_path.clone()),
            ("ZCODE_PLUGIN_ID", p.id.clone()),
            ("ZCODE_PLUGIN_NAME", p.name.clone()),
            ("ZCODE_PLUGIN_ROOT", p.root_path.clone()),
        ]);
    }
    out.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// JS `new Date(ms).toISOString()`.
pub fn iso_timestamp(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let rest = ms % 86_400_000;
    // Howard Hinnant `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}
