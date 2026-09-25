//! The main conversation's v4 `usage` state (Node `ProductProjection`
//! `onModelComplete`, `onCompactLifecycle` and `onModelSelected`). Spec
//! rust-m9-usage-logs §4.
use super::Engine;
use crate::contract::ModelIdentity;
use crate::domain::session::Session;
use crate::domain::usage::{context_tokens, model_usage};
use serde_json::{Value, json};

/// The step request in flight (`Event::RequestContext`).
pub(super) struct StepRequest {
    pub window: usize,
    pub breakdown: Option<Vec<Value>>,
}

const CUMULATIVE: [&str; 4] = [
    "inputTokens",
    "outputTokens",
    "cacheReadTokens",
    "cacheWriteTokens",
];

/// The threshold the previous window carried (Node never sets one live).
fn threshold(s: &Session) -> Value {
    s.usage["contextWindow"]
        .get("autoCompactThresholdTokens")
        .cloned()
        .unwrap_or(Value::Null)
}

/// A step's model completion: the goal's run tokens, and for a root session
/// (Node `querySource === "main_turn"`) the context window and the
/// cumulative usage. `message` is the index its assistant message takes.
pub(super) fn step_done(
    s: &mut Session,
    request: Option<StepRequest>,
    raw: &Value,
    (message, now): (usize, u64),
) {
    if let Some(goal) = s.goal.as_mut() {
        goal.account(raw, now);
    }
    // Node：子代理 step 的 querySource 是 subagent，不更新会话的 usage。
    if s.task_type == "subagent_child" {
        return;
    }
    let usage = model_usage(raw);
    let cache = s.cache_hits.record(message, &usage);
    let used = context_tokens(&usage).unwrap_or(0);
    let (window, breakdown) = match request {
        Some(request) => (Some(request.window as u64), request.breakdown),
        None => (s.usage["contextWindow"]["maxTokens"].as_u64(), None),
    };
    let context = window.map_or(Value::Null, |max| {
        let mut context = json!({"usedTokens": used, "maxTokens": max,
            "autoCompactThresholdTokens": threshold(s)});
        if let Some(cache) = cache {
            context["cache"] = cache;
        }
        if let Some(breakdown) = breakdown.filter(|b| !b.is_empty()) {
            context["breakdown"] = breakdown.into();
        }
        context
    });
    for key in CUMULATIVE {
        let total = s.usage["cumulative"][key].as_u64().unwrap_or(0);
        s.usage["cumulative"][key] = total
            .saturating_add(usage[key].as_u64().unwrap_or(0))
            .into();
    }
    s.usage["contextWindow"] = context;
}

/// Node `onCompactLifecycle` success: the context falls to the compacted
/// size; the window stays unknown when no window is known.
pub(super) fn compacted(s: &mut Session, tokens: usize, window: Option<u64>) {
    let Some(max) = s.usage["contextWindow"]["maxTokens"].as_u64().or(window) else {
        return;
    };
    s.usage["contextWindow"] = json!({"usedTokens": tokens, "maxTokens": max,
        "autoCompactThresholdTokens": threshold(s)});
}

impl Engine {
    /// The context window of the session's selected model.
    pub(super) fn model_window(&self, s: &Session) -> Option<u64> {
        let identity = ModelIdentity {
            provider_id: s.provider.clone(),
            model_id: s.model.clone(),
            reasoning_level: s.reasoning_level.clone(),
        };
        let model = match &self.registry {
            Some(registry) => registry.resolve(&identity).ok(),
            None => self.model.clone(),
        };
        model.map(|m| m.context_policy().window as u64)
    }

    /// Node `onModelSelected`: the new model's window, the used tokens kept.
    pub(super) fn window_selected(&mut self, id: &str) {
        let Some(window) = self.sessions.get(id).and_then(|s| self.model_window(s)) else {
            return;
        };
        let s = self.sessions.get_mut(id).unwrap();
        let context = &mut s.usage["contextWindow"];
        if !context.is_object() {
            *context =
                json!({"usedTokens": 0, "maxTokens": window, "autoCompactThresholdTokens": null});
        } else if context["maxTokens"].as_u64() != Some(window) {
            context["maxTokens"] = window.into();
        }
    }
}
