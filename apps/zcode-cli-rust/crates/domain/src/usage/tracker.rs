//! Usage facts of one run, derived from its events (Node `recordModelUsageFact`
//! and `recordTurnUsageFact`; tools in `tracker_tools.rs`). Spec
//! rust-m9-usage-logs §2.3.
use super::{ErrorInfo, Fact, ModelFact, RECORDED_SOURCES, Tokens, TurnFact, usage_total};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[path = "tracker_tools.rs"]
mod tools;
pub use tools::ToolEnd;

/// Who a run's facts belong to, fixed at run start.
#[derive(Clone, Debug, Default)]
pub struct Attribution {
    pub session_id: String,
    pub run_id: String,
    pub turn_id: String,
    pub trace_id: String,
    pub variant: Option<String>,
    pub mode: String,
    /// Node `runtime.config.agentName`: `zcode-agent`, a child `zcode-<type>`.
    pub agent: String,
    pub subagent: bool,
    /// Manual compaction (no user message; Node `compact.ts`).
    pub compact: bool,
}

/// The Node message ids around one model step, from the Node journal.
#[derive(Clone, Debug, Default)]
pub struct StepIds {
    pub user: Option<String>,
    pub assistant: Option<String>,
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
    span: String,
    attempt: u64,
    started_at: u64,
    provider: String,
    model: String,
    retries: u64,
    first_token_at: Option<u64>,
}

pub struct RunUsage {
    who: Attribution,
    started_at: u64,
    request: Option<Probe>,
    /// A completed agent step waiting for its `ModelDone` (tool call count).
    step: Option<ModelFact>,
    tools: BTreeMap<String, tools::ToolProbe>,
    /// Compaction requests retried after prompt-too-long (Node `attemptIndex`).
    compact_attempts: u64,
    requests: u64,
    first_model_start_at: Option<u64>,
    first_token_at: Option<u64>,
    retries: u64,
    tool_calls: u64,
    tool_errors: u64,
    tokens: Tokens,
    computed_total: u64,
    /// Some request reported usage (Node `aggregateModelUsage` is otherwise undefined).
    usage_seen: bool,
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Agent steps: their usage waits for the committed assistant message.
fn step_source(source: &str) -> bool {
    matches!(source, "main_turn" | "subagent")
}

/// Sources in the turn's own events (Node `events`): the verifier keeps its own.
fn in_turn(source: &str) -> bool {
    source != "target_completion_verification"
}

impl RunUsage {
    pub fn new(who: Attribution, now: u64) -> Self {
        Self {
            who,
            started_at: now,
            request: None,
            step: None,
            tools: BTreeMap::new(),
            compact_attempts: 0,
            requests: 0,
            first_model_start_at: None,
            first_token_at: None,
            retries: 0,
            tool_calls: 0,
            tool_errors: 0,
            tokens: Tokens::default(),
            computed_total: 0,
            usage_seen: false,
        }
    }

    fn model_fact(&self, probe: Probe, status: &'static str, ids: &StepIds, now: u64) -> ModelFact {
        let step = step_source(&probe.source);
        let assistant = ids.assistant.clone().filter(|_| step);
        // Node：step 以 assistant 消息为逻辑请求 id，其他来源用 span id。
        let logical = assistant.clone().unwrap_or_else(|| probe.span.clone());
        ModelFact {
            id: format!("usage_model_{}_{logical}_{}", probe.source, probe.attempt),
            logical_request_id: logical,
            attempt_index: probe.attempt,
            session_id: self.who.session_id.clone(),
            turn_id: Some(self.who.turn_id.clone()),
            trace_id: Some(self.who.trace_id.clone()),
            span_id: Some(probe.span),
            assistant_message_id: assistant,
            parent_user_message_id: ids.user.clone().filter(|_| step),
            query_source: probe.source,
            provider_id: probe.provider,
            model_id: probe.model,
            variant: self.who.variant.clone(),
            agent: self.who.agent.clone(),
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

    /// One `ModelStatus` payload; `span` names a new logical request.
    pub fn on_status(
        &mut self,
        status: &Value,
        ids: &StepIds,
        span: impl FnOnce() -> String,
        now: u64,
    ) -> Vec<Fact> {
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
                    facts.push(Fact::Model(Box::new(
                        self.model_fact(probe, "error", ids, now),
                    )));
                }
                if in_turn(source) {
                    self.requests += 1;
                    self.first_model_start_at.get_or_insert(now);
                }
                self.request = Some(Probe {
                    source: source.into(),
                    span: span(),
                    attempt: if source == "compact" {
                        self.compact_attempts
                    } else {
                        0
                    },
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
                    if in_turn(&probe.source) {
                        self.retries += 1;
                    }
                }
            }
            "model_request_failed" if status["retryable"] != true => {
                if let Some(probe) = self.request.take() {
                    let reason = status["reason"].as_str().unwrap_or("");
                    if probe.source == "compact" && reason == "context_exceeded" {
                        self.compact_attempts += 1;
                    }
                    let cancelled = reason == "cancelled";
                    let status_text = if cancelled { "cancelled" } else { "error" };
                    let mut fact = self.model_fact(probe, status_text, ids, now);
                    // Node `errorInfo.retryable ?? retryCount > 0`：终态失败事件带 retryable=false。
                    fact.retryable = false;
                    fact.context_exceeded = reason == "context_exceeded";
                    // Node 的模型适配器错误不是 CoreError：没有 code，message 是分类后的通用文案。
                    fact.error = ErrorInfo {
                        kind: text(&status["reason"]),
                        code: None,
                        message: Some(crate::model::describe(reason).1.into()),
                    };
                    facts.push(Fact::Model(Box::new(fact)));
                }
            }
            "model_request_completed" => {
                if let Some(probe) = self.request.take() {
                    let usage = &status["usage"];
                    let step = step_source(&probe.source);
                    let counted = in_turn(&probe.source);
                    let mut fact = self.model_fact(probe, "completed", ids, now);
                    fact.finish_reason = text(&status["finishReason"]);
                    fact.provider_metadata = text(&status[super::RAW_FINISH_REASON])
                        .map(|raw| json!({"rawFinishReason": raw}));
                    if usage.as_object().is_some_and(|u| !u.is_empty()) {
                        fact.tokens = Tokens::from_usage(usage);
                        fact.provider_total_tokens = usage["totalTokens"].as_u64();
                        fact.raw_usage = Some(usage.clone());
                        if counted {
                            self.tokens.add(&fact.tokens);
                            self.computed_total += usage_total(usage);
                            self.usage_seen = true;
                        }
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
    pub fn on_model_done(&mut self, message: Option<&Value>, ids: &StepIds) -> Vec<Fact> {
        let Some(mut fact) = self.step.take() else {
            return vec![];
        };
        fact.tool_call_count = message
            .and_then(|m| m["tool_calls"].as_array())
            .map_or(0, |calls| calls.len() as u64);
        if fact.assistant_message_id.is_none()
            && let Some(assistant) = &ids.assistant
        {
            fact.id = fact.id.replace(&fact.logical_request_id, assistant);
            fact.logical_request_id = assistant.clone();
            fact.assistant_message_id = Some(assistant.clone());
        }
        vec![Fact::Model(Box::new(fact))]
    }

    /// A tool's internal model request (Node `ModelComplete {stopReason:
    /// "tool_internal"}`): its usage joins the turn, not `model_usage`.
    pub fn on_nested_usage(&mut self, usage: &Value) {
        if usage.as_object().is_some_and(|u| !u.is_empty()) {
            self.tokens.add(&Tokens::from_usage(usage));
            self.computed_total += usage_total(usage);
            self.usage_seen = true;
        }
    }

    /// The run's total tokens (Node `aggregateModelUsage(...).totalTokens`),
    /// `None` when no request reported usage.
    pub fn total_tokens(&self) -> Option<u64> {
        self.usage_seen.then_some(self.computed_total)
    }

    /// Facts still open when the run ends, closed with the run's outcome.
    fn close_open(&mut self, outcome: Outcome, failure: &ErrorInfo, now: u64) -> Vec<Fact> {
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
                step.error = failure.clone();
            }
            facts.push(Fact::Model(Box::new(step)));
        }
        if let Some(probe) = self.request.take() {
            let mut fact = self.model_fact(probe, status, &StepIds::default(), now);
            fact.error = failure.clone();
            facts.push(Fact::Model(Box::new(fact)));
        }
        facts
    }

    /// The run ended; `failure` is the model failure's `(reason, code)` when it
    /// failed on one, `user` the turn's user message (none for compaction).
    pub fn finish(
        &mut self,
        outcome: Outcome,
        failure: Option<(&str, &str)>,
        user: Option<String>,
        now: u64,
    ) -> Vec<Fact> {
        let context_exceeded = failure.is_some_and(|f| f.0 == "context_exceeded");
        // Node `createTurnFailureError`：取消、超出上下文之外的失败都包装为 UnknownError。
        let (kind, code, retryable) = match outcome {
            Outcome::Completed => (None, None, false),
            Outcome::Cancelled => (Some("turn_cancelled"), Some("TURN_CANCELLED"), false),
            Outcome::Failed if context_exceeded => (
                Some("model_context_exceeded"),
                Some("MODEL_CONTEXT_EXCEEDED"),
                true,
            ),
            Outcome::Failed => (Some("unknown_error"), Some("UNKNOWN_ERROR"), false),
        };
        let error = ErrorInfo {
            kind: kind.map(str::to_owned),
            code: code.map(str::to_owned),
            message: None,
        };
        let mut facts = self.close_open(outcome, &error, now);
        facts.extend(self.close_tools(outcome, now));
        facts.push(Fact::Turn(Box::new(TurnFact {
            session_id: self.who.session_id.clone(),
            turn_id: self.who.turn_id.clone(),
            trace_id: Some(self.who.trace_id.clone()),
            user_message_id: user.filter(|_| !self.who.compact),
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
            retryable,
            cancelled_by_user: outcome == Outcome::Cancelled,
            context_exceeded,
            error,
        })));
        facts
    }
}
