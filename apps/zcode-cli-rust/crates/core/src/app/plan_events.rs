//! Engine side of plan mode: tool-driven plan switches, reminders the run
//! added, and ExitPlanMode feedback steered into the running turn.
use super::Engine;
use crate::contract::{Event, Guide};
use anyhow::{Context, Result};
use serde_json::json;
use tokio::sync::oneshot;

impl Engine {
    /// Run-scoped reminder and plan events (todo reminders included).
    pub(super) async fn plan_event(&mut self, id: &str, event: Event) -> Result<()> {
        match event {
            Event::TodoReminder { reply } => return self.todo_reminder(id, reply).await,
            Event::PlanMode {
                call_id,
                enable,
                reply,
            } => {
                let s = self.sessions.get(id).context("Session unavailable")?;
                // ExitPlanMode 只在 plan 开启时生效（Node session-mode-port 的同一检查）。
                if !enable && !s.plan_enabled {
                    let _ = reply.send(Err(crate::domain::plan_mode::NOT_IN_PLAN.into()));
                    return Ok(());
                }
                match self.apply_execution_state(id, None, Some(enable), Some(&call_id)) {
                    Ok(changed) => {
                        if changed {
                            self.publish(id, vec![])?;
                            self.persist(id, None).await?;
                        }
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error.to_string()));
                    }
                }
            }
            Event::PlanReminder {
                anchor,
                exit,
                message,
            } => {
                let s = self.sessions.get_mut(id).context("Session unavailable")?;
                s.runtime.plan_reminders.push((anchor, !exit, message));
                if exit && s.needs_plan_exit_reminder {
                    s.needs_plan_exit_reminder = false;
                    self.refresh_permissions();
                }
            }
            _ => unreachable!("not a plan event"),
        }
        Ok(())
    }

    /// Node `steerTurn({delivery: "guide", source: "plan_approval_feedback"})`:
    /// the feedback becomes a user input of the running turn.
    pub(super) async fn plan_feedback_guide(
        &mut self,
        id: &str,
        turn: &str,
        committed: oneshot::Sender<Option<Guide>>,
    ) -> Result<()> {
        let feedback = self
            .sessions
            .get_mut(id)
            .and_then(|s| s.runtime.plan_feedback.take())
            .context("Plan feedback missing")?;
        let now = self.clock.now();
        let entity = self.clock.id();
        let s = self.sessions.get_mut(id).unwrap();
        let mut row = s.row("userInput", turn, &entity, now);
        row["text"] = feedback.clone().into();
        row["origin"] = "realUser".into();
        row["guided"] = true.into();
        s.rows.push(row.clone());
        let message = json!({"role":"user","content":crate::domain::prompt::user_steer(&feedback)});
        s.append_message(message.clone());
        s.revision += 1;
        s.updated_at = now;
        self.publish(id, vec![json!({"op":"row.appended","row":row})])?;
        self.persist(id, None).await?;
        let _ = committed.send(Some(Guide {
            messages: vec![message],
            origin: None,
            tool_disallowlist: vec![],
        }));
        Ok(())
    }
}
