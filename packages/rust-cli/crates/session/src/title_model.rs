//! 会话标题的模型请求（docs/specs/rust-cli/titles.md §3）：提示词见 titles.md，模型经小模型
//! 槽位（model.md §6.1），辅助选项、超时与解析按 Node（`title-generation-sidecar.ts`、
//! `auxiliary-model-options.ts`）。不用 structured output。请求结束时同时给出这次请求的用量事实（工作线 F73，
//! usage.md §2.1.1）。

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::ids::{MessageId, RunId, SessionId};
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, ModelError, ModelEvent, ModelNotice, ModelPort,
    ModelRequest, ModelSelection, ModelStreamItem, QuerySource, RequestOptions, SystemBlock,
    small_fast_model,
};
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_prompts::texts::render;
use rust_cli_prompts::title;
use tokio_util::sync::CancellationToken;

use crate::title_parse::{clean_title, normalize_input};

/// Node `TITLE_GENERATION_TIMEOUT_MS`：请求的期限，不参与状态同步。
const TIMEOUT: Duration = Duration::from_secs(60);
/// Node `AUXILIARY_MAX_OUTPUT_TOKENS`。
const AUXILIARY_MAX_OUTPUT_TOKENS: u64 = 5_000;
/// 请求来源（`querySource`；名字只在 kernel-api 定义一处，用量表与请求头写成 Node 的 `session_title`，工作线 F71）。
const QUERY_SOURCE: &str = rust_cli_kernel_api::model::SIDE_SESSION_TITLE;

/// 标题生成用的模型端口与模型目录（交互形态的会话才有，`-p` 不生成，titles.md §2）。
#[derive(Clone)]
pub struct TitleModel {
    pub model: Arc<dyn ModelPort>,
    /// 取辅助选项（推理档位、输出上限）；显式覆盖时没有。
    pub directory: Option<Arc<dyn ModelDirectory>>,
}

/// 一次标题请求的结果：候选标题（失败、超时或解析为空时没有）与这次请求的用量事实（Node 不论结果都写一行，
/// usage.md §2.1.1）。
pub(crate) struct Generated {
    pub(crate) title: Option<String>,
    pub(crate) call: ModelCall,
}

/// 读流时收集的回复：文本、用量（`message_start` 的用量，`message_delta` 的输出覆盖，同主循环）、停止原因、
/// tool_use 块数、重试次数。
#[derive(Default)]
struct Reply {
    text: String,
    usage: Usage,
    stop_reason: Option<StopReason>,
    tool_calls: u32,
    retries: u32,
}

impl Reply {
    fn push(&mut self, item: ModelStreamItem) {
        match item {
            ModelStreamItem::Notice(ModelNotice::Retrying { .. }) => self.retries += 1,
            ModelStreamItem::Event(ModelEvent::MessageStart { usage, .. }) => self.usage = usage,
            ModelStreamItem::Event(ModelEvent::BlockStart { block, .. }) => match block {
                ContentBlock::Text { text } => self.text.push_str(&text),
                ContentBlock::ToolUse { .. } => self.tool_calls += 1,
                _ => {}
            },
            ModelStreamItem::Event(ModelEvent::BlockDelta {
                delta: BlockDelta::Text(delta),
                ..
            }) => self.text.push_str(&delta),
            ModelStreamItem::Event(ModelEvent::MessageDelta { stop_reason, usage }) => {
                self.stop_reason = stop_reason.or(self.stop_reason);
                self.usage.output_tokens = usage.output_tokens;
            }
            ModelStreamItem::Notice(_) | ModelStreamItem::Event(_) => {}
        }
    }
}

/// 首条输入能不能生成标题（长度下限按码点数，同 Node）。
pub(crate) fn long_enough(input: &str) -> bool {
    let normalized = normalize_input(input);
    !normalized.is_empty() && normalized.chars().count() >= title::limits().min_input_chars
}

impl TitleModel {
    /// Node `auxiliaryModelOptions`：推理档位取目录里的第一个，输出上限 `min(5000, 模型上限)`（这个上限也交给 Provider
    /// 补丁，工作线 F97）。
    async fn auxiliary(&self, selection: ModelSelection) -> (ModelSelection, u32) {
        let mut max_tokens = AUXILIARY_MAX_OUTPUT_TOKENS;
        let mut selection = selection;
        if let Some(directory) = &self.directory {
            let catalog = directory.catalog().await;
            let card = catalog
                .models
                .iter()
                .find(|card| card.provider == selection.provider && card.model == selection.model);
            if let Some(card) = card {
                if let Some(level) = card.reasoning_levels.first() {
                    selection.reasoning = Some(level.clone());
                }
                if card.max_output_tokens > 0 {
                    max_tokens = max_tokens.min(card.max_output_tokens);
                }
            }
        }
        (selection, u32::try_from(max_tokens).unwrap_or(u32::MAX))
    }

    /// 请求候选标题；失败、超时或解析为空时标题为 None（只记日志）。`main` 是会话当前的主模型；`run` 是提交这条输入的
    /// 回合（工作线 F64：每请求头的 `x-query-id` 与回合共用，model.md §2.4）；`clock` 与 `request` 是用量事实的计时与
    /// 逻辑请求 ID（工作线 F73）。
    pub(crate) async fn generate(
        &self,
        main: &ModelSelection,
        (session, run): (&SessionId, Option<&RunId>),
        input: &str,
        (clock, request_id): (&dyn Clock, MessageId),
        cancel: CancellationToken,
    ) -> Generated {
        let (selection, max_tokens) = self.auxiliary(small_fast_model(main)).await;
        let content = normalize_input(input);
        let user = render(
            title::user(),
            &[
                ("content", content.as_str()),
                ("language", title::language_default()),
            ],
        );
        let source = QuerySource::Side(QUERY_SOURCE.to_owned());
        let request = ModelRequest {
            selection: selection.clone(),
            system: vec![SystemBlock {
                text: title::system().to_owned(),
                cache_control: None,
            }],
            messages: vec![ApiMessage {
                role: ApiRole::User,
                content: vec![ApiContent {
                    block: ContentBlock::Text { text: user },
                    cache_control: None,
                }],
                output_config: None,
            }],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens,
            thinking: None,
            temperature: None,
            betas: Vec::new(),
            source: source.clone(),
            // 工作线 F52：`metadata.user_id` 的 `session_id`（model.md §3.3）。
            options: RequestOptions {
                session_id: Some(session.clone()),
                run: run.cloned(),
                // 修 bug（工作线 F97，titles.md §3）：目录模型的 Provider 补丁以配置的上限改写 max_tokens，这次请求自己的
                // `min(5000, 上限)` 在那里不生效（glm-5.3 发 128000）。Node 的补丁以请求自己的上限为输入，发 5000；
                // 这里把同一个上限交给补丁（同 WebSearch，F95）。显式覆盖的端点没有补丁，不看它。
                provider_max_output_tokens: Some(max_tokens),
                ..RequestOptions::default()
            },
        };
        let mut reply = Reply::default();
        let started = clock.now_ms();
        let collect = async {
            // 带通知的流：重试次数写进用量行（Node `model_retry_scheduled` 的次数）；请求与 `stream` 相同。
            let mut stream = self.model.stream_with_notices(request, cancel.clone());
            while let Some(item) = stream.next().await {
                reply.push(item?);
            }
            Ok::<(), ModelError>(())
        };
        let outcome = tokio::time::timeout(TIMEOUT, collect).await;
        let completed = clock.now_ms();
        let (status, error, title) = match outcome {
            Ok(Ok(())) => (CallStatus::Completed, None, clean_title(&reply.text)),
            Ok(Err(error)) => {
                tracing::warn!(%error, "session title generation failed");
                (CallStatus::Error, Some(error), None)
            }
            // 超时不是模型错误：行的状态是 error，错误列为空（usage.md D19）。
            Err(_) => {
                cancel.cancel();
                tracing::warn!("session title generation timed out");
                (CallStatus::Error, None, None)
            }
        };
        let call = ModelCall {
            message: request_id,
            source,
            selection,
            started_at_ms: started,
            // Node 的标题请求是非流式的 `generateText`，没有首字时间（usage.md §2.1.1）。
            first_token_at_ms: None,
            completed_at_ms: completed,
            status,
            stop_reason: reply.stop_reason,
            tool_calls: reply.tool_calls,
            // 全为 0 时不写（同主循环）。
            usage: Some(reply.usage).filter(|usage| *usage != Usage::default()),
            retries: reply.retries,
            error,
        };
        Generated { title, call }
    }
}

#[cfg(test)]
#[path = "title_model_tests.rs"]
mod tests;
