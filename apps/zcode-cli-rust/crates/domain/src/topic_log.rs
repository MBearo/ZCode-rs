//! Actor-owned retained delta log of a topic and the last published state
//! patch. Spec rust-m8-delivery §3–4.
use super::delivery::json_bytes;
use serde_json::{Map, Value};
use std::{collections::VecDeque, sync::Arc};

/// Deltas of one publication with their compact JSON sizes, shared by the log
/// and the delivery event.
pub type Deltas = Arc<[(Value, usize)]>;

pub fn sized(deltas: Vec<Value>) -> Deltas {
    deltas
        .into_iter()
        .map(|delta| {
            let size = json_bytes(&delta);
            (delta, size)
        })
        .collect()
}

#[derive(Clone, Debug)]
struct Entry {
    from: u64,
    deltas: Deltas,
    bytes: usize,
}

/// Recent publications `(from, to]` of one epoch, contiguous up to `head`.
#[derive(Clone, Debug)]
pub struct TopicLog {
    epoch: String,
    floor: u64,
    head: u64,
    entries: VecDeque<Entry>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl TopicLog {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            epoch: String::new(),
            floor: 0,
            head: 0,
            entries: VecDeque::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    /// Node `eventRetentionPerSession` plus an 8 MiB safety valve.
    pub fn conversation() -> Self {
        Self::new(2000, 8 * 1024 * 1024)
    }

    /// Node `SessionsIndexPublisher.maxDeltaLog`.
    pub fn index() -> Self {
        Self::new(512, 8 * 1024 * 1024)
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Records `(from, to]`; another epoch or a range that does not continue
    /// the log starts a new one at `from`.
    pub fn record(&mut self, epoch: &str, from: u64, to: u64, deltas: Deltas) {
        if self.epoch != epoch || self.head != from {
            self.epoch = epoch.to_owned();
            self.entries.clear();
            self.bytes = 0;
            self.floor = from;
        }
        let bytes = deltas.iter().map(|(_, size)| size).sum();
        self.bytes += bytes;
        self.entries.push_back(Entry {
            from,
            deltas,
            bytes,
        });
        self.head = to;
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= evicted.bytes;
            self.floor = self.entries.front().map_or(self.head, |e| e.from);
        }
    }

    /// Deltas after `base` up to `current` of `epoch`; `None` when the range
    /// is no longer retained or `base` is not an entry boundary. The caller
    /// has checked that `epoch` is the topic's current epoch.
    pub fn replay(&self, epoch: &str, base: u64, current: u64) -> Option<Vec<Value>> {
        if base == current {
            return Some(vec![]);
        }
        if self.epoch != epoch || self.head != current || base < self.floor || base > current {
            return None;
        }
        let start = self.entries.iter().position(|e| e.from == base)?;
        Some(
            self.entries
                .range(start..)
                .flat_map(|e| e.deltas.iter().map(|(delta, _)| delta.clone()))
                .collect(),
        )
    }
}

/// The session topic's retained log and the patch last published in its epoch.
#[derive(Clone, Debug)]
pub struct ConversationTopic {
    pub log: TopicLog,
    published: Option<(String, Map<String, Value>)>,
}

impl Default for ConversationTopic {
    fn default() -> Self {
        Self {
            log: TopicLog::conversation(),
            published: None,
        }
    }
}

impl ConversationTopic {
    /// The top-level keys of `patch` whose values changed since the last
    /// publication of `epoch` (all of them in a new epoch); `None` when none did.
    pub fn changed(&mut self, epoch: &str, patch: Value) -> Option<Value> {
        let Value::Object(patch) = patch else {
            return Some(patch);
        };
        let previous = match &mut self.published {
            Some((published, previous)) if published == epoch => previous,
            _ => {
                self.published = Some((epoch.to_owned(), patch.clone()));
                return Some(Value::Object(patch));
            }
        };
        let mut changed = Map::new();
        for (key, value) in patch {
            if previous.get(&key) != Some(&value) {
                previous.insert(key.clone(), value.clone());
                changed.insert(key, value);
            }
        }
        (!changed.is_empty()).then_some(Value::Object(changed))
    }

    pub fn bytes(&self) -> usize {
        self.log.bytes()
            + self.published.as_ref().map_or(0, |(_, patch)| {
                super::json_size::serialized_size(patch).expect("JSON maps serialize")
            })
    }
}

#[cfg(test)]
#[path = "topic_log_tests.rs"]
mod tests;
