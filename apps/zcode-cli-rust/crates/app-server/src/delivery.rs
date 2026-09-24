//! Topic subscriptions owned by the App Server (spec rust-m8-delivery §5–6).
//!
//! The runtime owns topic facts, sequence numbers and the retained log; this
//! registry decides which subscriber receives which frame and when. Each
//! subscriber buffers its profile's deltas until its flush window ends. A
//! delta is accepted only when it continues the subscriber's watermark; gaps,
//! overflow and backlog turn into snapshot recovery, and deltas that arrive
//! while a runtime reply is in flight are covered by that reply, so
//! correctness never depends on the relative timing of replies and events.
pub use super::subscription::Subscription;
use super::subscription::snapshot_payload;
use crate::domain::delivery::Profile;
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// A registered subscription and its initial frame.
pub struct Subscribed {
    pub id: String,
    pub mode: &'static str,
    pub replaced: bool,
    pub lines: Vec<String>,
}

/// Frames and runtime requests produced by one delivery step.
#[derive(Default)]
pub struct Output {
    pub lines: Vec<String>,
    /// `(subscription, topic)` needing a snapshot from the runtime.
    pub recover: Vec<(String, String)>,
    /// Topics whose subscription ended because its snapshot could not be encoded.
    pub released: Vec<String>,
}

pub struct Delivery {
    subscriptions: BTreeMap<String, Subscription>,
    /// Connections the Host reported saturated (Node `pausedConnections`).
    paused: BTreeSet<String>,
    epoch: String,
    next_id: u64,
}

impl Default for Delivery {
    fn default() -> Self {
        Self {
            subscriptions: BTreeMap::new(),
            paused: BTreeSet::new(),
            epoch: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
            next_id: 0,
        }
    }
}

impl Delivery {
    pub fn get(&self, id: &str) -> Option<&Subscription> {
        self.subscriptions.get(id)
    }

    /// Registers the subscription answered by the runtime's `topicOpen` reply.
    /// Only once its initial frame is encoded does it replace the connection's
    /// previous subscription on the topic (Node rollback keeps the old one).
    pub fn subscribe(
        &mut self,
        (topic, connection): (&str, &str),
        profile: Profile,
        state: &Value,
    ) -> Result<Subscribed> {
        self.next_id += 1;
        let id = format!("sub-{}-{}", self.epoch, self.next_id);
        let seq = state["seq"].as_u64().unwrap_or(0);
        let mut sub = Subscription::new(topic, connection, profile, seq);
        let lines = sub.apply(&id, "initial", state, false)?;
        let before = self.subscriptions.len();
        self.subscriptions
            .retain(|_, s| s.topic != topic || s.connection != connection);
        let replaced = self.subscriptions.len() != before;
        self.subscriptions.insert(id.clone(), sub);
        let mode = if state["mode"] == "resume" {
            "resume"
        } else {
            "snapshot"
        };
        Ok(Subscribed {
            id,
            mode,
            replaced,
            lines,
        })
    }

    pub fn unsubscribe(&mut self, id: &str) -> Option<Subscription> {
        self.subscriptions.remove(id)
    }

    /// Drop every subscription of a closed connection; returns their topics for pin release.
    pub fn close_connection(&mut self, connection: &str) -> Vec<String> {
        self.paused.remove(connection);
        let ids = self.ids(|s| s.connection == connection);
        ids.iter()
            .filter_map(|id| self.subscriptions.remove(id).map(|s| s.topic))
            .collect()
    }

    /// Runtime ended the topic; its pins are already gone.
    pub fn close_topic(&mut self, topic: &str) {
        self.subscriptions.retain(|_, s| s.topic != topic);
    }

    /// `saturated`: timers stop, buffers keep accumulating until they overflow.
    pub fn pause(&mut self, connection: &str) {
        self.paused.insert(connection.into());
        for sub in self.subscriptions.values_mut() {
            if sub.connection == connection {
                sub.due = None;
            }
        }
    }

    /// `drained`: every subscription of the connection flushes now.
    pub fn drain(&mut self, connection: &str, now: Instant) {
        if !self.paused.remove(connection) {
            return;
        }
        for sub in self.subscriptions.values_mut() {
            if sub.connection == connection {
                sub.due = Some(now);
            }
        }
    }

    /// Client resync: the reply replaces whatever the subscription buffered.
    pub fn resync_started(&mut self, id: &str) {
        if let Some(sub) = self.subscriptions.get_mut(id) {
            sub.buffer.clear();
            sub.due = None;
            sub.recovering += 1;
        }
    }

    /// Applies the runtime reply of a resync (`recovery`) or snapshot recovery (`online`).
    pub fn recovered(&mut self, id: &str, kind: &str, state: &Value) -> Result<Vec<String>> {
        let Some(sub) = self.subscriptions.get_mut(id) else {
            return Ok(vec![]);
        };
        sub.recovering = sub.recovering.saturating_sub(1);
        let applied = sub.apply(id, kind, state, true);
        if applied.is_err() {
            sub.resync = true;
        }
        applied
    }

    /// A recovery reply failed; the subscription recovers by snapshot on its next flush.
    pub fn recovery_failed(&mut self, id: &str, now: Instant) {
        if let Some(sub) = self.subscriptions.get_mut(id) {
            sub.recovering = sub.recovering.saturating_sub(1);
            sub.buffer.clear();
            sub.resync = true;
            if !self.paused.contains(&sub.connection) {
                sub.due = Some(now);
            }
        }
    }

    /// Buffers one runtime publication `(from, to]` for every subscriber of `topic`.
    pub fn deltas(
        &mut self,
        topic: &str,
        (from, to): (u64, u64),
        deltas: &[(Value, usize)],
        now: Instant,
    ) {
        for sub in self.subscriptions.values_mut() {
            if sub.topic != topic || sub.resync || sub.recovering > 0 || from < sub.seq {
                continue;
            }
            if from > sub.seq {
                // 序号断档说明中间增量未送达：不能伪造连续水位，改由快照补齐。
                sub.buffer.clear();
                sub.resync = true;
            } else {
                let profile = sub.profile;
                let batch = deltas.iter().filter(|(d, _)| profile.keeps(d)).cloned();
                if !sub.buffer.append(batch) {
                    // 慢订阅者只保留恢复意图，不继续积压（Node overflow）。
                    sub.buffer.clear();
                    sub.resync = true;
                }
                sub.seq = to;
            }
            if sub.due.is_none() && !self.paused.contains(&sub.connection) {
                sub.due = Some(now + sub.window);
            }
        }
    }

    /// Snapshot to every subscriber of `topic` (history reset, config change),
    /// serialized once; paused subscribers recover when drained.
    pub fn snapshot_topic(
        &mut self,
        topic: &str,
        kind: &str,
        seq: u64,
        snapshot: &Value,
    ) -> Output {
        let payload = snapshot_payload(snapshot);
        let mut out = Output::default();
        for id in self.ids(|s| s.topic == topic) {
            let sub = self
                .subscriptions
                .get_mut(&id)
                .expect("listed subscription");
            if self.paused.contains(&sub.connection) {
                sub.buffer.clear();
                sub.resync = true;
                continue;
            }
            match sub.snapshot_frame(&id, kind, seq, &payload) {
                Ok(lines) => out.lines.extend(lines),
                Err(_) => {
                    self.subscriptions.remove(&id);
                    out.released.push(topic.into());
                }
            }
        }
        out
    }

    /// Frames of every due subscription, and the snapshots they need.
    pub fn flush_due(&mut self, now: Instant) -> Output {
        let mut out = Output::default();
        for (id, sub) in &mut self.subscriptions {
            if sub.due.is_none_or(|due| due > now) || self.paused.contains(&sub.connection) {
                continue;
            }
            sub.due = None;
            if sub.recovering > 0 {
                continue;
            }
            if !sub.resync && (!sub.buffer.is_empty() || sub.sent_seq != sub.seq) {
                let deltas = sub.buffer.take();
                // 被 profile 过滤掉的 seq 也在区间内，客户端连续性判定不受过滤影响。
                match sub.deltas_frame(id, "online", (sub.sent_seq, sub.seq), &deltas) {
                    Ok(lines) => out.lines.extend(lines),
                    Err(_) => sub.resync = true,
                }
            }
            if sub.resync {
                sub.recovering += 1;
                out.recover.push((id.clone(), sub.topic.clone()));
            }
        }
        out
    }

    /// Output ended: every buffered delta is written; snapshots can no longer be served.
    pub fn flush_all(&mut self, now: Instant) -> Vec<String> {
        for sub in self.subscriptions.values_mut() {
            sub.due = Some(now);
        }
        self.flush_due(now).lines
    }

    pub fn next_due(&self) -> Option<Instant> {
        self.subscriptions
            .values()
            .filter(|s| !self.paused.contains(&s.connection))
            .filter_map(|s| s.due)
            .min()
    }

    /// Writer backlog: every buffer is dropped and recovered by snapshot later.
    pub fn invalidate_all(&mut self) {
        for sub in self.subscriptions.values_mut() {
            sub.buffer.clear();
            sub.resync = true;
            sub.due = None;
        }
    }

    /// Backlog relieved: subscriptions waiting for a snapshot become due.
    pub fn wake(&mut self, now: Instant) {
        for sub in self.subscriptions.values_mut() {
            if sub.resync && !self.paused.contains(&sub.connection) {
                sub.due = Some(now);
            }
        }
    }

    fn ids(&self, filter: impl Fn(&Subscription) -> bool) -> Vec<String> {
        self.subscriptions
            .iter()
            .filter(|(_, s)| filter(s))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
