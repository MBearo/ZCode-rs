//! Legacy `session/goal` (Node `server-operations.ts` `goalSession`) over the
//! V4 goal commands (spec 9.12).
use super::Engine;
use super::legacy_input::{ack_error, busy};
use super::legacy_session::params_error;
use crate::domain::{
    goal::Goal,
    legacy_goal::{PLAN_NOTE, changed, summary},
    legacy_input_params,
    zod::js_trim,
};
use anyhow::Result;
use serde_json::{Value, json};

const LOCKED: &str = "Cannot manage goals while a prompt is running";

fn reply(response: impl Into<String>, snapshot: Value, started: bool) -> Value {
    json!({"response": response.into(), "snapshot": snapshot, "startedTurn": started})
}

/// Node `appendPlanModeGoalContinuationNote`.
fn with_plan_note(mut response: String, plan: bool) -> String {
    if plan {
        response.push_str("\n\n");
        response.push_str(PLAN_NOTE);
    }
    response
}

impl Engine {
    pub(super) async fn legacy_goal(&mut self, raw: &Value) -> Result<Value> {
        let p = legacy_input_params::goal(raw).map_err(params_error)?;
        let id = self.legacy_resident(&p)?;
        self.legacy_expect(&id, &p)?;
        let action = p["action"].as_str().unwrap_or_default();
        // pause 是显式停止，允许打断持锁的运行；其余动作（包括 show）都要求未持锁。
        if action != "pause" && self.legacy_locked(&id) {
            return Err(busy(LOCKED));
        }
        self.touch_session(&id);
        let input = p["inputId"].as_str();
        let plan = self.submitted_execution_state(&id, &json!({}))?.plan_enabled;
        match action {
            "show" => {
                let text = summary(self.sessions[&id].goal.as_ref());
                Ok(reply(text, self.legacy_snapshot_with(&id, true)?, false))
            }
            "pause" => {
                let existed = self.sessions[&id].goal.is_some();
                if existed {
                    let c =
                        self.legacy_envelope(&id, ("legacy-session", "pauseGoal"), input, json!({}));
                    self.goal_command(&c, true).await?;
                }
                // Node 无论是否有目标都推进 revision。
                self.legacy_state_updated(&id, "goal_paused")?;
                let text = if existed { "" } else { "No goal to pause." };
                Ok(reply(text, self.legacy_snapshot_with(&id, false)?, false))
            }
            "resume" => {
                if self.sessions[&id].goal.is_none() {
                    let snapshot = self.legacy_snapshot_with(&id, true)?;
                    return Ok(reply("No goal to resume.", snapshot, false));
                }
                let c =
                    self.legacy_envelope(&id, ("legacy-session", "resumeGoal"), input, json!({}));
                let ack = self.goal_command(&c, true).await?;
                if ack["reasonCode"] == "activeTurn" {
                    return Err(busy(LOCKED));
                }
                if let Some(error) = ack_error(&ack) {
                    return Err(error);
                }
                self.legacy_state_updated(&id, "goal_resumed")?;
                let goal = self.sessions[&id].goal.as_ref();
                let text = goal.map_or_else(String::new, |g| changed("Goal resumed", g));
                let snapshot = self.legacy_snapshot_with(&id, false)?;
                Ok(reply(with_plan_note(text, plan), snapshot, !plan))
            }
            "clear" => {
                let cleared = self.clear_goal(&id).await?;
                self.legacy_state_updated(&id, "goal_cleared")?;
                let text = if cleared {
                    "Goal cleared."
                } else {
                    "No goal to clear."
                };
                Ok(reply(text, self.legacy_snapshot_with(&id, false)?, false))
            }
            _ => {
                let objective = js_trim(p["objective"].as_str().unwrap_or_default());
                if objective.is_empty() {
                    let usage = if action == "replace" {
                        "Usage: /goal replace <objective>"
                    } else {
                        "Usage: /goal <objective>"
                    };
                    return Ok(reply(usage, self.legacy_snapshot_with(&id, true)?, false));
                }
                // 重复的 set 也按 replace 处理（Node：已有目标时用户的 /goal 就是换目标）。
                let replaces = action == "replace" || self.sessions[&id].goal.is_some();
                if plan {
                    self.record_goal(&id, objective).await?;
                } else {
                    let payload = json!({"text": objective, "displayText": objective,
                        "heldQueueDisposition": "keepQueueAndSend"});
                    let c = self.legacy_envelope(
                        &id,
                        ("legacy-session", "sendGoalCommand"),
                        input,
                        payload,
                    );
                    let ack = self.command(c).await?;
                    if let Some(error) = ack_error(&ack) {
                        return Err(error);
                    }
                }
                let reason = if replaces { "goal_replaced" } else { "goal_set" };
                self.legacy_state_updated(&id, reason)?;
                // 新目标的用量与时长均为 0，与 Node setTarget 返回的目标同文。
                let fresh = Goal::new(String::new(), objective.into(), 0);
                let text = with_plan_note(changed("Goal active", &fresh), plan);
                Ok(reply(text, self.legacy_snapshot_with(&id, false)?, !plan))
            }
        }
    }
}
