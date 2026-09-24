//! Writing a Rust session as Node session records (spec rust-m11-node-storage
//! §5.1–5.2). The journal lives on the session; the engine calls its hooks at
//! the moments Node persists, and the store applies the queued writes with
//! Node's repository SQL in one transaction per commit. Pure: no IO.
mod assistant;
mod compact;
mod finish;
pub mod intent;
mod model_change;
mod notice;
mod queue;
pub mod records;
mod session;
pub mod timeline;
mod turn;

pub use assistant::{reasoning_parts, tool_input};
pub use compact::CompactStart;
pub use finish::Outcome;
pub use notice::Notice;
pub use queue::Notification;
pub use turn::{Admission, Prompt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The Node ids and progress of the session's current turn and model step.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeJournal {
    /// The session row exists in the Node database.
    #[serde(default)]
    pub created: bool,
    #[serde(default)]
    pub turn: Option<Turn>,
    /// The running compaction's timeline.
    #[serde(default)]
    pub compaction: Option<timeline::Compaction>,
    /// Node `latestConversationMessageId`: the last stored conversation message.
    #[serde(default)]
    pub latest: Option<String>,
    /// Node `pendingModelChangeTimeline`, stored when the next turn starts.
    #[serde(default)]
    pub model_change: Option<model_change::ModelChange>,
    #[serde(skip)]
    pub pending: Vec<Write>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    /// `anchor.turnId` (`turn_<run turn uuid>`).
    pub runtime: String,
    /// The user message that started the turn.
    pub user: String,
    /// The turn's messages in storage order (Node `orderedMessageIds`).
    pub messages: Vec<String>,
    /// Node `historyRoundCount`: assistant steps committed to the history.
    pub rounds: u64,
    /// The last completed assistant without tool calls (the fork boundary).
    pub boundary: Option<String>,
    pub step: Option<Step>,
}

/// One model request's assistant message.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub assistant: String,
    pub created: u64,
    pub provider: String,
    pub model: String,
    /// `model_request_completed` facts: finish reason and normalized usage.
    pub finish: Option<Value>,
    pub usage: Option<Value>,
    pub tools: Vec<Tool>,
    /// The step's assistant message was completed.
    pub completed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub call: String,
    pub part: String,
    pub index: usize,
    pub name: String,
    pub input: Value,
    pub started: Option<u64>,
    pub done: bool,
}

/// One Node repository write, stamped with the time it happened (Node's
/// `Date.now()` at the write).
#[derive(Clone, Debug, PartialEq)]
pub struct Write {
    pub at: u64,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Node `createSession` input (`CreateSessionInput`).
    CreateSession(Value),
    /// Node `updateSession` input (`UpdateSessionInput`).
    UpdateSession(Value),
    /// Node `saveSessionInput` (`{id, sessionID, kind, delivery, payload}`).
    SaveInput(Value),
    /// Node `promoteSessionInput`: the user message with its parts.
    PromoteInput {
        id: String,
        message: Value,
        parts: Vec<Value>,
    },
    Message(Value),
    Part(Value),
    RemoveMessage(String),
    /// Node `saveSessionEntry` (`SessionEntryInfo`).
    Entry(Value),
    /// Node `updateSessionInputs`: `[{id, text?, queuePosition?, delivery?, intent?}]`.
    UpdateInputs(Vec<Value>),
    /// Node `markSessionInputPromoted` (a notification batch's ledger rows).
    MarkInputPromoted {
        id: String,
        message: String,
    },
    /// Node `settleSessionInput` (only an `admitted` row changes).
    SettleInput {
        id: String,
        status: String,
        reason: Option<String>,
    },
    /// Node `rewindConversationToMessage`: `session.revert` cuts the active
    /// branch before the user message `target`; `anchor` is the requested
    /// message (the retried assistant, or `target` itself for an edit).
    Rewind {
        target: String,
        anchor: String,
    },
    /// Node stable conversation fork, committed with the child session:
    /// `{parent, boundary, command, revision, selection, execution}` (the
    /// parent runtime's selection and execution state Node reads live).
    Fork(Value),
    /// Node `persistCompactSummary` and the completed compaction timeline;
    /// the store selects the preserved tail in the stored transcript.
    CompactSummary(Value),
    /// Node `updateTodos`: `[{content, status, priority}]`.
    Todos(Vec<Value>),
    /// Node `persistStableForkCompletionBoundary` (reads the stored transcript).
    StableBoundary {
        boundary: String,
        start: String,
        rounds: u64,
        turn: String,
    },
}

impl NodeJournal {
    pub fn push(&mut self, at: u64, op: Op) {
        self.pending.push(Write { at, op });
    }

    /// The queued writes, emptying the queue.
    pub fn take(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.pending)
    }
}

/// Node `createMessageId` / `createPartId` over the engine's clock.
pub fn message_id(now: u64, uuid: &str) -> String {
    crate::node_ids::message_id(now, uuid)
}

pub fn part_id(now: u64, uuid: &str) -> String {
    crate::node_ids::part_id(now, uuid)
}

#[cfg(test)]
mod tests;
