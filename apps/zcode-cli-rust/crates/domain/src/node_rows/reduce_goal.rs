//! Goal verification reductions (Node `ProductProjection.onTargetVerification`):
//! one `goalVerify` marker per `targetId_iteration`, created by whichever
//! lifecycle event comes first, and the goal/control state it moves.
use super::events::Event;
use super::facts::{js_string, truthy};
use super::projection::{Delta, Projection};
use super::synth_goals::template;
use serde_json::{Value, json};

/// `PROTOCOL_V4_LIMITS.goalVerificationsRetained`.
const GOAL_VERIFICATIONS_RETAINED: usize = 20;

impl Projection {
    /// Node `goalVerifyLifecycleKey`.
    fn goal_key(payload: &Value, iteration: &Value) -> String {
        if truthy(&payload["targetId"]) {
            format!(
                "{}_{}",
                template(payload.get("targetId")),
                js_string(iteration)
            )
        } else {
            template(payload.get("verificationId"))
        }
    }

    /// Node `goalVerifyTurnId`: the anchored assistant's turn, a known anchor
    /// turn, else the event's turn.
    fn goal_turn(&self, payload: &Value, event: &Event) -> String {
        if truthy(&payload["anchorAssistantMessageId"]) {
            let message = js_string(&payload["anchorAssistantMessageId"]);
            let row = self
                .row_for_message(&message)
                .and_then(|id| self.find_row(id));
            if let Some(row) = row {
                return row["turnId"].as_str().unwrap_or("").to_owned();
            }
        }
        if truthy(&payload["anchorTurnId"]) {
            let anchor = js_string(&payload["anchorTurnId"]);
            let mapped = self
                .product_by_runtime
                .get(&anchor)
                .cloned()
                .unwrap_or(anchor);
            if self.headers.contains_key(&mapped) {
                return mapped;
            }
        }
        self.turn_of(event)
    }

    fn goal_marker_row(&mut self, event: &Event, key: &str, marker: Value) -> (Delta, u64) {
        let existing = self.goal_markers.get(key).and_then(|id| self.find_row(*id));
        if let Some(row) = existing.filter(|row| row["kind"] == "timelineMarker") {
            let mut row = row.clone();
            row["marker"] = marker;
            let id = row["rowId"].as_u64().unwrap_or(0);
            return (Delta::Upsert(row), id);
        }
        let turn = self.goal_turn(&event.payload, event);
        let mut row = self.row_base(event, &turn, key);
        let id = row["rowId"].as_u64().unwrap_or(0);
        row["kind"] = "timelineMarker".into();
        row["lane"] = "turnTailBoundary".into();
        row["marker"] = marker;
        self.goal_markers.insert(key.to_owned(), id);
        (Delta::Append(row), id)
    }

    /// Node `onTargetVerification`.
    pub fn on_target_verification(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let goal = self.state["goal"].clone();
        let has_goal = goal.is_object();
        let works = self.state["control"]["activeWorks"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let others: Vec<Value> = works
            .iter()
            .filter(|w| w["kind"] != "goalVerifier")
            .cloned()
            .collect();
        let status = payload["status"].as_str().unwrap_or("");
        let iteration_of = |fallback: Value| match &payload["goalIteration"] {
            Value::Null => fallback,
            other => other.clone(),
        };
        if status == "started" {
            let next = goal["iteration"]
                .as_f64()
                .map_or(json!(1), |n| super::events::num(n + 1.0));
            let iteration = iteration_of(if has_goal { next } else { json!(1) });
            let key = Self::goal_key(payload, &iteration);
            let verifying = has_goal.then(|| {
                let mut goal = goal.clone();
                goal["status"] = "verifying".into();
                goal["iteration"] = iteration.clone();
                goal
            });
            let mut work = json!({"kind": "goalVerifier"});
            if truthy(&payload["foregroundExecutionId"]) {
                work["foregroundExecutionId"] = payload["foregroundExecutionId"].clone();
            }
            work["startedAt"] = event.at.into();
            let target = if others.is_empty() {
                "goalVerifier"
            } else {
                "mixed"
            };
            let mut active = others;
            active.push(work);
            let control = json!({"phase": "running", "sessionEnded": false, "activeWorks": active,
                "canStop": true, "stopState": "stoppable", "stopTargetKind": target,
                "lastError": null, "apiRetry": null});
            let control = Delta::State(self.control_patch(control, verifying, None));
            let marker =
                json!({"type": "goalVerify", "iteration": iteration, "outcome": "running"});
            let (row, _) = self.goal_marker_row(event, &key, marker);
            return vec![row, control];
        }
        let iteration = iteration_of(match &goal["iteration"] {
            Value::Null => json!(1),
            other => other.clone(),
        });
        let verification = &payload["verification"];
        let outcome = match status {
            "completed" if truthy(&verification["passed"]) => "pass",
            "completed" => "notSatisfied",
            _ => "failed",
        };
        let goal_status = match (status, outcome) {
            ("cancelled", _) => "paused",
            ("failed_closed", _) => "failed",
            (_, "pass") => "verified",
            _ => "notSatisfied",
        };
        let key = Self::goal_key(payload, &iteration);
        let mut marker = json!({"type": "goalVerify", "iteration": iteration, "outcome": outcome});
        if status == "cancelled" {
            marker["detail"] = "cancelled".into();
        } else if truthy(&verification["reason"]) {
            marker["detail"] = verification["reason"].clone();
        }
        let (row, anchor) = self.goal_marker_row(event, &key, marker);
        let mut deltas = vec![row];
        let had_verifier = works.iter().any(|w| w["kind"] == "goalVerifier");
        let patch_control = had_verifier || goal["status"] == "verifying";
        let phase = match status {
            "cancelled" => "completedInterrupted",
            "failed_closed" => "error",
            _ => "completedSuccess",
        };
        let queue = &self.state["queue"];
        let held = (status == "cancelled"
            && payload["preserveQueueAutoDrainOnCancel"] != true
            && queue["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty()))
        .then(|| {
            let mut queue = queue.clone();
            queue["autoDrain"] = false.into();
            queue["pauseReason"] = "stopped".into();
            queue
        });
        let settled = if others.is_empty() {
            json!({"phase": phase, "sessionEnded": phase != "error", "activeWorks": [],
                "canStop": false, "stopState": "idle", "stopTargetKind": "unknown"})
        } else {
            json!({"phase": phase, "sessionEnded": phase != "error", "activeWorks": others,
                "stopTargetKind": "mixed"})
        };
        if !has_goal {
            // 冷合成流里没有 goal：只有确实在跑 verifier 时才收口 control。
            if patch_control {
                deltas.push(Delta::State(self.control_patch(settled, None, held)));
            }
            return deltas;
        }
        let mut verifications = goal["verifications"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if status != "cancelled" {
            let mut entry = json!({"iteration": iteration, "outcome": outcome, "at": event.at,
                "anchorRowId": anchor});
            for key in ["reason", "nextAction"] {
                if truthy(&verification[key]) {
                    entry[key] = verification[key].clone();
                }
            }
            verifications.push(entry);
            let excess = verifications
                .len()
                .saturating_sub(GOAL_VERIFICATIONS_RETAINED);
            verifications.drain(..excess);
        }
        let mut next = goal;
        next["status"] = goal_status.into();
        next["iteration"] = iteration;
        next["verifications"] = verifications.into();
        let patch = if patch_control {
            self.control_patch(settled, Some(next), held)
        } else {
            self.goal_patch(next)
        };
        deltas.push(Delta::State(patch));
        deltas
    }
}
