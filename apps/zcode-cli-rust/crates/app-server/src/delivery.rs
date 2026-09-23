//! Topic subscriptions owned by the App Server.
//!
//! The runtime owns topic facts and sequence numbers; this registry only
//! decides which subscriber receives which frame. A subscriber accepts a delta
//! only when it continues exactly from the last seq it was sent; stale deltas
//! are dropped and gaps trigger snapshot recovery, so correctness never
//! depends on the relative timing of replies and events.
use super::codec::{self, FrameHeader};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub struct Subscription {
    pub topic: String,
    pub connection: String,
    ordinal: u64,
    sent_seq: u64,
    /// Host reported the connection saturated; nothing is sent until drained.
    pub paused: bool,
    /// Deltas were skipped; the next frame for this subscriber must be a snapshot.
    pub needs_resync: bool,
    /// A recovery snapshot has been requested and not yet applied.
    pub recovering: bool,
}

pub struct Delivery {
    subscriptions: BTreeMap<String, Subscription>,
    epoch: String,
    next_id: u64,
}

impl Default for Delivery {
    fn default() -> Self {
        Self {
            subscriptions: BTreeMap::new(),
            epoch: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
            next_id: 0,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Delivery {
    pub fn get(&self, id: &str) -> Option<&Subscription> {
        self.subscriptions.get(id)
    }

    /// Register a subscription after the runtime opened its topic. Returns the new id and,
    /// when it replaces the connection's previous subscription on the same topic, that topic.
    pub fn subscribe(&mut self, topic: &str, connection: &str) -> (String, bool) {
        let before = self.subscriptions.len();
        self.subscriptions
            .retain(|_, s| s.topic != topic || s.connection != connection);
        let replaced = self.subscriptions.len() != before;
        self.next_id += 1;
        let id = format!("sub-{}-{}", self.epoch, self.next_id);
        self.subscriptions.insert(
            id.clone(),
            Subscription {
                topic: topic.into(),
                connection: connection.into(),
                ordinal: 0,
                sent_seq: 0,
                paused: false,
                needs_resync: false,
                recovering: false,
            },
        );
        (id, replaced)
    }

    pub fn unsubscribe(&mut self, id: &str) -> Option<Subscription> {
        self.subscriptions.remove(id)
    }

    /// Drop every subscription of a closed connection; returns their topics for pin release.
    pub fn close_connection(&mut self, connection: &str) -> Vec<String> {
        let ids = self.ids(|s| s.connection == connection);
        ids.iter()
            .filter_map(|id| self.subscriptions.remove(id).map(|s| s.topic))
            .collect()
    }

    /// Runtime ended the topic; its pins are already gone.
    pub fn close_topic(&mut self, topic: &str) {
        self.subscriptions.retain(|_, s| s.topic != topic);
    }

    pub fn set_paused(&mut self, connection: &str, paused: bool) {
        for sub in self
            .subscriptions
            .values_mut()
            .filter(|s| s.connection == connection)
        {
            sub.paused = paused;
            if paused {
                sub.needs_resync = true;
            }
        }
    }

    /// Explicit resync of one subscription: it resumes and waits for its recovery snapshot.
    pub fn resume(&mut self, id: &str) {
        if let Some(sub) = self.subscriptions.get_mut(id) {
            sub.paused = false;
            sub.recovering = true;
        }
    }

    /// Mark every subscriber on `topic` (all topics when `None`) as needing a snapshot.
    pub fn invalidate(&mut self, topic: Option<&str>) {
        for sub in self
            .subscriptions
            .values_mut()
            .filter(|s| topic.is_none_or(|t| s.topic == t))
        {
            sub.needs_resync = true;
        }
    }

    /// Subscribers that need a snapshot and can receive one now; marks them recovering.
    pub fn take_recoverable(&mut self, connection: Option<&str>) -> Vec<(String, String)> {
        self.subscriptions
            .iter_mut()
            .filter(|(_, s)| {
                s.needs_resync
                    && !s.paused
                    && !s.recovering
                    && connection.is_none_or(|c| s.connection == c)
            })
            .map(|(id, s)| {
                s.recovering = true;
                (id.clone(), s.topic.clone())
            })
            .collect()
    }

    fn ids(&self, filter: impl Fn(&Subscription) -> bool) -> Vec<String> {
        self.subscriptions
            .iter()
            .filter(|(_, s)| filter(s))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Send a snapshot to one subscriber and restart its sequence at `seq`.
    pub fn snapshot(
        &mut self,
        id: &str,
        delivery: &str,
        seq: u64,
        snapshot: &Value,
    ) -> Result<Vec<String>> {
        self.snapshot_payload(id, delivery, seq, &snapshot_payload(snapshot))
    }

    fn snapshot_payload(
        &mut self,
        id: &str,
        delivery: &str,
        seq: u64,
        payload: &str,
    ) -> Result<Vec<String>> {
        let Some(sub) = self.subscriptions.get_mut(id) else {
            return Ok(vec![]);
        };
        sub.needs_resync = false;
        sub.recovering = false;
        sub.sent_seq = seq;
        frame(id, sub, delivery, 0, seq, payload)
    }

    /// Snapshot to every live subscriber of `topic` (config replacement, history reset);
    /// the snapshot is serialized once and shared.
    pub fn snapshot_topic(
        &mut self,
        topic: &str,
        delivery: &str,
        seq: u64,
        snapshot: &Value,
    ) -> Result<Vec<String>> {
        let ids = self.ids(|s| s.topic == topic && !s.paused);
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let payload = snapshot_payload(snapshot);
        let mut lines = vec![];
        for id in ids {
            lines.extend(self.snapshot_payload(&id, delivery, seq, &payload)?);
        }
        Ok(lines)
    }

    /// Fan out one delta range; `payload` is the serialized `{"kind":"deltas",...}` object.
    pub fn deltas(
        &mut self,
        topic: &str,
        from: u64,
        to: u64,
        payload: &str,
    ) -> Result<Vec<String>> {
        let mut lines = vec![];
        for id in self.ids(|s| s.topic == topic) {
            let sub = self.subscriptions.get_mut(&id).unwrap();
            if sub.paused || sub.needs_resync || from < sub.sent_seq {
                continue;
            }
            if from > sub.sent_seq {
                // 序号断档说明中间增量未送达：不能伪造连续水位，改由快照补齐。
                sub.needs_resync = true;
                continue;
            }
            sub.sent_seq = to;
            lines.extend(frame(&id, sub, "online", from, to, payload)?);
        }
        Ok(lines)
    }
}

fn snapshot_payload(snapshot: &Value) -> String {
    json!({"kind":"snapshot","snapshot":snapshot}).to_string()
}

fn frame(
    id: &str,
    sub: &mut Subscription,
    delivery: &str,
    from: u64,
    to: u64,
    payload: &str,
) -> Result<Vec<String>> {
    sub.ordinal += 1;
    let frame_json = format!(
        r#"{{"topic":{},"subscriptionId":{},"fromSeq":{from},"toSeq":{to},"sentAt":{},"payload":{payload}}}"#,
        serde_json::to_string(&sub.topic)?,
        serde_json::to_string(id)?,
        now_ms(),
    );
    let logical_frame_id = format!("{id}-lf-{}", sub.ordinal);
    codec::encode(
        &FrameHeader {
            delivery_kind: delivery,
            logical_frame_id: &logical_frame_id,
            logical_frame_ordinal: sub.ordinal,
            topic: &sub.topic,
            subscription_id: id,
        },
        &frame_json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(lines: &[String]) -> Vec<Value> {
        lines
            .iter()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["params"]["frame"].clone())
            .collect()
    }

    #[test]
    fn deltas_continue_only_from_the_last_sent_seq() {
        let mut delivery = Delivery::default();
        let (id, _) = delivery.subscribe("conversation/s", "c");
        delivery.snapshot(&id, "initial", 5, &json!({})).unwrap();
        // 快照之前产生的增量已包含在快照内。
        assert!(
            delivery
                .deltas("conversation/s", 3, 5, "{}")
                .unwrap()
                .is_empty()
        );
        let sent = frames(&delivery.deltas("conversation/s", 5, 7, "{}").unwrap());
        assert_eq!(
            (sent[0]["fromSeq"].as_u64(), sent[0]["toSeq"].as_u64()),
            (Some(5), Some(7))
        );
        // 断档：不发送，并要求快照恢复。
        assert!(
            delivery
                .deltas("conversation/s", 9, 10, "{}")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            delivery.take_recoverable(None),
            vec![(id.clone(), "conversation/s".into())]
        );
        assert!(
            delivery.take_recoverable(None).is_empty(),
            "recovery is requested once"
        );
        delivery.snapshot(&id, "recovery", 10, &json!({})).unwrap();
        assert_eq!(
            delivery
                .deltas("conversation/s", 10, 11, "{}")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn replacing_and_closing_connections_report_released_topics() {
        let mut delivery = Delivery::default();
        let (first, replaced) = delivery.subscribe("conversation/s", "c");
        assert!(!replaced);
        let (second, replaced) = delivery.subscribe("conversation/s", "c");
        assert!(replaced && first != second);
        assert!(delivery.get(&first).is_none());
        assert_eq!(
            delivery.close_connection("c"),
            vec!["conversation/s".to_owned()]
        );
    }
}
