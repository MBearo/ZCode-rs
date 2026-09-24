//! Legacy `session/debug` observation of one session activation (Node
//! `session-debug.ts` over `zcodeTaskNetworkDebugStatusFromPayload`): bounded
//! network entries, main-turn rounds and the cache hit rate. Never persisted.
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, VecDeque};

const ROUNDS: usize = 200;
const NETWORK: usize = 100;
const DEDUPE: usize = 2000;
const HEADERS: usize = 32;
const HEADER_CHARS: usize = 512;
const MESSAGE_CHARS: usize = 2048;
const TYPES: [&str; 5] = [
    "model_request_started",
    "model_request_completed",
    "model_request_failed",
    "model_retry_scheduled",
    "model_stream_stalled",
];

/// A FIFO of recently seen keys (Node `remember`).
#[derive(Clone, Debug, Default)]
struct Seen {
    order: VecDeque<String>,
    keys: BTreeSet<String>,
}

impl Seen {
    fn remember(&mut self, key: &str) -> bool {
        if !self.keys.insert(key.to_owned()) {
            return false;
        }
        self.order.push_back(key.to_owned());
        if self.order.len() > DEDUPE
            && let Some(oldest) = self.order.pop_front()
        {
            self.keys.remove(&oldest);
        }
        true
    }
}

#[derive(Clone, Debug, Default)]
pub struct DebugLog {
    network: VecDeque<Value>,
    rounds: VecDeque<Value>,
    /// `(requests, total input, total cache read)` once a main-turn request completed.
    cache: Option<(u64, f64, f64)>,
    unknown_cache: bool,
    events: Seen,
    completed: Seen,
}

/// JS `String.prototype.slice(0, n)` in UTF-16 code units.
fn js_slice(text: &str, units: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += c.len_utf16();
            used <= units
        })
        .collect()
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

fn count(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite() && *n >= 0.0)
}

fn int(value: &Value, min: u64) -> Option<u64> {
    value.as_u64().filter(|n| *n >= min)
}

/// String-valued entries; the count is taken before bounding.
fn headers(value: &Value) -> (Map<String, Value>, usize) {
    let strings: Vec<(&String, &str)> = value
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k, v.as_str()?)))
                .collect()
        })
        .unwrap_or_default();
    let bounded = strings
        .iter()
        .take(HEADERS)
        .map(|(k, v)| (js_slice(k, HEADER_CHARS), js_slice(v, HEADER_CHARS).into()))
        .collect();
    (bounded, strings.len())
}

impl DebugLog {
    /// One model network status of the session (`event_id` / `trace_id` from
    /// its envelope, `now` when the payload has no parsable timestamp).
    pub fn observe(&mut self, payload: &Value, event_id: &str, trace_id: &str, now: u64) {
        let Some(kind) = text(&payload["type"]).filter(|t| TYPES.contains(t)) else {
            return;
        };
        if !self.events.remember(event_id) {
            return;
        }
        let recorded = text(&payload["timestamp"])
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map_or(now, |t| t.timestamp_millis().max(0) as u64);
        let mut entry = json!({"eventKey": event_id, "traceId": trace_id, "statusType": kind,
            "recordedAt": recorded});
        for key in [
            "queryId",
            "requestId",
            "providerId",
            "modelId",
            "providerKind",
            "transport",
            "baseURL",
            "querySource",
            "reason",
            "timestamp",
        ] {
            if let Some(value) = text(&payload[key]) {
                entry[key] = value.into();
            }
        }
        if let Some(message) = text(&payload["message"]) {
            entry["message"] = js_slice(message, MESSAGE_CHARS).into();
        }
        for (key, min) in [("attempt", 1), ("nextAttempt", 1), ("statusCode", 0)] {
            if let Some(value) = int(&payload[key], min) {
                entry[key] = value.into();
            }
        }
        if !payload["maxAttempts"].is_null() {
            entry["maxAttempts"] = payload["maxAttempts"].clone();
        }
        if let Some(retryable) = payload["retryable"].as_bool() {
            entry["retryable"] = retryable.into();
        }
        for key in ["durationMs", "delayMs", "idleMs", "timeoutMs"] {
            if count(&payload[key]).is_some() {
                entry[key] = payload[key].clone();
            }
        }
        for side in ["request", "response"] {
            let (bounded, total) = headers(&payload[format!("{side}Headers")]);
            let key = format!("{side}HeaderCount");
            entry[&key] = int(&payload[&key], 0).unwrap_or(total as u64).into();
            entry[format!("{side}Headers")] = bounded.into();
        }
        self.network.push_back(entry);
        if self.network.len() > NETWORK {
            self.network.pop_front();
        }
        let request = text(&payload["requestId"]).unwrap_or("");
        if kind == "model_request_completed"
            && payload["querySource"] == "main_turn"
            && self.completed.remember(request)
        {
            self.round(payload, event_id, request, recorded);
        }
    }

    fn round(&mut self, payload: &Value, event_id: &str, request: &str, recorded: u64) {
        let usage = &payload["usage"];
        let (input, output) = (count(&usage["inputTokens"]), count(&usage["outputTokens"]));
        let cache_read = count(&usage["cacheReadTokens"]);
        let generation = count(&payload["durationMs"])
            .zip(count(&payload["timeToFirstContentMs"]))
            .filter(|(duration, first)| duration > first)
            .map(|(duration, first)| duration - first);
        self.unknown_cache |= input.is_none() || cache_read.is_none();
        let (requests, total_input, total_read) = self.cache.unwrap_or_default();
        let totals = (
            requests + 1,
            total_input + input.unwrap_or(0.0),
            total_read + cache_read.unwrap_or(0.0),
        );
        self.cache = Some(totals);
        let mut used = Map::new();
        let total = count(&usage["totalTokens"]).or(input.zip(output).map(|(i, o)| i + o));
        for (key, value) in [
            ("inputTokens", input),
            ("outputTokens", output),
            ("totalTokens", total),
            ("reasoningTokens", count(&usage["reasoningTokens"])),
            ("cachedInputTokens", cache_read),
            ("cachedWriteInputTokens", count(&usage["cacheWriteTokens"])),
        ] {
            if let Some(value) = value {
                used.insert(key.into(), number(value));
            }
        }
        let hit = input
            .filter(|i| *i > 0.0)
            .zip(cache_read)
            .map(|(i, r)| number(r / i));
        let tps = output
            .zip(generation.filter(|g| *g > 0.0))
            .map(|(o, g)| number(o * 1000.0 / g));
        self.rounds
            .push_back(json!({"eventKey": event_id, "requestId": request,
            "requestIndex": totals.0, "recordedAt": recorded, "usage": used, "hitRate": hit,
            "generationDurationMs": generation.map(number), "tokensPerSecond": tps}));
        if self.rounds.len() > ROUNDS {
            self.rounds.pop_front();
        }
    }

    /// Node `SessionDebugSnapshot`.
    pub fn snapshot(&self, session_id: &str) -> Value {
        let cache = self.cache.map(|(requests, input, read)| {
            let rate = (!self.unknown_cache && input > 0.0).then(|| number(read / input));
            json!({"hitRateRequestCount": requests, "totalInputTokens": number(input),
                "totalCacheReadTokens": number(read), "hitRate": rate})
        });
        json!({"sessionId": session_id, "rounds": self.rounds, "networkEntries": self.network,
            "cache": cache})
    }
}

/// A JS number: integral values as integers, others as floats.
fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9e15 {
        (value as i64).into()
    } else {
        value.into()
    }
}

#[cfg(test)]
#[path = "session_debug_tests.rs"]
mod tests;
