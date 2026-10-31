//! 流式工具执行器。
//!
//! - tool_use 块一结束就加入；并发安全的调用可与其他并发安全的调用同时执行，其余单独执行；
//! - 结果按加入顺序交付：并发安全的调用之间按完成先后，但不越过一个仍在执行的不安全调用；
//! - Bash 出错不取消其余调用（执行器没有 sibling error，R4 F3）；
//! - 中断：用户中断时全部换成拒绝结果；提交新输入引起的中断只取消 `InterruptBehavior::Cancel` 的工具；
//! - 每次执行前先经 sink 登记操作意图（spec §6 规则 3）。

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use futures_util::future::{BoxFuture, FutureExt, join};
use futures_util::stream::{FuturesUnordered, StreamExt};
use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::checkpoint::DISPLAY_KEY;
use rust_cli_kernel_api::env::{OperationContext, Origin};
use rust_cli_kernel_api::ids::{MessageId, OwnerEpoch, ToolUseId};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, ToolCall};
use rust_cli_kernel_api::orchestration::{QueryEvent, ToolExecution};
use rust_cli_kernel_api::tool::{InterruptBehavior, Tool, ToolOutput, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::RunScope;
use crate::mcp_wait;
use crate::texts;

const PROGRESS: usize = 32;

enum Status {
    Queued,
    Executing,
    /// tool_result 与执行管线排在它前后的消息（hook 的附加上下文、Skill 的正文，
    /// docs/specs/rust-cli/ext.md §2.4），按提交顺序。
    Completed(Vec<Ready>),
    Yielded,
}

/// 可以交付的一条消息；tool_result 带这次调用的计量，与它同批提交（docs/specs/rust-cli/usage.md §2.3）。
pub(crate) struct Ready {
    pub(crate) message: Message,
    pub(crate) call: Option<ToolCall>,
}

impl Ready {
    fn plain(message: Message) -> Self {
        Self {
            message,
            call: None,
        }
    }
}

/// 结果里文字的 UTF-8 字节数（Node 没有序列化信息时 `outputSizeBytes` 的算法）。
fn output_bytes(output: &ToolOutput) -> u64 {
    output
        .content
        .iter()
        .map(|content| match content {
            ToolResultContent::Text { text } => text.len() as u64,
            _ => 0,
        })
        .sum()
}

struct Tracked {
    id: ToolUseId,
    name: String,
    input: Value,
    tool: Option<Arc<dyn Tool>>,
    safe: bool,
    assistant: MessageId,
    status: Status,
    /// 执行器开工的时刻（计量，usage.md §2.3）；没有开工就有结果时为 None。
    started_at: Option<u64>,
    /// 找不到、但它的 MCP 服务器在首次连接：执行时先等工具出现（ext.md §3.8，修复线 F36）。
    waits_for_mcp: bool,
}

pub(crate) struct ToolBatch {
    scope: Arc<RunScope>,
    tools: Vec<Tracked>,
    running: FuturesUnordered<BoxFuture<'static, (usize, ToolExecution)>>,
    /// 这次模型响应里等连接中服务器已用的时间（按响应记的预算，ext.md §3.8）。
    mcp_waited: Arc<AtomicU64>,
}

fn text_output(text: String) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error: true,
        display: Value::Null,
    }
}

/// 工具结束后才被中止时给模型的结果：中断文本。工具已经写完的文件仍要能回退（写入
/// 前已把文件记进 file history），所以带上原输出的 checkpoint，只带它（session-lifecycle.md §4.2）。
/// 以前整个输出被换掉，这次写入的 checkpoint 随之丢失。
fn interrupted(text: String, real: &ToolOutput) -> ToolOutput {
    let mut output = text_output(text);
    let checkpoint = &real.display[DISPLAY_KEY];
    if !checkpoint.is_null() {
        output.display = json!({ DISPLAY_KEY: checkpoint });
    }
    output
}

impl ToolBatch {
    pub(crate) fn new(scope: Arc<RunScope>) -> Self {
        Self {
            scope,
            tools: Vec::new(),
            running: FuturesUnordered::new(),
            mcp_waited: Arc::default(),
        }
    }

    pub(crate) fn add(&mut self, id: ToolUseId, name: String, input: Value, assistant: MessageId) {
        let tool = self.scope.parts.tools.find(&name);
        // 找不到、它的服务器在首次连接时排成不可并发的调用，执行时再等。
        let waits_for_mcp = tool.is_none() && mcp_wait::connecting(&self.scope, &name);
        let (safe, status) = match &tool {
            Some(tool) => (tool.is_concurrency_safe(&input), Status::Queued),
            None if waits_for_mcp => (false, Status::Queued),
            None => {
                let output = text_output(texts::no_such_tool(&name));
                let call = self.tool_call(&id, &name, None, &output, CallStatus::Error);
                let message = self.result(&id, &assistant, output);
                (
                    true,
                    Status::Completed(vec![Ready {
                        message,
                        call: Some(call),
                    }]),
                )
            }
        };
        self.tools.push(Tracked {
            id,
            name,
            input,
            tool,
            safe,
            assistant,
            status,
            started_at: None,
            waits_for_mcp,
        });
        self.process_queue();
    }

    /// 一次调用的计量：开工时刻没有时等于结束时刻。
    fn tool_call(
        &self,
        id: &ToolUseId,
        name: &str,
        started_at: Option<u64>,
        output: &ToolOutput,
        status: CallStatus,
    ) -> ToolCall {
        let completed_at = self.scope.parts.clock.now_ms();
        ToolCall {
            tool_use: id.clone(),
            name: name.to_owned(),
            started_at_ms: started_at.unwrap_or(completed_at),
            completed_at_ms: completed_at,
            status,
            output_bytes: output_bytes(output),
        }
    }

    fn can_execute(&self, safe: bool) -> bool {
        let executing: Vec<_> = self
            .tools
            .iter()
            .filter(|tool| matches!(tool.status, Status::Executing))
            .collect();
        executing.is_empty() || (safe && executing.iter().all(|tool| tool.safe))
    }

    fn process_queue(&mut self) {
        for index in 0..self.tools.len() {
            if !matches!(self.tools[index].status, Status::Queued) {
                continue;
            }
            if self.can_execute(self.tools[index].safe) {
                self.start(index);
            } else if !self.tools[index].safe {
                break;
            }
        }
    }

    /// 这个调用现在应当被取消的原因；没有时为 None。
    fn abort_text(&self, tool: &Tracked) -> Option<String> {
        match self.scope.abort.reason() {
            // 退出（修复线 F38）同用户中断：全部取消，进程树经取消路径回收。
            Some(AbortReason::User | AbortReason::Shutdown) => {
                Some(texts::REJECT_MESSAGE.to_owned())
            }
            Some(AbortReason::Submit) => {
                let cancels = tool
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.interrupt_behavior() == InterruptBehavior::Cancel);
                cancels.then(|| texts::REJECT_MESSAGE.to_owned())
            }
            None => None,
        }
    }

    fn start(&mut self, index: usize) {
        if let Some(text) = self.abort_text(&self.tools[index]) {
            let tracked = &self.tools[index];
            let output = text_output(text);
            // 被换成拒绝结果的调用都来自用户中断（含提交新输入）：Cancelled。
            let call = self.tool_call(
                &tracked.id,
                &tracked.name,
                None,
                &output,
                CallStatus::Cancelled,
            );
            let message = self.result(&tracked.id, &tracked.assistant, output);
            self.tools[index].status = Status::Completed(vec![Ready {
                message,
                call: Some(call),
            }]);
            return;
        }
        let started_at = self.scope.parts.clock.now_ms();
        let tracked = &mut self.tools[index];
        tracked.status = Status::Executing;
        tracked.started_at = Some(started_at);
        let scope = self.scope.clone();
        let cancel = self.scope.abort.token().child_token();
        let (id, name, input) = (
            tracked.id.clone(),
            tracked.name.clone(),
            tracked.input.clone(),
        );
        let run = match (tracked.tool.clone(), tracked.waits_for_mcp) {
            (Some(tool), _) => execute(scope, tool, id, name, input, cancel).boxed(),
            (None, true) => {
                let waited = self.mcp_waited.clone();
                execute_when_connected(scope, id, name, input, cancel, waited).boxed()
            }
            (None, false) => return,
        };
        self.running
            .push(run.map(move |output| (index, output)).boxed());
    }

    /// 这次响应里加入的调用数（日志用）。
    pub(crate) fn len(&self) -> usize {
        self.tools.len()
    }

    pub(crate) fn has_running(&self) -> bool {
        !self.running.is_empty()
    }

    pub(crate) fn has_unfinished(&self) -> bool {
        self.tools
            .iter()
            .any(|tool| !matches!(tool.status, Status::Yielded))
    }

    /// 下一个结束的调用。可以安全地放进 `select!`：没被选中时不会丢结果。
    pub(crate) async fn next_done(&mut self) -> Option<(usize, ToolExecution)> {
        self.running.next().await
    }

    /// 等下一个执行中的调用结束并处理它；没有执行中的调用时立即返回 false。
    pub(crate) async fn next_completion(&mut self) -> bool {
        let Some((index, output)) = self.next_done().await else {
            return false;
        };
        self.complete(index, output).await;
        true
    }

    /// 处理一个结束的调用（[`Self::next_done`] 的结果）。
    pub(crate) async fn complete(&mut self, index: usize, execution: ToolExecution) {
        let ToolExecution {
            output,
            before,
            after,
            ..
        } = execution;
        // 被中止的调用只交付拒绝结果，不带执行管线的其他消息；写入的 checkpoint 保留。
        let (output, before, after, status) = match self.abort_text(&self.tools[index]) {
            Some(text) => (
                interrupted(text, &output),
                Vec::new(),
                Vec::new(),
                CallStatus::Cancelled,
            ),
            None => {
                let status = if output.is_error {
                    CallStatus::Error
                } else {
                    CallStatus::Completed
                };
                (output, before, after, status)
            }
        };
        let tracked = &self.tools[index];
        let call = self.tool_call(
            &tracked.id,
            &tracked.name,
            tracked.started_at,
            &output,
            status,
        );
        log_finished(&self.scope, tracked, &output, &call);
        let _ = self
            .scope
            .sink
            .emit(QueryEvent::ToolFinished {
                tool_use: tracked.id.clone(),
                output: output.clone(),
            })
            .await;
        let message = self.result(&tracked.id, &tracked.assistant, output);
        let mut messages: Vec<Ready> = before.into_iter().map(Ready::plain).collect();
        messages.push(Ready {
            message,
            call: Some(call),
        });
        messages.extend(after.into_iter().map(Ready::plain));
        self.tools[index].status = Status::Completed(messages);
        self.process_queue();
    }

    /// 可以交付的结果（按规则保持顺序）。
    pub(crate) fn take_ready(&mut self) -> Vec<Ready> {
        let mut ready = Vec::new();
        for tool in &mut self.tools {
            match &tool.status {
                Status::Yielded | Status::Queued => {}
                Status::Completed(_) => {
                    if let Status::Completed(messages) =
                        std::mem::replace(&mut tool.status, Status::Yielded)
                    {
                        ready.extend(messages);
                    }
                }
                Status::Executing if !tool.safe => break,
                Status::Executing => {}
            }
        }
        ready
    }

    fn result(&self, id: &ToolUseId, assistant: &MessageId, output: ToolOutput) -> Message {
        Message::User(UserMessage {
            id: self.scope.parts.ids.message(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: output.content,
                is_error: output.is_error,
            }],
            origin: UserOrigin::ToolResult {
                assistant: assistant.clone(),
            },
        })
    }
}

/// 工具结束的日志（logging.md §4）：失败记 info，带错误文本的前 300 个字符（系统文字，不是用户内容）；
/// 成功只在开发构建记 debug。
/// 耗时与用量行同一份（`ToolCall`，usage.md §2.3）。
fn log_finished(scope: &RunScope, tracked: &Tracked, output: &ToolOutput, call: &ToolCall) {
    let duration_ms = call.completed_at_ms.saturating_sub(call.started_at_ms);
    if !output.is_error {
        tracing::debug!(event = "tool.end", session = %scope.session, tool_use = %tracked.id, tool = %tracked.name, duration_ms, "tool finished");
        return;
    }
    let head: String = output
        .content
        .iter()
        .find_map(|part| match part {
            // 截取只定义在 base 的 `log_text::head`（F58：以前这里与 ext 各写一份）。
            ToolResultContent::Text { text } => Some(rust_cli_base::log_text::head(text)),
            _ => None,
        })
        .unwrap_or_default();
    tracing::info!(
        event = "tool.failed",
        session = %scope.session,
        tool_use = %tracked.id,
        tool = %tracked.name,
        duration_ms,
        error = %head,
        "tool failed"
    );
}

/// 找不到、它的 MCP 服务器在首次连接的调用：先等工具出现（ext.md §3.8），找到了照常执行，否则回 `No such tool
/// available`。等的时候被中止时给中断文本，用户中断由 [`ToolBatch::complete`] 照常换成拒绝结果。
async fn execute_when_connected(
    scope: Arc<RunScope>,
    id: ToolUseId,
    name: String,
    input: Value,
    cancel: CancellationToken,
    waited: Arc<AtomicU64>,
) -> ToolExecution {
    match mcp_wait::wait_for_tool(&scope, &name, &waited, &cancel).await {
        Some(tool) => execute(scope, tool, id, name, input, cancel).await,
        None if cancel.is_cancelled() => {
            ToolExecution::output(text_output(texts::REJECT_MESSAGE.to_owned()))
        }
        None => ToolExecution::output(text_output(texts::no_such_tool(&name))),
    }
}

/// 执行一个调用：登记意图 → 通知开始 → 执行管线（进度同时转发）。管线排在结果前后的消息一并交出。
async fn execute(
    scope: Arc<RunScope>,
    tool: Arc<dyn Tool>,
    id: ToolUseId,
    name: String,
    input: Value,
    cancel: CancellationToken,
) -> ToolExecution {
    // 不再先落库操作意图（工作线 F119）：没有读者，执行侧也不按 operationId 重放。
    let operation = scope.parts.ids.operation();
    tracing::debug!(event = "tool.start", session = %scope.session, tool_use = %id, tool = %name, "tool started");
    let _ = scope
        .sink
        .emit(QueryEvent::ToolStarted {
            tool_use: id.clone(),
            name,
            input: input.clone(),
        })
        .await;
    let context = ToolUseContext {
        session: scope.session.clone(),
        run: scope.run.clone(),
        tool_use: id.clone(),
        agent: scope.agent.clone(),
        workspace: scope.current_workspace().await,
        mode: scope.current_mode().await,
        cancel,
        // 缺省机器（R3，remote-exec.md §20.6）：主线程是本机，子代理是派生时给的机器；执行管线再按调用的 `machine` 换。
        machine: scope.default_machine().await,
        local: scope.parts.local.clone(),
        artifacts: scope.parts.artifacts.clone(),
        interactions: scope.sink.clone(),
        operation: OperationContext {
            environment: scope.workspace.environment.clone(),
            generation: scope.parts.generation.clone(),
            owner_epoch: OwnerEpoch::LOCAL,
            operation,
            origin: Origin::Agent {
                session: scope.session.clone(),
                run: scope.run.clone(),
            },
        },
    };
    let (progress, mut updates) = mpsc::channel(PROGRESS);
    let run = scope
        .parts
        .executor
        .execute_full(tool, input, context, progress);
    let forward = async {
        while let Some(progress) = updates.recv().await {
            let event = QueryEvent::ToolProgress {
                tool_use: id.clone(),
                progress,
            };
            let _ = scope.sink.emit(event).await;
        }
    };
    let (output, ()) = join(run, forward).await;
    output
}

#[cfg(test)]
#[path = "executor_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "executor_message_tests.rs"]
mod message_tests;

#[cfg(test)]
#[path = "executor_checkpoint_tests.rs"]
mod checkpoint_tests;
