//! Usage facts of one run, derived from its events (Node `recordModelUsageFact`,
//! `recordTurnUsageFact` and the tool usage recorders).
use super::{
    ErrorInfo, Fact, ModelFact, RECORDED_SOURCES, Tokens, ToolFact, TurnFact, usage_total,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Failure texts are stored up to this size (the result text can be tool output).
const ERROR_MESSAGE_BYTES: usize = 1024;

/// Who a run's facts belong to, fixed at run start.
#[derive(Clone, Debug, Default)]
pub struct Attribution {
    pub session_id: String,
    pub run_id: String,
    pub turn_id: String,
    pub trace_id: String,
    pub variant: Option<String>,
    pub mode: String,
    pub subagent: bool,
    /// Manual compaction, recorded in every outcome (Node `compact.ts`).
    pub compact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    Cancelled,
}

/// A logical model request in flight.
struct Probe {
    source: String,
    started_at: u64,
    provider: String,
    model: String,
    retries: u64,
    first_token_at: Option<u64>,
}

struct ToolProbe {
    name: String,
    started_at: u64,
    executing_at: Option<u64>,
    requested: bool,
}

pub struct RunUsage {
    who: Attribution,
    started_at: u64,
    requests: u64,
    request: Option<Probe>,
    /// A completed agent step waiting for its `ModelDone` (tool call count).
    step: Option<ModelFact>,
    tools: BTreeMap<String, ToolProbe>,
    first_model_start_at: Option<u64>,
    first_token_at: Option<u64>,
    retries: u64,
    tool_calls: u64,
    tool_errors: u64,
    tokens: Tokens,
    computed_total: u64,
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn truncated(message: &str) -> String {
    let mut end = message.len().min(ERROR_MESSAGE_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

impl RunUsage {
    pub fn new(who: Attribution, now: u64) -> Self {
        Self {
            who,
            started_at: now,
            requests: 0,
            request: None,
            step: None,
            tools: BTreeMap::new(),
            first_model_start_at: None,
            first_token_at: None,
            retries: 0,
            tool_calls: 0,
            tool_errors: 0,
            tokens: Tokens::default(),
            computed_total: 0,
        }
    }

    fn model_fact(&self, probe: Probe, status: &'static str, now: u64) -> ModelFact {
        let logical = format!("{}:{}", self.who.run_id, self.requests);
        ModelFact {
            id: format!("usage_model_{}_{logical}_0", probe.source),
            logical_request_id: logical,
            session_id: self.who.session_id.clone(),
            turn_id: Some(self.who.turn_id.clone()),
            trace_id: Some(self.who.trace_id.clone()),
            query_source: probe.source,
            provider_id: probe.provider,
            model_id: probe.model,
            variant: self.who.variant.clone(),
            agent: "zcode-agent".into(),
            mode: self.who.mode.clone(),
            task_type: if self.who.subagent {
                "subagent_child"
            } else {
                "interactive"
            }
            .into(),
            status,
            started_at: probe.started_at,
            first_token_at: probe.first_token_at,
            completed_at: now,
            retry_count: probe.retries,
            retryable: probe.retries > 0,
            ..ModelFact::default()
        }
    }

    /// One `ModelStatus` payload; returns the facts it completes.
    pub fn on_status(&mut self, status: &Value, now: u64) -> Vec<Fact> {
        let source = status["querySource"].as_str().unwrap_or("");
        if !RECORDED_SOURCES.contains(&source) {
            return vec![];
        }
        let mut facts = vec![];
        match status["type"].as_str().unwrap_or("") {
            "model_request_started" if status["attempt"].as_u64().unwrap_or(1) <= 1 => {
                // 同一 run 的记录来源请求串行：未等到 ModelDone 的完成步骤照常记录，
                // 没有终态的请求按失败收口，都不丢记录。
                if let Some(step) = self.step.take() {
                    facts.push(Fact::Model(Box::new(step)));
                }
                if let Some(probe) = self.request.take() {
                    facts.push(Fact::Model(Box::new(self.model_fact(probe, "error", now))));
                }
                self.requests += 1;
                self.first_model_start_at.get_or_insert(now);
                self.request = Some(Probe {
                    source: source.into(),
                    started_at: now,
                    provider: status["providerId"].as_str().unwrap_or("").into(),
                    model: status["modelId"].as_str().unwrap_or("").into(),
                    retries: 0,
                    first_token_at: None,
                });
            }
            "model_retry_scheduled" => {
                if let Some(probe) = &mut self.request {
                    probe.retries += 1;
                    self.retries += 1;
                }
            }
            "model_request_failed" if status["retryable"] != true => {
                if let Some(probe) = self.request.take() {
                    let cancelled = status["reason"] == "cancelled";
                    let mut fact =
                        self.model_fact(probe, if cancelled { "cancelled" } else { "error" }, now);
                    fact.context_exceeded = status["reason"] == "context_exceeded";
                    fact.error = ErrorInfo {
                        kind: text(&status["reason"]),
                        code: text(&status["errorCode"]),
                        message: text(&status["message"]),
                    };
                    facts.push(Fact::Model(Box::new(fact)));
                }
            }
            "model_request_completed" => {
                if let Some(probe) = self.request.take() {
                    let usage = &status["usage"];
                    let step = matches!(probe.source.as_str(), "main_turn" | "subagent");
                    let mut fact = self.model_fact(probe, "completed", now);
                    fact.finish_reason = text(&status["finishReason"]);
                    if usage.as_object().is_some_and(|u| !u.is_empty()) {
                        fact.tokens = Tokens::from_usage(usage);
                        fact.provider_total_tokens = usage["totalTokens"].as_u64();
                        fact.raw_usage = Some(usage.clone());
                        self.tokens.add(&fact.tokens);
                        self.computed_total += usage_total(usage);
                    }
                    if step {
                        self.step = Some(fact);
                    } else {
                        facts.push(Fact::Model(Box::new(fact)));
                    }
                }
            }
            _ => {}
        }
        facts
    }

    /// A text or reasoning delta of the agent step in flight.
    pub fn on_text(&mut self, now: u64) {
        if let Some(probe) = &mut self.request {
            probe.first_token_at.get_or_insert(now);
            self.first_token_at.get_or_insert(now);
        }
    }

    /// The step's assistant message committed.
    pub fn on_model_done(&mut self, message: Option<&Value>) -> Vec<Fact> {
        let Some(mut fact) = self.step.take() else {
            return vec![];
        };
        fact.tool_call_count = message
            .and_then(|m| m["tool_calls"].as_array())
            .map_or(0, |calls| calls.len() as u64);
        vec![Fact::Model(Box::new(fact))]
    }

    fn tool_fact(&self, id: &str, probe: &ToolProbe, status: &'static str) -> ToolFact {
        ToolFact {
            session_id: self.who.session_id.clone(),
            turn_id: Some(self.who.turn_id.clone()),
            trace_id: Some(self.who.trace_id.clone()),
            tool_call_id: id.into(),
            tool_name: probe.name.clone(),
            approval_status: if probe.requested { "requested" } else { "none" },
            status,
            started_at: probe.started_at,
            ..ToolFact::default()
        }
    }

    pub fn on_tool_start(&mut self, call: &Value, now: u64) -> Vec<Fact> {
        let Some(id) = call["id"].as_str() else {
            return vec![];
        };
        let probe = ToolProbe {
            name: text(&call["function"]["name"]).unwrap_or_else(|| "unknown".into()),
            started_at: now,
            executing_at: None,
            requested: false,
        };
        let fact = self.tool_fact(id, &probe, "running");
        self.tools.insert(id.into(), probe);
        self.tool_calls += 1;
        vec![Fact::Tool(Box::new(fact))]
    }

    pub fn on_permission(&mut self, call_id: &str) {
        if let Some(probe) = self.tools.get_mut(call_id) {
            probe.requested = true;
        }
    }

    pub fn on_tool_executing(&mut self, call_id: &str, now: u64) {
        if let Some(probe) = self.tools.get_mut(call_id) {
            probe.executing_at.get_or_insert(now);
        }
    }

    /// `(failed, denied, cancelled)`: the result's flags and whether the run was cancelled.
    pub fn on_tool_done(
        &mut self,
        call_id: &str,
        (failed, denied, cancelled): (bool, bool, bool),
        result: &str,
        now: u64,
    ) -> Vec<Fact> {
        let Some(probe) = self.tools.remove(call_id) else {
            return vec![];
        };
        let status = match (failed || denied, cancelled && !denied) {
            (false, _) => "completed",
            (true, true) => "cancelled",
            (true, false) => "error",
        };
        let mut fact = self.tool_fact(call_id, &probe, status);
        fact.approval_status = match (denied, probe.requested) {
            (true, _) => "denied",
            (false, true) => "allowed",
            (false, false) => "none",
        };
        fact.completed_at = Some(now);
        fact.duration_ms = probe.executing_at.map(|at| now.saturating_sub(at));
        fact.output_bytes = result.len() as u64;
        if failed || denied {
            self.tool_errors += 1;
            fact.cancelled_by_user = status == "cancelled";
            fact.error = ErrorInfo {
                kind: Some(
                    match status {
                        "cancelled" => "tool_cancelled",
                        _ if denied => "permission_denied",
                        _ => "tool_execution_failed",
                    }
                    .into(),
                ),
                code: None,
                message: Some(truncated(result)),
            };
        }
        vec![Fact::Tool(Box::new(fact))]
    }

    /// Facts still open when the run ends, closed with the run's outcome.
    fn close_open(&mut self, outcome: Outcome, failure: Option<&ErrorInfo>, now: u64) -> Vec<Fact> {
        let status = if outcome == Outcome::Cancelled {
            "cancelled"
        } else {
            "error"
        };
        let mut facts = vec![];
        if let Some(mut step) = self.step.take() {
            // 已报告完成但没有提交的步骤（终止的空响应、提交前结束）按 run 的结局收口。
            if outcome != Outcome::Completed {
                step.status = status;
                step.error = failure.cloned().unwrap_or_default();
            }
            facts.push(Fact::Model(Box::new(step)));
        }
        if let Some(probe) = self.request.take() {
            let mut fact = self.model_fact(probe, status, now);
            fact.error = failure.cloned().unwrap_or_default();
            facts.push(Fact::Model(Box::new(fact)));
        }
        facts
    }

    /// The run ended; `failure` is the model failure's `(reason, code)` when it failed on one.
    pub fn finish(
        &mut self,
        outcome: Outcome,
        failure: Option<(&str, &str)>,
        now: u64,
    ) -> Vec<Fact> {
        let error = match outcome {
            Outcome::Completed => ErrorInfo::default(),
            Outcome::Cancelled => ErrorInfo {
                kind: Some("turn_cancelled".into()),
                ..ErrorInfo::default()
            },
            Outcome::Failed => ErrorInfo {
                kind: Some(failure.map_or("runtime", |f| f.0).into()),
                code: failure.map(|f| f.1.to_owned()),
                message: None,
            },
        };
        let mut facts = self.close_open(outcome, Some(&error), now);
        let tools = std::mem::take(&mut self.tools);
        for (id, probe) in tools {
            let status = if outcome == Outcome::Cancelled {
                "cancelled"
            } else {
                "error"
            };
            let mut fact = self.tool_fact(&id, &probe, status);
            fact.completed_at = Some(now);
            fact.cancelled_by_user = status == "cancelled";
            facts.push(Fact::Tool(Box::new(fact)));
        }
        // Node：普通轮只在成功时写 turn_usage；手动压缩在每种结局都写。
        if outcome == Outcome::Completed || self.who.compact {
            facts.push(Fact::Turn(Box::new(TurnFact {
                session_id: self.who.session_id.clone(),
                turn_id: self.who.turn_id.clone(),
                trace_id: Some(self.who.trace_id.clone()),
                status: match outcome {
                    Outcome::Completed => "completed",
                    Outcome::Failed => "error",
                    Outcome::Cancelled => "cancelled",
                },
                started_at: self.started_at,
                first_model_start_at: self.first_model_start_at,
                first_token_at: self.first_token_at,
                completed_at: now,
                model_request_count: self.requests,
                model_retry_count: self.retries,
                tool_call_count: self.tool_calls,
                tool_error_count: self.tool_errors,
                tokens: self.tokens,
                computed_total_tokens: self.computed_total,
                cancelled_by_user: outcome == Outcome::Cancelled,
                context_exceeded: failure.is_some_and(|f| f.0 == "context_exceeded"),
                error,
            })));
        }
        facts
    }
}
