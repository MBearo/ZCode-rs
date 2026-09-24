//! Turn lifecycle reductions (Node `ProductProjection.onTurnStarted`,
//! `onTurnComplete`, `onTurnError` and the header helpers) with the
//! `normalizeTurnStarted` fact they consume.
use super::events::Event;
use super::facts::{js_string, text, truthy};
use super::projection::{Delta, Projection, TurnModel};
use super::reduce_tools::notification_update;
use serde_json::{Map, Value, json};

/// Node `userInputOrigin`.
fn input_origin(source: Option<&str>) -> &'static str {
    match source {
        Some("background_task") => "backgroundResult",
        Some("goal-continuation") => "goalContinuation",
        Some("subagent" | "subagent_message") => "mailbox",
        Some("fork" | "plugin_reference" | "rewind" | "todo_reminder") => "synthetic",
        Some("workflow_launch") => "workflowLaunch",
        _ => "realUser",
    }
}

/// Node `turnHeaderOrigin`.
fn header_origin(source: Option<&str>) -> &'static str {
    match source {
        Some("background_task") => "backgroundResult",
        Some("goal-continuation") => "goalContinuation",
        Some("rewind") => "editRerun",
        Some("workflow_launch") => "workflowLaunch",
        _ => "userInput",
    }
}

/// Node `normalizeAttachments`.
fn attachments(payload: &Value) -> Option<Vec<Value>> {
    if let Some(refs) = payload["intent"]["attachmentRefs"]
        .as_array()
        .filter(|a| !a.is_empty())
    {
        return Some(refs.clone());
    }
    let metas = payload["attachments"]
        .as_array()
        .filter(|a| !a.is_empty())?;
    Some(
        metas
            .iter()
            .map(|meta| {
                let mut out = json!({});
                if truthy(&meta["ref"]) {
                    out["ref"] = meta["ref"].clone();
                }
                for key in ["fileName", "mime", "bytes"] {
                    out[key] = meta[key].clone();
                }
                out
            })
            .collect(),
    )
}

pub(super) fn put_truthy(map: &mut Value, key: &str, value: &Value) {
    if truthy(value) {
        map[key] = value.clone();
    }
}

pub(super) fn put_defined(map: &mut Value, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        map[key] = value.clone();
    }
}

impl Projection {
    /// Node `onTurnStarted` over the `normalizeTurnStarted` fact.
    pub fn on_turn_started(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let intent = &payload["intent"];
        let runtime = event
            .turn
            .clone()
            .unwrap_or_else(|| format!("turn-{}", js_string(&payload["turnNumber"])));
        let message = text(&payload["messageId"]).map(str::to_owned);
        let turn = message.clone().unwrap_or_else(|| runtime.clone());
        let entity = message
            .clone()
            .unwrap_or_else(|| format!("legacy:user:{}:{runtime}:{}", self.session_id, event.id));
        let source = payload["inputSource"].as_str();
        let model_only = payload["inputVisibility"] == "model-only";
        let execution = match &payload["executionKind"] {
            Value::Null => "agent".into(),
            other => other.clone(),
        };
        let source_command = match &intent["sourceCommandId"] {
            Value::Null => payload["inputId"].clone(),
            other => other.clone(),
        };
        let attachments = attachments(payload);

        self.current_turn = Some(runtime.clone());
        self.current_model_only = model_only;
        self.product_by_runtime.remove(&runtime);
        if turn != runtime {
            self.product_by_runtime
                .insert(runtime.clone(), turn.clone());
        }
        self.runtime_by_product
            .insert(turn.clone(), runtime.clone());
        self.split_ordinal.remove(&runtime);
        self.product_started = Some(event.at);
        self.streaming_text = None;
        self.streaming_reasoning = None;
        self.continuation_text = None;

        let mut deltas = self.background_notification(event);
        let config = &self.state["config"];
        let provider = config["provider"].as_str().unwrap_or("").to_owned();
        let model = config["model"].as_str().unwrap_or("").to_owned();
        let thought = config["thought"].clone();
        let has_model = !provider.is_empty() && !model.is_empty();
        let marker = match &self.last_turn_model {
            TurnModel::SourceLess if has_model => Some((
                format!("model-initial:{turn}:{provider}/{model}"),
                json!({"type": "modelChange", "toProvider": provider, "toModel": model, "toThought": thought}),
            )),
            TurnModel::Known {
                provider: from_p,
                model: from_m,
            } if has_model && (*from_p != provider || *from_m != model) => Some((
                format!("model-change:{turn}:{from_p}/{from_m}->{provider}/{model}"),
                json!({"type": "modelChange", "fromProvider": from_p, "fromModel": from_m,
                        "toProvider": provider, "toModel": model, "toThought": thought}),
            )),
            _ => None,
        };
        if let Some((entity, marker)) = marker {
            let mut row = self.row_base(event, &turn, &entity);
            row["kind"] = "timelineMarker".into();
            row["lane"] = "lightBoundary".into();
            row["marker"] = marker;
            deltas.push(Delta::Append(row));
        }
        if has_model {
            self.last_turn_model = TurnModel::Known { provider, model };
        }

        let mut header = self.row_base(event, &turn, &turn);
        header["kind"] = "turnHeader".into();
        header["origin"] = header_origin(source).into();
        header["executionKind"] = execution.clone();
        put_truthy(&mut header, "sourceCommandId", &source_command);
        put_truthy(&mut header, "originMeta", &payload["originMeta"]);
        put_truthy(&mut header, "workflowLaunch", &payload["workflowLaunch"]);
        header["state"] = "running".into();
        header["startedAt"] = header["createdAt"].clone();
        self.headers
            .insert(turn.clone(), header["rowId"].as_u64().unwrap_or(0));
        deltas.push(Delta::Append(header));

        if !model_only {
            let mut row = self.row_base(event, &turn, &entity);
            let row_id = row["rowId"].as_u64().unwrap_or(0);
            row["kind"] = "userInput".into();
            row["text"] = payload["input"].clone();
            row["origin"] = input_origin(source).into();
            put_truthy(&mut row, "sourceCommandId", &source_command);
            let root = match &intent["provenance"]["sourceCommandId"] {
                Value::Null => source_command.clone(),
                other => other.clone(),
            };
            put_truthy(&mut row, "rootSourceCommandId", &root);
            put_truthy(&mut row, "clientId", &intent["clientId"]);
            put_truthy(&mut row, "workflowLaunch", &payload["workflowLaunch"]);
            put_defined(&mut row, "epilogueStart", payload.get("epilogueStart"));
            if let Some(list) = &attachments {
                let placed: Vec<Value> = list
                    .iter()
                    .enumerate()
                    .map(|(index, attachment)| {
                        let mut attachment = attachment.clone();
                        if attachment["ref"].is_null() {
                            attachment["ref"] = format!("turn-attachment/{row_id}/{index}").into();
                        }
                        attachment
                    })
                    .collect();
                row["attachments"] = placed.into();
            }
            self.entity_by_row.insert(row_id, entity.clone());
            if let Some(message) = &message {
                let target = edit_target(
                    &entity,
                    &turn,
                    message,
                    payload,
                    &source_command,
                    &attachments,
                );
                self.message_by_row.insert(row_id, message.clone());
                self.edit_targets.insert(entity.clone(), target);
            }
            deltas.push(Delta::Append(row));
        }

        if execution == "agent" {
            let kind = if input_origin(source) == "goalContinuation" {
                "goalContinuation"
            } else {
                "primaryTurn"
            };
            let mut work = json!({ "kind": kind });
            put_truthy(
                &mut work,
                "foregroundExecutionId",
                &payload["foregroundExecutionId"],
            );
            work["startedAt"] = event.at.into();
            let control = json!({"phase": "running", "sessionEnded": false, "canStop": true,
                "stopState": "stoppable", "stopTargetKind": "assistant", "activeWorks": [work],
                "lastError": null, "apiRetry": null});
            deltas.push(Delta::State(self.control_patch(control, None, None)));
        }
        deltas
    }

    /// Node `applyBackgroundTaskNotification`: a background task's terminal
    /// state lands on the tool row that launched it.
    fn background_notification(&self, event: &Event) -> Vec<Delta> {
        let input = event.payload["input"].as_str().unwrap_or("");
        notification_update(self, input, event.at)
            .map(|row| vec![Delta::Upsert(row)])
            .unwrap_or_default()
    }
}

/// The canonical edit target of a real user row (Node `ConversationEditTarget`).
pub fn edit_target(
    entity: &str,
    turn: &str,
    message: &str,
    payload: &Value,
    source_command: &Value,
    attachments: &Option<Vec<Value>>,
) -> Value {
    let intent = &payload["intent"];
    let kind = if intent["kind"] == "sendGoalCommand" {
        "sendGoalCommand"
    } else {
        "sendText"
    };
    let text = match &intent["text"] {
        Value::Null => payload["input"].clone(),
        other => other.clone(),
    };
    let mut out = json!({"kind": kind, "text": text});
    put_truthy(&mut out, "sourceCommandId", source_command);
    put_truthy(&mut out, "clientId", &intent["clientId"]);
    if let Some(list) = attachments {
        out["attachments"] = list.clone().into();
    }
    put_truthy(&mut out, "queueItemId", &intent["queueItemId"]);
    put_defined(&mut out, "admissionSeq", intent.get("admissionSeq"));
    put_defined(&mut out, "admittedAt", intent.get("admittedAt"));
    for key in [
        "requestedDelivery",
        "admittedDelivery",
        "fallbackReasonCode",
        "modelSelection",
        "mode",
    ] {
        put_truthy(&mut out, key, &intent[key]);
    }
    put_defined(&mut out, "planEnabled", intent.get("planEnabled"));
    put_truthy(&mut out, "provenance", &intent["provenance"]);
    let mut target = Map::new();
    target.insert("entityId".into(), entity.into());
    target.insert("productTurnId".into(), turn.into());
    target.insert("transcriptMessageId".into(), message.into());
    target.insert("coveredByStableCompact".into(), false.into());
    target.insert("intent".into(), out);
    Value::Object(target)
}
