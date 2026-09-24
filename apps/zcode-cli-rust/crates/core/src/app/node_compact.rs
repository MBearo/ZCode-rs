//! Compaction and reminder facts as Node records (spec rust-m11-node-storage
//! §5.2): the compaction timeline and summary at Node's persistence points,
//! and the todo reminder notice.
use super::{Engine, Event};
use crate::domain::node_journal::{self as nj, CompactStart, Notice};
use serde_json::{Value, json};

/// The reminder body of a `<system-reminder>` message (Node stores the body;
/// nested tags were neutralised when wrapping).
fn reminder_body(content: &str) -> String {
    let inner = content
        .strip_prefix("<system-reminder>\n")
        .and_then(|c| c.strip_suffix("\n</system-reminder>"))
        .unwrap_or(content);
    inner
        .replace("&lt;system-reminder", "<system-reminder")
        .replace("&lt;/system-reminder", "</system-reminder")
}

/// Node `getUsageTotalTokens`.
fn total_tokens(usage: &Value) -> Value {
    if let Some(total) = usage.get("total_tokens").filter(|t| t.is_number()) {
        return total.clone();
    }
    let field = |key: &str| usage[key].as_u64().unwrap_or(0);
    if usage.is_object() {
        (field("prompt_tokens") + field("completion_tokens")).into()
    } else {
        Value::Null
    }
}

impl Engine {
    /// Node compaction persistence of one compaction event, before it is projected.
    pub(super) fn node_compact(&mut self, id: &str, event: &Event) {
        if !self.journaled(id) {
            return;
        }
        let now = self.clock.now();
        let Some(active) = self.active.get(id) else {
            return;
        };
        let (turn, trace) = (active.turn_id.clone(), active.origin.trace_id.clone());
        match event {
            Event::CompactStarted {
                manual,
                trigger,
                instructions,
                tokens,
                ..
            } => {
                let ids = (
                    format!("cmp_{}", self.clock.id()),
                    nj::message_id(now, &self.clock.id()),
                    nj::part_id(now, &self.clock.id()),
                );
                let s = self.sessions.get_mut(id).unwrap();
                // 手动压缩轮没有输入行，命令 id 记在轮头上（Node timeline 的 sourceCommandId）。
                let command = s
                    .rows
                    .iter()
                    .rev()
                    .find(|r| r["kind"] == "turnHeader" && r["turnId"] == turn.as_str())
                    .and_then(|r| r["sourceCommandId"].as_str())
                    .filter(|_| *manual)
                    .map(str::to_owned);
                s.node_compact_started(
                    now,
                    CompactStart {
                        ids,
                        trigger,
                        source_command: command.as_deref(),
                        pre_tokens: *tokens as u64,
                        custom_instructions: *instructions,
                    },
                );
            }
            Event::CompactDone {
                context,
                tokens,
                usage,
                body,
                groups,
                reminders,
                ..
            } => {
                let summarized = context
                    .offset
                    .saturating_sub(self.sessions[id].context.offset);
                if summarized == 0 {
                    // 没有可压缩内容：Node 记为 skipped 的时间线。
                    self.sessions
                        .get_mut(id)
                        .unwrap()
                        .node_compact_ended(now, "skipped", None);
                    return;
                }
                let reminders: Vec<Value> = reminders
                    .iter()
                    .map(|m| {
                        json!({"messageId": nj::message_id(now, &self.clock.id()),
                            "partId": nj::part_id(now, &self.clock.id()),
                            "source": m["_zcode_source"],
                            "content": reminder_body(m["content"].as_str().unwrap_or(""))})
                    })
                    .collect();
                let tools: serde_json::Map<String, Value> = self
                    .tool_names()
                    .into_iter()
                    .map(|t| (t, true.into()))
                    .collect();
                let s = &self.sessions[id];
                let selection = json!({"providerId": s.provider, "modelId": s.model});
                let done = json!({
                    "summaryMessageId": nj::message_id(now, &self.clock.id()),
                    "textPartId": nj::part_id(now, &self.clock.id()),
                    "compactionPartId": nj::part_id(now, &self.clock.id()),
                    "boundaryId": format!("compact_{}", self.clock.id()),
                    "content": context.summary.clone().unwrap_or_default(),
                    "body": body,
                    "selection": selection,
                    "tools": tools,
                    "turnId": crate::domain::node_ids::turn_id(&turn),
                    "traceId": trace,
                    "summarizedMessageCount": summarized,
                    "groupsPreserved": groups,
                    "postCompactTokenCount": total_tokens(usage),
                    "truePostCompactTokenCount": tokens,
                    "reminders": reminders,
                });
                self.sessions
                    .get_mut(id)
                    .unwrap()
                    .node_compact_done(now, done);
            }
            Event::CompactFailed { .. } => {
                self.sessions
                    .get_mut(id)
                    .unwrap()
                    .node_compact_ended(now, "failed", None);
            }
            _ => {}
        }
    }

    /// Node's todo reminder notice (`persistSyntheticUserNoticeForSession`).
    pub(super) fn node_todo_reminder(&mut self, id: &str, text: &str) {
        if !self.journaled(id) {
            return;
        }
        let now = self.clock.now();
        let (message, part) = (self.clock.id(), self.clock.id());
        let tools = self.tool_names();
        let s = self.sessions.get_mut(id).unwrap();
        s.node_notice(
            now,
            Notice {
                message: nj::message_id(now, &message),
                part: nj::part_id(now, &part),
                source: "todo_reminder",
                text,
                metadata: Some(json!({"runtimeMessage": {"source": "todo_reminder"}})),
                tools: &tools,
            },
        );
    }
}
