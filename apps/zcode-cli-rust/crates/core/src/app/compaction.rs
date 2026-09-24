//! One compaction of a run's history (Node `compactActiveConversation`).
//! Spec rust-m7-compact. The engine commits every step through receipts.
use super::context::{RunContext, hidden_request};
use crate::contract::{Event, EventSink, ModelFailure, ModelPort};
use crate::domain::compact::{self, MAX_SUMMARY_OUTPUT, MAX_SUMMARY_TOOLS};
use crate::domain::context::{ContextState, estimate, with_summary};
use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Trigger<'a> {
    /// `/compact` with its custom instructions.
    Manual(&'a str),
    Auto,
    Reactive,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Compacted,
    Skipped,
}

/// What the summary request shares with the agent step, and what follows the summary.
pub(super) struct Request<'a> {
    pub prefix: &'a [Value],
    pub tools: &'a [Value],
    /// The approved plan file reference.
    pub reminder: Option<Value>,
}

/// A compaction failure of Node's own (`createCoreError` with its retry flag).
#[derive(Debug)]
pub(super) struct Failure {
    message: &'static str,
    retryable: bool,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for Failure {}

/// Node `isAutoCompactRetryableError`: its own failures and model failures carry
/// the flag; any other error is retryable.
fn retryable(error: &anyhow::Error) -> bool {
    if let Some(failure) = error.downcast_ref::<Failure>() {
        return failure.retryable;
    }
    error
        .downcast_ref::<ModelFailure>()
        .is_none_or(|f| f.retryable)
}

async fn committed(receipt: oneshot::Receiver<()>, cancel: &CancellationToken) -> Result<()> {
    super::agent_loop::durable(receipt, cancel).await
}

impl RunContext {
    /// Summarizes the selected history; `Skipped` when nothing can be compacted.
    pub async fn compact(
        &mut self,
        model: &dyn ModelPort,
        sink: &EventSink,
        cancel: &CancellationToken,
        trigger: Trigger<'_>,
        request: Request<'_>,
    ) -> Result<Outcome> {
        let (manual, instructions) = match trigger {
            Trigger::Manual(instructions) => (true, Some(instructions)),
            _ => (false, None),
        };
        let before = self.estimated;
        let id = format!(
            "compact-{}-{}-{}",
            sink.run_id,
            self.state.offset,
            self.messages.len()
        );
        let split = compact::select(&self.messages, self.state.summary.is_some(), manual);
        // 自动与反应式压缩不可压缩时不产生时间线标记（Node 决策 not_enough_messages）。
        if split.is_none() && !manual {
            return Ok(Outcome::Skipped);
        }
        let (done, receipt) = oneshot::channel();
        sink.send(Event::CompactStarted {
            id: id.clone(),
            manual,
            tokens: before,
            committed: done,
        })
        .await?;
        committed(receipt, cancel).await?;
        let Some(split) = split else {
            // 手动压缩无可压缩内容：健康的 noop（Node skipped）。
            let (done, receipt) = oneshot::channel();
            sink.send(Event::CompactDone {
                id,
                context: self.state.clone(),
                tokens: before,
                usage: Value::Null,
                reminder: None,
                committed: done,
            })
            .await?;
            committed(receipt, cancel).await?;
            return Ok(Outcome::Skipped);
        };
        let attempts = if trigger == Trigger::Auto {
            compact::AUTO_ATTEMPTS
        } else {
            1
        };
        let mut attempt = 1;
        let (summary, usage) = loop {
            match self
                .summarize(model, sink, cancel, split, (instructions, &request))
                .await
            {
                Ok(done) => break done,
                Err(error) if !cancel.is_cancelled() && attempt < attempts && retryable(&error) => {
                    attempt += 1;
                }
                Err(error) => {
                    if !cancel.is_cancelled() {
                        let (done, receipt) = oneshot::channel();
                        sink.send(Event::CompactFailed {
                            id,
                            committed: done,
                        })
                        .await?;
                        committed(receipt, cancel).await?;
                    }
                    return Err(error);
                }
            }
        };
        let next = ContextState {
            offset: self.state.offset + split,
            summary: Some(compact::summary_message(&summary)),
        };
        let after = estimate(&with_summary(
            next.summary.as_deref(),
            &self.messages[split..],
        ));
        let (done, receipt) = oneshot::channel();
        sink.send(Event::CompactDone {
            id,
            context: next.clone(),
            tokens: after,
            usage,
            reminder: request.reminder.clone(),
            committed: done,
        })
        .await?;
        committed(receipt, cancel).await?;
        // 只有 owner 事务提交后，工作副本才能切换边界并发送下一次模型请求。
        self.state = next;
        self.messages.drain(..split);
        // offset 只统计 canonical 消息；临时消息不进入持久化摘要边界，被摘要覆盖的随之丢弃。
        self.transient
            .retain_mut(|t| match t.position.checked_sub(split) {
                Some(position) => {
                    t.position = position;
                    true
                }
                None => false,
            });
        if let Some(reminder) = request.reminder {
            self.push(reminder);
        }
        self.usage_anchor = None;
        self.estimated = after;
        Ok(Outcome::Compacted)
    }

    /// Node's summary request: the step's prefix, the summarized history and
    /// the compact prompt, with the step's tools and a 20K output cap.
    async fn summarize(
        &self,
        model: &dyn ModelPort,
        sink: &EventSink,
        cancel: &CancellationToken,
        split: usize,
        (instructions, request): (Option<&str>, &Request<'_>),
    ) -> Result<(String, Value)> {
        let cap = model.context_policy().max_output.min(MAX_SUMMARY_OUTPUT);
        let bound = model.with_max_output_tokens(cap)?;
        let model = bound.as_deref().unwrap_or(model);
        let mut messages = request.prefix.to_vec();
        messages.extend(with_summary(
            self.state.summary.as_deref(),
            &self.messages[..split],
        ));
        messages.push(json!({"role": "user", "content": compact::prompt(instructions)}));
        let tools = if request.tools.len() > MAX_SUMMARY_TOOLS {
            &[][..]
        } else {
            request.tools
        };
        let output = hidden_request(model, (messages, tools), sink, "compact", cancel).await?;
        let failure = |message, retryable| Failure { message, retryable };
        if !output.calls.is_empty() {
            return Err(failure(compact::TOOL_USE_DENIED, false).into());
        }
        let text = output.message["content"].as_str().unwrap_or("");
        // 长度截断且没有文本：按上下文超限处理（Node isCompactEmptyLengthFinish）。
        if output.output_limit && crate::domain::js_string::trim(text).is_empty() {
            return Err(failure(compact::TOO_LONG, false).into());
        }
        let summary = compact::format_summary(text);
        if summary.is_empty() {
            return Err(failure(compact::EMPTY_SUMMARY, true).into());
        }
        Ok((summary, output.usage))
    }
}

/// A compaction failure the run survives (model failures and Node's own
/// compaction errors); infrastructure errors still end the run.
pub(super) fn compaction_failed(error: &anyhow::Error) -> bool {
    error.is::<ModelFailure>() || error.is::<Failure>()
}
