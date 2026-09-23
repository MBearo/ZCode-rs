use super::Engine;
use crate::domain::{goal::Goal, protocol::Command, session::Session};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub(super) fn validate_objective(text: &str) -> Result<()> {
    ensure!(
        !text.trim().is_empty() && text.trim().chars().count() <= 4000,
        "Goal objective must contain 1 to 4000 characters"
    );
    Ok(())
}
pub(super) fn continuation(
    session: &mut Session,
    goal: &Goal,
    verdict: Option<&crate::domain::goal::Verdict>,
    turn: &str,
    now: u64,
) -> Value {
    let mut header = session.row("turnHeader", turn, turn, now);
    header["origin"] = "goalContinuation".into();
    header["state"] = "running".into();
    header["startedAt"] = now.into();
    session.rows.push(header);
    let mut content = goal.prompt("goalContinue", verdict);
    if !session.background.is_empty() {
        let statuses = session
            .background
            .values()
            .map(|t| json!({"task_id":t.id,"status":t.status,"outputFile":t.output_file}))
            .collect::<Vec<_>>();
        content.push_str(&format!(
            "\n<task-notification>{}</task-notification>",
            json!(statuses)
        ));
    }
    let message = json!({"role":"user","content":content});
    session.append_message(message.clone());
    message
}
impl Engine {
    pub(super) async fn goal_command(&mut self, c: &Command) -> Result<Value> {
        let id = c.session_id.as_deref().context("Session required")?;
        let s = &self.sessions[id];
        let Some(goal) = &s.goal else {
            return Ok(c.ack("noop", s.revision, None));
        };
        let pause = c.kind == "pauseGoal";
        if pause && !goal.active() {
            return Ok(c.ack("noop", s.revision, None));
        }
        if !pause && s.running() {
            return Ok(c.ack("rejected", s.revision, Some("activeTurn")));
        }
        if !pause {
            self.select(&json!({}), Some(self.session_selection(id)?))?;
            ensure!(
                s.mode == "yolo" && !s.plan_enabled,
                "Unsupported goal execution mode"
            );
        }
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).unwrap();
        let waiting = !s.queue.is_empty() || s.background.values().any(|t| t.status == "running");
        let goal = s.goal.as_mut().unwrap();
        let turn = if pause {
            goal.pause(now);
            None
        } else {
            ensure!(!goal.exhausted(), "Goal token budget exhausted");
            goal.start(now);
            if waiting {
                goal.settle(now);
                None
            } else {
                let goal = goal.clone();
                let turn = self.clock.id();
                continuation(s, &goal, None, &turn, now);
                s.run_id = Some(self.clock.id());
                s.phase = "running".into();
                s.last_error = None;
                Some(turn)
            }
        };
        s.revision += 1;
        s.updated_at = now;
        let ack = c.ack("accepted", s.revision, None);
        let deltas = if turn.is_some() {
            self.new_turn_rows(id)
        } else {
            vec![]
        };
        self.publish(id, deltas)?;
        self.persist(id, Some((c.key(), ack.clone()))).await?;
        self.acks.insert(c.key(), ack.clone());
        if let Some(turn) = turn {
            self.start_run(id, turn)?;
        } else if let Some(active) = self.active.get(id) {
            active.cancel.cancel();
            self.cancel_auth(id);
        }
        Ok(ack)
    }
    pub(super) async fn resume_background_goal(&mut self, id: &str) -> Result<()> {
        let s = &self.sessions[id];
        if s.running()
            || s.children.values().any(|t| t.running() || !t.notified)
            || !s.queue.is_empty()
            || s.background.values().any(|t| t.status == "running")
            || s.goal
                .as_ref()
                .is_none_or(|g| g.status != "active" || g.exhausted())
        {
            return Ok(());
        }
        // 后台结果先提交；只有仍 active 的目标可被唤醒，暂停目标绝不随迟到事件恢复。
        let c = Command {
            command_id: self.clock.id(),
            client_id: "goal-background".into(),
            session_id: Some(id.into()),
            kind: "resumeGoal".into(),
            payload: json!({}),
            issued_at: self.clock.now() as f64,
            base_revision: None,
            base_log_epoch: None,
        };
        self.goal_command(&c).await?;
        Ok(())
    }
}
