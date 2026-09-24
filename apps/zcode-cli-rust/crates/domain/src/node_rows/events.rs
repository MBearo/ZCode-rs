//! The session events the cold path synthesizes (the subset of Node
//! `SessionEvent` that `synthesizeEventsFromMessages` and the cold merge emit).
use serde_json::{Value, json};

/// Node `HYDRATION_TRACE_ID`: synthesized events rebuild a view, they are not
/// authoritative actions.
pub const HYDRATION_TRACE: &str = "hydrate-trace";
/// The trace of the persisted goal the cold merge inserts.
pub const GOAL_STATE_TRACE: &str = "trace-hydration";

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: String,
    /// `sequenceNumber` after the cold merge resequenced the stream.
    pub seq: u64,
    /// `timestamp.getTime()`.
    pub at: i64,
    pub kind: &'static str,
    pub turn: Option<String>,
    pub trace: &'static str,
    pub payload: Value,
}

impl Event {
    /// The fixture form: `{seq, at, type, turnId, payload}`.
    pub fn to_node(&self) -> Value {
        json!({
            "seq": self.seq,
            "at": self.at,
            "type": self.kind,
            "turnId": self.turn,
            "payload": self.payload,
        })
    }
}

/// `new Date(ms).getTime()`: JS Date truncates toward zero.
pub fn date_ms(ms: f64) -> i64 {
    ms.trunc() as i64
}

/// `new Date(ms).toISOString()`, the JSON form of a Date inside a payload.
pub fn iso(ms: f64) -> Value {
    chrono::DateTime::from_timestamp_millis(date_ms(ms))
        .map(|t| Value::String(t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()))
        .unwrap_or(Value::Null)
}

/// A JS number as JSON: integral values print without a fraction, and NaN or
/// infinities serialize as null.
pub fn num(n: f64) -> Value {
    if !n.is_finite() {
        Value::Null
    } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        Value::from(n as i64)
    } else {
        Value::from(n)
    }
}
