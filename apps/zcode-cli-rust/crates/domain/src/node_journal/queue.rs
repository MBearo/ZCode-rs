//! Busy-input ledger updates and guided prompts (Node `steering.ts`: queue
//! edits, removals, delivery fallbacks and the guide drain). Only a session
//! stored in the Node database records them.
use super::{Op, Prompt};
use crate::session::Session;
use serde_json::Value;

impl Session {
    /// Node `updateSessionInputs`: `[{id, text?, queuePosition?, delivery?, intent?}]`.
    pub fn node_update_inputs(&mut self, now: u64, updates: Vec<Value>) {
        if self.node.created && !updates.is_empty() {
            self.node.push(now, Op::UpdateInputs(updates));
        }
    }

    /// Node `settleSessionInput` of an admitted input (a removed queue item is
    /// `cancelled/user_removed`).
    pub fn node_settle_input(&mut self, now: u64, id: &str, status: &str, reason: &str) {
        if self.node.created {
            let (id, status, reason) = (id.into(), status.into(), Some(reason.into()));
            self.node.push(now, Op::SettleInput { id, status, reason });
        }
    }

    /// Node guide drain: the guided input's user message joins the running
    /// turn, promoted from its ledger row.
    pub fn node_guided_prompt(&mut self, now: u64, p: Prompt) {
        let Some(runtime) = self.node.turn.as_ref().map(|t| t.runtime.clone()) else {
            return;
        };
        let message = p.message.clone();
        self.push_prompt(now, p, &runtime);
        if let Some(turn) = self.node.turn.as_mut() {
            turn.messages.push(message);
        }
    }
}
