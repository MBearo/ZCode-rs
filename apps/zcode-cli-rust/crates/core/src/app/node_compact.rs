//! Compaction and reminder facts as Node records (spec rust-m11-node-storage
//! §5.2): the compaction timeline and summary at Node's persistence points,
//! and the todo reminder notice.
use super::{Engine, Event};
use crate::domain::node_history::incoming::presented;
use crate::domain::node_journal::{self as nj, CompactStart, Notice, Notification};
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
                prefix,
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
                        pre_tokens: (tokens + prefix) as u64,
                        custom_instructions: *instructions,
                    },
                );
            }
            Event::CompactDone {
                context,
                tokens,
                prefix,
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
                let summary = nj::message_id(now, &self.clock.id());
                let done = json!({
                    "summaryMessageId": summary,
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
                    "truePostCompactTokenCount": tokens + prefix,
                    "reminders": reminders,
                });
                let s = self.sessions.get_mut(id).unwrap();
                s.node_compact_done(now, done);
                s.runtime.compact_summary = Some(summary);
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

    /// A model-only reminder notice of `source` (todo reminder, plugin
    /// reference; Node `persistSyntheticUserNoticeForSession`).
    pub(super) fn node_model_notice(&mut self, id: &str, source: &str, text: &str) {
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
                source,
                text,
                metadata: None,
                tools: &tools,
            },
        );
    }

    /// A background subagent's result opens a parent turn (Node task
    /// notification, `task_notification` presentation). The model reads the
    /// presented text like Node's request, so the stored and live contexts match.
    pub(super) fn node_background_result(
        &mut self,
        id: &str,
        turn: &str,
        text: &str,
        task: &crate::domain::subagent::Task,
    ) {
        if !self.journaled(id) {
            return;
        }
        let now = self.clock.now();
        let ids = (
            format!("runtime_command_{}", self.clock.id()),
            nj::message_id(now, &self.clock.id()),
            nj::part_id(now, &self.clock.id()),
        );
        let tools = self.tool_names();
        let title = match task.description.trim() {
            "" => task.id.as_str(),
            title => title,
        };
        let origin = json!({"backgroundSource": "subagent", "title": title, "workId": task.id});
        let s = self.sessions.get_mut(id).unwrap();
        if let (Some(message), Some(content)) = (
            s.messages.iter_mut().rev().find(|m| m["role"] == "user"),
            presented(text, "task_notification"),
        ) {
            message["content"] = content.into();
        }
        s.node_task_notification(
            now,
            Notification {
                ids,
                text,
                task: Some(&task.id),
                origin: Some(origin),
                turn: Some(turn),
                tools: &tools,
            },
        );
    }

    /// Coordinator messages drained into a running child (Node `steerTurn`
    /// with `coordinator_steer`), presented to the model as Node does.
    pub(super) fn node_mailbox(
        &mut self,
        id: &str,
        turn: &str,
        items: &[Value],
        messages: &mut [Value],
    ) {
        if !self.journaled(id) {
            return;
        }
        let now = self.clock.now();
        let tools = self.tool_names();
        let ids: Vec<(String, String)> = items
            .iter()
            .map(|_| {
                (
                    nj::message_id(now, &self.clock.id()),
                    nj::part_id(now, &self.clock.id()),
                )
            })
            .collect();
        let s = self.sessions.get_mut(id).unwrap();
        let first = s.messages.len().saturating_sub(items.len());
        for (index, (item, (message, part))) in items.iter().zip(ids).enumerate() {
            let text = item["text"].as_str().unwrap_or("");
            let command = item["id"].as_str().unwrap_or("");
            if let Some(content) = presented(text, "coordinator_steer") {
                messages[index]["content"] = content.clone().into();
                s.messages[first + index]["content"] = content.into();
            }
            let ledger = format!(
                "pending_{}_{command}",
                crate::domain::node_ids::turn_id(turn)
            );
            s.node_coordinator_steer(now, (ledger, message, part), text, command, &tools);
        }
    }
}
