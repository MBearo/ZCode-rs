use super::{context::ContextState, goal::Goal, session::Session, todo::TodoItem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub inputs: Vec<InputBoundary>,
    pub responses: Vec<ResponseBoundary>,
    #[serde(skip)]
    pub action_rows: Vec<usize>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub context: ContextState,
    pub selection: Value,
    pub goal: Option<Goal>,
    pub todos: Vec<TodoItem>,
}
impl State {
    pub fn capture(s: &Session) -> Self {
        let mut goal = s.goal.clone();
        if let Some(goal) = &mut goal {
            // 边界只保存可恢复的当前目标，历史验证事实仍由原 timeline rows 拥有。
            goal.verifications.clear();
            goal.iterations.clear();
            goal.active_run_started_at_ms = None;
        }
        Self {
            context: s.context.clone(),
            selection: json!({"provider":s.provider,"model":s.model,"thought":s.reasoning_level,"thoughtLevels":s.thought_levels}),
            goal,
            todos: s.todos.clone(),
        }
    }
    pub fn restore(&self, s: &mut Session) {
        s.context = self.context.clone();
        s.provider = self.selection["provider"].as_str().unwrap().into();
        s.model = self.selection["model"].as_str().unwrap().into();
        s.reasoning_level = self.selection["thought"].as_str().unwrap().into();
        s.thought_levels =
            serde_json::from_value(self.selection["thoughtLevels"].clone()).unwrap_or_default();
        s.context_tokens = None;
        s.goal = self.goal.clone();
        s.todos = self.todos.clone();
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputBoundary {
    pub entity: String,
    pub turn: String,
    pub row: usize,
    pub user_row: usize,
    pub message: usize,
    pub state: State,
    pub kind: String,
    pub payload: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseBoundary {
    pub entity: String,
    pub turn: String,
    pub row: usize,
    pub message: usize,
    pub state: State,
}
impl Session {
    pub fn validate_history(&self) -> anyhow::Result<()> {
        let state_valid = |state: &State, messages: usize| {
            messages <= self.messages.len()
                && state.context.offset <= messages
                && ["provider", "model", "thought"]
                    .iter()
                    .all(|k| state.selection[k].is_string())
                && state.selection["thoughtLevels"]
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string))
        };
        let row_valid = |index: usize, entity: &str, turn: &str, kind: &str| {
            self.rows.get(index).is_some_and(|r| {
                r["entityId"] == entity && r["turnId"] == turn && r["kind"] == kind
            })
        };
        anyhow::ensure!(
            self.history.inputs.iter().all(|b| {
                b.row <= b.user_row
                    && state_valid(&b.state, b.message)
                    && row_valid(b.user_row, &b.entity, &b.turn, "userInput")
            }) && self.history.responses.iter().all(|b| {
                state_valid(&b.state, b.message)
                    && row_valid(b.row, &b.entity, &b.turn, "assistantText")
            }),
            "Invalid persisted history boundary"
        );
        Ok(())
    }
    pub fn record_response(&mut self, turn: &str) {
        if self.messages.last().is_none_or(|m| {
            m["role"] != "assistant" || m["tool_calls"].as_array().is_some_and(|c| !c.is_empty())
        }) {
            return;
        }
        let Some(row) = self.rows.iter().rposition(|r| {
            r["turnId"] == turn && r["kind"] == "assistantText" && r["state"] == "complete"
        }) else {
            return;
        };
        self.history.responses.retain(|b| b.turn != turn);
        self.history.responses.push(ResponseBoundary {
            entity: self.rows[row]["entityId"].as_str().unwrap().into(),
            turn: turn.into(),
            row,
            message: self.messages.len(),
            state: State::capture(self),
        });
    }
    pub fn history_actions(&mut self) -> Vec<Value> {
        let last_input = self
            .rows
            .iter()
            .rposition(|r| r["kind"] == "userInput" && r["origin"] == "realUser");
        let latest_turn = self
            .rows
            .iter()
            .rev()
            .find(|r| r["kind"] == "turnHeader")
            .and_then(|r| r["turnId"].as_str());
        let input = self.history.inputs.last().filter(|b| {
            self.rows
                .get(b.user_row)
                .is_some_and(|r| r["entityId"] == b.entity)
                && last_input == Some(b.user_row)
        });
        let response = self
            .rows
            .iter()
            .rposition(|r| r["kind"] == "assistantText" && r["turnId"].as_str() == latest_turn)
            .filter(|_| {
                self.history
                    .inputs
                    .iter()
                    .any(|i| Some(i.turn.as_str()) == latest_turn)
            });
        let mut wanted = Vec::new();
        if let Some(b) = input {
            wanted.push((
                b.user_row,
                json!({"canEdit":true,"editDisposition":"rewind"}),
            ));
        }
        if let Some(row) = response {
            let mut actions = json!({"canRetry":true});
            if self.history.responses.iter().any(|b| b.row == row) {
                actions["canFork"] = true.into();
            }
            wanted.push((row, actions));
        }
        let mut indices = std::mem::take(&mut self.history.action_rows);
        indices.extend(wanted.iter().map(|(i, _)| *i));
        indices.sort_unstable();
        indices.dedup();
        let mut deltas = Vec::new();
        for i in indices {
            let stable = self.history.responses.iter().any(|b| b.row == i);
            let mut next = wanted
                .iter()
                .find(|(pos, _)| *pos == i)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| {
                    if stable {
                        json!({"canFork":true})
                    } else {
                        json!({})
                    }
                });
            if self
                .rows
                .get(i)
                .is_some_and(|r| matches!(r["kind"].as_str(), Some("userInput" | "assistantText")))
                && self.file_checkpoints.iter().any(|c| {
                    !c.restored
                        && self
                            .rows
                            .get(i)
                            .and_then(|r| r["rowId"].as_u64())
                            .is_some_and(|row| c.row >= row)
                })
            {
                next["canRewindFiles"] = true.into();
            }
            if let Some(row) = self.rows.get_mut(i)
                && row.get("actions") != Some(&next)
            {
                row["actions"] = next;
                self.saved_rows = self.saved_rows.min(i);
                deltas.push(json!({"op":"row.upserted","row":row}));
            }
        }
        self.history.action_rows = wanted.iter().map(|(i, _)| *i).collect();
        deltas
    }
    pub fn cut_history(&mut self, row: usize, message: usize, state: &State) {
        self.rows.truncate(row);
        self.messages.truncate(message);
        self.history.inputs.retain(|b| b.row < row);
        self.history
            .responses
            .retain(|b| b.row < row && b.message <= message);
        self.history.action_rows.retain(|i| *i < row);
        self.history_rewrite = true;
        self.saved_rows = 0;
        self.saved_messages = 0;
        self.saved_inputs = 0;
        self.saved_responses = 0;
        state.restore(self);
        self.pending.clear();
        self.mailbox.clear();
        self.last_error = None;
        self.api_retry = None;
        self.run_id = None;
    }
}
