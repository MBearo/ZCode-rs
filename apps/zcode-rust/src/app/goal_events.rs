use super::Engine;
use crate::{
    contract::Event,
    domain::goal::{Goal, Verdict},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn goal_event(&mut self, id: &str, event: Event) -> Result<()> {
        let now = self.clock.now();
        let turn = self.active[id].turn_id.clone();
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        match event {
            Event::GoalStep { reply } => {
                if !s.queue.is_empty()
                    || s.children.values().any(|t| t.running() || !t.notified)
                    || s.background.values().any(|t| t.status == "running")
                    || s.goal.as_ref().is_none_or(|g| g.status != "active")
                {
                    let _ = reply.send(None);
                    return Ok(());
                }
                let goal = s.goal.as_mut().unwrap();
                goal.account(&Value::Null, now);
                if goal.exhausted() {
                    goal.pause(now);
                    s.revision += 1;
                    self.publish(id, vec![])?;
                    self.persist(id, None).await?;
                    let _ = reply.send(None);
                    return Ok(());
                }
                goal.status = "verifying".into();
                goal.iteration += 1;
                goal.iterations.push(json!({"iteration":goal.iteration,"items":crate::domain::todo::plan(&s.todos,s.todos_updated_at)["items"].as_array().cloned().unwrap_or_default(),"updatedAt":now}));
                let frozen = goal.clone();
                let mut row = s.row("timelineMarker", &turn, &self.clock.id(), now);
                row["marker"] =
                    json!({"type":"goalVerify","iteration":frozen.iteration,"outcome":"running"});
                s.rows.push(row.clone());
                s.revision += 1;
                s.updated_at = now;
                self.publish(id, vec![json!({"op":"row.appended","row":row})])?;
                self.persist(id, None).await?;
                let _ = reply.send(Some(frozen));
            }
            Event::GoalVerdict {
                target_id,
                verdict,
                usage,
                reply,
            } => {
                let Some(goal) = s
                    .goal
                    .as_ref()
                    .filter(|g| g.target_id == target_id && g.status == "verifying")
                else {
                    let _ = reply.send(None);
                    return Ok(());
                };
                let iteration = goal.iteration;
                let mut deltas = vec![];
                let row = s.rows.iter_mut().rev().find(|r| {
                    r["marker"]["type"] == "goalVerify"
                        && r["marker"]["iteration"] == iteration
                        && r["marker"]["outcome"] == "running"
                });
                let anchor = row
                    .as_ref()
                    .map(|r| r["rowId"].clone())
                    .unwrap_or(Value::Null);
                if let Some(row) = row {
                    row["marker"]["outcome"] = verdict.outcome.into();
                    row["marker"]["detail"] = verdict.reason.clone().into();
                    deltas.push(json!({"op":"row.upserted","row":row}));
                }
                super::goal_events::account_usage(s, &usage, now);
                let goal = s.goal.as_mut().unwrap();
                record(goal, &verdict, anchor, now);
                let can_continue = verdict.outcome == "notSatisfied"
                    && verdict.next_action.is_some()
                    && !goal.exhausted();
                if can_continue {
                    goal.status = "active".into();
                } else if goal.exhausted() && verdict.outcome != "pass" {
                    goal.status = "paused".into();
                }
                let keep_running = can_continue && s.queue.is_empty();
                let next = if keep_running {
                    goal.status = "active".into();
                    let frozen = goal.clone();
                    s.finish_rows("success", now);
                    deltas.extend(
                        s.rows
                            .iter()
                            .filter(|r| r["turnId"] == turn)
                            .map(|r| json!({"op":"row.upserted","row":r})),
                    );
                    let turn = self.clock.id();
                    let message =
                        super::goal_commands::continuation(s, &frozen, Some(&verdict), &turn, now);
                    self.active.get_mut(id).unwrap().turn_id = turn;
                    deltas.push(json!({"op":"row.appended","row":s.rows.last().unwrap()}));
                    Some((frozen, message))
                } else {
                    goal.settle(now);
                    None
                };
                s.revision += 1;
                s.updated_at = now;
                self.publish(id, deltas)?;
                self.persist(id, None).await?;
                let _ = reply.send(next);
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
fn record(goal: &mut Goal, verdict: &Verdict, anchor: Value, now: u64) {
    goal.status = if verdict.outcome == "pass" {
        "verified"
    } else {
        verdict.outcome
    }
    .into();
    let mut verification = json!({"iteration":goal.iteration,"outcome":verdict.outcome,"reason":verdict.reason,"anchorRowId":anchor,"at":now});
    if let Some(next) = &verdict.next_action {
        verification["nextAction"] = next.clone().into();
    }
    goal.verifications.push(verification);
}
pub(super) fn account_usage(s: &mut crate::domain::session::Session, usage: &Value, now: u64) {
    if let Some(goal) = s.goal.as_mut().filter(|g| g.active()) {
        goal.account(usage, now);
    }
    for (from, to) in [
        ("prompt_tokens", "inputTokens"),
        ("completion_tokens", "outputTokens"),
    ] {
        s.usage["cumulative"][to] = s.usage["cumulative"][to]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(usage[from].as_u64().unwrap_or(0))
            .into();
    }
    for (key, field) in [
        ("cacheReadTokens", "cached_tokens"),
        ("cacheWriteTokens", "cache_write_tokens"),
    ] {
        s.usage["cumulative"][key] = s.usage["cumulative"][key]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(usage["prompt_tokens_details"][field].as_u64().unwrap_or(0))
            .into();
    }
}
