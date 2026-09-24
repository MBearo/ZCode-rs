//! Per-turn model selection facts for the cold synthesis: the modelChange
//! marker needs a `model_selected` event before each turn whose model differs
//! (Node `transcript-hydration.ts` "MC-cold").
use super::facts::{js_string, truthy};
use super::synth::Synth;
use super::synth_goals::template;
use crate::node_history::Record;
use serde_json::{Value, json};

/// Node `HydratedTimelineModel`; `previous` is `None` when undefined.
#[derive(Clone, Debug)]
pub struct TurnModel {
    pub selection: Value,
    pub previous: Option<Value>,
}

impl TurnModel {
    pub fn of(selection: Value) -> Self {
        Self {
            selection,
            previous: None,
        }
    }
}

/// Node `hydratedModelKey`.
fn model_key(selection: &Value) -> String {
    let level = &selection["options"]["reasoningLevel"];
    format!(
        "{}\u{0}{}\u{0}{}",
        template(selection.get("providerId")),
        template(selection.get("modelId")),
        if level.is_null() {
            String::new()
        } else {
            js_string(level)
        }
    )
}

/// Node `turnModelSelectionOfUserMessage`.
pub fn user_selection(record: &Record) -> Option<Value> {
    if record.info["role"] != "user" {
        return None;
    }
    record
        .info
        .get("modelSelection")
        .filter(|v| !v.is_null())
        .cloned()
}

/// Node `assistantModelSelectionOf`.
pub fn assistant_selection(record: &Record) -> Option<Value> {
    let info = &record.info;
    if info["role"] != "assistant" || info["semantics"]["kind"] == "timeline_event" {
        return None;
    }
    if !truthy(&info["providerId"]) || !truthy(&info["modelId"]) {
        return None;
    }
    let mut selection = json!({
        "providerId": js_string(&info["providerId"]),
        "modelId": js_string(&info["modelId"]),
    });
    if truthy(&info["reasoningLevel"]) {
        selection["options"] = json!({"reasoningLevel": info["reasoningLevel"]});
    }
    Some(selection)
}

fn sparse(model: &Value) -> Value {
    let mut out = json!({});
    for key in ["providerId", "modelId"] {
        if let Some(value) = model.get(key) {
            out[key] = value.clone();
        }
    }
    if truthy(&model["options"]) {
        out["options"] = model["options"].clone();
    }
    out
}

/// Node `modelChangeToModelOf`: the last `model_change` timeline part.
pub fn model_change(record: &Record) -> Option<TurnModel> {
    let part = record
        .parts
        .iter()
        .rev()
        .find(|p| p["type"] == "timeline" && p["timelineType"] == "model_change")?;
    if !truthy(&part["toModel"]) {
        return None;
    }
    Some(TurnModel {
        selection: sparse(&part["toModel"]),
        previous: Some(if truthy(&part["fromModel"]) {
            sparse(&part["fromModel"])
        } else {
            Value::Null
        }),
    })
}

/// The selection the synthesis last announced and an explicit boundary that
/// waits for the next accepted turn.
#[derive(Default)]
pub struct ModelTracker {
    last_key: Option<String>,
    pending: Option<TurnModel>,
}

impl ModelTracker {
    /// Node `selectTurnModel`.
    fn select(&mut self, s: &mut Synth, model: Option<TurnModel>) {
        let Some(model) = model else {
            return;
        };
        let key = model_key(&model.selection);
        if self.last_key.as_deref() == Some(key.as_str()) {
            return;
        }
        self.last_key = Some(key);
        let mut payload = json!({"modelSelection": model.selection});
        if let Some(previous) = model.previous {
            payload["previousModelSelection"] = previous;
        }
        s.push("model_selected", payload, None, None);
    }

    /// Node `selectAcceptedTurnModel`: an explicit boundary beats the turn's
    /// possibly stale snapshot.
    pub fn accept(&mut self, s: &mut Synth, fallback: Option<Value>) {
        let model = self.pending.take().or_else(|| fallback.map(TurnModel::of));
        self.select(s, model);
    }

    /// Node `recordTimelineModel`.
    pub fn record(&mut self, s: &mut Synth, model: TurnModel) {
        self.pending = Some(model.clone());
        self.select(s, Some(model));
    }
}
