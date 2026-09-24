//! Pages of ended subagents (Node `paginateEndedSubagents` and its
//! base64url cursor).
use crate::subagent_query::non_empty;
use base64::Engine as _;
use serde_json::{Value, json};

fn cursor_of(item: &Value) -> String {
    let body = json!({"childSessionId": item["childSessionId"], "endedAt": item["endedAt"].as_u64().unwrap_or(0)});
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body.to_string())
}

/// Node `paginateEndedSubagents`: the page and its next cursor.
pub fn paginate(
    ended: &[Value],
    cursor: Option<&str>,
    limit: usize,
) -> (Vec<Value>, Option<String>) {
    let decoded = cursor
        .and_then(|c| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(c.trim_end_matches('='))
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| Some((non_empty(&v["childSessionId"])?, v["endedAt"].as_f64()?)));
    let start = match &decoded {
        Some((child, at)) => ended.iter().position(|item| {
            let ended_at = item["endedAt"].as_f64().unwrap_or(0.0);
            ended_at < *at
                || (ended_at == *at
                    && item["childSessionId"].as_str().unwrap_or("") < child.as_str())
        }),
        None => Some(0),
    };
    let Some(start) = start else {
        return (vec![], None);
    };
    let items: Vec<Value> = ended.iter().skip(start).take(limit).cloned().collect();
    let next = items
        .last()
        .filter(|_| start + items.len() < ended.len())
        .map(cursor_of);
    (items, next)
}
