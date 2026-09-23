use super::Engine;
use anyhow::{Context, Result};
use serde_json::json;

impl Engine {
    pub(super) async fn finish_child(&mut self, id: &str) -> Result<()> {
        let s = &self.sessions[id];
        if s.agent_profile.is_none()
            || s.running()
            || s.background.values().any(|t| t.status == "running")
            || s.children.values().any(|t| t.running())
        {
            return Ok(());
        }
        let parent = s.parent_id.clone().context("Child owner missing")?;
        let Some(task) = self
            .sessions
            .get(&parent)
            .and_then(|s| {
                s.children
                    .values()
                    .find(|t| t.child_id == id && t.running())
            })
            .cloned()
        else {
            return Ok(());
        };
        let mut output = s
            .messages
            .iter()
            .rev()
            .find(|m| {
                m["role"] == "assistant" && m["content"].as_str().is_some_and(|s| !s.is_empty())
            })
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_owned();
        let status = if s.phase == "completedSuccess" {
            "completed"
        } else if s.phase == "completedInterrupted" {
            "cancelled"
        } else {
            "failed"
        };
        if status != "completed" {
            output = format!(
                "Subagent {status}: {}",
                s.last_error
                    .as_ref()
                    .and_then(|e| e["message"].as_str())
                    .unwrap_or("execution interrupted")
            );
        }
        if output.len() > 120000 {
            let mut n = 119900;
            while !output.is_char_boundary(n) {
                n -= 1;
            }
            output.truncate(n);
            output
                .push_str("\n[Subagent result truncated; inspect child session for full content]");
        }
        let tools = s.rows.iter().filter(|r| r["kind"] == "toolCall").count() as u64;
        let tokens = s.usage["cumulative"]["inputTokens"].as_u64().unwrap_or(0)
            + s.usage["cumulative"]["outputTokens"].as_u64().unwrap_or(0);
        let output_file = self.tools.agent_output(id, &output).await?;
        let owner = self.sessions.get_mut(&parent).unwrap();
        let task = owner.children.get_mut(&task.id).unwrap();
        task.status = status.into();
        task.output = output;
        task.output_file = output_file;
        task.ended_at = Some(self.clock.now());
        task.tool_uses = tools;
        task.tokens = tokens;
        if !task.background {
            task.notified = true;
        }
        let task = task.clone();
        owner.revision += 1;
        let turn = owner
            .rows
            .iter()
            .find(|r| r["toolCallId"] == task.call_id)
            .and_then(|r| r["turnId"].as_str())
            .map(str::to_owned);
        let mut deltas = if let Some(turn) = turn {
            super::file_changes::hydrate(owner, self.tools.as_ref(), Some(&turn)).await?
        } else {
            vec![]
        };
        deltas.extend(owner.sync_subagent_row(&task.id));
        self.publish(&parent, deltas)?;
        self.persist(&parent, None).await?;
        if let Some(watch) = self.child_updates.get(id) {
            watch.send_replace(task);
        }
        self.deliver_children(&parent).await?;
        Box::pin(self.finish_child(&parent)).await?;
        Ok(())
    }
    pub(super) async fn deliver_children(&mut self, id: &str) -> Result<()> {
        let s = &self.sessions[id];
        if s.running() || !s.auto_drain || !s.queue.is_empty() {
            return Ok(());
        }
        let pending = s
            .children
            .values()
            .filter(|t| t.background && !t.running() && !t.notified)
            .take(1)
            .cloned()
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        self.select(&json!({}), Some(self.session_selection(id)?))?;
        let text = pending
            .iter()
            .map(|t| t.notification())
            .collect::<Vec<_>>()
            .join("\n");
        let c = super::subagents::child_command(id, &self.clock.id(), &text);
        let (turn, _) = self.admit_input(id, &c, None)?;
        let s = self.sessions.get_mut(id).unwrap();
        for task in &pending {
            s.children.get_mut(&task.id).unwrap().notified = true;
        }
        for row in s.rows.iter_mut().filter(|r| r["turnId"] == turn) {
            if row["kind"] == "turnHeader" || row["kind"] == "userInput" {
                row["origin"] = "backgroundResult".into();
            }
        }
        self.publish(id, self.new_turn_rows(id))?;
        self.persist(id, None).await?;
        self.start_run(id, turn)
    }
}
