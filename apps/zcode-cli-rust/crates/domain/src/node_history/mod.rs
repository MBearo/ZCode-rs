//! Rebuilding the model context of a Node transcript after a restart (Node
//! `session-history-hydrator.ts` and its helpers). Pure: artifact reads come
//! in through a callback. Spec rust-m11-node-storage §6.
mod attachment;
pub mod branch;
mod entries;
mod hydrate;
mod incoming;
pub mod reminders;

pub use branch::{Branch, active_messages, select_branch};
pub use entries::Entry;
pub use hydrate::{Hydrated, hydrate};

use serde_json::Value;

/// A decoded Node `MessageWithParts`: the message info and its parts in
/// storage order, both including their id members.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub info: Value,
    pub parts: Vec<Value>,
}

impl Record {
    pub fn id(&self) -> &str {
        self.info["id"].as_str().unwrap_or("")
    }
}

#[cfg(test)]
mod tests;
