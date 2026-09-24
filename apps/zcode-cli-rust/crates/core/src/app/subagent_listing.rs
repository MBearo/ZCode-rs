//! `session/subagents` (Node `listSessionSubagents`, spec
//! rust-m11-node-storage §6.3): the stored Agent calls of the parent and
//! their child sessions, with a resident runtime's live facts on top.
use super::Engine;
use crate::domain::execution::Phase;
use crate::domain::session::Session;
use crate::domain::subagent::Task;
use crate::domain::subagent_query::{self, BackgroundTask, Live, Relation};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

fn text(value: &str) -> Option<String> {
    Some(value.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Node `subagentEventRelations` of a resident parent's task
/// (`SubagentSpawned`, then `SubagentStopped` once it ended).
fn relation(task: &Task) -> Relation {
    let ended = !task.running();
    Relation {
        agent_id: text(&task.id),
        child_session_id: text(&task.child_id),
        description: text(&task.description).or_else(|| text(&task.prompt)),
        started_at: Some(task.started_at),
        stopped_at: task.ended_at.filter(|_| ended),
        stopped_status: ended.then_some(match task.status.as_str() {
            "cancelled" | "stopped" => "cancelled",
            "failed" | "error" => "failed",
            _ => "success",
        }),
        subagent_type: text(&task.agent_type),
        summary: ended
            .then(|| text(&task.output).or_else(|| text(&task.description)))
            .flatten(),
    }
}

/// Node `BackgroundTaskInfo.status` of a background child.
fn background(task: &Task) -> BackgroundTask {
    BackgroundTask {
        task_id: task.id.clone(),
        tool_call_id: task.call_id.clone(),
        child_session_id: task.child_id.clone(),
        status: match task.status.as_str() {
            "running" => "running",
            "completed" => "completed",
            "cancelled" => "cancelled",
            "interrupted" => "lost",
            _ => "failed",
        },
        started_at: Some(task.started_at),
        completed_at: task.ended_at,
    }
}

/// A resident child's projection status (Node `SessionProjection.status`).
fn child_status(s: &Session) -> Option<&'static str> {
    if s.running() {
        return Some(if s.pending.is_empty() {
            "running"
        } else {
            "waiting"
        });
    }
    match s.phase {
        Phase::CompletedSuccess => Some("completed"),
        Phase::Error => Some("error"),
        _ => None,
    }
}

impl Engine {
    fn live_subagents(&self, id: &str) -> Live {
        let Some(s) = self.sessions.get(id) else {
            return Live::default();
        };
        let mut live = Live {
            parent: true,
            ..Live::default()
        };
        for task in s.children.values() {
            live.relations.insert(task.call_id.clone(), relation(task));
            if task.background {
                live.background.push(background(task));
            }
            if let Some(status) = self.sessions.get(&task.child_id).and_then(child_status) {
                live.children.insert(task.child_id.clone(), status);
            }
        }
        live.active_calls = s
            .rows
            .iter()
            .filter(|r| {
                r["kind"] == "toolCall"
                    && matches!(
                        r["status"].as_str(),
                        Some("pending" | "running" | "pendingApproval")
                    )
            })
            .filter_map(|r| r["toolCallId"].as_str().map(str::to_owned))
            .collect();
        live
    }

    pub(super) async fn subagents_query(&mut self, p: &Value) -> Result<Value> {
        let id = p["sessionId"].as_str().unwrap_or_default();
        ensure!(!id.trim().is_empty(), "Session required");
        let limit = p["endedLimit"].as_u64().unwrap_or(20);
        ensure!((1..=100).contains(&limit), "Invalid ended limit");
        let live = self.live_subagents(id);
        let Some(facts) = self.store.subagent_facts(id, live.clone()).await? else {
            anyhow::bail!("Session not found: {id}");
        };
        let (running, ended) = subagent_query::project(
            &facts.messages,
            facts.revert.as_ref(),
            &facts.children,
            &live,
        );
        let (items, next) =
            subagent_query::paginate(&ended, p["endedCursor"].as_str(), limit as usize);
        let mut page = json!({"total": ended.len(), "items": items});
        if let Some(next) = next {
            page["nextCursor"] = next.into();
        }
        let revision = self.sessions.get(id).map_or(facts.updated, |s| s.revision);
        let children: Vec<&String> = facts.children.iter().map(|(id, _)| id).collect();
        Ok(
            json!({"revision": revision, "childSessionIds": children, "running": running,
            "ended": page}),
        )
    }
}
