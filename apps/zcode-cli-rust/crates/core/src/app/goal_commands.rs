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
/// `source`: the client command that asked for the continuation (Node
/// `continueActiveTarget({inputId})`); `None` for the goal loop and background resumes.
pub(super) fn continuation(
    session: &mut Session,
    goal: &Goal,
    verdict: Option<&crate::domain::goal::Verdict>,
    (turn, source): (&str, Option<&str>),
    now: u64,
) -> Value {
    let mut header = session.row("turnHeader", turn, turn, now);
    header["origin"] = "goalContinuation".into();
    if let Some(source) = source {
        header["sourceCommandId"] = source.into();
    }
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
    /// V4 `pauseGoal` / `resumeGoal`; `legacy` is `session/goal`, which keeps
    /// Node's legacy rules: pause only interrupts a run holding the legacy lock,
    /// and resume in plan mode records the goal as active without running it.
    pub(super) async fn goal_command(&mut self, c: &Command, legacy: bool) -> Result<Value> {
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
            // Node goal-compact.ts：plan 开启时不能恢复目标。
            if s.plan_enabled && !legacy {
                let mut ack = c.ack(
                    "rejected",
                    s.revision,
                    Some("guard.planGoalMutuallyExclusive"),
                );
                ack["message"] = "Plan and Goal cannot be active at the same time.".into();
                return Ok(ack);
            }
        }
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).unwrap();
        // legacy resume 在 plan 下只把目标置为 active、不续跑（Node continueGoalAfterChange）。
        let waiting = s.plan_enabled
            || !s.queue.is_empty()
            || s.background.values().any(|t| t.status == "running");
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
                let source = (c.client_id != "goal-background").then_some(c.command_id.as_str());
                continuation(s, &goal, None, (&turn, source), now);
                s.run_id = Some(self.clock.id());
                s.phase = crate::domain::execution::Phase::Running;
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
        } else if let Some(active) = self.active.get(id).filter(|a| !legacy || a.legacy_lock) {
            active.cancel.cancel();
            self.cancel_auth(id);
        }
        Ok(ack)
    }

    /// Legacy `session/goal clear`: removes the goal; `false` when there was none.
    pub(super) async fn clear_goal(&mut self, id: &str) -> Result<bool> {
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        if s.goal.take().is_none() {
            return Ok(false);
        }
        s.revision += 1;
        s.updated_at = now;
        self.publish(id, vec![])?;
        self.persist(id, None).await?;
        Ok(true)
    }

    /// Legacy `session/goal set` in plan mode: the goal is recorded as active
    /// and not run (Node `continueGoalAfterChange` with `canContinue` false).
    pub(super) async fn record_goal(&mut self, id: &str, objective: &str) -> Result<()> {
        validate_objective(objective)?;
        let now = self.clock.now();
        let mut goal = Goal::new(self.clock.id(), objective.trim().into(), now);
        goal.settle(now);
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        s.goal = Some(goal);
        s.revision += 1;
        s.updated_at = now;
        self.publish(id, vec![])?;
        self.persist(id, None).await
    }
    pub(super) async fn resume_background_goal(&mut self, id: &str) -> Result<()> {
        let s = &self.sessions[id];
        if s.running()
            || s.plan_enabled
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
        self.goal_command(&c, false).await?;
        Ok(())
    }
}
