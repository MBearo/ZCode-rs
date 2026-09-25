//! Legacy `session/event` stream of one session activation (Node
//! `server-operations.ts` live projection): per-delivery-kind seq domains and
//! the deterministic batching of `model.streaming` text deltas.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const DELIVERY_KINDS: [&str; 2] = ["desktop-continuous", "web-remote-replayable"];
const TEXT_MAX_CHARS: usize = 2 * 1024;
const TEXT_MAX_INTERVAL_MS: u64 = 250;

/// Pending merged deltas: the last delta event and the concatenated text.
#[derive(Clone, Debug)]
struct Batch {
    key: String,
    delta: String,
    /// UTF-16 code units of `delta` (JS `string.length`).
    units: usize,
    last: Value,
    updated_at: u64,
}

/// One delivery kind's numbering and batching state (Node keeps one per kind).
#[derive(Clone, Debug, Default)]
struct Domain {
    last_seq: u64,
    batch: Option<Batch>,
    flushed_first: BTreeSet<String>,
    last_flush: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default)]
pub struct LegacyStream {
    kind: Option<&'static str>,
    domains: BTreeMap<&'static str, Domain>,
    /// The running turn's totals for `turn.completed`.
    pub turn: Option<TurnTally>,
    /// Turns finished in this activation (`turn.started.turnNumber`).
    pub turns_completed: u64,
}

impl LegacyStream {
    /// `session/subscribe`: the last subscription's kind wins; returns its seq.
    pub fn subscribe(&mut self, kind: &str) -> Option<u64> {
        let kind = DELIVERY_KINDS.into_iter().find(|k| *k == kind)?;
        self.kind = Some(kind);
        Some(self.domains.entry(kind).or_default().last_seq)
    }

    pub fn kind(&self) -> Option<&'static str> {
        self.kind
    }

    /// Queues `event` (an envelope without `seq` and `deliveryKind`) and
    /// appends every event now due to `out`. Text and reasoning deltas are
    /// merged; any other event first flushes and resets the batching.
    pub fn push(&mut self, mut event: Value, out: &mut Vec<Value>) {
        let Some(kind) = self.kind else { return };
        event["deliveryKind"] = kind.into();
        let domain = self.domains.entry(kind).or_default();
        let Some(key) = batch_key(&event) else {
            domain.reset(out);
            domain.send(event, out);
            return;
        };
        if domain.batch.as_ref().is_some_and(|b| b.key != key) {
            domain.flush(out);
        }
        let delta = event["payload"]["delta"].as_str().unwrap_or("").to_owned();
        let at = event["timestamp"].as_u64().unwrap_or(0);
        let units = delta.encode_utf16().count();
        match &mut domain.batch {
            Some(batch) => {
                batch.delta.push_str(&delta);
                batch.units += units;
                batch.last = event;
                batch.updated_at = at;
            }
            None => {
                domain.batch = Some(Batch {
                    key: key.clone(),
                    delta,
                    units,
                    last: event,
                    updated_at: at,
                })
            }
        }
        if domain.flushed_first.insert(key.clone()) {
            domain.flush(out);
            return;
        }
        let batch = domain.batch.as_ref().unwrap();
        let interval = domain
            .last_flush
            .get(&key)
            .is_some_and(|last| batch.updated_at.saturating_sub(*last) >= TEXT_MAX_INTERVAL_MS);
        if batch.units >= TEXT_MAX_CHARS || interval {
            domain.flush(out);
        }
    }

    /// A runtime fact without a legacy event still ends the batching window.
    pub fn barrier(&mut self, out: &mut Vec<Value>) {
        if let Some(kind) = self.kind {
            self.domains.entry(kind).or_default().reset(out);
        }
    }

    /// The current domain's last seq (`eventSeq`).
    pub fn seq(&self) -> u64 {
        self.kind
            .and_then(|k| self.domains.get(k))
            .map_or(0, |d| d.last_seq)
    }
}

impl Domain {
    fn send(&mut self, mut event: Value, out: &mut Vec<Value>) {
        self.last_seq += 1;
        event["seq"] = self.last_seq.into();
        out.push(event);
    }

    fn flush(&mut self, out: &mut Vec<Value>) {
        let Some(batch) = self.batch.take() else {
            return;
        };
        self.last_flush.insert(batch.key, batch.updated_at);
        let mut event = batch.last;
        event["payload"]["delta"] = batch.delta.into();
        self.send(event, out);
    }

    fn reset(&mut self, out: &mut Vec<Value>) {
        self.flush(out);
        self.flushed_first.clear();
        self.last_flush.clear();
    }
}

/// Node batch key `kind:assistantMessageId:inputId:partId:parentToolUseId:toolCallId`
/// for non-empty text and reasoning deltas.
fn batch_key(event: &Value) -> Option<String> {
    let payload = &event["payload"];
    let kind = payload["kind"].as_str()?;
    let batchable = event["type"] == "model.streaming"
        && matches!(kind, "text_delta" | "reasoning_delta")
        && payload["delta"].as_str().is_some_and(|d| !d.is_empty());
    batchable.then(|| {
        let message = payload["assistantMessageId"].as_str().unwrap_or("");
        format!("{kind}:{message}::::")
    })
}

/// Node `ModelUsage` from a provider usage object (OpenAI-style fields).
pub fn model_usage(usage: &Value) -> [u64; 6] {
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let input = n(&usage["prompt_tokens"]);
    let output = n(&usage["completion_tokens"]);
    let total = usage["total_tokens"]
        .as_u64()
        .unwrap_or(input.saturating_add(output));
    [
        input,
        output,
        total,
        n(&usage["prompt_tokens_details"]["cached_tokens"]),
        n(&usage["prompt_tokens_details"]["cache_write_tokens"]),
        n(&usage["completion_tokens_details"]["reasoning_tokens"]),
    ]
}

/// `{inputTokens, outputTokens, totalTokens, cacheReadTokens, cacheWriteTokens, reasoningTokens}`.
pub fn usage_json(usage: [u64; 6]) -> Value {
    json!({"inputTokens": usage[0], "outputTokens": usage[1], "totalTokens": usage[2],
        "cacheReadTokens": usage[3], "cacheWriteTokens": usage[4], "reasoningTokens": usage[5]})
}

/// What started a run, decided once at run start: whether it holds the legacy
/// lock and which `state.updated` reason its end reports (spec 9.12).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum RunKind {
    /// A user or queued input, or a background continuation.
    #[default]
    Prompt,
    /// A manual compaction (`executionKind: "controlOnly"`).
    Compact,
    /// A goal set or continued by a client goal command.
    Goal,
}

impl RunKind {
    /// Node `afterStateMutation` reason at the run's end.
    pub fn end_reason(self, success: bool, cancelled: bool) -> &'static str {
        match (self, success, cancelled) {
            (Self::Prompt, true, _) => "prompt_completed",
            (Self::Prompt, false, _) => "prompt_failed",
            (Self::Compact, true, _) => "session_compacted",
            (Self::Compact, false, true) => "session_compact_cancelled",
            (Self::Compact, false, false) => "session_compact_failed",
            (Self::Goal, true, _) => "goal_continuation_completed",
            (Self::Goal, false, _) => "goal_continuation_failed",
        }
    }
}

/// Totals of the running turn (Node `turn_complete` payload inputs).
#[derive(Clone, Debug, Default)]
pub struct TurnTally {
    pub kind: RunKind,
    pub started_at: u64,
    pub input_id: Option<String>,
    /// The last model step's text (Node `loopState.modelResponse`).
    pub response: String,
    pub requests: u64,
    pub usage: [u64; 6],
    pub token_count: u64,
    pub tool_calls: u64,
    pub rounds: u64,
    /// Node `serverToolUse` sums (`webSearchRequests`, `webFetchRequests`).
    pub web_search: u64,
    pub web_fetch: u64,
    /// Assistant response id of the latest text (`assistantMessageId`).
    pub message_id: Option<String>,
    /// This turn's tool calls by id.
    pub tools: BTreeMap<String, ToolTrack>,
    /// Node `TurnMachine` phase (`None` before the first model response is
    /// `processing_input`); a turn error reports it as `turnPhase`.
    pub phase: Option<&'static str>,
}

impl TurnTally {
    pub fn phase(&self) -> &'static str {
        self.phase.unwrap_or("processing_input")
    }
}

/// One tool call's legacy lifecycle facts.
#[derive(Clone, Debug, Default)]
pub struct ToolTrack {
    pub name: String,
    /// Node `summarizeInput` of the model's input.
    pub input_summary: Value,
    pub started_at: Option<u64>,
    /// A permission prompt was shown; its answer already went out.
    pub prompted: bool,
    /// When the prompt went out, and how long its answer took (Node
    /// `permissionWaitMs`).
    pub permission_at: Option<u64>,
    pub permission_wait: Option<u64>,
    pub succeeded: Option<bool>,
}

/// Node `summarizeInput`.
pub fn input_summary(input: &Value) -> Value {
    match input {
        Value::Null => json!({"type": "null"}),
        Value::Array(items) => json!({"type": "array", "length": items.len()}),
        Value::Object(map) => {
            json!({"type": "object", "keys": map.keys().take(20).collect::<Vec<_>>()})
        }
        Value::String(s) => json!({"type": "string", "length": s.encode_utf16().count()}),
        Value::Bool(_) => json!({"type": "boolean"}),
        Value::Number(_) => json!({"type": "number"}),
    }
}

impl TurnTally {
    /// One committed model step; `raw` is its adapter usage.
    pub fn model_done(&mut self, raw: &Value, text: &str, calls: usize) {
        let usage = model_usage(raw);
        self.web_search += raw["server_tool_use"]["web_search_requests"]
            .as_u64()
            .unwrap_or(0);
        self.web_fetch += raw["server_tool_use"]["web_fetch_requests"]
            .as_u64()
            .unwrap_or(0);
        self.requests += 1;
        self.rounds += 1;
        for (sum, value) in self.usage.iter_mut().zip(usage) {
            *sum = sum.saturating_add(value);
        }
        self.token_count = self.token_count.saturating_add(usage[2]);
        self.tool_calls += calls as u64;
        // 修复：Node turn-model-step 每步覆盖 modelResponse，turn.completed.response 只是最后一步的文本。
        self.response = text.to_owned();
    }

    /// A tool's internal request (Node `ModelComplete {stopReason:
    /// "tool_internal"}`): its Node `ModelUsage` joins the summary.
    pub fn nested(&mut self, usage: &Value) {
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        let values = [
            n("inputTokens"),
            n("outputTokens"),
            crate::usage::usage_total(usage),
            n("cacheReadTokens"),
            n("cacheWriteTokens"),
            n("reasoningTokens"),
        ];
        self.requests += 1;
        for (sum, value) in self.usage.iter_mut().zip(values) {
            *sum = sum.saturating_add(value);
        }
        self.web_search += usage["serverToolUse"]["webSearchRequests"]
            .as_u64()
            .unwrap_or(0);
        self.web_fetch += usage["serverToolUse"]["webFetchRequests"]
            .as_u64()
            .unwrap_or(0);
    }

    /// Node `ModelUsageSummary`.
    /// A compaction in this turn: its summary request counts in the turn's
    /// usage; a manual compaction turn (Node `compact.ts`) is one round whose
    /// response and token count are the compaction's.
    pub fn compacted(&mut self, raw: &Value, done: bool, tokens: u64) {
        let usage = model_usage(raw);
        if done {
            self.requests += 1;
            for (sum, value) in self.usage.iter_mut().zip(usage) {
                *sum = sum.saturating_add(value);
            }
        }
        if self.kind == RunKind::Compact {
            self.rounds = 1;
            (self.response, self.token_count) = if done {
                ("Compacted".into(), usage[2])
            } else {
                (
                    "Context is up to date; no compression needed".into(),
                    tokens,
                )
            };
        }
    }

    pub fn summary(&self) -> Value {
        let mut usage = usage_json(self.usage);
        usage["source"] = "provider".into();
        usage["modelRequestCount"] = self.requests.into();
        usage["webSearchRequests"] = self.web_search.into();
        usage["webFetchRequests"] = self.web_fetch.into();
        usage
    }
}

#[cfg(test)]
#[path = "legacy_stream_tests.rs"]
mod tests;
