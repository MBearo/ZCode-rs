//! Goal records of a started `/goal` (spec rust-m11-node-storage §5.4): the
//! control-only user message promoted from its ledger row, the `resumed`
//! notice over a paused goal, and the continuation turn's notice.
use super::Engine;
use crate::domain::node_journal::{self as nj, Notice, Prompt};
use crate::domain::protocol::Command;
use serde_json::{Value, json};

impl Engine {
    /// Node `applyGoalCommand` → `recordExternalUserPrompt` and the first
    /// continuation turn (`turn`). `ids` are uuids for the user message and
    /// part, the resumed notice, and the continuation notice.
    pub(super) fn node_goal_prompt(
        &mut self,
        id: &str,
        turn: &str,
        c: &Command,
        metadata: Value,
        ids: (String, String, String, String, String),
        tools: &[String],
    ) {
        let now = self.clock.now();
        let (message, part, resumed, notice, notice_part) = ids;
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        let display = super::goal_commands::display_text(c);
        let message = nj::message_id(now, &message);
        if let Some(boundary) = s.history.inputs.last_mut() {
            boundary.node_message = Some(message.clone());
        }
        s.node_control_prompt(
            now,
            Prompt {
                message,
                part: nj::part_id(now, &part),
                turn: "",
                text: &display,
                command: Some(&c.command_id),
                queue_id: metadata["inputIntent"]["queueItemId"].as_str(),
                metadata: Some(metadata.clone()),
                tools,
                files: vec![],
            },
        );
        if std::mem::take(&mut s.node.goal_resumed) {
            let ids = (nj::message_id(now, &resumed), nj::part_id(now, &resumed));
            s.node_goal_state(now, ids, "resumed", tools);
        }
        let Some(goal) = s.goal.clone() else {
            return;
        };
        let text = super::goal_commands::continuation_text(&goal, None);
        s.node_goal_continuation(
            now,
            turn,
            Notice {
                message: nj::message_id(now, &notice),
                part: nj::part_id(now, &notice_part),
                source: "goal-continuation",
                text: &text,
                metadata: Some(json!({"targetId": goal.target_id, "visibility": "model-only"})),
                tools,
            },
        );
    }

    /// The continuation turn of a resumed goal or a failed verification.
    pub(super) fn node_goal_turn(&mut self, id: &str, turn: &str, text: &str) {
        if !self.journaled(id) {
            return;
        }
        let now = self.clock.now();
        let (message, part) = (self.clock.id(), self.clock.id());
        let tools = self.tool_names();
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        let Some(target) = s.goal.as_ref().map(|g| g.target_id.clone()) else {
            return;
        };
        s.node_goal_continuation(
            now,
            turn,
            Notice {
                message: nj::message_id(now, &message),
                part: nj::part_id(now, &part),
                source: "goal-continuation",
                text,
                metadata: Some(json!({"targetId": target, "visibility": "model-only"})),
                tools: &tools,
            },
        );
    }
}
