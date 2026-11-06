//! 摘要请求（docs/specs/rust-cli/compact.md §3.2）：经 [`QueryRunner`] 跑一次 `QuerySource::Compact`
//! 的子循环（fork：同一个 system 与工具清单，`maxTurns: 1`，工具一律拒绝，结果不落库）。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::compaction::prompt_too_long_gap;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, Usage};
use rust_cli_kernel_api::model::{ModelError, QuerySource};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, QueryError,
    QueryEvent, QueryRequest, QueryStop, RunSink, RunView, SessionOps, ToolExecution, ToolExecutor,
};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_kernel_api::tool::{Tool, ToolOutput, ToolProgress, ToolUseContext};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 压缩时拒绝一切工具调用。
pub struct DenyTools;

/// 拒绝的原文。
const DENIED: &str = "Tool use is not allowed during compaction";

#[async_trait]
impl ToolExecutor for DenyTools {
    async fn execute(
        &self,
        _tool: Arc<dyn Tool>,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        ToolExecution::output(ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: DENIED.to_owned(),
            }],
            is_error: true,
            display: Value::Null,
        })
    }
}

/// 只收集、不落库的输出端；没有人能回答交互，不持有会话（没有视图与对话，命令一律 `Unsupported`）。
#[derive(Default)]
pub(crate) struct CollectSink {
    messages: Mutex<Vec<Message>>,
}

impl CollectSink {
    fn take(&self) -> Vec<Message> {
        std::mem::take(&mut self.messages.lock_unpoisoned())
    }
}

#[async_trait]
impl Interactions for CollectSink {
    async fn ask(
        &self,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Err(OrchestrationError::Failed {
            message: DENIED.to_owned(),
        })
    }

    /// 摘要请求不发询问，没有可撤回的。
    async fn cancel_ask(&self, _tool_use: &rust_cli_kernel_api::ids::ToolUseId) {}

    /// 摘要请求不持有会话：没有会话级的询问入口。
    fn lasting(&self) -> Option<std::sync::Arc<dyn Interactions>> {
        None
    }
}

fn unsupported(what: &str) -> OrchestrationError {
    OrchestrationError::Unsupported {
        what: what.to_owned(),
    }
}

#[async_trait]
impl SessionOps for CollectSink {
    async fn view(&self) -> Option<RunView> {
        None
    }

    async fn owner_session(&self, session: &SessionId) -> SessionId {
        session.clone()
    }

    async fn conversation(&self) -> Option<Vec<Message>> {
        None
    }

    async fn set_plan(&self, _enabled: bool) -> Result<(), OrchestrationError> {
        Err(unsupported("changing plan mode in a summary request"))
    }

    async fn set_workspace(
        &self,
        _root: rust_cli_kernel_api::env::EnvPath,
    ) -> Result<(), OrchestrationError> {
        Err(unsupported(
            "changing the working directory in a summary request",
        ))
    }

    async fn arm_monitor(
        &self,
        _monitor: rust_cli_kernel_api::monitor::MonitorArmed,
        _events: rust_cli_kernel_api::machine::CommandEvents,
    ) -> Result<(), OrchestrationError> {
        Err(unsupported("monitors in a summary request"))
    }
}

#[async_trait]
impl RunSink for CollectSink {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.messages.lock_unpoisoned().push(message);
        Ok(())
    }

    async fn emit(&self, _event: QueryEvent) -> bool {
        true
    }

    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError> {
        self.messages.lock_unpoisoned().extend(messages);
        Ok(())
    }

    /// 摘要请求没有存储：只收集消息，丢弃计量（计量由压缩器在别处记）。
    async fn commit_metered(
        &self,
        message: Option<Message>,
        _metered: rust_cli_kernel_api::metering::Metered,
    ) -> Result<(), StoreError> {
        match message {
            Some(message) => self.commit_message(message).await,
            None => Ok(()),
        }
    }

    /// 摘要请求不压缩自己。
    async fn compact_started(&self, _trigger: rust_cli_kernel_api::message::CompactTrigger) {}

    async fn compact_failed(
        &self,
        _trigger: rust_cli_kernel_api::message::CompactTrigger,
        _message: String,
    ) {
    }

    /// 摘要请求没有插话与通知。
    async fn take_absorbed(&self) -> Vec<rust_cli_kernel_api::orchestration::Absorbed> {
        Vec::new()
    }

    /// 摘要请求没有目标。
    async fn goal_checked(
        &self,
        _condition: String,
        _verdict: rust_cli_kernel_api::hooks::StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        Ok(Vec::new())
    }
}

/// 一次摘要请求的结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Attempt {
    Summary {
        text: String,
        usage: Option<Usage>,
    },
    /// 上下文超限；`gap` 是从错误文本解析出的 token 差（kernel-api `prompt_too_long_gap`）。
    PromptTooLong {
        gap: Option<u64>,
    },
    Aborted,
    Failed {
        detail: String,
    },
}

fn text_of(message: &Message) -> Option<String> {
    let Message::Assistant(assistant) = message else {
        return None;
    };
    let text: Vec<&str> = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then(|| text.join("\n"))
}

/// 摘要文本：最后一条含 `<summary>` 的 assistant，没有时取最后一条有文本的。
fn summary_of(messages: &[Message]) -> Option<(String, Option<Usage>)> {
    let usage = |m: &Message| match m {
        Message::Assistant(assistant) => assistant.usage,
        _ => None,
    };
    let texts: Vec<(String, Option<Usage>)> = messages
        .iter()
        .filter_map(|m| text_of(m).map(|text| (text, usage(m))))
        .collect();
    texts
        .iter()
        .rev()
        .find(|(text, _)| text.contains("<summary>"))
        .or_else(|| texts.last())
        .map(|(text, usage)| (text.trim().to_owned(), *usage))
        .filter(|(text, _)| !text.is_empty())
}

pub(crate) async fn run(
    runner: &dyn rust_cli_kernel_api::orchestration::QueryRunner,
    mut request: QueryRequest,
    cancel: &CancellationToken,
) -> Attempt {
    request.source = QuerySource::Compact;
    request.max_turns = Some(1);
    let sink = Arc::new(CollectSink::default());
    let abort = Abort::new();
    let run = runner.run(request, sink.clone(), abort.clone());
    tokio::pin!(run);
    let stopped = tokio::select! {
        stopped = &mut run => stopped,
        () = cancel.cancelled() => {
            abort.abort(AbortReason::User);
            run.await
        }
    };
    if cancel.is_cancelled() {
        return Attempt::Aborted;
    }
    match stopped {
        Err(QueryError::Model(ModelError::PromptTooLong { message })) => Attempt::PromptTooLong {
            // 与主循环的初始差同一个解析函数（kernel-api）。
            gap: prompt_too_long_gap(&message),
        },
        Err(QueryError::Model(ModelError::Cancelled)) | Ok(QueryStop::Aborted) => Attempt::Aborted,
        Err(error) => Attempt::Failed {
            detail: error.to_string(),
        },
        Ok(QueryStop::ApiError { message } | QueryStop::Blocked { message }) => {
            Attempt::Failed { detail: message }
        }
        // 摘要请求的工具一律拒绝，不会有工具的 hook 要求停止；摘要不跑 Stop hooks。照完成处理（query.md §3）。
        Ok(
            QueryStop::Completed
            | QueryStop::MaxTurns
            | QueryStop::HookStopped
            | QueryStop::StopHookPrevented,
        ) => match summary_of(&sink.take()) {
            Some((text, usage)) => Attempt::Summary { text, usage },
            None => Attempt::Failed {
                detail: "summarization produced empty response".to_owned(),
            },
        },
    }
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
