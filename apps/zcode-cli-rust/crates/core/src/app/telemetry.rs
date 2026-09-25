//! Live session events as telemetry (spec rust-m9-usage-logs §5): every Node
//! `SessionEvent` the Engine observes becomes a `computer-use/operation-event`
//! and a `v4/telemetry/event` fact. Hydration never calls in here.
use super::Engine;
use crate::contract::Event;
use crate::contract::ServerMsg;
use crate::domain::telemetry::{self, Runtime};
use serde_json::{Value, json};

/// Node `mirrorSubagentToolEvent`: the child events the parent sees.
fn mirrored(kind: &str) -> bool {
    kind.starts_with("tool_call_") || kind.starts_with("permission_")
}

impl Engine {
    /// One live event of session `id` in `turn`.
    pub(super) fn session_event(
        &mut self,
        id: &str,
        turn: Option<&str>,
        kind: &str,
        payload: Value,
    ) {
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        s.event_seq += 1;
        let seq = s.event_seq;
        let runtime = Runtime {
            memory_enabled: None,
            model: (!s.model.is_empty()).then(|| (s.model.clone(), s.provider.clone())),
        };
        let parent = s
            .parent_id
            .clone()
            .filter(|_| s.task_type == "subagent_child" && mirrored(kind));
        let (event_id, at) = (self.clock.id(), self.clock.now());
        let event = telemetry::Event {
            id: &event_id,
            seq,
            at,
            session: id,
            turn,
            kind,
            payload: &payload,
        };
        self.emit_telemetry(&event, &runtime);
        if let Some(parent) = parent {
            self.mirror(id, &parent, kind, &payload);
        }
    }

    fn emit_telemetry(&mut self, event: &telemetry::Event, runtime: &Runtime) {
        if let Some(params) = telemetry::computer_use(event) {
            self.outbox.push(ServerMsg::HostNotification {
                method: "computer-use/operation-event",
                params,
            });
        }
        if let Some(params) = self.telemetry.normalize(event, runtime) {
            self.outbox.push(ServerMsg::HostNotification {
                method: "v4/telemetry/event",
                params,
            });
        }
    }

    /// The parent's copy of a child's tool or permission event (Node
    /// `mirrorSubagentToolEvent`): not stored, so its sequence is 0.
    fn mirror(&mut self, child: &str, parent: &str, kind: &str, payload: &Value) {
        let Some(p) = self.sessions.get(parent) else {
            return;
        };
        let Some(task) = p.children.values().find(|t| t.child_id == child) else {
            return;
        };
        let Some(call) = payload["toolCallId"].as_str() else {
            return;
        };
        let turn = p
            .rows
            .iter()
            .find(|r| r["kind"] == "toolCall" && r["toolCallId"] == task.call_id.as_str())
            .and_then(|r| r["turnId"].as_str())
            .map(str::to_owned);
        let mut out = payload.clone();
        out["toolCallId"] = format!("tool_subagent_{}_{call}", task.id).into();
        out["childSessionId"] = child.into();
        if task.background {
            out["background"] = true.into();
        }
        if kind.starts_with("tool_call_") {
            out["agentId"] = task.id.clone().into();
            out["agentType"] = task.agent_type.clone().into();
            out["childToolCallId"] = call.into();
            out["description"] = task.description.clone().into();
            out["parentToolCallId"] = task.call_id.clone().into();
            out["source"] = "subagent".into();
        }
        let runtime = Runtime {
            memory_enabled: None,
            model: (!p.model.is_empty()).then(|| (p.model.clone(), p.provider.clone())),
        };
        let (event_id, at) = (self.clock.id(), self.clock.now());
        let event = telemetry::Event {
            id: &event_id,
            seq: 0,
            at,
            session: parent,
            turn: turn.as_deref(),
            kind,
            payload: &out,
        };
        self.emit_telemetry(&event, &runtime);
    }

    /// The compaction a run leaves open when it ends (Node
    /// `finishCompactTimelineFailure`): its turn and `CompactFailed` payload.
    pub(super) fn open_compaction(&self, id: &str, event: &Event) -> Option<(String, Value)> {
        let Event::Finished {
            error, cancelled, ..
        } = event
        else {
            return None;
        };
        let active = self.active.get(id)?;
        let stopped = *cancelled || active.cancel.is_cancelled();
        if !stopped && error.is_none() {
            return None;
        }
        let status = if stopped { "interrupted" } else { "failed" };
        let update = json!({"endedAt": self.clock.now()});
        let payload = self
            .sessions
            .get(id)?
            .node_compact_payload(status, &update)?;
        Some((active.turn_id.clone(), payload))
    }

    pub(super) fn compaction_ended(&mut self, id: &str, ended: Option<(String, Value)>) {
        if let Some((turn, payload)) = ended {
            self.session_event(id, Some(&turn), "compact_failed", payload);
        }
    }

    /// Node `SubagentSpawned` (a running task) or `SubagentStopped` on the parent.
    pub(super) fn telemetry_subagent(
        &mut self,
        parent: &str,
        turn: Option<&str>,
        task: &crate::domain::subagent::Task,
        error: Option<&str>,
    ) {
        let spawned = task.running();
        let mut payload = json!({"agentId": task.id, "agentType": task.agent_type,
            "childSessionId": task.child_id, "parentToolCallId": task.call_id, "status": task.status});
        if task.background {
            payload["background"] = true.into();
        }
        if let Some(error) = error {
            payload["error"] = error.into();
        }
        let kind = if spawned {
            "subagent_spawned"
        } else {
            "subagent_stopped"
        };
        self.session_event(parent, turn, kind, payload);
    }

    /// Node `ModelStreaming` of one text or reasoning delta.
    pub(super) fn telemetry_chunk(&mut self, id: &str, turn: &str, event: &Event) {
        let Event::Text {
            text, reasoning, ..
        } = event
        else {
            return;
        };
        let kind = if *reasoning {
            "reasoning_delta"
        } else {
            "text_delta"
        };
        let mut payload = json!({"kind": kind, "delta": text});
        let step = self
            .sessions
            .get(id)
            .and_then(|s| s.node.turn.as_ref())
            .and_then(|t| t.step.as_ref());
        if let Some(step) = step {
            payload["assistantMessageId"] = step.assistant.clone().into();
        }
        self.session_event(id, Some(turn), "model_streaming", payload);
    }

    /// Node `TurnStarted` of the run starting `turn`: the admission input and
    /// what woke the turn.
    pub(super) fn telemetry_turn_started(
        &mut self,
        id: &str,
        turn: &str,
        admission: [(&str, Option<String>); 3],
    ) {
        let Some(s) = self.sessions.get(id) else {
            return;
        };
        let header = s
            .rows
            .iter()
            .rev()
            .find(|r| r["kind"] == "turnHeader" && r["turnId"] == turn);
        let mut payload = json!({});
        if s.task_type == "subagent_child" {
            // Node：子会话的轮次由子代理发起，没有 admission inputId。
            payload["inputSource"] = "subagent".into();
        } else if let Some(input) = super::legacy_stream::turn_input_id(s, turn) {
            payload["inputId"] = input.into();
        }
        match header.map(|h| h["origin"].as_str().unwrap_or("")) {
            Some("goalContinuation") => payload["inputSource"] = "goal-continuation".into(),
            Some("backgroundResult") => {
                payload["inputSource"] = "background_task".into();
                payload["backgroundSource"] = "subagent".into();
            }
            _ => {}
        }
        for (key, value) in admission {
            if let Some(value) = value {
                payload[key] = value.into();
            }
        }
        self.session_event(id, Some(turn), "turn_started", payload);
    }
}

/// The Node event of a finished call from its legacy payload: the result, the
/// error (Node `TOOL_EXECUTION_FAILED`), or a policy refusal
/// (`permission_denied`); a refusal after a prompt has no further event.
pub(super) fn tool_done_event(
    legacy: &Value,
    (failed, denied): (bool, bool),
    perf: Option<Value>,
) -> Option<(&'static str, Value)> {
    let call = &legacy["toolCallId"];
    if denied {
        return Some((
            "permission_denied",
            json!({"toolCallId": call, "toolName": legacy["toolName"]}),
        ));
    }
    if failed {
        return Some((
            "tool_call_error",
            json!({"toolCallId": call, "error": legacy["error"]}),
        ));
    }
    let mut result = json!({"success": true});
    if let Some(perf) = perf {
        result["perf"] = perf;
    }
    Some((
        "tool_call_result",
        json!({"toolCallId": call, "duration": legacy["duration"], "result": result}),
    ))
}
