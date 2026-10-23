//! 会话标题的模型请求（docs/specs/rust-cli/titles.md §3）：提示词见 titles.md，模型经小模型
//! 槽位（model.md §6.1），辅助选项、超时与解析按 Node（`title-generation-sidecar.ts`、
//! `auxiliary-model-options.ts`）。不用 structured output。

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::ids::{RunId, SessionId};
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, ModelEvent, ModelPort, ModelRequest,
    ModelSelection, QuerySource, RequestOptions, SystemBlock, small_fast_model,
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

/// 首条输入能不能生成标题（长度下限按码点数，同 Node）。
pub(crate) fn long_enough(input: &str) -> bool {
    let normalized = normalize_input(input);
    !normalized.is_empty() && normalized.chars().count() >= title::limits().min_input_chars
}

impl TitleModel {
    /// Node `auxiliaryModelOptions`：推理档位取目录里的第一个，输出上限 `min(5000, 模型上限)`。
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

    /// 请求候选标题；失败、超时或解析为空时为 None（只记日志）。`main` 是会话当前的主模型；`run` 是提交这条输入的回合
    /// （工作线 F64：每请求头的 `x-query-id` 与回合共用，model.md §2.4）。
    pub(crate) async fn generate(
        &self,
        main: &ModelSelection,
        (session, run): (&SessionId, Option<&RunId>),
        input: &str,
        cancel: CancellationToken,
    ) -> Option<String> {
        let (selection, max_tokens) = self.auxiliary(small_fast_model(main)).await;
        let content = normalize_input(input);
        let user = render(
            title::user(),
            &[
                ("content", content.as_str()),
                ("language", title::language_default()),
            ],
        );
        let request = ModelRequest {
            selection,
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
            source: QuerySource::Side(QUERY_SOURCE.to_owned()),
            // 工作线 F52：`metadata.user_id` 的 `session_id`（model.md §3.3）。
            options: RequestOptions {
                session_id: Some(session.clone()),
                run: run.cloned(),
                ..RequestOptions::default()
            },
        };
        let collect = async {
            let mut stream = self.model.stream(request, cancel.clone());
            let mut text = String::new();
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ModelEvent::BlockStart {
                        block: ContentBlock::Text { text: start },
                        ..
                    }) => text.push_str(&start),
                    Ok(ModelEvent::BlockDelta {
                        delta: BlockDelta::Text(delta),
                        ..
                    }) => text.push_str(&delta),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "session title generation failed");
                        return None;
                    }
                }
            }
            Some(text)
        };
        let Ok(text) = tokio::time::timeout(TIMEOUT, collect).await else {
            cancel.cancel();
            tracing::warn!("session title generation timed out");
            return None;
        };
        clean_title(&text?)
    }
}

#[cfg(test)]
#[path = "title_model_tests.rs"]
mod tests;
