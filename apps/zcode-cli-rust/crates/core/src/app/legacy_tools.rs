//! Tool and permission events of the legacy stream (Node `tool.updated`,
//! `permission.requested` and `permission.resolved`, `SM:395-452`, `SM:853-896`).
use super::Engine;
use crate::{
    contract::{PermissionAnswer, PermissionRequest},
    domain::{
        legacy_stream::{ToolTrack, input_summary},
        permission::protocol_options,
    },
};
use serde_json::{Value, json};

impl Engine {
    /// A committed model step's calls, in Node's schedule: consecutive
    /// concurrency-safe calls share a parallel group, others run alone.
    pub(super) fn legacy_scheduled(&mut self, id: &str, turn: &str, calls: &[Value]) {
        let name = |call: &Value| call["function"]["name"].as_str().unwrap_or("").to_owned();
        let call_id = |call: &Value| call["id"].as_str().unwrap_or("").to_owned();
        let mut groups: Vec<(bool, Vec<String>)> = vec![];
        for call in calls {
            let safe = self.tools.concurrent_safe_scoped(id, &name(call));
            match groups.last_mut() {
                Some((true, group)) if safe => group.push(call_id(call)),
                _ => groups.push((safe, vec![call_id(call)])),
            }
        }
        let order: Vec<String> = calls.iter().map(call_id).collect();
        let parallel: Vec<&Vec<String>> = groups.iter().map(|(_, g)| g).collect();
        let schedule = json!({"parallelGroups": parallel, "executionOrder": order});
        let Some(tally) = self
            .sessions
            .get_mut(id)
            .and_then(|s| s.runtime.legacy.turn.as_mut())
        else {
            return;
        };
        let mut events = vec![];
        for call in calls {
            let input = call["function"]["arguments"]
                .as_str()
                .map(|args| serde_json::from_str(args).unwrap_or_else(|_| args.into()))
                .unwrap_or(Value::Null);
            tally.tools.insert(
                call_id(call),
                ToolTrack {
                    name: name(call),
                    input_summary: input_summary(&input),
                    ..Default::default()
                },
            );
            let index = groups
                .iter()
                .position(|(_, g)| g.contains(&call_id(call)))
                .unwrap_or(0);
            let mut payload = json!({"toolCallId": call["id"], "toolName": name(call), "input": input,
                "parallelGroupIndex": index, "canRunParallel": groups[index].0,
                "schedule": schedule, "kind": "scheduled"});
            if let Some(message) = &tally.message_id {
                payload["assistantMessageId"] = message.clone().into();
            }
            events.push(("tool.updated", payload));
        }
        self.legacy_emit(id, Some(turn), events);
    }

    /// Node `tool_call_started`, after hooks and permission.
    pub(super) fn legacy_tool_executing(&mut self, id: &str, turn: &str, call: &str) {
        let now = self.clock.now();
        let Some(track) = self
            .sessions
            .get_mut(id)
            .and_then(|s| s.runtime.legacy.turn.as_mut())
            .and_then(|t| t.tools.get_mut(call))
        else {
            return;
        };
        track.started_at = Some(now);
        let name = track.name.clone();
        let read_only = self.tools.concurrent_safe_scoped(id, &name);
        // D16：Node 带出 readOnly（与 sideEffectScope）；strict schema 不含，Host 丢弃整条事件。
        let payload = json!({"toolCallId": call, "toolName": name, "startedAt": now,
            "readOnly": read_only, "kind": "started"});
        self.legacy_emit(id, Some(turn), vec![("tool.updated", payload)]);
    }

    /// The call's result. A policy refusal is Node `permission_denied`; a
    /// refusal after a prompt already went out as `permission.resolved`.
    pub(super) fn legacy_tool_done(
        &mut self,
        id: &str,
        turn: &str,
        call: &str,
        outcome: (bool, bool),
        result: Option<String>,
    ) {
        let (failed, denied) = outcome;
        let now = self.clock.now();
        let Some(track) = self
            .sessions
            .get_mut(id)
            .and_then(|s| s.runtime.legacy.turn.as_mut())
            .and_then(|t| t.tools.get_mut(call))
        else {
            return;
        };
        track.succeeded = Some(!failed);
        let result = result.unwrap_or_default();
        let payload = if denied {
            if track.prompted {
                return self.legacy_emit(id, Some(turn), vec![]);
            }
            let reason = if result.is_empty() {
                format!("Permission denied for {}", track.name)
            } else {
                result
            };
            json!({"toolCallId": call, "toolName": track.name, "reason": reason,
                "inputSummary": track.input_summary, "decision": "deny"})
        } else if failed {
            let message = if result.trim().is_empty() {
                "Tool execution failed".into()
            } else {
                result
            };
            json!({"toolCallId": call, "error": {"type": "tool_execution_failed", "message": message},
                "kind": "error"})
        } else {
            let duration = track.started_at.map_or(0, |at| now.saturating_sub(at));
            json!({"toolCallId": call, "result": {"success": true, "content": result},
                "duration": duration, "kind": "result"})
        };
        let kind = if denied {
            "permission.resolved"
        } else {
            "tool.updated"
        };
        self.legacy_emit(id, Some(turn), vec![(kind, payload)]);
    }

    /// Node `tool_batch_complete` for one executed group.
    pub(super) fn legacy_tool_batch(&mut self, id: &str, turn: &str, ids: Vec<String>) {
        let Some(tally) = self
            .sessions
            .get(id)
            .and_then(|s| s.runtime.legacy.turn.as_ref())
        else {
            return;
        };
        let succeeded = ids
            .iter()
            .filter(|call| tally.tools.get(*call).and_then(|t| t.succeeded) == Some(true))
            .count();
        let payload = json!({"toolCallIds": ids, "successCount": succeeded,
            "errorCount": ids.len() - succeeded, "kind": "batch"});
        self.legacy_emit(id, Some(turn), vec![("tool.updated", payload)]);
    }

    /// Node `permission_requested` through `SM:880-896`: the options are
    /// rebuilt for the legacy protocol, where both always-allow policies only
    /// drop the project option.
    pub(super) fn legacy_permission_requested(
        &mut self,
        id: &str,
        interaction: &str,
        call: &Value,
        request: &PermissionRequest,
        full_access: bool,
    ) {
        let tool = call["function"]["name"].as_str().unwrap_or("");
        let call_id = call["id"].as_str().unwrap_or("");
        let turn = self.active.get(id).map(|a| a.turn_id.clone());
        if let Some(track) = self
            .sessions
            .get_mut(id)
            .and_then(|s| s.runtime.legacy.turn.as_mut())
            .and_then(|t| t.tools.get_mut(call_id))
        {
            track.prompted = true;
        }
        let policy = request.options_policy.as_ref().map(|_| "no-always-allow");
        let reason = if request.reason.is_empty() {
            format!("Tool {tool} requires approval")
        } else {
            request.reason.clone()
        };
        let mut payload = json!({"requestId": interaction, "toolCallId": call_id, "toolName": tool,
            "riskLevel": request.risk_level, "reason": reason, "input": request.input,
            "suggestedPermissionUpdates": request.suggestions,
            "options": protocol_options(tool, &request.input, &request.suggestions, policy)});
        if full_access {
            // D16：Node 的 fullAccessSupported 不在 strict schema 中，Host 丢弃整条事件。
            payload["fullAccessSupported"] = true.into();
        }
        self.legacy_emit(id, turn.as_deref(), vec![("permission.requested", payload)]);
    }

    /// Node `permission_resolved` with the broker's decision.
    pub(super) fn legacy_permission_resolved(
        &mut self,
        owner: &str,
        call: &str,
        interaction: &str,
        answer: &PermissionAnswer,
    ) {
        let turn = self.active.get(owner).map(|a| a.turn_id.clone());
        let mut payload = json!({"requestId": interaction, "toolCallId": call});
        match answer {
            PermissionAnswer::Allow | PermissionAnswer::Fail(_) => {
                payload["decision"] = "allow".into();
            }
            PermissionAnswer::Deny { message, .. } => {
                payload["decision"] = "deny".into();
                payload["reason"] = message.clone().into();
            }
            PermissionAnswer::PlanRejected(feedback) => {
                payload["decision"] = "deny".into();
                if let Some(feedback) = feedback {
                    payload["reason"] = feedback.clone().into();
                }
            }
        }
        self.legacy_emit(
            owner,
            turn.as_deref(),
            vec![("permission.resolved", payload)],
        );
    }
}
