//! One compaction of a run's history (Node `compactActiveConversation`).
//! Spec rust-m7-compact. The engine commits every step through receipts.
use super::context::{RunContext, hidden_request};
use crate::contract::{Event, EventSink, ModelFailure, ModelPort, ToolPort};
use crate::domain::compact::{self, MAX_SUMMARY_OUTPUT, MAX_SUMMARY_TOOLS, Plan};
use crate::domain::compact_ptl;
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
    /// After a `context_exceeded` request; the tokens it was over, when known.
    Reactive(Option<u64>),
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
    /// Where the session's read state lives (re-attached files).
    pub port: &'a dyn ToolPort,
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

/// One summary request: the summary, or the provider said the prompt was too long.
enum Attempt {
    Done(String, Value),
    TooLong(Option<u64>),
}

/// Node `parsePromptTooLongTokenGap` over a model failure's provider message.
pub(super) fn overflow(failure: &ModelFailure) -> Option<u64> {
    failure
        .detail
        .as_ref()
        .and_then(|d| d.provider_error_message.as_deref())
        .and_then(compact_ptl::token_gap)
        .or_else(|| compact_ptl::token_gap(failure.message))
}

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

/// A compaction failure the run survives (model failures and Node's own
/// compaction errors); infrastructure errors still end the run.
pub(super) fn compaction_failed(error: &anyhow::Error) -> bool {
    error.is::<ModelFailure>() || error.is::<Failure>()
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
        let summary = self.state.summary.as_deref();
        let base = compact::plan(&self.messages, summary.is_some(), manual, 0);
        // 反应式压缩按原请求超出的 token 预先多保留近期组（Node initialPromptTooLongCause）。
        let first = match trigger {
            Trigger::Reactive(gap) => compact_ptl::initial(&self.messages, summary, gap).or(base),
            _ => base,
        };
        // 自动与反应式压缩不可压缩时不产生时间线标记（Node 决策 not_enough_messages）。
        if first.is_none() && !manual {
            return Ok(Outcome::Skipped);
        }
        let (done, receipt) = oneshot::channel();
        sink.send(Event::CompactStarted {
            id: id.clone(),
            manual,
            trigger: match trigger {
                Trigger::Manual(_) => "manual",
                Trigger::Auto => "auto",
                Trigger::Reactive(_) => "reactive",
            },
            instructions: instructions.is_some_and(|text| !text.trim().is_empty()),
            tokens: before,
            committed: done,
        })
        .await?;
        committed(receipt, cancel).await?;
        let Some(first) = first else {
            // 手动压缩无可压缩内容：健康的 noop（Node skipped）。
            let (done, receipt) = oneshot::channel();
            sink.send(Event::CompactDone {
                id,
                context: self.state.clone(),
                tokens: before,
                usage: Value::Null,
                body: String::new(),
                groups: 0,
                reminders: vec![],
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
        let (summary, usage, plan) = loop {
            // 外层重试从初始选轮重新开始（Node 自动压缩的 retrying）。
            match self
                .summarize_selection(
                    model,
                    sink,
                    cancel,
                    (first, manual),
                    (instructions, &request),
                )
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
        let split = plan.split;
        let preserved = compact_ptl::read_paths(&self.messages[split..]);
        let views = request.port.take_reads(&sink.session_id).await;
        // 与 Node 一致：压缩后的提醒带来源（计划文件引用、已读文件上下文），落库与冷读取据此还原。
        let tagged = |mut message: Value, source: &str| {
            message["_zcode_source"] = source.into();
            message
        };
        let mut reminders: Vec<Value> = request
            .reminder
            .into_iter()
            .map(|m| tagged(m, "plan_file_reference"))
            .collect();
        reminders.extend(
            compact_ptl::read_reminders(&views, &preserved)
                .iter()
                .map(|body| crate::domain::plan_mode::reminder_message(body))
                .map(|m| tagged(m, "resume_referenced_session_context")),
        );
        let next = ContextState {
            offset: self.state.offset + split,
            summary: Some(compact::summary_message(&summary)),
        };
        let kept = estimate(&with_summary(
            next.summary.as_deref(),
            &self.messages[split..],
        ));
        // Engine 追加提醒后以此值为准；本地副本先记不含提醒的估算，push 时再累加。
        let after = kept + estimate(&reminders);
        let (done, receipt) = oneshot::channel();
        sink.send(Event::CompactDone {
            id,
            context: next.clone(),
            tokens: after,
            usage,
            body: compact::format_summary(&summary),
            groups: plan.preserved,
            reminders: reminders.clone(),
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
        self.usage_anchor = None;
        self.estimated = kept;
        for reminder in reminders {
            self.push(reminder);
        }
        Ok(Outcome::Compacted)
    }

    /// Node's prompt-too-long loop: automatic compaction preserves more recent
    /// groups, manual compaction drops the oldest ones (at most 3 times).
    async fn summarize_selection(
        &self,
        model: &dyn ModelPort,
        sink: &EventSink,
        cancel: &CancellationToken,
        (mut plan, manual): (Plan, bool),
        prompt: (Option<&str>, &Request<'_>),
    ) -> Result<(String, Value, Plan)> {
        let summary = self.state.summary.as_deref();
        let mut truncated: Option<Vec<Value>> = None;
        let mut retries = 0;
        loop {
            let list = truncated
                .clone()
                .unwrap_or_else(|| with_summary(summary, &self.messages[..plan.split]));
            let gap = match self
                .summarize(model, sink, cancel, list.clone(), prompt)
                .await?
            {
                Attempt::Done(text, usage) => return Ok((text, usage, plan)),
                Attempt::TooLong(gap) => gap,
            };
            if !manual {
                if let Some(next) =
                    compact_ptl::reselect(&self.messages, summary, plan.preserved, gap)
                {
                    plan = next;
                    retries += 1;
                    continue;
                }
            } else if retries < compact_ptl::MAX_RETRIES
                && let Some(cut) = compact_ptl::truncate(&list, gap)
            {
                truncated = Some(cut);
                retries += 1;
                continue;
            }
            return Err(Failure {
                message: compact::TOO_LONG,
                retryable: false,
            }
            .into());
        }
    }

    /// Node's summary request: the step's prefix, the summarized history and
    /// the compact prompt, with the step's tools and a 20K output cap.
    async fn summarize(
        &self,
        model: &dyn ModelPort,
        sink: &EventSink,
        cancel: &CancellationToken,
        list: Vec<Value>,
        (instructions, request): (Option<&str>, &Request<'_>),
    ) -> Result<Attempt> {
        let cap = model.context_policy().max_output.min(MAX_SUMMARY_OUTPUT);
        let bound = model.with_max_output_tokens(cap)?;
        let model = bound.as_deref().unwrap_or(model);
        let mut messages = request.prefix.to_vec();
        messages.extend(list);
        messages.push(json!({"role": "user", "content": compact::prompt(instructions)}));
        let tools = if request.tools.len() > MAX_SUMMARY_TOOLS {
            &[][..]
        } else {
            request.tools
        };
        let output = match hidden_request(model, (messages, tools), sink, "compact", cancel).await {
            Ok(output) => output,
            Err(error) => {
                if let Some(failure) = error.downcast_ref::<ModelFailure>()
                    && failure.reason == "context_exceeded"
                {
                    return Ok(Attempt::TooLong(overflow(failure)));
                }
                return Err(error);
            }
        };
        let failure = |message, retryable| Failure { message, retryable };
        if !output.calls.is_empty() {
            return Err(failure(compact::TOOL_USE_DENIED, false).into());
        }
        let text = output.message["content"].as_str().unwrap_or("");
        // 长度截断且没有文本：按上下文超限处理（Node isCompactEmptyLengthFinish）。
        if output.output_limit && crate::domain::js_string::trim(text).is_empty() {
            return Ok(Attempt::TooLong(None));
        }
        let summary = compact::format_summary(text);
        if summary.is_empty() {
            return Err(failure(compact::EMPTY_SUMMARY, true).into());
        }
        Ok(Attempt::Done(summary, output.usage))
    }
}
