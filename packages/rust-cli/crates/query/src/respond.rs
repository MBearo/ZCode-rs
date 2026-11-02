//! 一次模型响应：流式请求、tool_use 块结束即开工、提交 assistant 消息与工具结果
//! （主循环里从发请求到进入下一轮的那一段）。

use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, Metered, ModelCall};
use rust_cli_kernel_api::model::{
    BlockDelta, ModelError, ModelEvent, ModelNotice, ModelRequest, ModelStreamItem,
};
use rust_cli_kernel_api::orchestration::{QueryError, QueryEvent};

use crate::RunScope;
use crate::assembler::{Assembled, Assembler};
use crate::defaults;
use crate::executor::{Ready, ToolBatch};
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
    /// 流内出错时只交出了 thinking（没有 text 与 tool_use），错误可以重发（docs/specs/rust-cli/provider-errors.md §3.3，
    /// 修复线 F113）。还没有提交：主循环按次数决定不提交、重发，还是照 `Truncated` 部分收尾。`assistant` 只含结束的
    /// thinking 块（可能为空）。
    ThinkingOnly {
        assistant: AssistantMessage,
        error: ModelError,
        call: ModelCall,
    },
    /// 工具都已执行完，结果已提交：发下一次请求。
    ToolsFinished,
    /// 工具都已执行完，结果已提交，本批有工具的 hook 要求停止：以 `hook_stopped` 结束（query.md §3，缺陷 D3）。
    HookStopped,
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

/// 提交一个交出的结果：tool_result 带这次调用的计量，同批提交（usage.md §2.3）。
async fn commit_ready(
    scope: &RunScope,
    history: &mut Vec<Message>,
    ready: Ready,
) -> Result<(), QueryError> {
    match ready.call {
        Some(call) => commit_metered(scope, history, ready.message, Metered::Tool(call)).await,
        None => commit(scope, history, ready.message).await,
    }
}

/// 流还开着时把已经能交出的结果取出来暂存（query.md §1.6）。中止之后不取：结果在流结束后按规则交出。
fn hand_out(scope: &RunScope, batch: &mut ToolBatch, handed: &mut Vec<Ready>) {
    if !scope.abort.is_aborted() {
        handed.extend(batch.take_ready());
    }
}

/// 一次请求的计时（usage.md §1）：请求前、第一个非空的文本或推理增量、流结束各读一次注入的时钟。
/// 选择与来源是请求时的快照（运行中切换模型不影响这一行）。
struct Timing {
    message: MessageId,
    source: rust_cli_kernel_api::model::QuerySource,
    selection: rust_cli_kernel_api::model::ModelSelection,
    started: u64,
    first_token: Option<u64>,
    /// 首个内容（footer 的 TPS，修复线 F105，tui-display.md §2.6）：只在内存，不写进用量表。
    first_content: Option<u64>,
    retries: u32,
}

/// 用量表那一列的首字（Node `firstModelTokenAt`）：第一个非空的文本或推理增量。
fn token_starts(event: &ModelEvent) -> bool {
    matches!(
        event,
        ModelEvent::BlockDelta {
            delta: BlockDelta::Text(text) | BlockDelta::Thinking(text),
            ..
        } if !text.is_empty()
    )
}

/// 首个内容：首字之外，工具参数的增量与 tool_use 块的开始也算（修复线 F105，用户决定 2026-10-27）。以前只认首字，只有
/// 工具调用的响应没有 TPS。
pub(crate) fn content_starts(event: &ModelEvent) -> bool {
    token_starts(event)
        || matches!(
            event,
            ModelEvent::BlockDelta {
                delta: BlockDelta::InputJson(_),
                ..
            } | ModelEvent::BlockStart {
                block: ContentBlock::ToolUse { .. },
                ..
            }
        )
}

impl Timing {
    /// 记下首字与首个内容；两者落在同一个事件上时只读一次时钟。
    fn observe(&mut self, event: &ModelEvent, clock: &dyn rust_cli_kernel_api::clock::Clock) {
        let token = self.first_token.is_none() && token_starts(event);
        let content = self.first_content.is_none() && content_starts(event);
        if token || content {
            let now = clock.now_ms();
            if token {
                self.first_token = Some(now);
            }
            if content {
                self.first_content = Some(now);
            }
        }
    }

    fn call(
        &self,
        completed_at_ms: u64,
        status: CallStatus,
        assembled: (Option<StopReason>, u32, Usage),
        error: Option<ModelError>,
    ) -> ModelCall {
        let (stop_reason, tool_calls, usage) = assembled;
        ModelCall {
            first_token_at_ms: self.first_token,
            first_content_at_ms: self.first_content,
            stop_reason,
            tool_calls,
            // 失败与中断时只有已经报告的用量（Node 用失败事件里的用量）；全为 0 时不写。
            usage: Some(usage).filter(|usage| *usage != Usage::default()),
            retries: self.retries,
            error,
            ..ModelCall::new(
                self.message.clone(),
                self.source.clone(),
                self.selection.clone(),
                self.started,
                completed_at_ms,
                status,
            )
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

/// 流式阶段的结果：组装器、已开工的调用、流还开着时已交出待提交的结果、流内的错误与计时。
struct Streamed {
    assembler: Assembler,
    batch: ToolBatch,
    handed: Vec<Ready>,
    failure: Option<ModelError>,
    timing: Timing,
    deltas: u64,
}

/// 流结束后这次响应怎样收尾。
enum Ending {
    /// 交给主循环决定（没有 tool_use、流内失败只交出了 thinking、半截没有 tool_use）。
    Decide(Responded),
    /// 一个块都没结束就失败：计量单独提交，等已开工的调用结束，以模型错误结束。
    Failed { call: ModelCall, error: ModelError },
    /// 有 tool_use 或已中止：提交 assistant，再按交出规则提交工具结果。
    Commit {
        assistant: AssistantMessage,
        call: ModelCall,
    },
}

pub(crate) async fn respond(
    scope: &Arc<RunScope>,
    history: &mut Vec<Message>,
    request: ModelRequest,
    response: usize,
) -> Result<Responded, QueryError> {
    let assistant_id = scope.parts.ids.message();
    let mut streamed = stream(scope, request, response, &assistant_id).await;
    let aborted_streaming = scope.abort.is_aborted();
    // 流读完却没有正常收尾（没有 `message_stop`、有没结束的块）：与流中途出错同样处理（R5-1，原来用 `?` 提前
    // 返回，丢下已经开工的工具）。
    if streamed.failure.is_none() && !aborted_streaming {
        streamed.failure = streamed.assembler.incomplete();
    }
    // 流结束：读一次时钟，日志与用量行都用它（usage.md §1）。
    let completed = scope.parts.clock.now_ms();
    log_end(scope, &streamed, completed, aborted_streaming);
    let Streamed {
        assembler,
        mut batch,
        handed,
        failure,
        timing,
        ..
    } = streamed;
    let ending = finish(
        scope,
        (assistant_id, assembler, timing),
        failure,
        (completed, aborted_streaming),
    );
    match ending {
        Ending::Decide(responded) => Ok(responded),
        Ending::Failed { call, error } => {
            // 没有内容的失败也是一次请求：计量单独提交（usage.md §2.2）；已经开工的调用先等它们结束。
            scope
                .sink
                .commit_metered(None, Metered::Model(call))
                .await?;
            while batch.next_completion().await {}
            Err(error.into())
        }
        Ending::Commit { assistant, call } => {
            commit_batch(scope, history, (assistant, call), handed, &mut batch).await?;
            if aborted_streaming {
                return interrupted(scope, history, false).await;
            }
            if scope.abort.is_aborted() {
                return interrupted(scope, history, true).await;
            }
            // 先看中止、再看停止请求（query.md §3）。
            if batch.stop_requested() {
                return Ok(Responded::HookStopped);
            }
            Ok(Responded::ToolsFinished)
        }
    }
}

/// 发请求、读流：增量与通知转给界面，tool_use 块一结束就开工，流还开着时已经能交出的结果先暂存。
async fn stream(
    scope: &Arc<RunScope>,
    request: ModelRequest,
    response: usize,
    assistant_id: &MessageId,
) -> Streamed {
    let mut timing = Timing {
        message: assistant_id.clone(),
        source: request.source.clone(),
        selection: request.selection.clone(),
        started: scope.parts.clock.now_ms(),
        first_token: None,
        first_content: None,
        retries: 0,
    };
    // 运行日志（logging.md §4）：请求的开始与结束、失败。计时与重试数和用量行同一份（`Timing`，usage.md §1），
    // 不另外计时。
    tracing::info!(
        event = "model.request.start",
        session = %scope.session,
        agent = scope.agent.as_ref().map(rust_cli_kernel_api::ids::AgentId::as_str),
        source = ?request.source,
        model = %request.selection.model,
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
    // 修 bug（工作线 F81 R1）：以前流式中只记下工具完成，流结束、assistant 提交之后才开始交出，两个并发安全的调用都在
    // 流结束前完成时按加入顺序给出。现在一完成就按交出规则取出、先暂存（assistant 还没落库），提交 assistant 之后
    // 按暂存的顺序提交。
    let mut handed: Vec<Ready> = Vec::new();
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
                    on_notice(scope, &mut timing, notice).await;
                }
                Some(Ok(ModelStreamItem::Event(event))) => {
                    // 首字（Node `firstModelTokenAt`）与首个内容（F105）。
                    timing.observe(&event, scope.parts.clock.as_ref());
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
                                // 找不到的工具加入时就有结果。
                                hand_out(scope, &mut batch, &mut handed);
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
                hand_out(scope, &mut batch, &mut handed);
            }
        }
    }
    Streamed {
        assembler,
        batch,
        handed,
        failure,
        timing,
        deltas,
    }
}

/// 模型端口的通知：重试算进这次调用并记日志；同一份通知交给界面。
async fn on_notice(scope: &RunScope, timing: &mut Timing, notice: ModelNotice) {
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
            model = %timing.selection.model,
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

/// 流结束的日志：增量数、停顿、请求的结束或失败。
fn log_end(scope: &RunScope, streamed: &Streamed, completed: u64, aborted_streaming: bool) {
    let model = &streamed.timing.selection.model;
    let duration_ms = completed.saturating_sub(streamed.timing.started);
    let retries = streamed.timing.retries;
    let tools = streamed.batch.len();
    tracing::debug!(event = "model.stream", session = %scope.session, deltas = streamed.deltas, "model stream events");
    if let Some(ModelError::StreamIdle { idle_ms }) = &streamed.failure {
        // 看门狗中止且不再重发（静默重发记的是 `model.retry`，logging.md §4）。
        tracing::warn!(
            event = "model.stream.idle",
            session = %scope.session,
            model = %model,
            idle_ms,
            "model stream idle; aborted"
        );
    }
    match &streamed.failure {
        Some(error) => tracing::info!(
            event = "model.request.failed",
            session = %scope.session,
            model = %model,
            duration_ms,
            tools,
            retries,
            error = %error,
            "model request failed"
        ),
        None => tracing::info!(
            event = "model.request.end",
            session = %scope.session,
            model = %model,
            duration_ms,
            tools,
            retries,
            aborted = aborted_streaming,
            "model request finished"
        ),
    }
}

/// 判断这次响应怎样收尾：组装 assistant 消息与这次调用的计量（`completed` 是流结束的时刻）。
fn finish(
    scope: &RunScope,
    (assistant_id, assembler, timing): (MessageId, Assembler, Timing),
    failure: Option<ModelError>,
    (completed, aborted_streaming): (u64, bool),
) -> Ending {
    // 修 bug（修复线 F38，query.md §1.4）：以前被中断时没写完的块一律丢掉，已经流出的半句不进历史——下一条请求里
    // 没有它，esc 撤回也把“已经流出文字”判成没有回答。主线程的 query 中止时保留部分消息，
    // 留下没写完的文字块（`trim` 后不为空）；子代理不保留，退出（`Shutdown`）时不中止、不走这一步。
    let keep_partial = aborted_streaming
        && scope.agent.is_none()
        && scope.abort.reason() != Some(AbortReason::Shutdown);
    let (assembled, error_of_call) = match failure {
        None if keep_partial => (assembler.finish_aborted(), None),
        None => (assembler.finish(), None),
        Some(error) => match partial(&assistant_id, assembler, error, (&timing, completed)) {
            Partial::WithTools(assembled, error) => (assembled, Some(error)),
            Partial::Ended(ending) => return *ending,
        },
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
        return Ending::Decide(Responded::Done(assistant, call));
    }
    Ending::Commit { assistant, call }
}

/// 流在中途出错时的部分收尾。
enum Partial {
    /// 结束的块里有 tool_use：照常提交、执行，这次调用记为失败。
    WithTools(Assembled, ModelError),
    /// 直接收尾（只交出了 thinking、一个块都没有、半截没有 tool_use）；少见的路径，装箱让两个变体一样大。
    Ended(Box<Ending>),
}

/// 流在中途出错（部分收尾）：只留结束的块。
fn partial(
    assistant_id: &MessageId,
    assembler: Assembler,
    error: ModelError,
    (timing, completed): (&Timing, u64),
) -> Partial {
    let failed_call = |usage| {
        timing.call(
            completed,
            CallStatus::Error,
            (None, 0, usage),
            Some(error.clone()),
        )
    };
    // 只交出了 thinking：交给主循环决定重发（F113，provider-errors.md §3.3）。以前同下面的部分收尾，只结束了
    // thinking 时提交它、以错误结束，thinking 没结束时直接以模型错误结束。
    if assembler.thinking_only() && crate::recovery::resends_after_thinking(&error) {
        let partial = assembler.finish();
        let call = failed_call(partial.usage);
        let assistant = AssistantMessage {
            id: assistant_id.clone(),
            content: partial.content,
            model: partial.model,
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(partial.usage),
        };
        return Partial::Ended(Box::new(Ending::Decide(Responded::ThinkingOnly {
            assistant,
            error,
            call,
        })));
    }
    // 一个块都没有时照旧报错，已经开工的调用先等它们结束。
    let mut partial = assembler.finish();
    if partial.content.is_empty() {
        let call = failed_call(partial.usage);
        return Partial::Ended(Box::new(Ending::Failed { call, error }));
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
    if tools {
        return Partial::WithTools(partial, error);
    }
    let call = failed_call(partial.usage);
    let assistant = AssistantMessage {
        id: assistant_id.clone(),
        content: partial.content,
        model: partial.model,
        stop_reason: partial.stop_reason,
        usage: Some(partial.usage),
    };
    Partial::Ended(Box::new(Ending::Decide(Responded::Truncated {
        assistant,
        error,
        call,
    })))
}

/// 提交 assistant（没有内容时只提交计量）与这批工具的结果：先是流还开着时暂存的，再按交出规则等其余的。
async fn commit_batch(
    scope: &RunScope,
    history: &mut Vec<Message>,
    (assistant, call): (AssistantMessage, ModelCall),
    handed: Vec<Ready>,
    batch: &mut ToolBatch,
) -> Result<(), QueryError> {
    if assistant.content.is_empty() {
        scope
            .sink
            .commit_metered(None, Metered::Model(call))
            .await?;
    } else {
        let message = Message::Assistant(assistant);
        commit_metered(scope, history, message, Metered::Model(call)).await?;
    }
    for ready in handed {
        commit_ready(scope, history, ready).await?;
    }
    loop {
        for ready in batch.take_ready() {
            commit_ready(scope, history, ready).await?;
        }
        if !batch.has_unfinished() || !batch.next_completion().await {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "respond_metering_tests.rs"]
mod metering_tests;

#[cfg(test)]
#[path = "respond_bad_input_tests.rs"]
mod bad_input_tests;

#[cfg(test)]
#[path = "respond_order_tests.rs"]
mod order_tests;

#[cfg(test)]
#[path = "respond_server_tests.rs"]
mod server_tests;
