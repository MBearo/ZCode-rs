//! Turn, model request, stream, usage and compaction facts (Node
//! `ConversationTelemetryFactNormalizer.normalize`).
use super::{Event, Normalizer, Runtime, hostname, non_negative, number, text};
use serde_json::{Map, Value, json};

/// `inputSource` values the fact schema accepts.
const INPUT_SOURCES: [&str; 12] = [
    "background_task",
    "fork",
    "goal_state_change",
    "goal-continuation",
    "plugin_reference",
    "rewind",
    "selection_side_chat",
    "subagent",
    "subagent_message",
    "todo_reminder",
    "workflow_launch",
    "shared_context",
];
const STATUSES: [&str; 5] = [
    "model_request_started",
    "model_request_completed",
    "model_request_failed",
    "model_retry_scheduled",
    "model_stream_stalled",
];

/// Node `parseAutomationRunId` of the admission input: `(trigger, scheduledAt)`
/// when it is a run id of `automation`.
fn automation_run(input: &str, automation: &str) -> Option<(&'static str, Option<u64>)> {
    let (id, rest) = input.split_once(':').filter(|(id, _)| !id.is_empty())?;
    if id != automation {
        return None;
    }
    if let Some(run) = rest.strip_prefix("manual:") {
        return (!run.is_empty()).then_some(("manual", None));
    }
    let at: u64 = rest.parse().ok().filter(|at| *at > 0 && *at < (1 << 53))?;
    Some(("schedule", Some(at)))
}

/// Node `isStepUsageQuerySource`: agent steps (the main turn and children).
fn step_source(source: Option<&str>) -> bool {
    matches!(
        source,
        None | Some("main_turn" | "subagent" | "workflow_child")
    )
}

fn copy(fact: &mut Map<String, Value>, payload: &Value, keys: &[&str]) {
    for key in keys {
        if let Some(value) = payload.get(*key).filter(|v| !v.is_null()) {
            fact.insert((*key).into(), value.clone());
        }
    }
}

impl Normalizer {
    pub(super) fn turn_started(
        &mut self,
        event: &Event,
        runtime: &Runtime,
        key: Option<&str>,
    ) -> Option<Map<String, Value>> {
        let payload = event.payload;
        let input = text(&payload["inputId"]);
        if let (Some(key), Some(input)) = (key, input) {
            self.command_by_turn.set(key.into(), input.into());
        }
        let mut fact = Self::base(event, runtime, input, "turn.started");
        let automation = text(&payload["automationId"]);
        let off_peak = text(&payload["offPeakTaskId"]);
        let run_type = text(&payload["offPeakRunType"]);
        // Node schema 的交叉约束：自动化与闲时任务互斥，runType 需要闲时任务。
        if (automation.is_some() && off_peak.is_some())
            || (run_type.is_some() && off_peak.is_none())
        {
            return None;
        }
        if let Some(automation) = automation {
            fact.insert("automationId".into(), automation.into());
            if let Some((trigger, at)) = input.and_then(|i| automation_run(i, automation)) {
                fact.insert("taskTrigger".into(), trigger.into());
                if let Some(at) = at {
                    fact.insert("scheduledAt".into(), at.into());
                }
            }
        }
        if let Some(task) = off_peak {
            fact.insert("offPeakTaskId".into(), task.into());
        }
        if let Some(kind) = run_type {
            if !matches!(kind, "init" | "resume") {
                return None;
            }
            fact.insert("offPeakRunType".into(), kind.into());
        }
        if let Some(kind) = text(&payload["executionKind"]) {
            if !matches!(kind, "agent" | "controlOnly") {
                return None;
            }
            fact.insert("executionKind".into(), kind.into());
        }
        if let Some(source) = text(&payload["inputSource"]) {
            // Node 的 schema 不收未知来源：整条事实被丢弃。
            if !INPUT_SOURCES.contains(&source) {
                return None;
            }
            fact.insert("inputSource".into(), source.into());
        }
        if let Some(source) = text(&payload["backgroundSource"])
            .filter(|s| matches!(*s, "bash" | "subagent" | "workflow"))
        {
            fact.insert("backgroundSource".into(), source.into());
        }
        Some(fact)
    }

    pub(super) fn model_status(
        &mut self,
        event: &Event,
        runtime: &Runtime,
        command: Option<&str>,
    ) -> Option<Map<String, Value>> {
        let p = event.payload;
        let status = p["type"].as_str()?;
        if !STATUSES.contains(&status) {
            return None;
        }
        let request = text(&p["requestId"])?;
        let provider = p["providerId"].as_str().unwrap_or("");
        let model = p["modelId"].as_str().unwrap_or("");
        self.model_by_session
            .set(event.session.into(), (model.into(), provider.into()));
        let host = hostname(&p["baseURL"]);
        let mut fact = Self::base(event, runtime, command, "model.request.status");
        fact.insert("requestId".into(), request.into());
        fact.insert("status".into(), status.into());
        fact.insert("providerId".into(), provider.into());
        fact.insert("modelId".into(), model.into());
        if let Some(kind) = text(&p["providerKind"]) {
            fact.insert("providerKind".into(), kind.into());
        }
        if let Some(host) = &host {
            fact.insert("providerHostname".into(), host.as_str().into());
        }
        fact.insert("transport".into(), p["transport"].clone());
        let source = text(&p["querySource"]);
        if let Some(source) = source {
            fact.insert("querySource".into(), source.into());
        }
        if let Some(query) = text(&p["queryId"]) {
            fact.insert("queryId".into(), query.into());
        }
        fact.insert("attempt".into(), p["attempt"].clone());
        fact.insert("maxAttempts".into(), p["maxAttempts"].clone());
        match status {
            "model_request_completed" => copy(&mut fact, p, &["durationMs"]),
            "model_request_failed" => copy(
                &mut fact,
                p,
                &["durationMs", "reason", "retryable", "statusCode"],
            ),
            "model_retry_scheduled" => copy(
                &mut fact,
                p,
                &["delayMs", "nextAttempt", "reason", "statusCode"],
            ),
            "model_stream_stalled" => copy(&mut fact, p, &["idleMs", "timeoutMs"]),
            _ => {}
        }
        if status == "model_request_completed" && step_source(source) {
            let mut identity =
                json!({"requestId": request, "providerId": provider, "modelId": model});
            if let Some(kind) = text(&p["providerKind"]) {
                identity["providerKind"] = kind.into();
            }
            if let Some(host) = host {
                identity["providerHostname"] = host.into();
            }
            let key = format!("{}\0{}", event.session, source.unwrap_or(""));
            let mut queue = self.completed_requests.remove(&key).unwrap_or_default();
            queue.push(identity);
            self.completed_requests.set(key, queue);
        }
        Some(fact)
    }

    pub(super) fn chunk(
        &mut self,
        event: &Event,
        runtime: &Runtime,
        command: Option<&str>,
    ) -> Option<Map<String, Value>> {
        let p = event.payload;
        let channel = match p["kind"].as_str() {
            Some("text_delta") => "text",
            Some("reasoning_delta") => "thought",
            _ => return None,
        };
        let parent = super::tools::streaming_parent(p);
        let stream = format!(
            "{}\0{}\0{channel}\0{}\0{}",
            event.session,
            event.turn.unwrap_or(""),
            p["partId"].as_str().unwrap_or(""),
            parent.unwrap_or("")
        );
        let mut fact = Self::base(event, runtime, command, "stream.chunk");
        fact.insert("channel".into(), channel.into());
        let length = p["delta"].as_str().map_or(0, |d| d.encode_utf16().count());
        fact.insert("chunkLength".into(), length.into());
        fact.insert("firstChunk".into(), self.first_chunks.add(stream).into());
        for key in ["assistantMessageId", "partId"] {
            if let Some(value) = text(&p[key]) {
                fact.insert(key.into(), value.into());
            }
        }
        if let Some(parent) = parent {
            fact.insert("parentToolCallId".into(), parent.into());
        }
        Some(fact)
    }

    /// Node `ModelComplete`: every completion takes its request identity off
    /// the queue; only agent steps become `usage.delta`.
    pub(super) fn usage(
        &mut self,
        event: &Event,
        runtime: &Runtime,
        command: Option<&str>,
    ) -> Option<Map<String, Value>> {
        let p = event.payload;
        let source = text(&p["querySource"]);
        let key = format!("{}\0{}", event.session, source.unwrap_or(""));
        let mut queue = self.completed_requests.remove(&key).unwrap_or_default();
        let request = (!queue.is_empty()).then(|| queue.remove(0));
        if !queue.is_empty() {
            self.completed_requests.set(key, queue);
        }
        if !step_source(source) || (source.is_none() && p["stopReason"] == "tool_internal") {
            return None;
        }
        let usage = &p["usage"];
        let field = |key: &str| non_negative(&usage[key]);
        let read = field("cacheReadTokens").or_else(|| field("cacheTokens"));
        // Node getModelUsageTotalTokens：provider total，否则 input（或 cache）加 output。
        let total = field("totalTokens").unwrap_or_else(|| {
            field("inputTokens")
                .unwrap_or_else(|| read.unwrap_or(0.0) + field("cacheWriteTokens").unwrap_or(0.0))
                + field("outputTokens").unwrap_or(0.0)
        });
        let mut fact = Self::base(event, runtime, command, "usage.delta");
        if let Some(request) = request.as_ref().and_then(Value::as_object) {
            fact.extend(request.clone());
        }
        for (key, value) in [
            ("inputTokens", field("inputTokens")),
            ("outputTokens", field("outputTokens")),
            ("totalTokens", Some(total)),
            ("reasoningTokens", field("reasoningTokens")),
            ("cacheReadTokens", read),
            ("cacheWriteTokens", field("cacheWriteTokens")),
        ] {
            fact.insert(key.into(), number(value.unwrap_or(0.0)));
        }
        Some(fact)
    }

    pub(super) fn compaction(
        &mut self,
        event: &Event,
        runtime: &Runtime,
    ) -> Option<Map<String, Value>> {
        let p = event.payload;
        let status = p["status"]
            .as_str()
            .filter(|s| matches!(*s, "completed" | "failed" | "interrupted"))?;
        let model = self
            .model_by_session
            .get(event.session)
            .cloned()
            .or_else(|| runtime.model.clone());
        let mut fact = Self::base(
            event,
            runtime,
            text(&p["sourceCommandId"]),
            "compaction.terminal",
        );
        fact.insert("operationId".into(), p["operationId"].clone());
        for key in ["messageId", "summaryMessageId"] {
            if let Some(value) = text(&p[key]) {
                fact.insert(key.into(), value.into());
            }
        }
        fact.insert("status".into(), status.into());
        fact.insert("trigger".into(), p["trigger"].clone());
        for key in ["compactReason", "reason"] {
            if let Some(value) = text(&p[key]) {
                fact.insert(key.into(), value.into());
            }
        }
        copy(
            &mut fact,
            p,
            &[
                "attempt",
                "maxAttempts",
                "startedAt",
                "endedAt",
                "preCompactTokenCount",
                "postCompactTokenCount",
                "truePostCompactTokenCount",
            ],
        );
        if let Some((name, provider)) = model {
            fact.insert("modelName".into(), name.into());
            fact.insert("modelProvider".into(), provider.into());
        }
        Some(fact)
    }
}

/// Node `TurnComplete` / `TurnError` → `turn.terminal`.
pub(super) fn terminal(
    event: &Event,
    runtime: &Runtime,
    command: Option<&str>,
) -> Map<String, Value> {
    let p = event.payload;
    let command = text(&p["inputId"]).or(command);
    let mut fact = Normalizer::base(event, runtime, command, "turn.terminal");
    if event.kind == "turn_complete" {
        let result = p["resultType"].as_str().unwrap_or("");
        let status = match result {
            "success" => "success",
            "cancelled" => "interrupted",
            _ => "failed",
        };
        fact.insert("status".into(), status.into());
        fact.insert("resultType".into(), result.into());
        fact.insert("durationMs".into(), p["duration"].clone());
        fact.insert("tokenCount".into(), p["tokenCount"].clone());
        fact.insert("toolCallCount".into(), p["toolCallCount"].clone());
        if result == "cancelled" {
            fact.insert("errorCode".into(), "USER_INTERRUPT".into());
            fact.insert("errorMessage".into(), "User stopped generation".into());
        }
    } else {
        let error = &p["error"];
        fact.insert("status".into(), "failed".into());
        let code = error
            .get("code")
            .filter(|c| !c.is_null())
            .unwrap_or(&error["type"]);
        fact.insert("errorCode".into(), code.clone());
        fact.insert("errorMessage".into(), error["message"].clone());
        if let Some(retryable) = error.get("retryable").filter(|r| r.is_boolean()) {
            fact.insert("errorRetryable".into(), retryable.clone());
        }
    }
    for flag in ["backgroundSubagentResultConsumed", "workflowResultConsumed"] {
        if p[flag] == true {
            fact.insert(flag.into(), true.into());
        }
    }
    if event.kind == "turn_error" {
        fact.insert("turnPhase".into(), p["turnPhase"].clone());
    }
    fact
}
