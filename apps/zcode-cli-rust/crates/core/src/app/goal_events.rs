//! The goal loop's durable barriers (spec rust-m11-node-storage §5.4): a
//! finished continuation turn settles the goal run and starts the
//! completion verification; the verdict completes the goal or opens the
//! next continuation turn (Node `continueActiveTargetLoop`).
use super::Engine;
use crate::domain::goal::{Goal, Verdict, Verifying};
use crate::domain::node_journal::Outcome;
use anyhow::{Context, Result};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn goal_event(
        &mut self,
        id: &str,
        event: crate::contract::Event,
    ) -> Result<()> {
        use crate::contract::Event;
        let now = self.clock.now();
        let turn = self.active[id].turn_id.clone();
        let (event_id, trace) = (self.clock.id(), self.clock.id());
        let clock = self.clock.clone();
        let mut ids = move || clock.id();
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        let trace = s.trace_id.clone().unwrap_or(trace);
        match event {
            Event::GoalStep { reply } => {
                if !s.queue.is_empty()
                    || s.children.values().any(|t| t.running() || !t.notified)
                    || s.background.values().any(|t| t.status == "running")
                    || s.goal.as_ref().is_none_or(|g| g.target_status != "active")
                {
                    let _ = reply.send(None);
                    return Ok(());
                }
                // Node accountTargetTurnCompletion → 稳定边界 → verifier：先结算本轮再验证。
                let goal = s.goal.as_mut().unwrap();
                goal.finish_run(now, true, None);
                let anchor = s
                    .node
                    .turn
                    .as_ref()
                    .map(|t| (t.boundary.clone(), t.runtime.clone()));
                s.node_finished(now, Outcome::Success, &mut ids);
                let goal = s.goal.as_mut().unwrap();
                if goal.target_status != "active" {
                    s.revision += 1;
                    self.publish(id, vec![])?;
                    self.persist(id, None).await?;
                    let _ = reply.send(None);
                    return Ok(());
                }
                goal.status = "verifying".into();
                goal.iteration += 1;
                goal.verifying = Some(Verifying {
                    id: event_id.chars().take(16).collect(),
                    started: now,
                    anchor: anchor.as_ref().and_then(|(boundary, _)| boundary.clone()),
                    turn: anchor.map(|(_, runtime)| runtime),
                });
                goal.iterations.push(json!({"iteration":goal.iteration,"items":crate::domain::todo::plan(&s.todos,s.todos_updated_at)["items"].as_array().cloned().unwrap_or_default(),"updatedAt":now}));
                let frozen = goal.clone();
                s.node_goal_verification(now, (self.clock.id(), trace), None);
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
                    .filter(|g| g.target_id == target_id && g.verifying.is_some())
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
                    row["marker"]["outcome"] = verdict.outcome().into();
                    if !verdict.reason.is_empty() {
                        row["marker"]["detail"] = verdict.reason.clone().into();
                    }
                    deltas.push(json!({"op":"row.upserted","row":row}));
                }
                // Node：verifier 的用量只进会话用量，不计入目标 tokens。
                account_usage(s, &usage, now);
                s.node_goal_verification(now, (event_id, trace), Some(&verdict));
                let goal = s.goal.as_mut().unwrap();
                record(goal, &verdict, anchor, now);
                goal.verifying = None;
                if verdict.passed {
                    goal.set_status("complete", now);
                }
                // Node：plan 开启期间不自动续跑目标。
                let keep_running = verdict.continues()
                    && !goal.exhausted()
                    && s.queue.is_empty()
                    && !s.plan_enabled;
                let next = if keep_running {
                    let goal = s.goal.as_mut().unwrap();
                    let input = goal.loop_input.clone().unwrap_or_default();
                    goal.start_run(&input, now);
                    let frozen = goal.clone();
                    s.finish_rows("success", now);
                    deltas.extend(
                        s.rows
                            .iter()
                            .filter(|r| r["turnId"] == turn)
                            .map(|r| json!({"op":"row.upserted","row":r})),
                    );
                    let turn = self.clock.id();
                    let message = super::goal_commands::continuation(
                        s,
                        &frozen,
                        Some(&verdict),
                        (&turn, None),
                        now,
                    );
                    self.active.get_mut(id).unwrap().turn_id = turn.clone();
                    let text = super::goal_commands::continuation_text(&frozen, Some(&verdict));
                    self.node_goal_turn(id, &turn, &text);
                    let s = self.sessions.get_mut(id).unwrap();
                    deltas.push(json!({"op":"row.appended","row":s.rows.last().unwrap()}));
                    Some((frozen, message))
                } else {
                    None
                };
                let s = self.sessions.get_mut(id).unwrap();
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

/// Node `onTargetVerification`: the goal's verification list and status
/// (a cancelled verification lists nothing).
fn record(goal: &mut Goal, verdict: &Verdict, anchor: Value, now: u64) {
    goal.status = verdict.goal_status().into();
    if verdict.status == "cancelled" {
        return;
    }
    let mut verification = json!({"iteration":goal.iteration,"outcome":verdict.outcome(),"at":now,"anchorRowId":anchor});
    if !verdict.reason.is_empty() {
        verification["reason"] = verdict.reason.clone().into();
    }
    if let Some(next) = &verdict.next_action {
        verification["nextAction"] = next.clone().into();
    }
    goal.verifications.push(verification);
    let excess = goal.verifications.len().saturating_sub(20);
    goal.verifications.drain(..excess);
}

pub(super) fn account_usage(s: &mut crate::domain::session::Session, usage: &Value, now: u64) {
    if let Some(goal) = s.goal.as_mut() {
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
