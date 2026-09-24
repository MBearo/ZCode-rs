//! Boundary marker and goal reductions (Node `ProductProjection.onCompactLifecycle`,
//! `onTargetChanged`, `onSessionForked`).
use super::events::{Event, num};
use super::facts::{js_string, truthy};
use super::projection::{Delta, Projection};
use super::synth_goals::template;
use serde_json::{Map, Value, json};

/// Node `mapCompactMarkerStatus`.
fn marker_status(status: &Value) -> &'static str {
    match status.as_str() {
        Some("started" | "retrying") => "running",
        Some("completed") => "success",
        Some("skipped") => "noop",
        Some("interrupted") => "cancelled",
        _ => "failed",
    }
}

/// Node `mapGoalStatus`.
fn goal_status(status: &Value) -> &'static str {
    match status.as_str() {
        Some("active") => "active",
        Some("complete") => "verified",
        _ => "paused",
    }
}

fn goal_of(target: &Value, base: Option<&Value>) -> Value {
    let mut goal = base.cloned().unwrap_or_else(|| json!({}));
    goal["targetId"] = target["targetID"].clone();
    goal["objective"] = target["objective"].clone();
    goal["summaryTitle"] = target["summaryTitle"].clone();
    goal["timeUsedSeconds"] = target["timeUsedSeconds"].clone();
    goal["activeRunStartedAtMs"] = target["activeRunStartedAtMs"].clone();
    goal["status"] = goal_status(&target["status"]).into();
    goal
}

impl Projection {
    /// Node `rowIdForMessageId`.
    pub fn row_for_message(&self, message: &str) -> Option<u64> {
        if let Some(id) = self.continuation_by_message.get(message) {
            return Some(*id);
        }
        self.message_by_row
            .iter()
            .find(|(_, m)| m.as_str() == message)
            .map(|(id, _)| *id)
    }

    /// Node `onCompactLifecycle`: one marker row per operation.
    pub fn on_compact(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let operation = template(payload.get("operationId"));
        let existing = self
            .compact_markers
            .get(&operation)
            .and_then(|id| self.find_row(*id))
            .cloned();
        let prev = existing
            .as_ref()
            .filter(|row| row["kind"] == "timelineMarker" && row["marker"]["type"] == "compact")
            .map(|row| row["marker"].clone());
        let status = marker_status(&payload["status"]);
        if status == "success" {
            let coverage = [&payload["tailStartMessageId"], &payload["anchorMessageId"]]
                .into_iter()
                .find(|v| !v.is_null())
                .filter(|v| truthy(v))
                .and_then(|m| self.row_for_message(&js_string(m)));
            if let Some(coverage) = coverage {
                let boundary = self.stable_compact_boundary.unwrap_or(0).max(coverage);
                self.stable_compact_boundary = Some(boundary);
                for (row, entity) in &self.entity_by_row {
                    if *row > boundary {
                        continue;
                    }
                    if let Some(target) = self.edit_targets.get_mut(entity) {
                        target["coveredByStableCompact"] = true.into();
                    }
                }
            }
        }
        let prev_field = |key: &str| {
            prev.as_ref()
                .map(|m| m[key].clone())
                .filter(|v| !v.is_null())
        };
        let tokens_after = [
            &payload["truePostCompactTokenCount"],
            &payload["postCompactTokenCount"],
        ]
        .into_iter()
        .find(|v| !v.is_null())
        .cloned()
        .or_else(|| prev_field("tokensAfter"));
        let origin = prev_field("origin").unwrap_or_else(|| {
            if payload["trigger"] == "manual" {
                "manual"
            } else {
                "auto"
            }
            .into()
        });
        let mut marker = json!({"type": "compact", "origin": origin, "status": status});
        let before = payload
            .get("preCompactTokenCount")
            .filter(|v| !v.is_null())
            .cloned();
        if payload.get("preCompactTokenCount").is_some() || prev_field("tokensBefore").is_some() {
            marker["tokensBefore"] = before
                .or_else(|| prev_field("tokensBefore"))
                .unwrap_or(Value::Null);
        }
        if let Some(after) = &tokens_after {
            marker["tokensAfter"] = after.clone();
        }
        let summary = payload.get("summaryMessageId");
        if summary.is_some() || prev_field("summaryRef").is_some_and(|v| truthy(&v)) {
            marker["summaryRef"] = match summary {
                Some(id) => js_string(id).into(),
                None => prev_field("summaryRef").unwrap_or(Value::Null),
            };
        }
        let mut deltas = Vec::new();
        let source = payload
            .get("sourceCommandId")
            .filter(|v| truthy(v))
            .cloned();
        match existing.filter(|row| row["kind"] == "timelineMarker") {
            Some(mut row) => {
                row["marker"] = marker;
                if let Some(source) = source {
                    row["sourceCommandId"] = source;
                }
                deltas.push(Delta::Upsert(row));
            }
            None => {
                let turn = self.turn_of(event);
                let mut row = self.row_base(event, &turn, &operation);
                row["kind"] = "timelineMarker".into();
                row["lane"] = "assistantWork".into();
                row["marker"] = marker;
                if let Some(source) = source {
                    row["sourceCommandId"] = source;
                }
                self.compact_markers
                    .insert(operation, row["rowId"].as_u64().unwrap_or(0));
                deltas.push(Delta::Append(row));
            }
        }
        let works = self.state["control"]["activeWorks"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let others: Vec<Value> = works
            .into_iter()
            .filter(|w| w["kind"] != "compact")
            .collect();
        let control = if status == "running" {
            let target = if others.is_empty() {
                "compact"
            } else {
                "mixed"
            };
            let mut works = others;
            works.push(json!({"kind": "compact", "startedAt": event.at}));
            json!({"activeWorks": works, "canStop": true, "stopState": "stoppable", "stopTargetKind": target})
        } else if others.is_empty() {
            json!({"activeWorks": [], "canStop": false, "stopState": "idle", "stopTargetKind": "unknown"})
        } else {
            json!({ "activeWorks": others })
        };
        deltas.push(Delta::State(self.control_patch(control, None, None)));
        if let (true, Some(after)) = (status == "success", tokens_after.and_then(|v| v.as_f64())) {
            self.window_used = after;
            let usage = &self.state["usage"];
            let max = usage["contextWindow"]["maxTokens"]
                .as_f64()
                .or(self.window_max);
            let mut usage = usage.clone();
            usage["contextWindow"] = match max {
                None => Value::Null,
                Some(max) => json!({"usedTokens": num(after), "maxTokens": num(max),
                    "autoCompactThresholdTokens": self.state["usage"]["contextWindow"]["autoCompactThresholdTokens"]}),
            };
            let mut patch = Map::new();
            patch.insert("usage".into(), usage);
            deltas.push(Delta::State(patch));
        }
        deltas
    }

    /// Node `onTargetChanged`.
    pub fn on_target_changed(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let target = &payload["target"];
        match payload["action"].as_str() {
            Some("set") => {
                if !truthy(target) {
                    return Vec::new();
                }
                let mut goal = goal_of(target, None);
                goal["iteration"] = 0.into();
                goal["verifications"] = json!([]);
                goal["iterations"] = json!([]);
                vec![Delta::State(self.goal_patch(goal))]
            }
            Some("cleared") => {
                if self.state["goal"].is_null() {
                    return Vec::new();
                }
                vec![Delta::State(self.goal_patch(Value::Null))]
            }
            _ => {
                let goal = &self.state["goal"];
                if goal.is_null() || !truthy(target) {
                    return Vec::new();
                }
                let next = goal_of(target, Some(goal));
                let same = [
                    "targetId",
                    "objective",
                    "summaryTitle",
                    "timeUsedSeconds",
                    "activeRunStartedAtMs",
                    "status",
                ]
                .iter()
                .all(|key| next[*key] == goal[*key]);
                if same {
                    return Vec::new();
                }
                vec![Delta::State(self.goal_patch(next))]
            }
        }
    }

    /// Node `onSessionForked`: the child's fork notice (never on the parent).
    pub fn on_session_forked(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let parent = js_string(&payload["originalSessionId"]);
        if parent == self.session_id {
            return Vec::new();
        }
        let target = match &payload["targetMessageId"] {
            Value::Null => "unknown".to_owned(),
            other => js_string(other),
        };
        let turn = self.turn_of(event);
        let mut row = self.row_base(event, &turn, &format!("fork:{parent}:{target}"));
        row["kind"] = "timelineMarker".into();
        row["lane"] = "turnTailBoundary".into();
        row["marker"] = json!({"type": "forkNotice", "parentSessionId": parent, "parentRowId": 0});
        vec![Delta::Append(row)]
    }
}
