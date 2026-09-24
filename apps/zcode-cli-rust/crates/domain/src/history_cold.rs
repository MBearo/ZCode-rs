//! Edit, retry and fork boundaries of a session loaded from Node records
//! (spec rust-m11-node-storage §6.1): the cold projection names each
//! command-addressable row's stored message and each real input's edit target;
//! the rebuilt model context names each message's stored source.
use super::history::{InputBoundary, ResponseBoundary, State};
use super::session::Session;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// What the cold load knows about the stored transcript behind the rows.
pub struct ColdAnchors<'a> {
    /// Row id → stored message id (Node `messageIdByRowId`).
    pub row_messages: &'a BTreeMap<u64, String>,
    /// Entity → Node `ConversationEditTarget`.
    pub edit_targets: &'a HashMap<String, Value>,
    /// The stored message id of each of `Session.messages`.
    pub sources: &'a [String],
}

/// The runtime payload of a Node edit target's intent.
fn payload(intent: &Value) -> Value {
    let mut payload = json!({"text": intent["text"]});
    for key in ["attachments", "modelSelection", "mode", "planEnabled"] {
        if let Some(value) = intent.get(key).filter(|v| !v.is_null()) {
            payload[key] = value.clone();
        }
    }
    if let Some(provenance) = intent.get("provenance").filter(|v| !v.is_null()) {
        payload["_provenance"] = provenance.clone();
    }
    payload
}

impl Session {
    /// Rebuilds `history.inputs` and `history.responses` from the cold rows.
    /// Boundary states are the loaded state: Node keeps no per-turn snapshot.
    pub fn restore_boundaries(&mut self, anchors: ColdAnchors) {
        let state = State::capture(self);
        let first_entry = |message: &str| anchors.sources.iter().position(|s| s == message);
        let mut inputs = vec![];
        let mut responses = vec![];
        let mut opened: Vec<&str> = vec![];
        for (index, row) in self.rows.iter().enumerate() {
            let turn = row["turnId"].as_str().unwrap_or("");
            let entity = row["entityId"].as_str().unwrap_or("");
            let message = row["rowId"]
                .as_u64()
                .and_then(|id| anchors.row_messages.get(&id));
            if row["kind"] == "userInput" && row["origin"] == "realUser" {
                let (Some(message), Some(target)) = (message, anchors.edit_targets.get(entity))
                else {
                    continue;
                };
                let Some(entry) = first_entry(message) else {
                    continue;
                };
                // 轮内第一条输入从轮头开始截断；轮内后续（引导）输入从自身所在行开始。
                let header = self.rows[..index]
                    .iter()
                    .rposition(|r| r["kind"] == "turnHeader" && r["turnId"] == turn)
                    .filter(|_| !opened.contains(&turn));
                opened.push(turn);
                let intent = &target["intent"];
                inputs.push(InputBoundary {
                    entity: entity.into(),
                    turn: turn.into(),
                    row: header.unwrap_or(index),
                    user_row: index,
                    message: entry,
                    state: state.clone(),
                    kind: intent["kind"].as_str().unwrap_or("sendText").into(),
                    payload: payload(intent),
                    node_message: Some(message.clone()),
                });
            } else if row["kind"] == "assistantText" && row["state"] == "complete" {
                let Some(message) = message else {
                    continue;
                };
                let Some(last) = anchors.sources.iter().rposition(|s| s == message) else {
                    continue;
                };
                let calls = self.messages[last]["tool_calls"]
                    .as_array()
                    .is_some_and(|c| !c.is_empty());
                if calls || self.messages[last]["role"] != "assistant" {
                    continue;
                }
                responses.retain(|b: &ResponseBoundary| b.turn != turn);
                responses.push(ResponseBoundary {
                    entity: entity.into(),
                    turn: turn.into(),
                    row: index,
                    message: last + 1,
                    state: state.clone(),
                    node_message: Some(message.clone()),
                });
            }
        }
        self.history.inputs = inputs;
        self.history.responses = responses;
    }
}
