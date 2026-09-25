//! The legacy `session/event` turn end (`turn_complete` / `turn_error`) and
//! Node `afterStateMutation` (`state.updated`), split from `legacy_stream`.
use super::Engine;
use crate::{contract::ServerMsg, domain::execution::Phase};
use anyhow::Result;
use serde_json::{Value, json};

impl Engine {
    /// Node `turn_complete` / `turn_error`, then `afterLegacyStateMutation`
    /// (`prompt_completed` / `prompt_failed`).
    pub(super) fn legacy_turn_finished(&mut self, id: &str, turn: &str) -> Result<()> {
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).unwrap();
        let tally = s.runtime.legacy.turn.take().unwrap_or_default();
        s.runtime.legacy.turns_completed += 1;
        let duration = now.saturating_sub(tally.started_at);
        let success = s.phase == Phase::CompletedSuccess;
        let reason = tally
            .kind
            .end_reason(success, !success && s.phase != Phase::Error);
        // Node 旧 projection 的 totalTokenCount 是本进程各轮 turn_complete.tokenCount 之和。
        if s.phase == Phase::CompletedSuccess {
            s.runtime.legacy_tokens = s.runtime.legacy_tokens.saturating_add(tally.token_count);
        }
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
