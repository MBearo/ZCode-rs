//! Turn completion reductions (Node `ProductProjection.onTurnComplete`,
//! `onTurnError`, `upsertTurnHeader` and `markStableForkAssistant`).
use super::events::Event;
use super::projection::{Delta, Projection};
use super::reduce_turn::put_truthy;
use serde_json::{Value, json};

/// Node `mapTurnResultToHeaderState`.
fn header_state(result: &Value) -> &'static str {
    match result.as_str() {
        Some("success") => "completedSuccess",
        Some("cancelled") => "completedInterrupted",
        _ => "failed",
    }
}

impl Projection {
    /// Node `upsertTurnHeader`.
    pub fn upsert_header(
        &self,
        event: &Event,
        state: &str,
        active_ms: Option<Value>,
        rounds: Option<&Value>,
    ) -> Vec<Delta> {
        let Some(row) = self.header_for(event) else {
            return Vec::new();
        };
        let mut row = row.clone();
        row["state"] = state.into();
        row["endedAt"] = event.at.into();
        if let Some(active) = active_ms {
            row["activeMs"] = active;
        }
        if let Some(rounds) = rounds {
            row["historyRoundCount"] = rounds.clone();
        }
        if let Some(segments) = row.get("workSegments").cloned() {
            row["workSegments"] = complete_segments(&segments, event.at);
        }
        vec![Delta::Upsert(row)]
    }

    /// Node `leaveDraftAfterControlOnlyTurn`.
    fn leave_draft(&self, phase: &str) -> Vec<Delta> {
        if self.state["control"]["phase"] != "draft" {
            return Vec::new();
        }
        let control = json!({"phase": phase, "sessionEnded": phase != "error", "canStop": false,
            "stopState": "idle", "stopTargetKind": "unknown", "activeWorks": []});
        vec![Delta::State(self.control_patch(control, None, None))]
    }

    fn control_only(&self, event: &Event) -> bool {
        self.header_for(event)
            .is_some_and(|h| h["executionKind"] == "controlOnly")
    }

    /// Node `onTurnComplete`.
    pub fn on_turn_complete(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        self.continuation_text = None;
        let state = header_state(&payload["resultType"]);
        let result = payload["resultType"].as_str().unwrap_or("");
        let rounds = payload.get("historyRoundCount");
        if self.control_only(event) {
            let mut deltas = self.upsert_header(event, state, None, rounds);
            let phase = if result == "success" {
                "completedSuccess"
            } else {
                "completedInterrupted"
            };
            deltas.extend(self.leave_draft(phase));
            self.current_turn = None;
            self.current_model_only = false;
            return deltas;
        }
        let phase = match result {
            "success" => "completedSuccess",
            "cancelled" => "completedInterrupted",
            _ => "error",
        };
        let goal = &self.state["goal"];
        let paused_goal = (result == "cancelled"
            && matches!(goal["status"].as_str(), Some("active" | "verifying")))
        .then(|| {
            let mut goal = goal.clone();
            goal["status"] = "paused".into();
            goal
        });
        let queue = &self.state["queue"];
        let held = (result == "cancelled"
            && payload["preserveQueueAutoDrainOnCancel"] != true
            && queue["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
            && (queue["autoDrain"] != false || queue["pauseReason"] != "stopped"))
            .then(|| held_queue(queue, "stopped"));
        let close = if result == "success" {
            "complete"
        } else {
            "interrupted"
        };
        let mut deltas = self.close_streaming_rows(close);
        let tool_status = if result == "cancelled" {
            "cancelled"
        } else {
            "error"
        };
        deltas.extend(self.close_open_tool_rows(event, tool_status));
        let active = self.active_ms_for_completion(event, payload.get("duration"));
        deltas.extend(self.upsert_header(event, state, active, rounds));
        if result == "success" {
            deltas.extend(self.mark_stable_fork(event));
        }
        let control = json!({"phase": phase, "sessionEnded": phase != "error", "canStop": false,
            "stopState": "idle", "stopTargetKind": "unknown", "activeWorks": [], "apiRetry": null});
        deltas.push(Delta::State(self.control_patch(control, paused_goal, held)));
        self.current_turn = None;
        self.current_model_only = false;
        deltas
    }

    /// Node `onTurnError` (it keeps the current runtime turn, like Node).
    pub fn on_turn_error(&mut self, event: &Event) -> Vec<Delta> {
        let error = &event.payload["error"];
        self.continuation_text = None;
        if self.control_only(event) {
            let mut deltas = self.upsert_header(event, "failed", None, None);
            deltas.extend(self.leave_draft("error"));
            self.current_turn = None;
            self.current_model_only = false;
            return deltas;
        }
        let queue = &self.state["queue"];
        let held = queue["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
            .then(|| held_queue(queue, "error"));
        let mut deltas = self.close_streaming_rows("interrupted");
        deltas.extend(self.close_open_tool_rows(event, "error"));
        deltas.extend(self.upsert_header(event, "failed", None, None));
        let code = [&error["code"], &error["type"]]
            .into_iter()
            .find(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| "fault.runtime.unknown".into());
        let source = match &error["attribution"]["source"] {
            Value::Null => "runtime".into(),
            other => other.clone(),
        };
        let recoverable = match &error["retryable"] {
            Value::Null => true.into(),
            other => other.clone(),
        };
        let mut last = json!({"code": code, "message": error["message"], "recoverable": recoverable,
            "at": event.at, "source": source, "traceId": event.trace});
        for key in [
            "detail",
            "underlyingErrorMessage",
            "underlyingErrorDetail",
            "attribution",
        ] {
            put_truthy(&mut last, key, &error[key]);
        }
        let control = json!({"phase": "error", "sessionEnded": false, "canStop": false,
            "stopState": "idle", "stopTargetKind": "unknown", "activeWorks": [],
            "lastError": last, "apiRetry": null});
        deltas.push(Delta::State(self.control_patch(control, None, held)));
        deltas
    }

    /// Node `activeMsForCompletion`: only a turn split by queue drains
    /// measures its last segment; otherwise the transcript duration stands.
    fn active_ms_for_completion(&self, event: &Event, duration: Option<&Value>) -> Option<Value> {
        let runtime = self.runtime_turn(event);
        match self.product_started {
            Some(started) if self.split_ordinal.contains_key(&runtime) => {
                Some((event.at - started).max(0).into())
            }
            _ => duration.cloned(),
        }
    }

    /// Node `markStableForkAssistant`: the turn's last assistant text becomes a
    /// fork target.
    fn mark_stable_fork(&self, event: &Event) -> Vec<Delta> {
        let turn = self.turn_of(event);
        let start = self
            .headers
            .get(&turn)
            .and_then(|id| self.row_index(*id))
            .map_or(0, |index| index + 1);
        let found = self.rows[start.min(self.rows.len())..]
            .iter()
            .rev()
            .find(|row| row["kind"] == "assistantText" && row["turnId"] == turn.as_str());
        let Some(row) = found.filter(|row| {
            self.message_by_row
                .contains_key(&row["rowId"].as_u64().unwrap_or(0))
        }) else {
            return Vec::new();
        };
        let mut row = row.clone();
        row["state"] = "complete".into();
        let mut actions = row["actions"].as_object().cloned().unwrap_or_default();
        actions.insert("canFork".into(), true.into());
        row["actions"] = Value::Object(actions);
        vec![Delta::Upsert(row)]
    }
}

pub(super) fn held_queue(queue: &Value, reason: &str) -> Value {
    let mut queue = queue.clone();
    queue["autoDrain"] = false.into();
    queue["pauseReason"] = reason.into();
    queue
}

/// Node `completeWorkSegments`: closes the last open segment.
pub fn complete_segments(segments: &Value, ended: i64) -> Value {
    let list = segments.as_array().cloned().unwrap_or_default();
    let last = list.len().saturating_sub(1);
    Value::Array(
        list.into_iter()
            .enumerate()
            .map(|(index, mut segment)| {
                if index == last && segment.get("endedAt").is_none() {
                    let started = segment["startedAt"].as_i64().unwrap_or(0);
                    segment["endedAt"] = ended.into();
                    segment["activeMs"] = (ended - started).max(0).into();
                }
                segment
            })
            .collect(),
    )
}
