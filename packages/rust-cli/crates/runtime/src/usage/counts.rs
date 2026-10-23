//! `/context` 的计数（docs/specs/rust-cli/context-usage.md §3）：经模型端口的 count_tokens，失败或不支持时
//! 本地估算。成功的结果按请求内容缓存在内核里；消息的
//! 计数不缓存。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, CountTokensRequest, ModelPort, ModelSelection, ToolDefinition,
};
use tokio_util::sync::CancellationToken;

/// 每次计数附带的工具开销：只有工具的请求减去它才是工具本身。
pub(crate) const TOOL_OVERHEAD: u64 = 500;

/// 计数结果的缓存：键是请求的 JSON 文本。
#[derive(Default)]
pub(crate) struct Cache(Mutex<HashMap<String, u64>>);

/// 一次 `/context` 的计数器。
pub(crate) struct Counter<'a> {
    pub(crate) model: &'a Arc<dyn ModelPort>,
    pub(crate) selection: &'a ModelSelection,
    pub(crate) cache: &'a Cache,
}

/// 一段文字作为一条 user 消息。
pub(crate) fn user_text(text: &str) -> Vec<ApiMessage> {
    vec![ApiMessage {
        role: ApiRole::User,
        content: vec![ApiContent {
            block: ContentBlock::Text {
                text: text.to_owned(),
            },
            cache_control: None,
        }],
        output_config: None,
    }]
}

/// JS `Math.round(text.length / per)`，长度按 UTF-16。
pub(crate) fn rough(text: &str, per: u64) -> u64 {
    let units = text.encode_utf16().count() as u64;
    (units * 2 + per) / (per * 2)
}

/// 本地估算：消息按内容块（字符数 / 每 token 字符数，图片 2000），有工具时加 JSON 长度 / 2 与
/// 开销。每 token 字符数按模型（修 bug（F10，context-usage.md D5）：以前一律按 4）。
fn estimate(messages: &[ApiMessage], tools: &[ToolDefinition], per: u64) -> u64 {
    let mut total: u64 = messages
        .iter()
        .flat_map(|message| &message.content)
        .map(|content| match &content.block {
            ContentBlock::Text { text } => rough(text, per),
            // 每张图的估计与压缩共用（2000，F55）。
            ContentBlock::Image { .. } => rust_cli_compact::IMAGE_TOKENS,
            other => rough(&serde_json::to_string(other).unwrap_or_default(), per),
        })
        .sum();
    if !tools.is_empty() {
        let json = serde_json::to_string(tools).unwrap_or_default();
        total += rough(&json, 2) + TOOL_OVERHEAD;
    }
    total
}

impl Counter<'_> {
    /// 计一次；`cached` 为真时先查缓存、成功后写缓存（消息的计数传 false）。
    pub(crate) async fn count(
        &self,
        messages: Vec<ApiMessage>,
        tools: Vec<ToolDefinition>,
        cached: bool,
    ) -> u64 {
        let key =
            serde_json::to_string(&(&self.selection.model, &messages, &tools)).unwrap_or_default();
        if cached && let Some(found) = self.lookup(&key) {
            return found;
        }
        let request = CountTokensRequest {
            selection: self.selection.clone(),
            messages,
            tools,
        };
        // 没有调用方会取消 `/context`：用一个不会被取消的令牌。
        match self
            .model
            .count_tokens(request.clone(), CancellationToken::new())
            .await
        {
            Ok(counted) => {
                if cached {
                    self.cache
                        .0
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(key, counted);
                }
                counted
            }
            Err(error) => {
                tracing::debug!(%error, "count_tokens unavailable; estimating locally");
                let per = rust_cli_prompts::models::chars_per_token(&self.selection.model);
                estimate(&request.messages, &request.tools, per)
            }
        }
    }

    fn lookup(&self, key: &str) -> Option<u64> {
        self.cache
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .copied()
    }

    /// 一段文字（system 段落、规则文件、agent、MCP instructions）。
    pub(crate) async fn text(&self, text: &str) -> u64 {
        self.count(user_text(text), Vec::new(), true).await
    }

    /// 一组工具（没有消息，发 `foo`）。
    pub(crate) async fn tools(&self, tools: Vec<ToolDefinition>) -> u64 {
        self.count(Vec::new(), tools, true).await
    }
}
