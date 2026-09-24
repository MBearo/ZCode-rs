//! Assistant content reductions (Node `ProductProjection.onModelStreaming`,
//! the text/reasoning row lifecycle, `onAssistantFeedbackUpdated` and the
//! guided `onTurnSteerDrained`), with the `normalizeModelStreaming` identity.
use super::events::Event;
use super::facts::{text, truthy};
use super::projection::{Delta, Projection};
use super::reduce_finish::complete_segments;
use serde_json::{Value, json};

/// The canonical identity of a streaming segment (Node
/// `CanonicalAssistantSegmentFact`).
struct Segment {
    entity: String,
    message: Option<String>,
}

impl Projection {
    /// Node `openSegmentIdentity` of the open text or reasoning row.
    fn open_segment(&self, row: Option<u64>) -> Option<Segment> {
        let row = row?;
        Some(Segment {
            entity: self.entity_by_row.get(&row)?.clone(),
            message: self.message_by_row.get(&row).cloned(),
        })
    }

    /// Node `normalizeModelStreaming`.
    fn segment(&self, event: &Event) -> Segment {
        let payload = &event.payload;
        let kind = payload["kind"].as_str().unwrap_or("");
        let lane_open = if kind.starts_with("text_") {
            self.streaming_text
        } else if kind.starts_with("reasoning_") {
            self.streaming_reasoning
        } else {
            None
        };
        let starts = matches!(kind, "text_start" | "reasoning_start" | "tool_input_start");
        let inherited = if starts {
            None
        } else {
            self.open_segment(lane_open)
        };
        let message = text(&payload["assistantMessageId"])
            .map(str::to_owned)
            .or_else(|| inherited.as_ref().and_then(|s| s.message.clone()));
        let part = text(&payload["partId"]).map(str::to_owned);
        let tool = text(&payload["toolCallId"]).map(str::to_owned);
        let tool_lane = kind.starts_with("tool_") || kind == "tool_call";
        let intrinsic = if tool_lane {
            tool.clone().or(part.clone())
        } else {
            part.clone().or(tool)
        };
        let entity = message
            .clone()
            .or_else(|| inherited.map(|s| s.entity))
            .or(intrinsic)
            .unwrap_or_else(|| {
                format!(
                    "legacy:assistant:{}:{}:{}",
                    self.session_id,
                    self.runtime_turn(event),
                    part.unwrap_or_else(|| event.id.clone())
                )
            });
        Segment { entity, message }
    }

    /// Node `onModelStreaming` for the text and reasoning lanes the cold
    /// synthesis emits; late content outside a running turn is dropped.
    pub fn on_model_streaming(&mut self, event: &Event) -> Vec<Delta> {
        let segment = self.segment(event);
        if !self.is_running() {
            return Vec::new();
        }
        let payload = &event.payload;
        let delta = payload["delta"].as_str().unwrap_or("").to_owned();
        match payload["kind"].as_str() {
            Some("text_start") => self.open_text(event, &segment),
            Some("text_delta") => {
                let mut deltas = match self.streaming_text {
                    None => self.open_text(event, &segment),
                    Some(_) => Vec::new(),
                };
                let row = self.streaming_text.unwrap_or(0);
                deltas.push(Delta::Text {
                    row,
                    path: "text",
                    append: delta,
                });
                deltas
            }
            Some("text_end") => self.close_text("complete"),
            Some("reasoning_start") => self.open_reasoning(event, &segment),
            Some("reasoning_delta") => {
                let mut deltas = match self.streaming_reasoning {
                    None => self.open_reasoning(event, &segment),
                    Some(_) => Vec::new(),
                };
                let row = self.streaming_reasoning.unwrap_or(0);
                deltas.push(Delta::Text {
                    row,
                    path: "text",
                    append: delta,
                });
                deltas
            }
            Some("reasoning_end") => self.close_reasoning("complete"),
            _ => Vec::new(),
        }
    }

    /// Node `openTextRow`: a new row, or the output-token continuation of the
    /// row right before it in the same turn.
    fn open_text(&mut self, event: &Event, segment: &Segment) -> Vec<Delta> {
        let mut deltas = self.close_text("complete");
        let continuation = self
            .continuation_text
            .take()
            .and_then(|id| self.find_row(id))
            .cloned();
        let turn = self.turn_of(event);
        let last = self.rows.last().map(|row| row["rowId"].clone());
        if let Some(mut row) = continuation.filter(|row| {
            row["kind"] == "assistantText"
                && row["turnId"] == turn.as_str()
                && last.as_ref() == Some(&row["rowId"])
        }) {
            let id = row["rowId"].as_u64().unwrap_or(0);
            let object = row.as_object_mut().expect("rows are objects");
            for key in ["actions", "assistantResponseId", "feedback"] {
                object.shift_remove(key);
            }
            object.insert("entityId".into(), segment.entity.clone().into());
            if let Some(message) = &segment.message {
                object.insert("assistantResponseId".into(), message.clone().into());
            }
            object.insert("state".into(), "streaming".into());
            self.streaming_text = Some(id);
            self.entity_by_row.insert(id, segment.entity.clone());
            if let Some(previous) = self.message_by_row.get(&id) {
                self.continuation_by_message.insert(previous.clone(), id);
            }
            if let Some(message) = &segment.message {
                self.message_by_row.insert(id, message.clone());
            }
            deltas.push(Delta::Upsert(row));
            return deltas;
        }
        let mut row = self.row_base(event, &turn, &segment.entity);
        let id = row["rowId"].as_u64().unwrap_or(0);
        row["kind"] = "assistantText".into();
        if let Some(message) = &segment.message {
            row["assistantResponseId"] = message.clone().into();
        }
        row["text"] = "".into();
        row["state"] = "streaming".into();
        self.streaming_text = Some(id);
        self.entity_by_row.insert(id, segment.entity.clone());
        if let Some(message) = &segment.message {
            self.message_by_row.insert(id, message.clone());
        }
        deltas.push(Delta::Append(row));
        deltas
    }

    /// Node `closeTextRow`.
    fn close_text(&mut self, state: &str) -> Vec<Delta> {
        let Some(id) = self.streaming_text.take() else {
            return Vec::new();
        };
        match self
            .find_row(id)
            .filter(|row| row["kind"] == "assistantText")
        {
            Some(row) => {
                let mut row = row.clone();
                row["state"] = state.into();
                vec![Delta::Upsert(row)]
            }
            None => Vec::new(),
        }
    }

    /// Node `openReasoningRow`.
    fn open_reasoning(&mut self, event: &Event, segment: &Segment) -> Vec<Delta> {
        let mut deltas = self.close_reasoning("complete");
        let turn = self.turn_of(event);
        let mut row = self.row_base(event, &turn, &segment.entity);
        let id = row["rowId"].as_u64().unwrap_or(0);
        row["kind"] = "reasoning".into();
        if let Some(message) = &segment.message {
            row["assistantResponseId"] = message.clone().into();
        }
        row["text"] = "".into();
        row["state"] = "streaming".into();
        self.streaming_reasoning = Some(id);
        self.entity_by_row.insert(id, segment.entity.clone());
        deltas.push(Delta::Append(row));
        deltas
    }

    /// Node `closeReasoningRow`.
    fn close_reasoning(&mut self, state: &str) -> Vec<Delta> {
        let Some(id) = self.streaming_reasoning.take() else {
            return Vec::new();
        };
        match self.find_row(id).filter(|row| row["kind"] == "reasoning") {
            Some(row) => {
                let mut row = row.clone();
                row["state"] = state.into();
                vec![Delta::Upsert(row)]
            }
            None => Vec::new(),
        }
    }

    /// Node `closeStreamingRows`.
    pub fn close_streaming_rows(&mut self, state: &str) -> Vec<Delta> {
        let mut deltas = self.close_text(state);
        deltas.extend(self.close_reasoning(state));
        deltas
    }

    /// Node `onAssistantFeedbackUpdated`.
    pub fn on_feedback(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let Some(row) = self
            .rows
            .iter()
            .find(|row| row["kind"] == "assistantText" && row["entityId"] == payload["entityId"])
        else {
            return Vec::new();
        };
        let mut row = row.clone();
        if payload["feedback"].is_null() {
            if row.get("feedback").is_none() {
                return Vec::new();
            }
            row.as_object_mut()
                .expect("rows are objects")
                .shift_remove("feedback");
            return vec![Delta::Upsert(row)];
        }
        if row["feedback"] == payload["feedback"] {
            return Vec::new();
        }
        row["feedback"] = payload["feedback"].clone();
        vec![Delta::Upsert(row)]
    }

    /// Node `onTurnSteerDrained` for the guided inputs the cold synthesis
    /// emits: an inline real-user row opening a new work segment.
    pub fn on_steer_drained(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let mut deltas = Vec::new();
        for item in payload["drainedInputs"].as_array().into_iter().flatten() {
            let intent = &item["intent"];
            let message = text(&item["messageId"]).map(str::to_owned);
            let pending = item["pendingInputId"].as_str().unwrap_or("").to_owned();
            let entity = message.clone().unwrap_or_else(|| pending.clone());
            let turn = self.turn_of(event);
            let mut row = self.row_base(event, &turn, &entity);
            let id = row["rowId"].as_u64().unwrap_or(0);
            row["kind"] = "userInput".into();
            row["text"] = item["text"].clone();
            row["origin"] = "realUser".into();
            row["guided"] = true.into();
            if truthy(&intent["sourceCommandId"]) {
                row["sourceCommandId"] = intent["sourceCommandId"].clone();
            }
            let root = match &intent["provenance"]["sourceCommandId"] {
                Value::Null => intent["sourceCommandId"].clone(),
                other => other.clone(),
            };
            if truthy(&root) {
                row["rootSourceCommandId"] = root;
            }
            if truthy(&intent["clientId"]) {
                row["clientId"] = intent["clientId"].clone();
            }
            let refs = intent["attachmentRefs"]
                .as_array()
                .filter(|a| !a.is_empty());
            if let Some(refs) = refs {
                row["attachments"] = refs.clone().into();
            }
            self.entity_by_row.insert(id, entity.clone());
            if let Some(message) = message.filter(|_| intent["kind"] != "compact") {
                let source = intent["sourceCommandId"].clone();
                let attachments = intent["attachmentRefs"].as_array().cloned();
                let steer = json!({"input": item["text"], "intent": intent});
                let target = super::reduce_turn::edit_target(
                    &entity,
                    &turn,
                    &message,
                    &steer,
                    &source,
                    &attachments,
                );
                self.message_by_row.insert(id, message);
                self.edit_targets.insert(entity.clone(), target);
            }
            deltas.extend(self.guided_segment(event, &entity));
            deltas.push(Delta::Append(row));
        }
        deltas
    }

    /// Node `openGuidedWorkSegment`.
    fn guided_segment(&self, event: &Event, trigger: &str) -> Vec<Delta> {
        let Some(header) = self
            .header_for(event)
            .filter(|h| h["executionKind"] != "controlOnly")
        else {
            return Vec::new();
        };
        let mut header = header.clone();
        let existing = match header.get("workSegments") {
            Some(segments) => segments.clone(),
            None => {
                json!([{"segmentId": format!("{}:initial", header["turnId"].as_str().unwrap_or("")),
                "startedAt": header["startedAt"]}])
            }
        };
        let mut segments = complete_segments(&existing, event.at);
        if let Some(list) = segments.as_array_mut() {
            list.push(
                json!({"segmentId": trigger, "triggerEntityId": trigger, "startedAt": event.at}),
            );
        }
        header["workSegments"] = segments;
        vec![Delta::Upsert(header)]
    }
}
