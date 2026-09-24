//! Tool row reductions (Node `ProductProjection.onToolCallScheduled`,
//! `onToolCallActivity`, `onToolCallResult`, `closeOpenToolRows`,
//! `applyBackgroundTaskNotification`) and `buildToolOutput`.
use super::events::Event;
use super::projection::{Delta, Projection, is_open_foreground_tool};
use super::reduce_plan::{self, cua_action};
use super::synth_parts::hide_invalid_tool;
use crate::js_json::stringify;
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

/// `PROTOCOL_V4_LIMITS.toolOutputFinalHeadBytes` / `TailBytes`.
const OUTPUT_HEAD_BYTES: usize = 32 * 1024;
const OUTPUT_TAIL_BYTES: usize = 32 * 1024;

/// The display kinds a V4 tool row carries (Node `toProtocolToolCallDisplay`).
const ROW_DISPLAY_KINDS: [&str; 11] = [
    "node_repl_images",
    "task_output",
    "respond_to_coordinator",
    "mcp_tool",
    "create_workflow",
    "get_workflow_run",
    "list_workflow_runs",
    "eval_workflow_snippet",
    "saved_workflow_list",
    "list_models",
    "resume_workflow_run",
];

/// Node `buildToolOutput`: head and tail of a long output, the rest by ref.
pub fn build_output(content: &str, display: &Value, call: &str) -> Value {
    let display = (display["kind"] != "node_repl_images" && !display.is_null()).then_some(display);
    let bytes = content.as_bytes();
    let mut output = if bytes.len() <= OUTPUT_HEAD_BYTES + OUTPUT_TAIL_BYTES {
        json!({ "text": content })
    } else {
        let head = String::from_utf8_lossy(&bytes[..OUTPUT_HEAD_BYTES]);
        let tail = String::from_utf8_lossy(&bytes[bytes.len() - OUTPUT_TAIL_BYTES..]);
        json!({ "text": format!("{head}\n…\n{tail}") })
    };
    if let Some(display) = display {
        output["display"] = display.clone();
    }
    if bytes.len() > OUTPUT_HEAD_BYTES + OUTPUT_TAIL_BYTES {
        output["truncated"] =
            json!({"totalBytes": bytes.len(), "ref": format!("tool-output/{call}")});
    }
    output
}

/// Node `stringifyToolInput`.
fn input_text(input: Option<&Value>) -> String {
    match input {
        None | Some(Value::Null) => "{}".into(),
        Some(input) => stringify(input),
    }
}

fn task_tag_regex(tag: &str) -> regress::Regex {
    regress::Regex::with_flags(&format!(r"<{tag}>([\s\S]*?)<\/{tag}>"), "u").expect("valid regex")
}

/// Node `readTaskNotificationTag`.
fn task_tag(text: &str, tag: &str) -> Option<String> {
    static TAGS: OnceLock<Vec<(&'static str, regress::Regex)>> = OnceLock::new();
    let tags = TAGS.get_or_init(|| {
        ["tool-use-id", "error", "result", "status", "summary"]
            .into_iter()
            .map(|tag| (tag, task_tag_regex(tag)))
            .collect()
    });
    let regex = &tags.iter().find(|(name, _)| *name == tag)?.1;
    let found = regex.find(text)?;
    let value = text[found.group(1)?].trim();
    (!value.is_empty()).then(|| {
        value
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    })
}

/// Node `applyBackgroundTaskNotification`: the tool row a `<task-notification>`
/// input settles, updated.
pub fn notification_update(p: &Projection, input: &str, at: i64) -> Option<Value> {
    let trimmed = input.trim();
    if !trimmed.starts_with("<task-notification>") {
        return None;
    }
    let call = task_tag(trimmed, "tool-use-id")?;
    let row = p.find_tool_row(&call)?;
    let status = match task_tag(trimmed, "status").as_deref() {
        Some("failed" | "lost") => "error",
        Some("stopped" | "killed") => "cancelled",
        _ => "success",
    };
    let error = task_tag(trimmed, "error");
    let content = task_tag(trimmed, "result")
        .or_else(|| task_tag(trimmed, "summary"))
        .or_else(|| error.clone());
    let mut next = row.clone();
    next["status"] = status.into();
    if let Some(content) = &content {
        next["output"] = build_output(content, &Value::Null, &call);
    }
    next["endedAt"] = at.into();
    if status == "error" {
        let message = error
            .or(content)
            .unwrap_or_else(|| "Background task failed.".into());
        next["error"] = json!({"code": "fault.runtime.backgroundTaskFailed", "message": message});
    } else {
        next.as_object_mut()
            .expect("rows are objects")
            .shift_remove("error");
    }
    Some(next)
}

impl Projection {
    /// Node `onToolCallScheduled`.
    pub fn on_tool_scheduled(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        if payload["source"] == "subagent" || !self.is_running() {
            return Vec::new();
        }
        let call = payload["toolCallId"].as_str().unwrap_or("").to_owned();
        let name = &payload["toolName"];
        if hide_invalid_tool(name, None) {
            return Vec::new();
        }
        let input = payload.get("input");
        let cua_app = cua_action(name.as_str().unwrap_or(""))
            .filter(|action| *action != "list_apps")
            .and_then(|_| {
                reduce_plan::resolve_cua_app(input.unwrap_or(&Value::Null), &self.list_apps)
            });
        let display = payload.get("display").filter(|d| d["kind"] == "mcp_tool");
        let plan = self.todo_plan(event, reduce_plan::steps_from_input(name, input));
        let mut fields = Map::new();
        if let Some(message) = payload
            .get("assistantMessageId")
            .filter(|m| super::facts::truthy(m))
        {
            fields.insert(
                "assistantResponseId".into(),
                super::facts::js_string(message).into(),
            );
        }
        let row = match self.find_tool_row(&call) {
            Some(existing) => {
                let mut row = existing.clone();
                let object = row.as_object_mut().expect("rows are objects");
                object.extend(fields);
                object.insert("inputText".into(), input_text(input).into());
                insert_defined(object, "input", input);
                if let Some(app) = cua_app {
                    object.insert("cuaApp".into(), app);
                }
                insert_defined(object, "display", display);
                Delta::Upsert(row)
            }
            None => {
                let mut row = self.row_base(event, &self.turn_of(event), &call);
                let object = row.as_object_mut().expect("rows are objects");
                object.insert("kind".into(), "toolCall".into());
                object.extend(fields);
                object.insert("toolCallId".into(), call.clone().into());
                insert_defined(object, "toolName", Some(name));
                object.insert("status".into(), "inputStreaming".into());
                object.insert("inputText".into(), input_text(input).into());
                insert_defined(object, "input", input);
                if let Some(app) = cua_app {
                    object.insert("cuaApp".into(), app);
                }
                insert_defined(object, "display", display);
                self.tool_rows
                    .insert(call, row["rowId"].as_u64().unwrap_or(0));
                Delta::Append(row)
            }
        };
        let mut deltas = vec![row];
        deltas.extend(plan);
        deltas
    }

    /// Node `onToolCallActivity` → `projectToolActivity` for `ToolCallStarted`.
    pub fn on_tool_started(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        if payload["source"] == "subagent" || !self.is_running() {
            return Vec::new();
        }
        let Some(row) = self.find_tool_row(payload["toolCallId"].as_str().unwrap_or("")) else {
            return Vec::new();
        };
        let mut row = row.clone();
        row["status"] = "running".into();
        row["startedAt"] = event.at.into();
        if payload["display"]["kind"] == "mcp_tool" {
            row["display"] = payload["display"].clone();
        }
        vec![Delta::Upsert(row)]
    }

    /// Node `onToolCallResult`.
    pub fn on_tool_result(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        if payload["source"] == "subagent" || !self.is_running() {
            return Vec::new();
        }
        let call = payload["toolCallId"].as_str().unwrap_or("").to_owned();
        let Some(row) = self.find_tool_row(&call).cloned() else {
            return Vec::new();
        };
        let result = &payload["result"];
        let success = result["success"] == true;
        let name = row["toolName"].as_str().unwrap_or("");
        if success && cua_action(name) == Some("list_apps") {
            let content = result["content"].as_str().unwrap_or("");
            if let Some(snapshot) = reduce_plan::parse_list_apps(content, &result["display"]) {
                self.list_apps = snapshot;
            }
        }
        let display = &result["display"];
        let row_display = display["kind"]
            .as_str()
            .is_some_and(|kind| ROW_DISPLAY_KINDS.contains(&kind));
        let mut next = row.clone();
        next["status"] = if success { "success" } else { "error" }.into();
        let content = result["content"].as_str().unwrap_or("");
        next["output"] = build_output(content, display, &call);
        if row_display {
            next["display"] = display.clone();
        }
        next["endedAt"] = event.at.into();
        if !success {
            let error = &result["error"];
            let code = error.get("type").filter(|v| !v.is_null()).cloned();
            let message = error.get("message").filter(|v| !v.is_null()).cloned();
            next["error"] = json!({
                "code": code.unwrap_or_else(|| "fault.runtime.toolFailed".into()),
                "message": message.unwrap_or_else(|| "Tool execution failed.".into()),
            });
        }
        let plan = if success {
            self.todo_plan(
                event,
                reduce_plan::steps_from_output(&row["toolName"], &result["content"]),
            )
        } else {
            Vec::new()
        };
        let mut deltas = vec![Delta::Upsert(next)];
        deltas.extend(plan);
        deltas
    }

    /// Node `closeOpenToolRows`: a finished turn settles its open foreground tools.
    pub fn close_open_tool_rows(&mut self, event: &Event, status: &str) -> Vec<Delta> {
        let mut open: Vec<Value> = self
            .open_tools
            .iter()
            .filter_map(|call| self.find_tool_row(call))
            .filter(|row| is_open_foreground_tool(row))
            .cloned()
            .collect();
        open.sort_by_key(|row| row["rowId"].as_u64().unwrap_or(0));
        let mut deltas = Vec::new();
        let mut closed = Vec::new();
        for row in open {
            let mut next = row;
            next["status"] = status.into();
            let input = next["inputText"].as_str().unwrap_or("").to_owned();
            next["inputText"] = input.into();
            next["endedAt"] = event.at.into();
            let object = next.as_object_mut().expect("rows are objects");
            object.shift_remove("approvalInteractionId");
            if status == "error" {
                object.insert(
                    "error".into(),
                    json!({"code": "fault.runtime.toolLifecycleIncomplete",
                        "message": "Tool call ended without a terminal event."}),
                );
            } else {
                object.shift_remove("error");
            }
            closed.push(next["toolCallId"].clone());
            deltas.push(Delta::Upsert(next));
        }
        let pending = self.state["pendingInteractions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let kept: Vec<Value> = pending
            .iter()
            .filter(|interaction| {
                let payload = &interaction["payload"];
                !(matches!(payload["kind"].as_str(), Some("permission" | "userInput"))
                    && payload["toolCallId"].is_string()
                    && closed.contains(&payload["toolCallId"]))
            })
            .cloned()
            .collect();
        if kept.len() != pending.len() {
            let mut patch = Map::new();
            patch.insert("pendingInteractions".into(), kept.into());
            deltas.push(Delta::State(patch));
        }
        deltas
    }
}

/// Inserts a JS member that may be undefined.
fn insert_defined(object: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        object.insert(key.into(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_outputs_keep_head_and_tail_bytes_like_node_buffers() {
        let short = build_output("ok", &json!({"kind": "node_repl_images"}), "c1");
        assert_eq!(short, json!({"text": "ok"}));
        // 3 字节字符跨越 32 KiB 边界：与 Node Buffer.toString 一样按最大非法子序列替换。
        let long = "€".repeat(30_000);
        let out = build_output(&long, &json!({"kind": "mcp_tool"}), "c1");
        assert_eq!(
            out["truncated"],
            json!({"totalBytes": 90_000, "ref": "tool-output/c1"})
        );
        assert_eq!(out["display"], json!({"kind": "mcp_tool"}));
        let text = out["text"].as_str().unwrap();
        let (head, tail) = text.split_once("\n…\n").unwrap();
        assert_eq!(head, format!("{}\u{fffd}", "€".repeat(10_922)));
        assert_eq!(tail, format!("\u{fffd}\u{fffd}{}", "€".repeat(10_922)));
    }
}
