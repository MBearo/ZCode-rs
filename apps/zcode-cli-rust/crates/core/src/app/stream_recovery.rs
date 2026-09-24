//! Retry state of the V4 control and stream recovery facts (spec
//! rust-m7-stream-recovery). The engine owns both; runs only report events.
use super::Engine;
use crate::contract::Event;
use crate::domain::stream_recovery::{self as recovery, MAX_RETRIES};
use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::oneshot;

impl Engine {
    /// Tracks the agent step request and derives `apiRetry` from one event of
    /// the session's active run (before the projection, which skips events
    /// after cancellation).
    pub(super) fn observe_retry(&mut self, id: &str, event: &Event) -> Result<()> {
        let now = self.clock.now();
        let (Some(active), Some(s)) = (self.active.get_mut(id), self.sessions.get_mut(id)) else {
            return Ok(());
        };
        let next = match event {
            Event::ModelStatus(status) => {
                active.step.observe_status(status);
                recovery::status_retry(status, s.api_retry.as_ref(), now)
            }
            Event::Text {
                response_id,
                text,
                reasoning,
            } => {
                active.step.observe_text(response_id, text, *reasoning);
                Some(None)
            }
            Event::ModelDone { .. } => Some(None),
            _ => None,
        };
        let Some(next) = next.filter(|next| *next != s.api_retry) else {
            return Ok(());
        };
        s.api_retry = next;
        // 文本与模型完成随后的投影会发布状态；网络状态不经过投影，这里单独发布。
        if matches!(event, Event::ModelStatus(_)) {
            s.updated_at = now;
            self.publish(id, vec![])?;
        }
        Ok(())
    }

    /// Node `recoverPartialAssistantOutputFailure`: closes the failed
    /// response's rows, reports the recovery and answers the next request's
    /// `streamRecovery`.
    pub(super) async fn stream_recovery(
        &mut self,
        id: &str,
        turn: &str,
        retry: u32,
        reply: oneshot::Sender<Value>,
    ) -> Result<()> {
        let now = self.clock.now();
        let fallback = self.clock.id();
        let probe = std::mem::take(&mut self.active.get_mut(id).unwrap().step);
        let response = probe.response.clone().unwrap_or(fallback);
        let attempt_id = format!("{response}:end-of-stream");
        let anchor_id = format!("{response}:previous-message-anchor");
        let failed_request = probe
            .failed
            .as_ref()
            .map(|f| f.0.clone())
            .or(probe.started.clone());
        let (reason, message) = probe
            .failed
            .as_ref()
            .map_or(("", ""), |f| (f.1.as_str(), f.2.as_str()));
        let kind = recovery::failure_kind(reason, message);
        let s = self.sessions.get_mut(id).unwrap();
        let mut deltas = vec![];
        for row in &mut s.rows {
            if row["turnId"] == turn && row["state"] == "streaming" {
                row["state"] = "interrupted".into();
                deltas.push(json!({"op": "row.upserted", "row": row}));
            }
        }
        s.api_retry = Some(recovery::recovery_state(
            retry,
            MAX_RETRIES,
            now,
            recovery::kind_reason_code(kind),
        ));
        s.updated_at = now;
        self.publish(id, deltas)?;
        let mut started = json!({"attemptId": attempt_id, "assistantMessageId": response,
            "failureKind": kind, "message": message, "retryNumber": retry, "maxRetries": MAX_RETRIES});
        let anchor = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "reason": "no_tool_committed", "committedToolCallIds": []});
        let tail = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "assistantMessageId": response, "discardedReasoningBytes": probe.reasoning_bytes,
            "discardedTextBytes": probe.text_bytes, "discardedToolCallIds": []});
        let mut retried = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "retryNumber": retry, "maxRetries": MAX_RETRIES, "streamMode": "sse"});
        let mut status = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "maxRetries": MAX_RETRIES, "retryNumber": retry});
        if let Some(request) = failed_request {
            started["failedRequestId"] = request.clone().into();
            retried["failedRequestId"] = request.clone().into();
            status["recoveredFromRequestId"] = request.into();
        }
        let events = [started, anchor, tail, retried]
            .into_iter()
            .map(|mut payload| {
                if let Some(retry) = recovery::legacy_retry(&payload) {
                    payload["_meta"] = json!({"zcode": {"apiRetry": retry}});
                }
                ("streamRecovery.updated", payload)
            })
            .collect();
        self.legacy_emit(id, Some(turn), events);
        self.persist(id, None).await?;
        tracing::info!(
            target: "zcode::runtime",
            event = "model.stream.recovery",
            session_id = id,
            retry,
            failure_kind = kind,
            "Model stream recovery started"
        );
        let _ = reply.send(status);
        Ok(())
    }
}
