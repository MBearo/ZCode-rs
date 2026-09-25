//! Engine side of the legacy `session/event` stream (Node live projection in
//! `server-operations.ts`): runtime facts of root sessions become legacy events
//! for the subscribed delivery kind, next to the V4 projection.
use super::legacy_session::params_error;
use super::{Engine, Event};
use crate::{
    contract::{RuntimeError, ServerMsg},
    domain::{
        execution::Phase,
        legacy_params,
        legacy_stream::{RunKind, TurnTally, model_usage, usage_json},
    },
};
use anyhow::Result;
use serde_json::{Value, json};

/// What `apply_event` hands the legacy projection, taken before the V4
/// projection consumes the runtime event.
pub(super) enum Fact {
    Delta {
        message_id: String,
        text: String,
        reasoning: bool,
    },
    ModelDone {
        content: String,
        /// The step's tool calls; arguments only when subscribed.
        calls: Vec<Value>,
        usage: Value,
    },
    ToolDone {
        call: String,
        failed: bool,
        denied: bool,
        result: Option<String>,
        /// The tool's internal model usage (Node `tool_internal` completion).
        nested: Option<Value>,
    },
    Finished,
    /// Any other fact: ends the delta batching window.
    Barrier,
}

/// `subscribed`: text is only copied when a legacy stream will send it.
pub(super) fn fact(event: &Event, subscribed: bool) -> Fact {
    match event {
        Event::Text {
            response_id,
            text,
            reasoning,
        } if subscribed => Fact::Delta {
            message_id: response_id.clone(),
            text: text.clone(),
            reasoning: *reasoning,
        },
        Event::ModelDone { message, usage, .. } => {
            let Some(message) = message else {
                return Fact::Barrier;
            };
            let calls = message["tool_calls"].as_array().map_or(vec![], |calls| {
                calls
                    .iter()
                    .map(|c| match subscribed {
                        true => c.clone(),
                        false => {
                            json!({"id": c["id"], "function": {"name": c["function"]["name"]}})
                        }
                    })
                    .collect()
            });
            Fact::ModelDone {
                content: message["content"].as_str().unwrap_or("").into(),
                calls,
                usage: usage.clone(),
            }
        }
        Event::ToolDone {
            id,
            result,
            failed,
            denied,
            facts,
            ..
        } => Fact::ToolDone {
            call: id.clone(),
            failed: *failed,
            denied: *denied,
            result: subscribed.then(|| result.clone()),
            nested: facts.model_usage.clone(),
        },
        Event::Finished { .. } => Fact::Finished,
        _ => Fact::Barrier,
    }
}

impl Engine {
    /// `session/subscribe`: the last subscription's delivery kind wins.
    pub(super) fn legacy_subscribe(&mut self, raw: &Value) -> Result<Value> {
        let p = legacy_params::subscribe(raw).map_err(params_error)?;
        let id = p["sessionId"].as_str().unwrap_or_default().to_owned();
        let kind = p["deliveryKind"].as_str().unwrap_or_default();
        let Some(s) = self.sessions.get_mut(&id) else {
            return Err(RuntimeError::Coded {
                code: -32004,
                message: format!("Session is not active: {id}"),
            }
            .into());
        };
        let seq = s.runtime.legacy.subscribe(kind).unwrap_or_default();
        self.touch_session(&id);
        // Host 从不传 afterSeq；回放恒为空（见 spec 9.8）。
        let mut result = json!({"sessionId": id, "eventSeq": seq, "events": []});
        if p["includeSnapshot"] == true {
            let mut snapshot = self.legacy_snapshot_with(&id, false)?;
            snapshot["runtime"]["deliveryKind"] = kind.into();
            snapshot["runtime"]["eventSeq"] = seq.into();
            result["snapshot"] = snapshot;
        }
        Ok(result)
    }

    /// Sends `events` (`(type, payload)`) on `id`'s legacy stream; with no
    /// events it only closes the batching window. `turn` is the envelope's turn.
    pub(super) fn legacy_emit(&mut self, id: &str, turn: Option<&str>, events: Vec<(&str, Value)>) {
        let subscribed = self
            .sessions
            .get(id)
            .is_some_and(|s| s.runtime.legacy.kind().is_some());
        if !subscribed {
            return;
        }
        let ids: Vec<String> = events.iter().map(|_| self.clock.id()).collect();
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).unwrap();
        let mut out = vec![];
        if events.is_empty() {
            s.runtime.legacy.barrier(&mut out);
        }
        for ((kind, payload), event_id) in events.into_iter().zip(ids) {
            let mut event = json!({"eventId": event_id, "sessionId": id, "timestamp": now,
                "type": kind, "payload": payload});
            if let Some(turn) = turn {
                event["turnId"] = turn.into();
            }
            if let Some(trace) = s.runtime_trace.as_ref().or(s.trace_id.as_ref()) {
                event["traceId"] = trace.clone().into();
            }
            s.runtime.legacy.push(event, &mut out);
        }
        self.outbox
            .extend(out.into_iter().map(|params| ServerMsg::HostNotification {
                method: "session/event",
                params,
            }));
    }

    /// Node `turn_started` of a root session's run; the turn totals start here.
    pub(super) fn legacy_turn_started(&mut self, id: &str, turn: &str, kind: RunKind) {
        let now = self.clock.now();
        let Some(s) = self.sessions.get_mut(id).filter(|s| s.parent_id.is_none()) else {
            return;
        };
        let header = s
            .rows
            .iter()
            .rev()
            .find(|r| r["kind"] == "turnHeader" && r["turnId"] == turn);
        let input = s
            .rows
            .iter()
            .rev()
            .find(|r| r["kind"] == "userInput" && r["turnId"] == turn);
        let submitted = input.and_then(|r| r["sourceCommandId"].as_str());
        // 压缩与目标命令的运行没有 userInput 行：inputId 取 header 的提交命令（Node 同样带出）。
        let command = submitted.or_else(|| header.and_then(|h| h["sourceCommandId"].as_str()));
        let mut payload = if kind == RunKind::Compact {
            // Node compact.ts：维护命令只有这四个字段（没有 executionStartedAt）。
            let instructions = s.compact_instructions.as_deref().unwrap_or("").trim();
            let input = match instructions {
                "" => "/compact".to_owned(),
                text => format!("/compact {text}"),
            };
            json!({"turnNumber": s.runtime.legacy.turns_completed, "input": input,
                "inputVisibility": "model-only"})
        } else {
            json!({"turnNumber": s.runtime.legacy.turns_completed,
                "input": input.map_or("", |r| r["text"].as_str().unwrap_or("")),
                // D16：Node 带出 executionStartedAt（不在 strict schema 中，Host 因此丢弃整条事件）。
                "executionStartedAt": now})
        };
        if let Some(command) = command {
            payload["inputId"] = command.into();
        }
        if let Some(command) = submitted {
            payload["queryId"] = command.into();
        }
        if let Some(entity) = input.and_then(|r| r["entityId"].as_str()) {
            payload["messageId"] = entity.into();
        }
        if header.is_some_and(|h| h["origin"] == "goalContinuation") {
            payload["input"] = s
                .messages
                .last()
                .map_or(json!(""), |m| m["content"].clone());
            payload["inputSource"] = "goal-continuation".into();
            payload["inputVisibility"] = "model-only".into();
            if let Some(goal) = &s.goal {
                payload["targetId"] = goal.target_id.clone().into();
            }
        }
        let mut events = vec![];
        // Node 首轮在 turn.started 之前发出 first_input 标题。只看首个输入所在的轮：
        // 之后的压缩或目标续跑不新增输入，原先的条件会让它们重复发出标题事件。
        if s.history.inputs.len() == 1
            && s.history.inputs[0].turn == turn
            && s.title_source == "generated"
        {
            events.push((
                "session.titleUpdated",
                json!({"previousTitle": "", "source": "first_input", "title": s.title}),
            ));
        }
        events.push(("turn.started", payload));
        s.runtime.legacy.turn = Some(TurnTally {
            kind,
            started_at: now,
            input_id: command.map(str::to_owned),
            ..Default::default()
        });
        self.legacy_emit(id, Some(turn), events);
    }

    /// Projects one applied runtime fact of `id`'s run in `turn`.
    pub(super) fn legacy_fact(&mut self, id: &str, turn: &str, fact: Fact) -> Result<()> {
        let Some(s) = self.sessions.get_mut(id).filter(|s| s.parent_id.is_none()) else {
            return Ok(());
        };
        match fact {
            Fact::Delta {
                message_id,
                text,
                reasoning,
            } => {
                if let Some(tally) = &mut s.runtime.legacy.turn {
                    tally.message_id = Some(message_id.clone());
                }
                let kind = if reasoning {
                    "reasoning_delta"
                } else {
                    "text_delta"
                };
                let payload = json!({"assistantMessageId": message_id, "delta": text,
                    "done": false, "kind": kind});
                self.legacy_emit(id, Some(turn), vec![("model.streaming", payload)]);
            }
            Fact::ModelDone {
                content,
                calls,
                usage,
            } => {
                if let Some(tally) = &mut s.runtime.legacy.turn {
                    tally.model_done(&usage, &content, calls.len());
                }
                let usage = model_usage(&usage);
                let mut payload = json!({"content": content, "querySource": "main_turn",
                    "stopReason": if calls.is_empty() { "stop" } else { "tool-calls" },
                    "usage": usage_json(usage), "toolCallCount": calls.len()});
                if let Some(window) = s.usage["contextWindow"]["maxTokens"].as_u64() {
                    payload["contextWindow"] = window.into();
                }
                self.legacy_emit(id, Some(turn), vec![("session.updated", payload)]);
                self.legacy_scheduled(id, turn, &calls);
            }
            Fact::ToolDone {
                call,
                failed,
                denied,
                result,
                nested,
            } => {
                self.legacy_tool_done(id, turn, &call, (failed, denied), result);
                // Node 在工具结果之后追加 tool_internal 的 model_complete，旧协议作为 session.updated 转发。
                if let Some(usage) = nested {
                    let s = self.sessions.get_mut(id).unwrap();
                    if let Some(tally) = &mut s.runtime.legacy.turn {
                        tally.nested(&usage);
                    }
                    let payload = json!({"content": "", "stopReason": "tool_internal",
                        "usage": usage, "toolCallCount": 0});
                    self.legacy_emit(id, Some(turn), vec![("session.updated", payload)]);
                }
            }
            Fact::Barrier => self.legacy_emit(id, Some(turn), vec![]),
            Fact::Finished => return self.legacy_turn_finished(id, turn),
        }
        Ok(())
    }

    /// Node `turn_complete` / `turn_error`, then `afterLegacyStateMutation`
    /// (`prompt_completed` / `prompt_failed`).
    fn legacy_turn_finished(&mut self, id: &str, turn: &str) -> Result<()> {
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).unwrap();
        let tally = s.runtime.legacy.turn.take().unwrap_or_default();
        s.runtime.legacy.turns_completed += 1;
        let duration = now.saturating_sub(tally.started_at);
        let success = s.phase == Phase::CompletedSuccess;
        let reason = tally
            .kind
            .end_reason(success, !success && s.phase != Phase::Error);
        let (kind, mut payload) = match s.phase {
            Phase::CompletedSuccess => (
                "turn.completed",
                json!({"response": tally.response, "tokenCount": tally.token_count,
                    "usage": tally.summary(), "toolCallCount": tally.tool_calls,
                    "historyRoundCount": tally.rounds, "duration": duration, "resultType": "success"}),
            ),
            Phase::Error => {
                let error = s.last_error.clone().unwrap_or_default();
                let code = error["code"].as_str().unwrap_or("fault.runtime.execution");
                let mut detail = json!({"type": code, "code": code,
                    "message": error["message"].as_str().filter(|m| !m.trim().is_empty()).unwrap_or("Turn execution failed")});
                if error["attribution"].is_object() {
                    detail["attribution"] = error["attribution"].clone();
                    detail["retryable"] = error["recoverable"].clone();
                }
                (
                    "turn.failed",
                    json!({"error": detail, "turnPhase": "execution"}),
                )
            }
            _ => (
                "turn.completed",
                json!({"response": "", "tokenCount": 0, "usage": tally.summary(), "toolCallCount": 0,
                    "historyRoundCount": tally.rounds, "duration": duration, "resultType": "cancelled"}),
            ),
        };
        if let Some(input) = &tally.input_id {
            payload["inputId"] = input.clone().into();
        }
        self.legacy_emit(id, Some(turn), vec![(kind, payload)]);
        self.legacy_state_updated(id, reason)
    }

    /// Node `afterStateMutation` for a non-configuration reason: the legacy
    /// revision advances and the Host gets the current settings. Sent whether
    /// or not a legacy stream is subscribed.
    pub(super) fn legacy_state_updated(&mut self, id: &str, reason: &str) -> Result<()> {
        self.legacy_state_patch(id, reason, None)
    }

    /// `legacy_state_updated` with Node `afterPromptAccepted`'s `patch`
    /// (`{status: "running"}`) in place of the settings.
    pub(super) fn legacy_state_patch(
        &mut self,
        id: &str,
        reason: &str,
        patch: Option<Value>,
    ) -> Result<()> {
        let s = self.sessions.get_mut(id).unwrap();
        s.runtime.state_revision += 1;
        let revision = s.runtime.state_revision;
        let s = &self.sessions[id];
        let workspace = s
            .runtime
            .workspace
            .clone()
            .unwrap_or_else(|| self.rebuilt_workspace(id));
        let patch = patch.unwrap_or_else(|| self.legacy_settings(s, false));
        self.outbox.push(ServerMsg::HostNotification {
            method: "state.updated",
            params: json!({"patch": patch, "reason": reason, "revision": revision,
                "scope": "session", "sessionId": id, "type": "state.updated", "workspace": workspace}),
        });
        Ok(())
    }
}
