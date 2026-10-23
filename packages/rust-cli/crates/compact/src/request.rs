//! 摘要请求（docs/specs/rust-cli/compact.md §3.2）：经 [`QueryRunner`] 跑一次 `QuerySource::Compact`
//! 的子循环（fork：同一个 system 与工具清单，`maxTurns: 1`，工具一律拒绝，结果不落库）。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::compaction::prompt_too_long_gap;
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, Message, Usage};
use rust_cli_kernel_api::model::{ModelError, QuerySource};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, QueryError,
    QueryEvent, QueryRequest, QueryStop, RunSink, ToolExecutor,
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
    ) -> ToolOutput {
        ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: DENIED.to_owned(),
            }],
            is_error: true,
            display: Value::Null,
        }
    }
}

/// 只收集、不落库的输出端；没有人能回答交互。
#[derive(Default)]
pub(crate) struct CollectSink {
    messages: Mutex<Vec<Message>>,
}

impl CollectSink {
    fn take(&self) -> Vec<Message> {
        std::mem::take(&mut self.messages.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

#[async_trait]
impl Interactions for CollectSink {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Err(OrchestrationError::Failed {
            message: DENIED.to_owned(),
        })
    }
}

#[async_trait]
impl RunSink for CollectSink {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message);
        Ok(())
    }

    async fn commit_intent(
        &self,
        _operation: OperationId,
        _fingerprint: String,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn emit(&self, _event: QueryEvent) -> bool {
        true
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
        Ok(QueryStop::Completed | QueryStop::MaxTurns) => match summary_of(&sink.take()) {
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
