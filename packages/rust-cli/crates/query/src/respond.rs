//! 一次模型响应：流式请求、tool_use 块结束即开工、提交 assistant 消息与工具结果
//! （主循环里从发请求到进入下一轮的那一段）。

use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, Metered, ModelCall};
use rust_cli_kernel_api::model::{
    BlockDelta, ModelError, ModelEvent, ModelNotice, ModelRequest, ModelStreamItem,
};
use rust_cli_kernel_api::orchestration::{QueryError, QueryEvent};

use crate::RunScope;
use crate::assembler::Assembler;
use crate::defaults;
use crate::executor::ToolBatch;
use crate::texts;

pub(crate) enum Responded {
    /// 没有 tool_use。响应还没有提交（内容可能为空）：由主循环按恢复分支决定提交与否，计量与它同批
    /// （usage.md §2.5）。
    Done(AssistantMessage, ModelCall),
    /// 流在已有结束的块、没有 tool_use 时出错。只含结束的块的半截响应还没有提交（docs/specs/rust-cli/
    /// query.md §3 ④）。
    Truncated {
        assistant: AssistantMessage,
        error: ModelError,
        call: ModelCall,
    },
    /// 工具都已执行完，结果已提交：发下一次请求。
    ToolsFinished,
    Aborted,
}

/// 提交一条消息并放进这次 query 的历史。
pub(crate) async fn commit(
    scope: &RunScope,
    history: &mut Vec<Message>,
    message: Message,
) -> Result<(), QueryError> {
    scope.sink.commit_message(message.clone()).await?;
    history.push(message);
    Ok(())
}

/// 同 [`commit`]，计量与消息在同一个存储事务里（usage.md §2.5）。
pub(crate) async fn commit_metered(
    scope: &RunScope,
    history: &mut Vec<Message>,
    message: Message,
    metered: Metered,
) -> Result<(), QueryError> {
    scope
        .sink
        .commit_metered(Some(message.clone()), metered)
        .await?;
    history.push(message);
    Ok(())
}

/// 一次请求的计时（usage.md §1）：请求前、第一个非空的文本或推理增量、流结束各读一次注入的时钟。
/// 选择与来源是请求时的快照（运行中切换模型不影响这一行）。
struct Timing {
    message: rust_cli_kernel_api::ids::MessageId,
    source: rust_cli_kernel_api::model::QuerySource,
    selection: rust_cli_kernel_api::model::ModelSelection,
    started: u64,
    first_token: Option<u64>,
    retries: u32,
}

impl Timing {
    fn call(
        self,
        completed_at_ms: u64,
        status: CallStatus,
        assembled: (Option<StopReason>, u32, Usage),
        error: Option<ModelError>,
    ) -> ModelCall {
        let (stop_reason, tool_calls, usage) = assembled;
        ModelCall {
            message: self.message,
            source: self.source,
            selection: self.selection,
            started_at_ms: self.started,
            first_token_at_ms: self.first_token,
            completed_at_ms,
            status,
            stop_reason,
            tool_calls,
            // 失败与中断时只有已经报告的用量（Node 用失败事件里的用量）；全为 0 时不写。
            usage: Some(usage).filter(|usage| *usage != Usage::default()),
            retries: self.retries,
            error,
        }
    }
}

/// 中断消息（运行中提交新输入引起的中断不追加，后面的新输入本身就是上下文）。主循环在发请求之前看到中止时
/// 也写它（query.md §1.2）。退出（`Shutdown`，修复线 F38）不写：收到退出信号不中止 query，没有这条标记。
pub(crate) async fn mark_interrupted(
    scope: &RunScope,
    history: &mut Vec<Message>,
    tool_use: bool,
) -> Result<(), QueryError> {
    if !matches!(
        scope.abort.reason(),
        Some(AbortReason::Submit | AbortReason::Shutdown)
    ) {
        let text = if tool_use {
            texts::INTERRUPT_MESSAGE_FOR_TOOL_USE
        } else {
            texts::INTERRUPT_MESSAGE
        };
        let message = Message::User(UserMessage {
            id: scope.parts.ids.message(),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            origin: UserOrigin::Human,
        });
        commit(scope, history, message).await?;
    }
    Ok(())
}

async fn interrupted(
    scope: &RunScope,
    history: &mut Vec<Message>,
    tool_use: bool,
) -> Result<Responded, QueryError> {
    mark_interrupted(scope, history, tool_use).await?;
    Ok(Responded::Aborted)
}

pub(crate) async fn respond(
    scope: &Arc<RunScope>,
    history: &mut Vec<Message>,
    request: ModelRequest,
    response: usize,
) -> Result<Responded, QueryError> {
    let assistant_id = scope.parts.ids.message();
    let mut timing = Timing {
        message: assistant_id.clone(),
        source: request.source.clone(),
        selection: request.selection.clone(),
        started: scope.parts.clock.now_ms(),
        first_token: None,
        retries: 0,
    };
    // 运行日志（logging.md §4）：请求的开始与结束、失败。计时与重试数和用量行同一份（`Timing`，usage.md §1），
    // 不另外计时。
    let model = request.selection.model.clone();
    tracing::info!(
        event = "model.request.start",
        session = %scope.session,
        agent = scope.agent.as_ref().map(rust_cli_kernel_api::ids::AgentId::as_str),
        source = ?request.source,
        model = %model,
        response,
        "model request started"
    );
    let mut deltas: u64 = 0;
    // 带通知的流：重试记日志（带会话，logging.md §4），通知转给界面，事件照旧处理。
    let mut stream = scope
        .parts
        .model
        .stream_with_notices(request, scope.abort.token().clone());
    let mut assembler = Assembler::default();
    let mut batch = ToolBatch::new(scope.clone());
    let mut failure = None;
    loop {
        tokio::select! {
            item = stream.next() => match item {
                None | Some(Err(ModelError::Cancelled)) => break,
                Some(Err(error)) => {
                    failure = Some(error);
                    break;
                }
                Some(Ok(ModelStreamItem::Notice(notice))) => {
                    if let ModelNotice::Retrying {
                        attempt,
                        max_retries,
                        delay_ms,
                        error,
                    } = &notice
                    {
                        // 适配器内部的重试算在这次调用里（Node `retry_count`），同时记日志（带会话，logging.md §4）。
                        timing.retries += 1;
                        tracing::warn!(
                            event = "model.retry",
                            session = %scope.session,
                            model = %model,
                            attempt,
                            max_retries,
                            delay_ms,
                            error = %error,
                            "model request failed; retrying"
                        );
                    }
                    // 同一份通知交给界面：主线程转成内核事件，子代理转给父工具调用与子代理列表
                    // （docs/specs/rust-cli/stall-visibility.md §1）。
                    let _ = scope.sink.emit(QueryEvent::ModelNotice { notice }).await;
                }
                Some(Ok(ModelStreamItem::Event(event))) => {
                    // Node `firstModelTokenAt`：第一个非空的文本或推理增量。
                    if timing.first_token.is_none()
                        && let ModelEvent::BlockDelta {
                            delta: BlockDelta::Text(text) | BlockDelta::Thinking(text),
                            ..
                        } = &event
                        && !text.is_empty()
                    {
                        timing.first_token = Some(scope.parts.clock.now_ms());
                    }
                    if let ModelEvent::BlockDelta { index, delta } = &event {
                        deltas += 1;
                        let delta = QueryEvent::Delta {
                            message: response,
                            index: *index,
                            delta: delta.clone(),
                        };
                        let _ = scope.sink.emit(delta).await;
                    }
                    // 缺陷修复（R5-1）：原来 `assembler.push(event)?` 在协议错误时提前返回，已经开工的工具被丢下，
                    // assistant 消息、结果与计量都不落库。错误改为结束读流，走下面流中途出错的路径（等工具、提交）。
                    match assembler.push(event) {
                        Ok(Some(mut block)) => {
                            defaults::apply(scope.parts.tools.as_ref(), &mut block);
                            if let ContentBlock::ToolUse { id, name, input } = block {
                                batch.add(id, name, input, assistant_id.clone());
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
            },
            Some((index, output)) = batch.next_done(), if batch.has_running() => {
                batch.complete(index, output).await;
            }
        }
    }
    let aborted_streaming = scope.abort.is_aborted();
    // 流读完却没有正常收尾（没有 `message_stop`、有没结束的块）：与流中途出错同样处理（R5-1，原来用 `?` 提前
    // 返回，丢下已经开工的工具）。
    if failure.is_none() && !aborted_streaming {
        failure = assembler.incomplete();
    }
    // 流结束：读一次时钟，日志与用量行都用它（usage.md §1）。
    let completed = scope.parts.clock.now_ms();
    let duration_ms = completed.saturating_sub(timing.started);
    let retries = timing.retries;
    tracing::debug!(event = "model.stream", session = %scope.session, deltas, "model stream events");
    if let Some(ModelError::StreamIdle { idle_ms }) = &failure {
        // 看门狗中止且不再重发（静默重发记的是 `model.retry`，logging.md §4）。
        tracing::warn!(
            event = "model.stream.idle",
            session = %scope.session,
            model = %model,
            idle_ms,
            "model stream idle; aborted"
        );
    }
    match &failure {
        Some(error) => tracing::info!(
            event = "model.request.failed",
            session = %scope.session,
            model = %model,
            duration_ms,
            tools = batch.len(),
            retries,
            error = %error,
            "model request failed"
        ),
        None => tracing::info!(
            event = "model.request.end",
            session = %scope.session,
            model = %model,
            duration_ms,
            tools = batch.len(),
            retries,
            aborted = aborted_streaming,
            "model request finished"
        ),
    }
    let mut error_of_call = None;
    // 修 bug（修复线 F38，query.md §1.4）：以前被中断时没写完的块一律丢掉，已经流出的半句不进历史——下一条请求里
    // 没有它，esc 撤回也把“已经流出文字”判成没有回答。主线程的 query 中止时保留部分消息，
    // 留下没写完的文字块（`trim` 后不为空）；子代理不保留，退出（`Shutdown`）时不中止、不走这一步。
    let keep_partial = aborted_streaming
        && scope.agent.is_none()
        && scope.abort.reason() != Some(AbortReason::Shutdown);
    let assembled = match failure {
        None if keep_partial => assembler.finish_aborted(),
        None => assembler.finish(),
        Some(error) => {
            // 流在中途出错（部分收尾）：只留结束的块。一个都没有时照旧报错，已经开工的
            // 调用先等它们结束。
            let mut partial = assembler.finish();
            if partial.content.is_empty() {
                // 没有内容的失败也是一次请求：计量单独提交（usage.md §2.2）。
                let call = timing.call(
                    completed,
                    CallStatus::Error,
                    (None, 0, partial.usage),
                    Some(error.clone()),
                );
                scope
                    .sink
                    .commit_metered(None, Metered::Model(call))
                    .await?;
                while batch.next_completion().await {}
                return Err(error.into());
            }
            let tools = partial
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));
            partial.stop_reason = Some(if tools {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            });
            if !tools {
                let call = timing.call(
                    completed,
                    CallStatus::Error,
                    (None, 0, partial.usage),
                    Some(error.clone()),
                );
                let assistant = AssistantMessage {
                    id: assistant_id,
                    content: partial.content,
                    model: partial.model,
                    stop_reason: partial.stop_reason,
                    usage: Some(partial.usage),
                };
                return Ok(Responded::Truncated {
                    assistant,
                    error,
                    call,
                });
            }
            error_of_call = Some(error);
            partial
        }
    };
    let tool_uses = assembled
        .content
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
        .count();
    let status = if error_of_call.is_some() {
        CallStatus::Error
    } else if aborted_streaming {
        CallStatus::Cancelled
    } else {
        CallStatus::Completed
    };
    let call = timing.call(
        completed,
        status,
        (
            assembled.stop_reason,
            u32::try_from(tool_uses).unwrap_or(u32::MAX),
            assembled.usage,
        ),
        error_of_call,
    );
    let assistant = AssistantMessage {
        id: assistant_id,
        // 回放用的输入与执行用的一致：整理工具输入（defaults.rs）。
        content: assembled
            .content
            .into_iter()
            .map(|mut block| {
                defaults::apply(scope.parts.tools.as_ref(), &mut block);
                block
            })
            .collect(),
        model: assembled.model,
        stop_reason: assembled.stop_reason,
        usage: Some(assembled.usage),
    };
    if tool_uses == 0 && !aborted_streaming && !scope.abort.is_aborted() {
        return Ok(Responded::Done(assistant, call));
    }
    if assistant.content.is_empty() {
        scope
            .sink
            .commit_metered(None, Metered::Model(call))
            .await?;
    } else {
        let message = Message::Assistant(assistant);
        commit_metered(scope, history, message, Metered::Model(call)).await?;
    }
    loop {
        for ready in batch.take_ready() {
            match ready.call {
                Some(call) => {
                    commit_metered(scope, history, ready.message, Metered::Tool(call)).await?;
                }
                None => commit(scope, history, ready.message).await?,
            }
        }
        if !batch.has_unfinished() || !batch.next_completion().await {
            break;
        }
    }
    if aborted_streaming {
        return interrupted(scope, history, false).await;
    }
    if scope.abort.is_aborted() {
        return interrupted(scope, history, true).await;
    }
    Ok(Responded::ToolsFinished)
}

#[cfg(test)]
#[path = "respond_metering_tests.rs"]
mod metering_tests;

#[cfg(test)]
#[path = "respond_bad_input_tests.rs"]
mod bad_input_tests;
