//! Local time-to-first-token facts (Node `LocalTtftRecorder`,
//! `local-ttft.ts`; spec rust-m9-usage-logs §7). Only `v4/command` envelopes
//! that carry `ttft` are recorded; the recorder follows admission and live
//! telemetry facts and never decides anything for the runtime. Times are
//! epoch milliseconds as floats (Node `localTtftNow`).
mod facts;

pub use facts::{Detail, Facts};
use serde_json::Value;
use std::collections::BTreeMap;

/// The private model status key of Node `modelCall.logicalCallId`; only the
/// recorder reads it, and it is removed before any Node-visible output.
pub const LOGICAL_CALL_KEY: &str = "_zcode_logical_call_id";
/// Node `LOCAL_TTFT_MAX_PENDING`, `LOCAL_TTFT_MAX_DETAILS`, `LOCAL_TTFT_TTL_MS`.
const MAX_PENDING: usize = 128;
const MAX_DETAILS: usize = 64;
const TTL_MS: f64 = 300_000.0;
/// Node `LocalTtftClockWatch`: a tick is unreliable past these bounds.
const MAX_PAUSE_MS: f64 = 5_000.0;
const MAX_DRIFT_MS: f64 = 100.0;

/// What the main model request of a record is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestState {
    Response,
    Preparation,
    Failed,
}

#[derive(Debug)]
pub struct Recorder {
    pub instance: String,
    /// Active records and retired ones kept for frame attachment, in order.
    records: Vec<Facts>,
    completed: Vec<Facts>,
    request_state: BTreeMap<String, RequestState>,
    signatures: BTreeMap<String, String>,
    /// The last clock tick `(monotonic, wall)`.
    clock: Option<(f64, f64)>,
    checkpoints: Vec<Facts>,
}

impl Recorder {
    pub fn new(instance: String) -> Self {
        Self {
            instance,
            records: vec![],
            completed: vec![],
            request_state: BTreeMap::new(),
            signatures: BTreeMap::new(),
            clock: None,
            checkpoints: vec![],
        }
    }

    /// Node `receive`: a command with `ttft` starts a record; `false` when the
    /// recorder is full (the ACK then says `ttftExcluded: capacity`).
    /// `(now, wall)`: the TTFT clock and the wall clock (the clock watch compares them).
    pub fn receive(
        &mut self,
        command: &str,
        session: Option<&str>,
        (ttft, busy): (&Value, bool),
        (now, wall): (f64, f64),
    ) -> bool {
        self.prune(now);
        let known = |r: &Facts| r.command_id == command;
        if self.records.iter().any(known) || self.completed.iter().any(known) {
            return true;
        }
        if self.records.len() >= MAX_PENDING {
            return false;
        }
        let Some(observation) = ttft["observationId"].as_str() else {
            return true;
        };
        self.records.push(Facts::new(
            (observation, &self.instance, command),
            session,
            now,
            if busy { "queued" } else { "idle" },
        ));
        self.clock.get_or_insert((now, wall));
        true
    }

    /// Node `TurnSteerQueued`: the input waits in the session queue.
    pub fn queued(&mut self, command: &str, session: &str) {
        let Some(record) = self.record(command) else {
            return;
        };
        if record.session_id.as_deref().is_some_and(|s| s != session) {
            return;
        }
        record.session_id = Some(session.into());
        record.send_mode = "queued";
        self.checkpoint(command);
    }

    /// Node `TurnSteerDrained` as a guide: the input joined the running turn.
    pub fn guided(&mut self, command: &str) {
        let Some(record) = self.record(command) else {
            return;
        };
        record.send_mode = "guided";
        self.checkpoint(command);
        self.retire(command);
    }

    /// Node `TurnSteerDiscarded` (not promoted): the queued input never ran.
    pub fn discarded(&mut self, command: &str, turn_failed: bool) {
        let Some(record) = self.record(command) else {
            return;
        };
        record.terminal = Some(if turn_failed { "failed" } else { "cancelled" });
        self.checkpoint(command);
        self.retire(command);
    }

    pub fn active(&self) -> bool {
        !self.records.is_empty()
    }

    fn record(&mut self, command: &str) -> Option<&mut Facts> {
        self.records.iter_mut().find(|r| r.command_id == command)
    }

    /// Node `checkpoint`: a changed record goes out with the next revision.
    fn checkpoint(&mut self, command: &str) {
        let Some(record) = self.records.iter_mut().find(|r| r.command_id == command) else {
            return;
        };
        let signature = record.signature();
        if self.signatures.get(command) == Some(&signature) {
            return;
        }
        self.signatures.insert(command.into(), signature);
        record.revision = Some(record.revision.unwrap_or(0) + 1);
        self.checkpoints.push(record.clone());
    }

    /// The records to send (`v4/telemetry/local-ttft`), in order.
    pub fn take_checkpoints(&mut self) -> Vec<Facts> {
        std::mem::take(&mut self.checkpoints)
    }

    /// Node `admitted`: the inbox accepted the command.
    pub fn admitted(&mut self, command: &str, now: f64) {
        if let Some(record) = self.record(command) {
            record.admitted_at.get_or_insert(now);
            self.checkpoint(command);
        }
    }

    /// A command the inbox did not admit: its record and pending checkpoints go.
    pub fn forget(&mut self, command: &str) {
        self.records.retain(|r| r.command_id != command);
        self.checkpoints.retain(|r| r.command_id != command);
        self.request_state.remove(command);
        self.signatures.remove(command);
    }

    /// Node `retire`: no more checkpoints; kept (bounded) for frames.
    fn retire(&mut self, command: &str) {
        let Some(index) = self.records.iter().position(|r| r.command_id == command) else {
            return;
        };
        let record = self.records.remove(index);
        self.request_state.remove(command);
        self.signatures.remove(command);
        self.completed.push(record);
        if self.completed.len() > MAX_PENDING {
            self.completed.remove(0);
        }
        if self.records.is_empty() {
            self.clock = None;
        }
    }

    /// Node `prune`: records older than the TTL are dropped silently.
    fn prune(&mut self, now: f64) {
        let fresh = |r: &Facts| now - r.received_at <= TTL_MS;
        let stale: Vec<String> = self
            .records
            .iter()
            .filter(|r| !fresh(r))
            .map(|r| r.command_id.clone())
            .collect();
        for command in stale {
            self.request_state.remove(&command);
            self.signatures.remove(&command);
        }
        self.records.retain(fresh);
        self.completed.retain(fresh);
    }

    /// Node `forSession`: the latest record of `session` (of `command`, else one with a turn).
    pub fn for_session(&mut self, session: &str, command: Option<&str>, now: f64) -> Option<Facts> {
        self.prune(now);
        self.completed
            .iter()
            .chain(&self.records)
            .rev()
            .find(|r| {
                r.session_id.as_deref() == Some(session)
                    && command.map_or(r.turn_id.is_some(), |c| r.command_id == c)
            })
            .cloned()
    }

    /// Node `LocalTtftClockWatch` (1 s ticks while records exist): a paused or
    /// drifting clock invalidates every record without output.
    pub fn tick(&mut self, now: f64, wall: f64) {
        let Some((previous, previous_wall)) = self.clock.replace((now, wall)) else {
            return;
        };
        let elapsed = now - previous;
        let unreliable = !(0.0..=MAX_PAUSE_MS).contains(&elapsed)
            || ((wall - previous_wall) - elapsed).abs() > MAX_DRIFT_MS;
        if unreliable {
            let open: Vec<String> = self
                .records
                .iter_mut()
                .filter(|r| r.output_at.is_none())
                .map(|r| {
                    r.clock_invalid = Some(true);
                    r.command_id.clone()
                })
                .collect();
            for command in open {
                self.checkpoint(&command);
            }
        }
        self.prune(now);
        if self.records.is_empty() {
            self.clock = None;
        }
    }
}

#[cfg(test)]
mod tests;
