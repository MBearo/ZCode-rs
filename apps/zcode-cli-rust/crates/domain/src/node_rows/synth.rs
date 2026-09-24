//! Transcript → session events (Node `synthesizeEventsFromMessages`) and the
//! cold merge that adds the persisted goal (Node `mergeColdConversationEvents`
//! without in-memory events: a restarted runtime has none).
use super::events::{Event, GOAL_STATE_TRACE, HYDRATION_TRACE, date_ms};
use super::facts::{self, created_ms, end_ms, finite, js_string, text};
use super::policy::text_of;
use super::schemas;
use super::synth_compact::legacy_operation_id;
use super::synth_goals::{self, GoalEntry, GoalFact, template};
use super::synth_models::{self, ModelTracker};
use super::synth_parts::{self, TurnResult, is_cancellation, is_stream_recovery_discard};
use super::synth_turn::next_turn;
use crate::node_history::Record;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// Emission state shared by the part synthesizers.
pub struct Synth<'a> {
    pub events: Vec<Event>,
    seq: u64,
    base_ms: f64,
    pub compacts_emitted: HashSet<String>,
    pub durable_compacts: HashMap<String, &'a Value>,
    pub goals_emitted: HashSet<String>,
}

impl Synth<'_> {
    /// Node `push`: a source timestamp only restores display facts; order is `seq`.
    pub fn push(
        &mut self,
        kind: &'static str,
        payload: Value,
        turn: Option<&str>,
        at: Option<f64>,
    ) {
        self.seq += 1;
        let at = at.unwrap_or(self.base_ms + self.seq as f64);
        self.events.push(Event {
            id: format!("hydrate-{}", self.seq),
            seq: self.seq,
            at: date_ms(at),
            kind,
            turn: turn.map(str::to_owned),
            trace: HYDRATION_TRACE,
            payload,
        });
    }

    fn fallback_start(&self, record: &Record) -> f64 {
        created_ms(record).unwrap_or(self.base_ms + self.seq as f64)
    }
}

/// What the cold synthesis reads beside the transcript.
#[derive(Clone, Copy, Default)]
pub struct Sources<'a> {
    /// The context window of the selected model, when the registry knows it.
    pub context_window: Option<f64>,
    pub goal_entries: &'a [GoalEntry],
    /// Per-turn file change summaries keyed by the user message id.
    pub file_changes: Option<&'a HashMap<String, Value>>,
}

struct Collected {
    failure: Option<Value>,
    result: TurnResult,
    tool_calls: u64,
    rounds: f64,
    ended: f64,
}

pub(super) struct Turns<'a, 'b> {
    pub s: Synth<'a>,
    pub messages: &'a [Record],
    pub models: ModelTracker,
    by_anchor: HashMap<String, Vec<GoalFact>>,
    context_window: Option<f64>,
    pub turn_number: u64,
    last_turn: Option<String>,
    sources: Sources<'b>,
}

fn failure_of(error: &Value) -> Value {
    let data = synth_parts::error_data(error);
    let message = data
        .and_then(|d| text(d.get("message").unwrap_or(&Value::Null)))
        .map(Value::from)
        .unwrap_or_else(|| error["name"].clone());
    let mut failure = json!({"type": error["name"], "message": message});
    if let Some(attribution) = schemas::error_attribution(data.map_or(&Value::Null, |d| {
        d.get("attribution").unwrap_or(&Value::Null)
    })) {
        failure["attribution"] = attribution;
    }
    if let Some(retryable) = data
        .and_then(|d| d.get("retryable"))
        .filter(|v| v.is_boolean())
    {
        failure["retryable"] = retryable.clone();
    }
    if let Some(data) = error.get("data") {
        failure["data"] = data.clone();
    }
    failure
}

impl Turns<'_, '_> {
    pub fn fork_notice(&mut self, record: &Record, turn: Option<&str>) {
        if let Some(context) = facts::fork_context(record) {
            let mut payload =
                json!({"forkPoint": 0, "originalSessionId": context["parentSessionId"]});
            for key in ["restoredFileCount", "targetCheckpointId", "targetMessageId"] {
                if let Some(value) = context.get(key) {
                    payload[key] = value.clone();
                }
            }
            self.s.push("session_forked", payload, turn, None);
        }
    }

    /// Node `collectTurnOutput`: the assistant output up to the next boundary.
    fn collect(&mut self, mut index: usize, turn: &str, started: f64) -> (usize, Collected) {
        let mut out = Collected {
            failure: None,
            result: TurnResult::Success,
            tool_calls: 0,
            rounds: 0.0,
            ended: started,
        };
        let mut awaiting_recovery = false;
        while let Some(record) = self.messages.get(index) {
            if facts::is_turn_boundary(record) {
                break;
            }
            index += 1;
            if facts::is_provider_context_assistant(record) {
                continue;
            }
            if facts::is_fork_timeline(record) {
                self.fork_notice(record, Some(turn));
                continue;
            }
            if record.info["role"] == "user" && facts::steer_delivery(record) == Some("guide") {
                self.guide(record, turn);
                continue;
            }
            if record.info["role"] != "assistant" {
                continue;
            }
            let error = record.info.get("error").filter(|e| !e.is_null());
            awaiting_recovery = error.is_some_and(is_stream_recovery_discard);
            if let Some(error) = error.filter(|e| !is_cancellation(e) && !awaiting_recovery) {
                out.failure = Some(failure_of(error));
            }
            if let Some(model) = synth_models::model_change(record) {
                self.models.record(&mut self.s, model);
                continue;
            }
            out.rounds =
                finite(&record.info["anchor"]["historyRoundCount"]).unwrap_or(out.rounds + 1.0);
            let end = end_ms(record);
            if let Some(end) = end {
                out.ended = out.ended.max(end);
            }
            let (result, tool_calls) = synth_parts::assistant_parts(&mut self.s, record, turn);
            out.result = out.result.and(result);
            out.tool_calls += tool_calls;
            let finish = record.info["finish"]
                .as_str()
                .map(|f| f.trim().to_lowercase());
            if finish.as_deref() == Some("length") && tool_calls == 0 {
                // 持久化 finish=length 是请求级事实：零用量的 ModelComplete 只恢复续写资格。
                let payload = json!({"content": "", "stopReason": "length", "querySource": "main_turn",
                    "toolCallCount": 0, "usage": zero_usage()});
                self.s.push("model_complete", payload, Some(turn), end);
            }
            if let Some(anchored) = self.by_anchor.get(&js_string(&record.info["id"])) {
                for fact in anchored.clone() {
                    synth_goals::push_fact(&mut self.s, &fact, Some(turn));
                }
            }
        }
        if awaiting_recovery {
            out.result = out.result.and(TurnResult::Cancelled);
        }
        (index, out)
    }

    fn guide(&mut self, record: &Record, turn: &str) {
        let intent = facts::input_intent(record);
        let id = js_string(&record.info["id"]);
        let pending = intent
            .as_ref()
            .and_then(|i| i.get("queueItemId").filter(|v| !v.is_null()).cloned())
            .unwrap_or_else(|| format!("hydrate-steer-{id}").into());
        let mut input = json!({"pendingInputId": pending, "messageId": id,
            "text": text_of(&record.parts), "delivery": "guide"});
        if let Some(intent) = intent {
            input["intent"] = intent;
        }
        let payload = json!({"pendingInputIds": [pending], "injectedMessageIds": [id],
            "drainedInputs": [input], "targetTurnId": turn});
        self.s.push(
            "turn_steer_drained",
            payload,
            Some(turn),
            created_ms(record),
        );
    }

    /// Node `finishTurn`.
    fn finish(&mut self, turn: &str, started: f64, out: Collected, file_changes: Option<&Value>) {
        if let Some(failure) = out.failure {
            let payload = json!({"error": failure, "turnPhase": "model"});
            self.s
                .push("turn_error", payload, Some(turn), Some(out.ended));
            return;
        }
        let mut complete =
            json!({"content": "", "stopReason": "end_turn", "querySource": "main_turn"});
        if let Some(window) = self.context_window {
            complete["contextWindow"] = super::events::num(window);
        }
        complete["usage"] = zero_usage();
        if let Some(changes) = file_changes {
            complete["fileChanges"] = changes.clone();
        }
        self.s
            .push("model_complete", complete, Some(turn), Some(out.ended));
        let payload = json!({"response": "", "tokenCount": 0, "toolCallCount": out.tool_calls,
            "historyRoundCount": super::events::num(out.rounds),
            "duration": super::events::num((out.ended - started).max(0.0)),
            "resultType": out.result.as_str()});
        self.s
            .push("turn_complete", payload, Some(turn), Some(out.ended));
    }

    /// Node turn opening: the id, and the start taken before any model event.
    pub fn open(&mut self, record: &Record) -> Opened {
        self.turn_number += 1;
        let turn = format!("hydrate-turn-{}", self.turn_number);
        self.last_turn = Some(turn.clone());
        Opened {
            started: self.s.fallback_start(record),
            turn,
        }
    }

    /// Starts the opened turn, collects its output from `from` and finishes
    /// it; `changes` is the user message whose file change summary it carries.
    pub fn run(
        &mut self,
        opened: Opened,
        payload: Value,
        from: usize,
        changes: Option<&str>,
    ) -> usize {
        let Opened { turn, started } = opened;
        self.s
            .push("turn_started", payload, Some(&turn), Some(started));
        let (next, out) = self.collect(from, &turn, started);
        let changes = changes.and_then(|id| self.sources.file_changes?.get(id));
        self.finish(&turn, started, out, changes);
        next
    }
}

pub(super) struct Opened {
    turn: String,
    started: f64,
}

fn zero_usage() -> Value {
    json!({"inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 0, "cacheWriteTokens": 0})
}

/// Node `synthesizeEventsFromMessages`.
pub fn synthesize(messages: &[Record], sources: Sources) -> Vec<Event> {
    let mut durable_compacts: HashMap<String, &Value> = HashMap::new();
    for part in messages.iter().flat_map(|m| &m.parts) {
        let durable = ["timelineStatus", "tail_start_id", "compactBoundary"]
            .iter()
            .any(|key| facts::truthy(&part[*key]));
        if part["type"] != "compaction" || !durable {
            continue;
        }
        let operation = js_string(&legacy_operation_id(part));
        let replace = durable_compacts.get(&operation).is_none_or(|existing| {
            !facts::truthy(&existing["tail_start_id"]) && facts::truthy(&part["tail_start_id"])
        });
        if replace {
            durable_compacts.insert(operation, part);
        }
    }
    let entry_facts = synth_goals::merge_entry_facts(sources.goal_entries);
    let mut by_anchor: HashMap<String, Vec<GoalFact>> = HashMap::new();
    for fact in &entry_facts {
        if let Some(anchor) = fact.anchor_message.as_ref().filter(|a| facts::truthy(a)) {
            by_anchor
                .entry(template(Some(anchor)))
                .or_default()
                .push(fact.clone());
        }
    }
    let base_ms = messages
        .first()
        .and_then(|m| m.info["time"]["created"].as_f64())
        .unwrap_or(0.0);
    let mut t = Turns {
        s: Synth {
            events: Vec::new(),
            seq: 0,
            base_ms,
            compacts_emitted: HashSet::new(),
            durable_compacts,
            goals_emitted: HashSet::new(),
        },
        messages,
        models: ModelTracker::default(),
        by_anchor,
        context_window: sources.context_window,
        turn_number: 0,
        last_turn: None,
        sources,
    };
    let mut created = json!({"mode": "default"});
    if let Some(window) = sources.context_window {
        created["contextWindow"] = super::events::num(window);
    }
    t.s.push("session_created", created, None, None);
    let mut index = 0;
    while let Some(record) = messages.get(index) {
        index = next_turn(&mut t, record, index);
    }
    let last_turn = t.last_turn.clone();
    for fact in &entry_facts {
        synth_goals::push_fact(&mut t.s, fact, last_turn.as_deref());
    }
    t.s.events
}

/// Node `mergeColdConversationEvents` for a restarted runtime: the transcript
/// events, the persisted goal right after `session_created`, resequenced.
pub fn cold_events(messages: &[Record], sources: Sources, target: Option<&Value>) -> Vec<Event> {
    let mut events = synthesize(messages, sources);
    if let Some(target) = target.filter(|t| facts::truthy(t)) {
        let goal = Event {
            id: "hydrate-goal-state".into(),
            seq: 0,
            at: date_ms(finite(&target["time"]["updated"]).unwrap_or(f64::NAN)),
            kind: "target_changed",
            turn: None,
            trace: GOAL_STATE_TRACE,
            payload: json!({"action": "set", "source": "runtime", "target": target}),
        };
        events.insert(1.min(events.len()), goal);
    }
    for (index, event) in events.iter_mut().enumerate() {
        event.seq = index as u64 + 1;
    }
    events
}
