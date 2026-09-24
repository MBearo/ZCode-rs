//! What `-p` prints (Node `prompt-command.ts`): the json / stream-json result
//! object in Node's key order and the error line of a failed turn.
use serde::Serialize;
use serde_json::Value;

/// `ModelUsageSummary` in Node's key order.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Usage {
    source: String,
    model_request_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    web_fetch_requests: u64,
    web_search_requests: u64,
}

impl Usage {
    /// From `turn.completed.usage`; `None` when the turn reported none.
    pub(crate) fn from_payload(usage: &Value) -> Option<Self> {
        usage.as_object()?;
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        Some(Self {
            source: usage["source"].as_str().unwrap_or("provider").into(),
            model_request_count: n("modelRequestCount"),
            input_tokens: n("inputTokens"),
            output_tokens: n("outputTokens"),
            total_tokens: n("totalTokens"),
            cache_read_tokens: n("cacheReadTokens"),
            cache_write_tokens: n("cacheWriteTokens"),
            reasoning_tokens: n("reasoningTokens"),
            web_fetch_requests: n("webFetchRequests"),
            web_search_requests: n("webSearchRequests"),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Projection {
    status: String,
    turn_count: u64,
    total_token_count: u64,
    context_used: Option<u64>,
    context_window: Option<u64>,
}

impl Projection {
    /// From the legacy snapshot's `projection`; unknown context sizes are `null`.
    pub(crate) fn from_snapshot(projection: &Value) -> Self {
        let known = |key: &str| projection[key].as_u64().filter(|n| *n > 0);
        Self {
            status: projection["status"].as_str().unwrap_or("idle").into(),
            turn_count: projection["turnCount"].as_u64().unwrap_or(0),
            total_token_count: projection["totalTokenCount"].as_u64().unwrap_or(0),
            context_used: known("contextUsed"),
            context_window: known("contextWindow"),
        }
    }
}

/// The json summary; with `kind` it is the stream-json `result` line.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    pub session_id: String,
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub response: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub event_count: u64,
    pub projection: Projection,
}

/// A failed turn as Node's `createTurnFailureError` wraps it:
/// `(message, cause)` for `Error: <message>` and the `--verbose` cause.
pub(crate) fn failure(error: &Value) -> (String, Option<String>) {
    let own = error["message"].as_str().unwrap_or("").to_owned();
    let message = match error["attribution"]["reason"].as_str() {
        Some("context_exceeded") => "Model request exceeded the provider context window.".into(),
        Some("model_output_limit_exceeded") if !own.is_empty() => own.clone(),
        _ => "Turn execution failed".to_owned(),
    };
    let cause = (!own.is_empty() && own != message).then_some(own);
    (message, cause)
}

/// `Error: <message>[ (traceId: <id>)]`.
pub(crate) fn error_line(message: &str, trace: Option<&str>) -> String {
    match trace {
        Some(trace) => format!("Error: {message} (traceId: {trace})\n"),
        None => format!("Error: {message}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_keeps_node_key_order() {
        let summary = Summary {
            kind: Some("result"),
            session_id: "s".into(),
            trace_id: Some("t".into()),
            turn_id: None,
            response: "hi".into(),
            usage: Usage::from_payload(&json!({"webSearchRequests": 0, "source": "provider",
                "inputTokens": 10, "outputTokens": 4, "totalTokens": 14, "modelRequestCount": 1})),
            event_count: 3,
            projection: Projection::from_snapshot(
                &json!({"status": "idle", "turnCount": 1, "totalTokenCount": 14, "contextUsed": 0}),
            ),
        };
        assert_eq!(
            serde_json::to_string(&summary).unwrap(),
            r#"{"type":"result","sessionId":"s","traceId":"t","response":"hi","usage":{"source":"provider","modelRequestCount":1,"inputTokens":10,"outputTokens":4,"totalTokens":14,"cacheReadTokens":0,"cacheWriteTokens":0,"reasoningTokens":0,"webFetchRequests":0,"webSearchRequests":0},"eventCount":3,"projection":{"status":"idle","turnCount":1,"totalTokenCount":14,"contextUsed":null,"contextWindow":null}}"#
        );
    }

    #[test]
    fn failures_are_wrapped_like_node() {
        let provider = json!({"message": "Provider authentication failed.",
            "attribution": {"reason": "auth_failed"}});
        assert_eq!(
            failure(&provider),
            (
                "Turn execution failed".into(),
                Some("Provider authentication failed.".into())
            )
        );
        let context = json!({"message": "Model context window exceeded.",
            "attribution": {"reason": "context_exceeded"}});
        assert_eq!(
            failure(&context).0,
            "Model request exceeded the provider context window."
        );
        let limit = json!({"message": "The model's response exceeded the output token maximum.",
            "attribution": {"reason": "model_output_limit_exceeded"}});
        assert_eq!(
            failure(&limit),
            (limit["message"].as_str().unwrap().into(), None)
        );
        assert_eq!(
            error_line("Turn execution failed", Some("t1")),
            "Error: Turn execution failed (traceId: t1)\n"
        );
    }
}
