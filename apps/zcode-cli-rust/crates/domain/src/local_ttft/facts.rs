//! The `LocalTtftFacts` record and how live facts advance it (Node
//! `LocalTtftRecorder.fact` / `event` / `output`, `local-ttft-compaction.ts`).
use super::{MAX_DETAILS, Recorder, RequestState};
use serde::Serialize;
use serde_json::Value;

/// Node `localTtftDetailSchema`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub id: String,
    pub stage: &'static str,
    pub start: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    pub source: &'static str,
}

/// Node `LocalTtftFacts` (schema order).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    pub version: u8,
    pub observation_id: String,
    pub instance_id: String,
    pub command_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub details: Vec<Detail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub send_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_invalid: Option<bool>,
    pub received_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admitted_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<&'static str>,
}

impl Facts {
    pub(super) fn new(
        (observation, instance, command): (&str, &str, &str),
        session: Option<&str>,
        now: f64,
        send_mode: &'static str,
    ) -> Self {
        Self {
            version: 1,
            observation_id: observation.into(),
            instance_id: instance.into(),
            command_id: command.into(),
            session_id: session.map(str::to_owned),
            turn_id: None,
            query_id: None,
            request_id: None,
            logical_call_id: None,
            provider: None,
            model: None,
            details: vec![],
            truncated: None,
            revision: None,
            send_mode,
            clock_invalid: None,
            received_at: now,
            admitted_at: None,
            execution_at: None,
            request_at: None,
            output_at: None,
            output_kind: None,
            terminal: None,
        }
    }

    /// The record without its revision (Node's checkpoint signature).
    pub(super) fn signature(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(map) = value.as_object_mut() {
            map.remove("revision");
        }
        value.to_string()
    }

    /// The facts on a frame: with the CLI version and the conversation turn.
    pub fn attached(&self, cli_version: &str, product_turn: Option<&str>) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        value["cliVersion"] = cli_version.into();
        if let Some(turn) = product_turn {
            value["productTurnId"] = turn.into();
        }
        value
    }

    fn push(&mut self, detail: Detail) {
        if self.details.len() < MAX_DETAILS {
            self.details.push(detail);
        } else {
            self.truncated = Some(true);
        }
    }
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

impl Recorder {
    /// Node `fact`: a live conversation telemetry fact; `execution` is the
    /// turn's `executionStartedAt` for `turn.started`.
    pub fn fact(&mut self, fact: &Value, execution: Option<f64>, now: f64) {
        self.prune(now);
        let Some(command) = text(&fact["sourceCommandId"]).map(str::to_owned) else {
            return;
        };
        let session = fact["sessionId"].as_str().unwrap_or("");
        let turn = text(&fact["turnId"]).map(str::to_owned);
        let state = self.request_state.get(&command).copied();
        let Some(record) = self.record(&command) else {
            return;
        };
        if record.session_id.as_deref().is_some_and(|s| s != session) {
            return;
        }
        let kind = fact["kind"].as_str().unwrap_or("");
        if kind == "turn.started" {
            record.session_id = Some(session.into());
            record.turn_id = turn;
            if record.execution_at.is_none() {
                record.execution_at = execution;
            }
            return self.checkpoint(&command);
        }
        if record.turn_id.is_none() && kind == "turn.terminal" {
            record.turn_id.clone_from(&turn);
        }
        if record.turn_id != turn {
            return;
        }
        let mut next_state = state;
        if kind == "model.request.status" && record.output_at.is_none() {
            let source = fact["querySource"].as_str();
            let role = match source {
                Some("main_turn") => "response",
                Some("compact") => "preparation",
                _ => return,
            };
            let request = fact["requestId"].as_str().unwrap_or("");
            let id = format!("attempt:{request}");
            let status = fact["status"].as_str().unwrap_or("");
            let known = record.details.iter().any(|d| d.id == id);
            if status == "model_request_started" && !known {
                next_state = Some(if role == "response" {
                    RequestState::Response
                } else {
                    RequestState::Preparation
                });
                record.push(Detail {
                    id: id.clone(),
                    stage: "attempt",
                    start: now,
                    end: None,
                    outcome: None,
                    request_id: Some(request.into()),
                    logical_call_id: None,
                    role: Some(role),
                    source: "cli",
                });
                for wait in record.details.iter_mut() {
                    if wait.stage == "retry_wait" && wait.end.is_none() && wait.role == Some(role) {
                        wait.end = Some(now);
                        wait.outcome = Some("completed");
                    }
                }
                if role == "response" {
                    record.request_at.get_or_insert(now);
                    if record.query_id.is_none() {
                        record.query_id = text(&fact["queryId"]).map(str::to_owned);
                    }
                    record.request_id = Some(request.into());
                    record.model = fact["modelId"]
                        .as_str()
                        .map(|m| m.chars().take(128).collect());
                    record.provider = fact["providerId"]
                        .as_str()
                        .map(|p| p.chars().take(128).collect());
                }
            }
            let ended = matches!(status, "model_request_failed" | "model_request_completed");
            let current = record.request_id.as_deref() == Some(request);
            if let Some(attempt) = record.details.iter_mut().find(|d| d.id == id)
                && ended
                && attempt.end.is_none()
            {
                attempt.end = Some(now);
                let failed = status == "model_request_failed";
                attempt.outcome = Some(if failed { "failed" } else { "completed" });
                if role == "response" && failed && current {
                    next_state = Some(RequestState::Failed);
                }
            }
            let retry = format!("retry:{request}");
            if status == "model_retry_scheduled" && !record.details.iter().any(|d| d.id == retry) {
                record.push(Detail {
                    id: retry,
                    stage: "retry_wait",
                    start: now,
                    end: None,
                    outcome: None,
                    request_id: Some(request.into()),
                    logical_call_id: None,
                    role: Some(role),
                    source: "cli",
                });
            }
        }
        if let Some(state) = next_state {
            self.request_state.insert(command.clone(), state);
        }
        self.checkpoint(&command);
        let scheduled = kind == "tool.lifecycle"
            && fact["phase"] == "scheduled"
            && fact.get("parentToolCallId").is_none();
        if scheduled {
            self.output(session, turn.as_deref(), "tool", now);
        }
        if kind == "turn.terminal" {
            if let Some(record) = self.record(&command) {
                record.terminal = Some(match fact["status"].as_str() {
                    Some("success") => "completed",
                    Some("failed") => "failed",
                    _ => "cancelled",
                });
            }
            self.checkpoint(&command);
            self.retire(&command);
        }
    }

    /// Node `event(ModelNetworkStatus)`: the logical call of the latest record
    /// of `session` (its current request) and of the request's attempt.
    pub fn logical_call(&mut self, session: &str, request: &str, logical: &str, now: f64) {
        let Some(command) = self.for_session(session, None, now).map(|r| r.command_id) else {
            return;
        };
        let Some(record) = self.record(&command) else {
            return;
        };
        if record.request_id.as_deref() == Some(request) {
            record.logical_call_id = Some(logical.into());
        }
        let attempt = record
            .details
            .iter_mut()
            .find(|d| d.stage == "attempt" && d.request_id.as_deref() == Some(request));
        if let Some(attempt) = attempt {
            attempt.logical_call_id = Some(logical.into());
        }
        self.checkpoint(&command);
    }

    /// Node `output`: the first visible output of the turn's main response.
    pub fn output(&mut self, session: &str, turn: Option<&str>, kind: &'static str, now: f64) {
        let mut retired = vec![];
        for record in &mut self.records {
            let state = self.request_state.get(&record.command_id);
            if matches!(
                state,
                Some(RequestState::Failed | RequestState::Preparation)
            ) {
                continue;
            }
            let ours = record.session_id.as_deref() == Some(session)
                && record.turn_id.as_deref() == turn
                && record.request_at.is_some();
            if ours && record.output_at.is_none() {
                record.output_at = Some(now);
                record.output_kind = Some(kind);
                let attempt = format!("attempt:{}", record.request_id.as_deref().unwrap_or(""));
                if let Some(attempt) = record.details.iter_mut().find(|d| d.id == attempt)
                    && attempt.end.is_none()
                {
                    attempt.end = Some(now);
                    attempt.outcome = Some("first_output");
                }
            }
            if ours && kind == "text" {
                retired.push(record.command_id.clone());
            }
        }
        for command in retired {
            self.retire(&command);
        }
    }

    /// Node `observeLocalTtftCompaction` of the latest record of `session`.
    pub fn compaction(
        &mut self,
        session: &str,
        turn: Option<&str>,
        event: (&str, &Value),
        now: f64,
    ) {
        let (kind, payload) = event;
        let Some(command) = self.for_session(session, None, now).map(|r| r.command_id) else {
            return;
        };
        let Some(record) = self.record(&command) else {
            return;
        };
        if record.turn_id.as_deref() != turn || record.output_at.is_some() {
            return;
        }
        let id = format!("compact:{}", payload["operationId"].as_str().unwrap_or(""));
        let known = record.details.iter().any(|d| d.id == id);
        if !known && kind == "compact_started" {
            record.push(Detail {
                id: id.clone(),
                stage: "compaction",
                start: now,
                end: None,
                outcome: None,
                request_id: None,
                logical_call_id: None,
                role: None,
                source: "cli",
            });
        }
        if kind != "compact_started"
            && let Some(detail) = record.details.iter_mut().find(|d| d.id == id)
            && detail.end.is_none()
        {
            if now < detail.start {
                record.clock_invalid = Some(true);
            } else {
                detail.end = Some(now);
                detail.outcome = Some(match payload["status"].as_str() {
                    Some("interrupted") => "cancelled",
                    _ if kind == "compact_completed" => "completed",
                    _ => "failed",
                });
            }
        }
        self.checkpoint(&command);
    }
}
