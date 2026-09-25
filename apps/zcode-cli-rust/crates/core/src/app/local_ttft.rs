//! Local TTFT around the Engine (Node `v4-gateway.ts` with `LocalTtftRecorder`;
//! spec rust-m9-usage-logs §7): commands carrying `ttft` start records, live
//! session events advance them, checkpoints go out as
//! `v4/telemetry/local-ttft`, and conversation frames carry the facts of
//! their turns. Nothing here changes the conversation.
use super::Engine;
use crate::contract::ServerMsg;
use crate::domain::protocol::Command;
use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Node `localTtftNow`: epoch milliseconds with sub-millisecond precision,
/// monotonic within the process.
pub(super) struct TtftClock {
    epoch_ms: f64,
    start: std::time::Instant,
}

impl Default for TtftClock {
    fn default() -> Self {
        let epoch_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        Self {
            epoch_ms,
            start: std::time::Instant::now(),
        }
    }
}

impl TtftClock {
    pub fn now(&self) -> f64 {
        self.epoch_ms + self.start.elapsed().as_secs_f64() * 1000.0
    }
}

fn wall_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_millis() as f64)
}

impl Engine {
    /// `v4/command` with the recorder around it: a full recorder marks the
    /// ACK `ttftExcluded: capacity`; an admitted command stamps `admittedAt`.
    pub(super) async fn ttft_command(&mut self, command: Command, raw: &Value) -> Result<Value> {
        let id = command.command_id.clone();
        let mut excluded = false;
        if let Some(ttft) = raw.get("ttft").filter(|t| t.is_object()) {
            let session = command.session_id.clone();
            // Node：会话可停止（有运行中的工作）时输入按排队发送。
            let busy = session
                .as_deref()
                .is_some_and(|s| self.active.contains_key(s));
            let now = (self.ttft_clock.now(), wall_ms());
            excluded = !self
                .ttft
                .receive(&id, session.as_deref(), (ttft, busy), now);
        }
        // Node 在 inbox 接纳时（执行副作用前）记 admittedAt；Rust 的接纳与执行在同一调用里，
        // 先记下并暂存 checkpoint，被拒绝的命令整条记录作废（Node 不接纳即无 checkpoint）。
        self.ttft.admitted(&id, self.ttft_clock.now());
        let mut ack = self.command(command).await?;
        if ack["status"] == "rejected" {
            self.ttft.forget(&id);
        }
        if excluded {
            ack["ttftExcluded"] = "capacity".into();
        }
        self.ttft_flush();
        Ok(ack)
    }

    /// Checkpoints as `v4/telemetry/local-ttft`.
    pub(super) fn ttft_flush(&mut self) {
        for facts in self.ttft.take_checkpoints() {
            if let Ok(params) = serde_json::to_value(facts) {
                self.outbox.push(ServerMsg::HostNotification {
                    method: "v4/telemetry/local-ttft",
                    params,
                });
            }
        }
    }

    /// A live session event and its telemetry fact (Node `LocalTtftRecorder.event`).
    pub(super) fn ttft_event(
        &mut self,
        event: &crate::domain::telemetry::Event,
        fact: Option<&Value>,
    ) {
        if !self.ttft.active() {
            return;
        }
        let now = self.ttft_clock.now();
        let (session, turn, payload) = (event.session, event.turn, event.payload);
        if event.kind.starts_with("compact_") {
            self.ttft
                .compaction(session, turn, (event.kind, payload), now);
        }
        if let Some(fact) = fact {
            let execution = payload["executionStartedAt"].as_f64();
            self.ttft.fact(
                fact,
                execution.filter(|_| event.kind == "turn_started"),
                now,
            );
        }
        let logical = payload[crate::domain::local_ttft::LOGICAL_CALL_KEY].as_str();
        if let Some(logical) = logical.filter(|_| event.kind == "model_network_status") {
            let request = payload["requestId"].as_str().unwrap_or("");
            self.ttft.logical_call(session, request, logical, now);
        }
        if event.kind == "model_streaming"
            && fact.is_some_and(|f| f.get("parentToolCallId").is_none())
        {
            let kind = match payload["kind"].as_str() {
                Some("text_delta") => "text",
                Some("reasoning_delta") => "reasoning",
                _ => "",
            };
            let visible = payload["delta"]
                .as_str()
                .is_some_and(|d| !crate::domain::js_string::trim(d).is_empty());
            if !kind.is_empty() && visible {
                self.ttft.output(session, turn, kind, now);
            }
        }
        self.ttft_flush();
    }

    /// The first tool-call output of a run's response (Node `output(tool)` on
    /// `tool_input_delta` / `tool_call`); a cancelled run has none.
    pub(super) fn ttft_tool(&mut self, session: &str) -> Result<()> {
        let Some(active) = self
            .active
            .get(session)
            .filter(|a| !a.cancel.is_cancelled())
        else {
            return Ok(());
        };
        if self.ttft.active() {
            let turn = active.turn_id.clone();
            self.ttft
                .output(session, Some(&turn), "tool", self.ttft_clock.now());
            self.ttft_flush();
        }
        Ok(())
    }

    /// The clock watch (1 s while records exist).
    pub(super) fn ttft_tick(&mut self) {
        if self.ttft.active() {
            self.ttft.tick(self.ttft_clock.now(), wall_ms());
            self.ttft_flush();
        }
    }

    /// The facts a conversation frame of `session` carries (Node
    /// `flushReservation`): the records of the turns its deltas touch, else the latest.
    pub(super) fn ttft_observations(&mut self, session: &str, deltas: &[Value]) -> Vec<Value> {
        let now = self.ttft_clock.now();
        if self.ttft.for_session(session, None, now).is_none() {
            return vec![];
        }
        let Some(s) = self.sessions.get(session) else {
            return vec![];
        };
        let mut turns = std::collections::BTreeSet::new();
        for delta in deltas {
            let turn = match delta["op"].as_str() {
                Some("row.appended" | "row.upserted") => delta["row"]["turnId"].as_str(),
                Some("row.delta") => s
                    .rows
                    .iter()
                    .find(|r| r["rowId"] == delta["rowId"])
                    .and_then(|r| r["turnId"].as_str()),
                _ => None,
            };
            turns.extend(turn.map(str::to_owned));
        }
        let headers: Vec<(String, String)> = s
            .rows
            .iter()
            .filter(|r| r["kind"] == "turnHeader")
            .filter_map(|r| {
                Some((
                    r["turnId"].as_str()?.to_owned(),
                    r["sourceCommandId"].as_str()?.to_owned(),
                ))
            })
            .collect();
        let mut candidates: Vec<_> = headers
            .iter()
            .filter(|(turn, _)| turns.contains(turn))
            .filter_map(|(_, command)| self.ttft.for_session(session, Some(command), now))
            .collect();
        if candidates.is_empty() {
            candidates.extend(self.ttft.for_session(session, None, now));
        }
        let version = env!("CARGO_PKG_VERSION");
        let mut out: Vec<Value> = vec![];
        for facts in candidates {
            if out
                .iter()
                .any(|o| o["observationId"] == facts.observation_id.as_str())
            {
                continue;
            }
            let product = headers
                .iter()
                .find(|(_, c)| *c == facts.command_id)
                .map(|(t, _)| t.as_str());
            out.push(facts.attached(version, product));
        }
        out.truncate(17);
        out
    }

    /// `v4/commands/query {clock: true}`: the calibration probe answers at once.
    pub(super) fn ttft_clock_probe(&self, p: &Value) -> Result<Option<Value>> {
        if p["clock"] != true {
            return Ok(None);
        }
        let received = self.ttft_clock.now();
        let keys = p["commands"].as_array().context("Command keys required")?;
        let results: Vec<Value> = keys
            .iter()
            .map(|key| json!({"key": key, "result": "unknown"}))
            .collect();
        Ok(Some(
            json!({"results": results, "clock": {"instanceId": self.ttft.instance,
            "receivedAt": received, "sentAt": self.ttft_clock.now()}}),
        ))
    }
}
