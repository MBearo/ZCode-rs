//! One topic subscription of the App Server: its flush state and the frames
//! it encodes (spec rust-m8-delivery §5).
use super::codec::{self, FrameHeader};
use crate::domain::delivery::{self, Buffer, Profile};
use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};

pub struct Subscription {
    pub topic: String,
    pub connection: String,
    pub(super) profile: Profile,
    pub(super) window: Duration,
    ordinal: u64,
    /// `fromSeq` of the next frame.
    pub(super) sent_seq: u64,
    /// Seq up to which sent frames and the buffer cover the topic.
    pub(super) seq: u64,
    pub(super) buffer: Buffer,
    /// Deltas were skipped (gap, overflow, backlog); the next frame is a snapshot.
    pub(super) resync: bool,
    /// Runtime replies in flight; each covers every delta that arrives meanwhile.
    pub(super) recovering: u32,
    pub(super) due: Option<Instant>,
    /// `,"ttft":…` members for the next online deltas frame (local TTFT).
    pub(super) ttft: Option<String>,
}

impl Subscription {
    pub(super) fn new(topic: &str, connection: &str, profile: Profile, seq: u64) -> Self {
        // sessions-index 与 workspace-config 变更后立即刷新（Node flushIndex/flushConfig）。
        let window = if topic.starts_with("conversation/") {
            profile.window()
        } else {
            Duration::ZERO
        };
        Self {
            topic: topic.into(),
            connection: connection.into(),
            profile,
            window,
            ordinal: 0,
            sent_seq: seq,
            seq,
            buffer: Buffer::default(),
            resync: false,
            recovering: 0,
            due: None,
            ttft: None,
        }
    }

    /// Encodes one frame; the subscriber state changes only once it is encoded.
    fn frame(
        &mut self,
        id: &str,
        kind: &str,
        (from, to): (u64, u64),
        payload: &str,
    ) -> Result<Vec<String>> {
        self.frame_with(id, kind, (from, to), payload, "")
    }

    /// `extra` holds frame members after the payload (`,"ttft":…`).
    fn frame_with(
        &mut self,
        id: &str,
        kind: &str,
        (from, to): (u64, u64),
        payload: &str,
        extra: &str,
    ) -> Result<Vec<String>> {
        let ordinal = self.ordinal + 1;
        let frame_json = format!(
            r#"{{"topic":{},"subscriptionId":{},"fromSeq":{from},"toSeq":{to},"sentAt":{},"payload":{payload}{extra}}}"#,
            Value::from(self.topic.as_str()),
            Value::from(id),
            now_ms(),
        );
        let lines = codec::encode(
            &FrameHeader {
                delivery_kind: kind,
                logical_frame_id: &format!("{id}-lf-{ordinal}"),
                logical_frame_ordinal: ordinal,
                topic: &self.topic,
                subscription_id: id,
            },
            &frame_json,
        )?;
        self.ordinal = ordinal;
        self.sent_seq = to;
        self.seq = to;
        Ok(lines)
    }

    pub(super) fn deltas_frame(
        &mut self,
        id: &str,
        kind: &str,
        range: (u64, u64),
        deltas: &[Value],
    ) -> Result<Vec<String>> {
        self.frame(id, kind, range, &deltas_payload(deltas))
    }

    /// An `online` deltas frame, with the pending local TTFT facts.
    pub(super) fn online_frame(
        &mut self,
        id: &str,
        range: (u64, u64),
        deltas: &[Value],
    ) -> Result<Vec<String>> {
        let extra = self.ttft.take().unwrap_or_default();
        self.frame_with(id, "online", range, &deltas_payload(deltas), &extra)
    }

    pub(super) fn snapshot_frame(
        &mut self,
        id: &str,
        kind: &str,
        seq: u64,
        payload: &str,
    ) -> Result<Vec<String>> {
        self.buffer.clear();
        self.resync = false;
        self.due = None;
        self.frame(id, kind, (0, seq), payload)
    }

    /// Applies a runtime `{seq, mode, from, deltas | snapshot}` reply; an aligned
    /// resume sends a frame only when `always` (resync closes the client's flight).
    pub(super) fn apply(
        &mut self,
        id: &str,
        kind: &str,
        state: &Value,
        always: bool,
    ) -> Result<Vec<String>> {
        let seq = state["seq"].as_u64().unwrap_or(0);
        if state["mode"] != "resume" {
            return self.snapshot_frame(id, kind, seq, &snapshot_payload(&state["snapshot"]));
        }
        self.buffer.clear();
        self.resync = false;
        self.due = None;
        let from = state["from"].as_u64().unwrap_or(seq);
        if from == seq && !always {
            (self.sent_seq, self.seq) = (seq, seq);
            return Ok(vec![]);
        }
        let deltas = state["deltas"].as_array().cloned().unwrap_or_default();
        let deltas = delivery::replay(self.profile, deltas);
        self.deltas_frame(id, kind, (from, seq), &deltas)
    }
}

fn deltas_payload(deltas: &[Value]) -> String {
    let mut payload = String::from(r#"{"kind":"deltas","deltas":["#);
    for (i, delta) in deltas.iter().enumerate() {
        if i > 0 {
            payload.push(',');
        }
        payload.push_str(&delta.to_string());
    }
    payload.push_str("]}");
    payload
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub(super) fn snapshot_payload(snapshot: &Value) -> String {
    format!(r#"{{"kind":"snapshot","snapshot":{snapshot}}}"#)
}
