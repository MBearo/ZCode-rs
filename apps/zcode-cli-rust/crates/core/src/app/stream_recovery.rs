//! Retry state of the V4 control and stream recovery facts (spec
//! rust-m7-stream-recovery). The engine owns both; runs only report events.
use super::Engine;
use crate::contract::Event;
use crate::domain::stream_recovery as recovery;
use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::oneshot;

impl Engine {
    /// Node `modelAnomalyGuard` of the effective config (spec §5).
    pub fn with_anomaly_guard(mut self, guard: &Value) -> Self {
        self.anomaly_guard = crate::domain::model_anomaly::Guard::from_config(guard);
        self
    }

    /// Tracks the agent step request and derives `apiRetry` from one event of
    /// the session's active run (before the projection, which skips events
    /// after cancellation).
    pub(super) fn observe_step(&mut self, id: &str, event: &Event) -> Result<()> {
        let now = self.clock.now();
        let (Some(active), Some(s)) = (self.active.get_mut(id), self.sessions.get_mut(id)) else {
            return Ok(());
        };
        if let Event::Finished { cancelled, .. } = event
            && (*cancelled || active.cancel.is_cancelled())
        {
            commit_stopped_output(s, &std::mem::take(&mut active.step));
            return Ok(());
        }
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
            Event::ModelDone { .. } => {
                // 已提交的响应不再是“停止时的部分输出”。
                active.step.response = None;
                Some(None)
            }
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
        (retry, max): (u32, u32),
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
            max,
            now,
            recovery::kind_reason_code(kind),
        ));
        s.updated_at = now;
        self.publish(id, deltas)?;
        let mut started = json!({"attemptId": attempt_id, "assistantMessageId": response,
            "failureKind": kind, "message": message, "retryNumber": retry, "maxRetries": max});
        let anchor = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "reason": "no_tool_committed", "committedToolCallIds": []});
        let tail = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "assistantMessageId": response, "discardedReasoningBytes": probe.reasoning_bytes,
            "discardedTextBytes": probe.text_bytes, "discardedToolCallIds": []});
        let mut retried = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "retryNumber": retry, "maxRetries": max, "streamMode": "sse"});
        let mut status = json!({"attemptId": attempt_id, "anchorId": anchor_id,
            "maxRetries": max, "retryNumber": retry});
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

/// Node's cancel branch: text and reasoning already streamed by the step in
/// flight join the history as an assistant message, so the next turn sees what
/// the user saw.
fn commit_stopped_output(
    s: &mut crate::domain::session::Session,
    probe: &crate::domain::stream_recovery::StepProbe,
) {
    let (text, reasoning) = streamed_output(s, probe);
    if text.is_empty() && reasoning.is_empty() {
        return;
    }
    let mut message = json!({"role": "assistant", "content": text});
    if !reasoning.is_empty() {
        message["reasoning_content"] = reasoning.into();
    }
    if let Some((provider, model)) = &probe.model {
        message["_zcode_origin"] = json!({"provider": provider, "model": model});
    }
    s.append_message(message);
}

/// The text and reasoning the step in flight streamed so far.
pub(super) fn streamed_output(
    s: &crate::domain::session::Session,
    probe: &crate::domain::stream_recovery::StepProbe,
) -> (String, String) {
    let Some(response) = &probe.response else {
        return Default::default();
    };
    let text_of = |kind: &str| {
        s.rows
            .iter()
            .filter(|r| r["assistantResponseId"] == response.as_str() && r["kind"] == kind)
            .filter_map(|r| r["text"].as_str())
            .collect::<String>()
    };
    (text_of("assistantText"), text_of("reasoning"))
}

/// Reports a recovery of the run's agent step to the engine and returns the next
/// request's `streamRecovery` after the optional wait (`delay_ms`).
pub(super) async fn retry_request(
    sink: &crate::contract::EventSink,
    cancel: &tokio_util::sync::CancellationToken,
    (retry, max): (u32, u32),
    delay_ms: u64,
) -> Result<std::sync::Arc<Value>> {
    use anyhow::Context;
    let (reply, receipt) = oneshot::channel();
    sink.send(Event::StreamRecovery { retry, max, reply })
        .await?;
    let status = tokio::select! {biased;
        _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
        status = receipt => status.context("Stream recovery commit failed")?,
    };
    if delay_ms > 0 {
        tokio::select! {biased;
            _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
        }
    }
    Ok(std::sync::Arc::new(status))
}
