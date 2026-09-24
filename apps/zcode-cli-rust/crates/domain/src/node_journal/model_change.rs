//! Node's model change timeline (`recordPendingModelChange`,
//! `persistPendingModelChangeTimeline`): a selection switch is recorded and
//! stored as a separator when the next turn starts.
use super::Op;
use super::timeline::timeline_records;
use crate::session::Session;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChange {
    /// Node `requestId` (a part id naming the timeline message and part).
    pub request: String,
    pub from: Option<Value>,
    pub to: Value,
}

fn same(left: &Value, right: &Value) -> bool {
    left["providerId"] == right["providerId"]
        && left["modelId"] == right["modelId"]
        && left["options"]["reasoningLevel"] == right["options"]["reasoningLevel"]
}

/// A timeline model with its `provider/model` label.
fn labelled(selection: &Value) -> Value {
    let mut out = json!({"providerId": selection["providerId"], "modelId": selection["modelId"]});
    if let Some(options) = selection.get("options").filter(|o| !o.is_null()) {
        out["options"] = options.clone();
    }
    let label = format!(
        "{}/{}",
        selection["providerId"].as_str().unwrap_or(""),
        selection["modelId"].as_str().unwrap_or("")
    );
    out["label"] = label.into();
    out
}

impl Session {
    /// Node `recordPendingModelChange`: consecutive switches keep the first
    /// source; switching back clears the change.
    pub fn node_record_model_change(&mut self, request: String, from: Option<Value>, to: Value) {
        if !self.node.created {
            return;
        }
        let existing = self.node.model_change.take();
        let from = existing.as_ref().map_or(from, |e| e.from.clone());
        if from.as_ref().is_some_and(|from| same(from, &to)) {
            return;
        }
        let request = existing.map_or(request, |e| e.request);
        self.node.model_change = Some(ModelChange { request, from, to });
    }

    /// Node `persistPendingModelChangeTimeline` at a turn's start.
    pub(super) fn node_flush_model_change(&mut self, now: u64) {
        let Some(change) = self.node.model_change.take() else {
            return;
        };
        let message = format!("msg_{}_message", change.request);
        let part = format!("part_{}_timeline", change.request);
        let latest = self.node.latest.clone();
        let mut draft =
            json!({"timelineType": "model_change", "display": "separator", "status": "completed"});
        if let Some(latest) = &latest {
            draft["anchorMessageId"] = latest.clone().into();
        }
        if let Some(from) = &change.from {
            draft["fromModel"] = labelled(from);
        }
        draft["toModel"] = labelled(&change.to);
        draft["time"] = json!({"start": now, "end": now});
        let parent = latest.unwrap_or_else(|| message.clone());
        let (info, part) = timeline_records(
            &self.node_host(),
            (&message, &part),
            &parent,
            (now, Some(now)),
            draft,
        );
        self.node.push(now, Op::Message(info));
        self.node.push(now, Op::Part(part));
    }
}
