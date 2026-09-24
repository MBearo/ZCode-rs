//! Model configuration and usage reductions (Node
//! `ProductProjection.onModelSelected` and `onModelComplete`).
use super::events::{Event, HYDRATION_TRACE, num};
use super::projection::{Delta, Projection, TurnModel};
use serde_json::{Map, Value, json};

/// Node `cloneSparseModelSelection`.
fn sparse_selection(selection: &Value) -> Value {
    let mut out = json!({"providerId": selection["providerId"], "modelId": selection["modelId"]});
    if super::facts::truthy(&selection["options"]) {
        out["options"] = selection["options"].clone();
    }
    out
}

/// Node `sameSparseModelSelection`.
fn same_selection(left: Option<&Value>, right: &Value) -> bool {
    let Some(left) = left else {
        return false;
    };
    left["providerId"] == right["providerId"]
        && left["modelId"] == right["modelId"]
        && left["options"]["reasoningLevel"] == right["options"]["reasoningLevel"]
}

impl Projection {
    /// Node `onModelSelected`: config only; the modelChange marker waits for
    /// the next turn. Hydration selections never claim seed authority.
    pub fn on_model_selected(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        if payload.get("previousModelSelection") == Some(&Value::Null) {
            self.last_turn_model = TurnModel::SourceLess;
        }
        debug_assert_eq!(event.trace, HYDRATION_TRACE);
        let prev = &self.state["config"];
        let selection = &payload["modelSelection"];
        let provider = selection["providerId"].clone();
        let model = selection["modelId"].clone();
        let thought = [
            &payload["effectiveReasoningLevel"],
            &selection["options"]["reasoningLevel"],
        ]
        .into_iter()
        .find(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| "".into());
        let model_selection = sparse_selection(selection);
        let levels = match &payload["supportedThoughtLevels"] {
            Value::Array(levels) => Value::Array(levels.clone()),
            _ => prev["thoughtLevels"].clone(),
        };
        let window = match payload.get("contextWindow") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => {
                let n = value
                    .as_f64()
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .map(f64::floor);
                n.map(Some)
            }
        };
        if let Some(window) = window {
            self.window_max = window.filter(|n| *n > 0.0);
        }
        let previous_window = &self.state["usage"]["contextWindow"];
        if let Some(used) = previous_window["usedTokens"].as_f64() {
            self.window_used = used;
        }
        let window_changed = match window {
            None => false,
            Some(None) => !previous_window.is_null(),
            Some(Some(n)) => previous_window["maxTokens"].as_f64() != Some(n),
        };
        let config_changed = !(prev["provider"] == provider
            && prev["model"] == model
            && same_selection(prev.get("modelSelection"), &model_selection)
            && prev["thought"] == thought
            && prev["thoughtLevels"] == levels);
        let transition = (payload["origin"] == "registryFallback"
            && payload["previousModelSelection"].is_object()
            && (payload["previousModelSelection"]["providerId"] != provider
                || payload["previousModelSelection"]["modelId"] != model))
            .then(|| {
                let previous = &payload["previousModelSelection"];
                json!({"eventId": event.id, "origin": payload["origin"],
                    "from": {"provider": previous["providerId"], "model": previous["modelId"]},
                    "to": {"provider": provider, "model": model}})
            });
        if !config_changed && !window_changed && transition.is_none() {
            return Vec::new();
        }
        let mut patch = Map::new();
        if config_changed {
            let mut config = prev.clone();
            config["modelSelection"] = model_selection;
            config["provider"] = provider;
            config["model"] = model;
            config["thought"] = thought;
            config["thoughtLevels"] = levels;
            patch.insert("config".into(), config);
        }
        if let Some(transition) = transition {
            patch.insert("modelTransition".into(), transition);
        }
        if window_changed {
            let mut usage = self.state["usage"].clone();
            usage["contextWindow"] = match window.flatten() {
                None => Value::Null,
                Some(n) if previous_window.is_object() => {
                    let mut window = previous_window.clone();
                    window["maxTokens"] = num(n);
                    window
                }
                Some(n) => json!({"usedTokens": num(self.window_used), "maxTokens": num(n),
                    "autoCompactThresholdTokens": null}),
            };
            patch.insert("usage".into(), usage);
        }
        vec![Delta::State(patch)]
    }

    /// Node `onModelComplete` for main-turn completions (the only kind the
    /// cold synthesis emits, always with zero usage).
    pub fn on_model_complete(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let main = match payload.get("querySource") {
            Some(source) => source == "main_turn",
            None => payload["stopReason"] != "tool_internal",
        };
        if main {
            self.continuation_text = None;
        }
        let length = payload["stopReason"]
            .as_str()
            .is_some_and(|reason| reason.trim().to_lowercase() == "length");
        if main && length && payload["toolCallCount"] == 0 {
            let turn = self.turn_of(event);
            if let Some(last) = self.rows.last().filter(|row| {
                row["kind"] == "assistantText"
                    && row["turnId"] == turn.as_str()
                    && row["state"] == "complete"
            }) {
                self.continuation_text = last["rowId"].as_u64();
            }
        }
        let mut deltas = Vec::new();
        let changes = &payload["fileChanges"];
        let supports_changes = main || payload["querySource"] == "subagent";
        if supports_changes && changes["files"].as_f64().is_some_and(|files| files > 0.0) {
            let header = self
                .headers
                .get(&self.turn_of(event))
                .and_then(|id| self.find_row(*id))
                .filter(|row| row["kind"] == "turnHeader");
            if let Some(header) = header {
                let mut header = header.clone();
                header["fileChanges"] = json!({"additions": changes["additions"],
                    "deletions": changes["deletions"], "files": changes["files"], "state": "active"});
                deltas.push(Delta::Upsert(header));
            }
        }
        if !main {
            return deltas;
        }
        // 冷合成的 ModelComplete 用量恒为零：上下文水位归零，累计值不变。
        self.window_used = 0.0;
        let max = payload["contextWindow"].as_f64().or(self.window_max);
        let usage = &self.state["usage"];
        let window = match max {
            None => Value::Null,
            Some(max) => {
                let mut window = json!({"usedTokens": 0, "maxTokens": num(max),
                    "autoCompactThresholdTokens": usage["contextWindow"]["autoCompactThresholdTokens"]});
                if super::facts::truthy(&payload["cacheHit"]) {
                    window["cache"] = payload["cacheHit"].clone();
                }
                if payload["contextUsageBreakdown"]
                    .as_array()
                    .is_some_and(|b| !b.is_empty())
                {
                    window["breakdown"] = payload["contextUsageBreakdown"].clone();
                }
                window
            }
        };
        let mut patch = Map::new();
        patch.insert(
            "usage".into(),
            json!({"contextWindow": window, "cumulative": usage["cumulative"]}),
        );
        deltas.push(Delta::State(patch));
        deltas
    }
}
