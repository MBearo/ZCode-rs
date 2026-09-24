//! Replaying cold events through the product projection (Node
//! `ProductProjection` hydration replay): the subset of the reducer the
//! synthesized events reach, with the same delta semantics (every handler
//! reads the snapshot before the event, then its deltas apply in order).
use super::events::Event;
use super::projection_state::{self as guards, Context};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Node `ConversationDelta` (without `row.removed`: cold replay never rewinds).
pub enum Delta {
    Append(Value),
    Upsert(Value),
    /// `row.delta`: appends to a streamable text path of a row.
    Text {
        row: u64,
        path: &'static str,
        append: String,
    },
    State(Map<String, Value>),
}

/// Node `TurnModelBaseline`.
#[derive(Clone, Debug)]
pub enum TurnModel {
    SilentInitial,
    SourceLess,
    Known { provider: String, model: String },
}

/// The rebuilt conversation: rows and the rest of the snapshot state.
#[derive(Clone, Debug, PartialEq)]
pub struct Cold {
    pub rows: Vec<Value>,
    pub state: Map<String, Value>,
    /// The stored message of each command-addressable row (Node
    /// `messageIdByRowId`) and the edit targets by entity (`editTargetsByEntity`).
    pub messages: BTreeMap<u64, String>,
    pub edit_targets: HashMap<String, Value>,
}

pub struct Projection {
    pub session_id: String,
    pub rows: Vec<Value>,
    index: HashMap<u64, usize>,
    pub state: Map<String, Value>,
    next_row: u64,
    pub streaming_text: Option<u64>,
    pub streaming_reasoning: Option<u64>,
    pub continuation_text: Option<u64>,
    pub tool_rows: HashMap<String, u64>,
    pub list_apps: HashMap<i64, Value>,
    pub open_tools: HashSet<String>,
    pub subagent_rows: HashMap<String, u64>,
    /// Node iterates this map in insertion order, which is row order here.
    pub message_by_row: BTreeMap<u64, String>,
    pub continuation_by_message: HashMap<String, u64>,
    pub entity_by_row: BTreeMap<u64, String>,
    pub edit_targets: HashMap<String, Value>,
    pub current_editable: Option<String>,
    pub stable_compact_boundary: Option<u64>,
    pub headers: HashMap<String, u64>,
    pub compact_markers: HashMap<String, u64>,
    pub goal_markers: HashMap<String, u64>,
    pub product_by_runtime: HashMap<String, String>,
    pub runtime_by_product: HashMap<String, String>,
    pub split_ordinal: HashMap<String, u64>,
    pub product_started: Option<i64>,
    pub current_turn: Option<String>,
    pub current_model_only: bool,
    pub window_max: Option<f64>,
    pub window_used: f64,
    pub last_turn_model: TurnModel,
}

impl Projection {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.into(),
            rows: Vec::new(),
            index: HashMap::new(),
            state: guards::initial(),
            next_row: 1,
            streaming_text: None,
            streaming_reasoning: None,
            continuation_text: None,
            tool_rows: HashMap::new(),
            list_apps: HashMap::new(),
            open_tools: HashSet::new(),
            subagent_rows: HashMap::new(),
            message_by_row: BTreeMap::new(),
            continuation_by_message: HashMap::new(),
            entity_by_row: BTreeMap::new(),
            edit_targets: HashMap::new(),
            current_editable: None,
            stable_compact_boundary: None,
            headers: HashMap::new(),
            compact_markers: HashMap::new(),
            goal_markers: HashMap::new(),
            product_by_runtime: HashMap::new(),
            runtime_by_product: HashMap::new(),
            split_ordinal: HashMap::new(),
            product_started: None,
            current_turn: None,
            current_model_only: false,
            window_max: None,
            window_used: 0.0,
            last_turn_model: TurnModel::SilentInitial,
        }
    }

    /// Node `applyHydrationEvent`.
    fn apply_event(&mut self, event: &Event) {
        let mut deltas = if event.kind == "assistant_feedback_updated" {
            self.on_feedback(event)
        } else {
            self.reduce(event)
        };
        if self.subagents_touched(&deltas) {
            let subagents = self.materialize_subagents(&deltas);
            deltas.extend(subagents);
        }
        self.apply(deltas);
    }

    fn reduce(&mut self, event: &Event) -> Vec<Delta> {
        match event.kind {
            "session_created" => {
                self.window_max = event.payload["contextWindow"].as_f64();
                Vec::new()
            }
            "turn_started" => self.on_turn_started(event),
            "model_streaming" => self.on_model_streaming(event),
            "model_selected" => self.on_model_selected(event),
            "model_complete" => self.on_model_complete(event),
            "tool_call_scheduled" => self.on_tool_scheduled(event),
            "tool_call_started" => self.on_tool_started(event),
            "tool_call_result" => self.on_tool_result(event),
            "turn_steer_drained" => self.on_steer_drained(event),
            "turn_complete" => self.on_turn_complete(event),
            "turn_error" => self.on_turn_error(event),
            "compact_started" | "compact_completed" | "compact_failed" => self.on_compact(event),
            "target_changed" => self.on_target_changed(event),
            "target_completion_verification" => self.on_target_verification(event),
            "session_forked" => self.on_session_forked(event),
            "subagent_spawned" => self.on_subagent_spawned(event),
            "subagent_stopped" => self.on_subagent_stopped(event),
            _ => Vec::new(),
        }
    }

    pub fn apply(&mut self, deltas: Vec<Delta>) {
        for delta in deltas {
            match delta {
                Delta::Append(row) => {
                    self.track_tool(&row);
                    let id = row_id(&row);
                    self.index.insert(id, self.rows.len());
                    self.rows.push(row);
                }
                Delta::Upsert(row) => {
                    if let Some(&index) = self.index.get(&row_id(&row)) {
                        self.track_tool(&row);
                        self.rows[index] = row;
                    }
                }
                Delta::Text { row, path, append } => {
                    if let Some(&index) = self.index.get(&row) {
                        append_text(&mut self.rows[index], path, &append);
                    }
                }
                Delta::State(patch) => self.state.extend(patch),
            }
        }
    }

    /// Node `updateToolIndexesAfterDeltas` for appended/upserted tool rows.
    fn track_tool(&mut self, row: &Value) {
        if row["kind"] != "toolCall" {
            return;
        }
        let id = row["toolCallId"].as_str().unwrap_or("").to_owned();
        if is_open_foreground_tool(row) {
            self.open_tools.insert(id);
        } else {
            self.open_tools.remove(&id);
        }
    }

    pub fn find_row(&self, id: u64) -> Option<&Value> {
        self.index.get(&id).map(|&index| &self.rows[index])
    }

    pub fn row_index(&self, id: u64) -> Option<usize> {
        self.index.get(&id).copied()
    }

    /// Node `rowBase`.
    pub fn row_base(&mut self, event: &Event, turn: &str, entity: &str) -> Value {
        let id = self.next_row;
        self.next_row += 1;
        self.entity_by_row.insert(id, entity.into());
        json!({
            "rowId": id,
            "turnId": turn,
            "entityId": entity,
            "productTurnId": turn,
            "visibility": "visible",
            "createdAt": event.at,
            "createdAtSeq": event.seq,
        })
    }

    pub fn runtime_turn(&self, event: &Event) -> String {
        event
            .turn
            .clone()
            .or_else(|| self.current_turn.clone())
            .unwrap_or_else(|| "turn-unknown".into())
    }

    /// Node `turnIdOf`.
    pub fn turn_of(&self, event: &Event) -> String {
        let runtime = self.runtime_turn(event);
        self.product_by_runtime
            .get(&runtime)
            .cloned()
            .unwrap_or(runtime)
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.state["control"]["phase"].as_str(),
            Some("running" | "prewarming")
        )
    }

    /// Node `turnHeaderForEvent`.
    pub fn header_for(&self, event: &Event) -> Option<&Value> {
        let id = *self.headers.get(&self.turn_of(event))?;
        self.find_row(id).filter(|row| row["kind"] == "turnHeader")
    }

    pub fn find_tool_row(&self, call: &str) -> Option<&Value> {
        let id = *self.tool_rows.get(call)?;
        self.find_row(id).filter(|row| row["kind"] == "toolCall")
    }

    fn followup_mode(&self) -> String {
        self.state["config"]["followupMode"]
            .as_str()
            .unwrap_or("queue")
            .to_owned()
    }

    /// Node `controlPatch`: `goal`/`queue` are replaced only when given.
    pub fn control_patch(
        &self,
        control: Value,
        goal: Option<Value>,
        queue: Option<Value>,
    ) -> Map<String, Value> {
        let mut next = self.state["control"].clone();
        if let (Some(next), Value::Object(changes)) = (next.as_object_mut(), control) {
            next.extend(changes);
        }
        let goal_value = goal.clone().unwrap_or_else(|| self.state["goal"].clone());
        let queue_value = queue.clone().unwrap_or_else(|| self.state["queue"].clone());
        let context = Context::of(&next, &goal_value, &queue_value);
        let (availability, routing) = (
            guards::availability(&context),
            guards::input_routing(&context, &self.followup_mode()),
        );
        let mut patch = Map::new();
        patch.insert("control".into(), next);
        if let Some(goal) = goal {
            patch.insert("goal".into(), goal);
        }
        if let Some(queue) = queue {
            patch.insert("queue".into(), queue);
        }
        patch.insert("availability".into(), availability);
        patch.insert("inputRouting".into(), routing);
        patch
    }

    /// Node `goalPatch`.
    pub fn goal_patch(&self, goal: Value) -> Map<String, Value> {
        let context = Context::of(&self.state["control"], &goal, &self.state["queue"]);
        let availability = guards::availability(&context);
        let mut patch = Map::new();
        patch.insert("goal".into(), goal);
        patch.insert("availability".into(), availability);
        patch
    }
}

pub fn row_id(row: &Value) -> u64 {
    row["rowId"].as_u64().unwrap_or(0)
}

/// Node `isOpenForegroundToolRow`.
pub fn is_open_foreground_tool(row: &Value) -> bool {
    row["backgrounded"] != true
        && matches!(
            row["status"].as_str(),
            Some("inputStreaming" | "pendingApproval" | "running")
        )
}

/// Node `appendToRow`.
fn append_text(row: &mut Value, path: &str, append: &str) {
    let kind = row["kind"].as_str().unwrap_or("");
    let target = match (path, kind) {
        ("text", "assistantText" | "reasoning") => &mut row["text"],
        ("inputText", "toolCall") => &mut row["inputText"],
        ("output.text", "toolCall") if row["output"].is_object() => &mut row["output"]["text"],
        ("summaryText", "subagent") => &mut row["summaryText"],
        _ => return,
    };
    let joined = format!("{}{append}", target.as_str().unwrap_or(""));
    *target = joined.into();
}

/// Node `ProductProjection` batch hydration: every event, then the command
/// row actions of the final snapshot.
pub fn replay(session_id: &str, events: &[Event]) -> Cold {
    let mut projection = Projection::new(session_id);
    for event in events {
        projection.apply_event(event);
    }
    let actions = projection.materialize_actions();
    projection.apply(actions);
    Cold {
        rows: projection.rows,
        state: projection.state,
        messages: projection.message_by_row,
        edit_targets: projection.edit_targets,
    }
}
