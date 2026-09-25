//! Rebuilding the model context of a Node transcript after a restart (Node
//! `session-history-hydrator.ts` and its helpers). Pure: artifact reads come
//! in through a callback. Spec rust-m11-node-storage §6.
mod attachment;
pub mod branch;
mod entries;
mod hydrate;
pub mod incoming;
pub mod reminders;

pub(crate) use attachment::prompt_attachment;
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

/// Node `contextUsageFromPersistedMessages` (`used`) over the active
/// messages: the latest compaction's post-compact count, else the latest
/// assistant's `tokens` (`total`, or `input + output`).
pub fn context_used(active: &[impl AsRef<Record>]) -> Option<u64> {
    let positive = |v: &Value| v.as_u64().filter(|n| *n > 0);
    for record in active.iter().rev() {
        let record = record.as_ref();
        let info = &record.info;
        if info["role"] == "user" && branch::truthy(info.get("summary")) {
            let boundary = record
                .parts
                .iter()
                .find(|p| p["type"] == "compaction" && p["compactBoundary"].is_object())
                .map(|p| &p["compactBoundary"]);
            if let Some(boundary) = boundary {
                let count = match &boundary["truePostCompactTokenCount"] {
                    Value::Null => &boundary["postCompactTokenCount"],
                    value => value,
                };
                if let Some(used) = positive(count) {
                    return Some(used);
                }
            }
        }
        if info["role"] != "assistant" || branch::truthy(info.get("summary")) {
            continue;
        }
        let tokens = &info["tokens"];
        if let Some(total) = positive(&tokens["total"]) {
            return Some(total);
        }
        if let Some(input) = positive(&tokens["input"]) {
            return Some(input + tokens["output"].as_u64().unwrap_or(0));
        }
    }
    None
}

/// Node `mainTurnCacheHitAggregateFromMessages` over the active branch: each
/// non-summary assistant record's cache use, at the model message index of
/// its first rebuilt message (`sources` names the stored message of each).
pub fn cache_hits(active: &[impl AsRef<Record>], sources: &[String]) -> crate::usage::CacheHits {
    let mut cursor = 0;
    let mut entries = vec![];
    for record in active {
        let record = record.as_ref();
        let start = cursor;
        while sources.get(cursor).is_some_and(|s| s == record.id()) {
            cursor += 1;
        }
        let info = &record.info;
        if info["role"] != "assistant" || branch::truthy(info.get("summary")) {
            continue;
        }
        if let Some(cache) = crate::usage::CacheUse::stored(&info["tokens"]) {
            entries.push((start, cache));
        }
    }
    crate::usage::CacheHits::new(entries)
}

#[cfg(test)]
mod tests;
