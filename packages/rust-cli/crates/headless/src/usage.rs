//! `-p "/usage"`（别名 `/cost`、`/stats`，docs/specs/rust-cli/usage.md §6）：非交互的定义只输出会话部分
//! （API key 用户没有套餐用量）。不开回合、不发请求；新会话不建会话行。
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rust_cli_kernel_api::message::{Message, Usage};
use rust_cli_kernel_api::metering::SessionSummary;
use rust_cli_kernel_api::session_api::{EndReason, Kernel};

use crate::{PrintOptions, existing_session};

/// 整个 prompt 是 `/usage`、`/cost` 或 `/stats`（可带被忽略的参数）。
pub(crate) fn is_usage(prompt: &str) -> bool {
    let prompt = prompt.trim();
    let name = prompt.split_whitespace().next().unwrap_or_default();
    matches!(name, "/usage" | "/cost" | "/stats")
}

/// 按模型累计 assistant 消息的用量（与 TUI 的视图同一套口径）。
fn by_model(messages: &[Message]) -> Vec<(String, Usage)> {
    let mut models: BTreeMap<String, Usage> = BTreeMap::new();
    for message in messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        let Some(usage) = assistant.usage else {
            continue;
        };
        let total = models.entry(assistant.model.clone()).or_default();
        total.input_tokens += usage.input_tokens;
        total.output_tokens += usage.output_tokens;
        total.cache_read_input_tokens += usage.cache_read_input_tokens;
        total.cache_creation_input_tokens += usage.cache_creation_input_tokens;
    }
    models.into_iter().collect()
}

/// 命令输出的文字（不带结尾的换行；text 输出补上，headless.md §3.6）。
pub(crate) async fn text(
    kernel: Arc<dyn Kernel>,
    options: &PrintOptions,
    started: Instant,
) -> Result<String> {
    let mut summary = SessionSummary::default();
    // `-c` / `-r`：那个会话的 API 时长（用量表）与用量；新会话（含找不到会话的 `-c`）还没有请求。代码改动是
    // 本进程的，`-p` 没有。
    if let Some(session) = existing_session(kernel.as_ref(), options).await? {
        summary.api_duration_ms = kernel.session_metering(session.id()).await?.api_duration_ms;
        summary.models = by_model(&session.view().await?.messages);
        let _ = session.close(EndReason::Other).await;
    }
    summary.wall_duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(summary.text())
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
