//! Delivery profiles and the per-subscriber flush buffer (Node
//! `DELIVERY_PROFILES`, `filterConversationDeltasForProfile`,
//! `coalesceConversationDeltas`, `appendConversationSubscriberBuffer`).
//! Spec rust-m8-delivery §1.
use super::json_size::serialized_size;
use serde_json::Value;
use std::time::Duration;

/// Node `PROTOCOL_V4_LIMITS.subscriberBufferMaxOps`.
pub const MAX_OPS: usize = 500;
/// Node `PROTOCOL_V4_LIMITS.subscriberBufferMaxBytes`.
pub const MAX_BYTES: usize = 1024 * 1024;
/// `{"kind":"deltas","deltas":[]}`: the frame payload around the deltas.
const PAYLOAD_BYTES: usize = 29;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Continuous,
    Replayable,
}

impl Profile {
    /// Node: only the Host-injected `desktop-continuous` is continuous.
    pub fn from_client_mode(mode: &str) -> Self {
        if mode == "desktop-continuous" {
            Self::Continuous
        } else {
            Self::Replayable
        }
    }

    pub fn window(self) -> Duration {
        Duration::from_millis(match self {
            Self::Continuous => 30,
            Self::Replayable => 150,
        })
    }

    /// `row.delta` passes only on the profile's stream paths; other ops always pass.
    pub fn keeps(self, delta: &Value) -> bool {
        if delta["op"] != "row.delta" {
            return true;
        }
        match delta["path"].as_str() {
            Some("text") => true,
            Some("inputText" | "output.text" | "summaryText") => self == Self::Continuous,
            _ => false,
        }
    }
}

/// UTF-8 bytes of `value` as compact JSON.
pub fn json_bytes(value: &Value) -> usize {
    serialized_size(value).expect("JSON values serialize")
}

fn row_id(delta: &Value) -> &Value {
    &delta["row"]["rowId"]
}

/// Coalesced deltas with their compact JSON sizes.
#[derive(Debug, Default)]
pub struct Buffer {
    deltas: Vec<(Value, usize)>,
    bytes: usize,
}

impl Buffer {
    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }

    pub fn clear(&mut self) {
        self.deltas.clear();
        self.bytes = 0;
    }

    /// UTF-8 bytes of the `{"kind":"deltas","deltas":[...]}` payload.
    pub fn payload_bytes(&self) -> usize {
        PAYLOAD_BYTES + self.bytes + self.deltas.len().saturating_sub(1)
    }

    /// Appends a filtered batch; `false` when the coalesced buffer is over the
    /// op or byte limit (the caller clears it and requires a snapshot).
    pub fn append(&mut self, batch: impl IntoIterator<Item = (Value, usize)>) -> bool {
        for (delta, size) in batch {
            self.push(delta, size);
        }
        self.deltas.len() <= MAX_OPS && self.payload_bytes() <= MAX_BYTES
    }

    pub fn take(&mut self) -> Vec<Value> {
        self.bytes = 0;
        std::mem::take(&mut self.deltas)
            .into_iter()
            .map(|(delta, _)| delta)
            .collect()
    }

    /// One step of Node's left fold over the window.
    fn push(&mut self, delta: Value, size: usize) {
        if delta["op"] == "row.upserted" {
            // 规则 3：吞掉同 rowId 更早的 row.delta；不越过屏障与同行的上一代整行。
            let mut i = self.deltas.len();
            while i > 0 {
                i -= 1;
                let prev = &self.deltas[i].0;
                if prev["op"] == "row.removed" {
                    break;
                }
                if prev["op"] == "row.delta" && prev["rowId"] == *row_id(&delta) {
                    let (_, removed) = self.deltas.remove(i);
                    self.bytes -= removed;
                    continue;
                }
                if (prev["op"] == "row.upserted" || prev["op"] == "row.appended")
                    && row_id(prev) == row_id(&delta)
                {
                    break;
                }
            }
        }
        if let Some((last, last_size)) = self.deltas.last_mut() {
            let before = *last_size;
            if delta["op"] == "row.delta"
                && last["op"] == "row.delta"
                && last["rowId"] == delta["rowId"]
                && last["path"] == delta["path"]
            {
                let append = delta["append"].as_str().unwrap_or_default();
                if let Some(Value::String(text)) = last.get_mut("append") {
                    text.push_str(append);
                }
                // 转义后的字节数不含两侧引号。
                *last_size += serialized_size(&append).expect("strings serialize") - 2;
                self.bytes += *last_size - before;
                return;
            }
            if delta["op"] == "state.updated" && last["op"] == "state.updated" {
                if let (Some(merged), Some(patch)) =
                    (last["patch"].as_object_mut(), delta["patch"].as_object())
                {
                    for (key, value) in patch {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                *last_size = json_bytes(last);
                self.bytes = self.bytes - before + *last_size;
                return;
            }
            if delta["op"] == "row.upserted"
                && last["op"] == "row.upserted"
                && row_id(last) == row_id(&delta)
            {
                *last = delta;
                *last_size = size;
                self.bytes = self.bytes - before + size;
                return;
            }
        }
        self.bytes += size;
        self.deltas.push((delta, size));
    }
}

/// Node `filterConversationDeltasForProfile` then `coalesceConversationDeltas`
/// over a replayed range.
pub fn replay(profile: Profile, deltas: Vec<Value>) -> Vec<Value> {
    let mut buffer = Buffer::default();
    for delta in deltas.into_iter().filter(|d| profile.keeps(d)) {
        let size = json_bytes(&delta);
        buffer.push(delta, size);
    }
    buffer.take()
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
