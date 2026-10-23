//! `anthropic-beta` 列表（docs/specs/rust-cli/model.md §3.1）：按请求实际用到的特性生成。
//!
//! 条件是请求里有没有用到对应特性（看最终的请求体，Provider 补丁加的字段也算），不看模型能力表与实验开关；
//! 没用到的特性不发 beta（差异见 spec）。
//!
//! 顺序按组装的先后（模型表 → 每轮表 → 请求期追加）；比较时排序，顺序不影响结果。

use rust_cli_kernel_api::message::{ContentBlock, ImageSource, ToolResultContent};
use rust_cli_kernel_api::model::{ApiMessage, ApiRole, QuerySource};
use serde_json::Value;

pub(crate) const INTERLEAVED_THINKING: &str = "interleaved-thinking-2025-05-14";
pub(crate) const CONTEXT_MANAGEMENT: &str = "context-management-2025-06-27";
pub(crate) const PROMPT_CACHING_SCOPE: &str = "prompt-caching-scope-2026-01-05";
pub(crate) const MID_CONVERSATION_SYSTEM: &str = "mid-conversation-system-2026-04-07";
pub(crate) const THINKING_TOKEN_COUNT: &str = "thinking-token-count-2026-05-13";
pub(crate) const PER_TURN_CONTROL: &str = "per-turn-control-2026-07-01";
pub(crate) const EFFORT: &str = "effort-2025-11-24";
pub(crate) const ADVANCED_TOOL_USE: &str = "advanced-tool-use-2025-11-20";
pub(crate) const THINKING_DISPLAY_UPDATES: &str = "thinking-display-updates-2026-08-18";
pub(crate) const CACHE_DIAGNOSIS: &str = "cache-diagnosis-2026-04-07";
pub(crate) const STRUCTURED_OUTPUTS: &str = "structured-outputs-2025-12-15";
/// `web_search_20260209` 带的 beta（`@ai-sdk/anthropic` 3.0.81 随这个工具加，model.md §3.2）。
pub(crate) const CODE_EXECUTION_WEB_TOOLS: &str = "code-execution-web-tools-2026-02-09";
/// 兼容端点照 Node 发的两项（工作线 F64，model.md §3.1 的“兼容端点”）：PDF 文件块、SDK 的 structured output。
pub(crate) const PDFS: &str = "pdfs-2024-09-25";
pub(crate) const NODE_STRUCTURED_OUTPUTS: &str = "structured-outputs-2025-11-13";

/// AI SDK 3.0.81 的 `getModelCapabilities` 里支持 structured output 的模型（按子串认）：带普通工具时它发
/// `structured-outputs-2025-11-13`。表里其他 `claude-sonnet-4-` / `claude-opus-4-` 的版本不支持。
const NODE_STRUCTURED_OUTPUT_MODELS: [&str; 8] = [
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "claude-sonnet-4-5",
    "claude-opus-4-5",
    "claude-haiku-4-5",
    "claude-opus-4-1",
];

/// PDF 的 media type。
const PDF: &str = "application/pdf";

/// 请求体的 `messages` 里与 beta 有关的三项（修复线 F6：消息直接写成文本，不再有值可看，改从类型化消息判断）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MessageFlags {
    /// 有 `role: system` 的消息。
    pub(crate) system: bool,
    /// 有消息级的 `output_config`（写出的条件：`output_config.effort` 有值）。
    pub(crate) per_turn: bool,
    /// 有块的 `cache_control` 带 `scope`。
    pub(crate) cache_scope: bool,
    /// tool_result 里有 PDF 的 document 块（工作线 F64：兼容端点照 Node 发 `pdfs`）。
    pub(crate) pdf: bool,
}

impl MessageFlags {
    pub(crate) fn of(messages: &[ApiMessage]) -> Self {
        Self {
            system: messages.iter().any(|m| m.role == ApiRole::System),
            per_turn: messages.iter().any(|m| {
                m.output_config
                    .as_ref()
                    .is_some_and(|config| config.effort.is_some())
            }),
            cache_scope: messages.iter().any(|m| {
                m.content.iter().any(|content| {
                    content
                        .cache_control
                        .as_ref()
                        .is_some_and(|cache| cache.scope.is_some())
                })
            }),
            pdf: messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|content| has_pdf(&content.block)),
        }
    }

    /// 请求体里（补丁合并之后）的 `messages` 值。
    pub(crate) fn of_values(messages: &Value) -> Self {
        let messages = messages.as_array().map(Vec::as_slice).unwrap_or(&[]);
        Self {
            system: messages.iter().any(|m| m["role"] == "system"),
            per_turn: messages.iter().any(|m| m.get("output_config").is_some()),
            cache_scope: messages
                .iter()
                .flat_map(|message| items(&message["content"]))
                .any(scoped),
            pdf: messages
                .iter()
                .flat_map(|message| items(&message["content"]))
                .any(|item| is_pdf(item) || items(&item["content"]).any(is_pdf)),
        }
    }
}

fn has_pdf(block: &ContentBlock) -> bool {
    let ContentBlock::ToolResult { content, .. } = block else {
        return false;
    };
    content.iter().any(|item| match item {
        ToolResultContent::Document {
            source:
                ImageSource::Base64 { media_type, .. } | ImageSource::Artifact { media_type, .. },
        } => media_type == PDF,
        _ => false,
    })
}

fn is_pdf(item: &Value) -> bool {
    item["type"] == "document" && item["source"]["media_type"] == PDF
}

/// 兼容端点（方案不带一方的专有项）要发的 beta：照 Node 在同一端点上发的（AI SDK 3.0.81 的条件与 Node 的
/// `toolSearchHeaders`，model.md §3.1 的“兼容端点”），顺序同 SDK（消息 → 工具 → 头里的）。看最终的请求体；调用方显式给的
/// beta 不发（Node 没有对应）。修 bug（F64）：以前只留延迟工具与 WebSearch 的两项，对话中间有 system 消息时少发
/// `mid-conversation-system`（Node 发）。
pub(crate) fn node_betas(body: &Value, messages: MessageFlags) -> Vec<String> {
    let tools = || items(&body["tools"]);
    let model = body["model"].as_str().unwrap_or_default();
    let structured = tools().any(|tool| tool.get("type").is_none())
        && NODE_STRUCTURED_OUTPUT_MODELS
            .iter()
            .any(|name| model.contains(name));
    let rules: [(&str, bool); 5] = [
        (MID_CONVERSATION_SYSTEM, messages.system),
        (PDFS, messages.pdf),
        (NODE_STRUCTURED_OUTPUTS, structured),
        (
            CODE_EXECUTION_WEB_TOOLS,
            tools().any(|tool| tool["type"] == crate::anthropic_request::NODE_WEB_SEARCH),
        ),
        (
            ADVANCED_TOOL_USE,
            tools().any(|tool| tool["defer_loading"] == true),
        ),
    ];
    rules
        .into_iter()
        .filter(|(_, used)| *used)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// 这次请求要发的 beta：看最终的请求体（Provider 补丁已合并，所以补丁加的 thinking / effort 也带上对应
/// beta），消息部分看 `messages`，再接上调用方显式给的 `explicit`（去重，保持先后）。
pub(crate) fn betas(body: &Value, messages: MessageFlags, explicit: &[String]) -> Vec<String> {
    let thinking = matches!(
        body["thinking"]["type"].as_str(),
        Some("enabled" | "adaptive")
    );
    let per_turn = messages.per_turn;
    let rules: [(&str, bool); 12] = [
        // 模型表（jN）：交错思考与思考 token 计数都跟着 thinking 走。
        (INTERLEAVED_THINKING, thinking),
        (CONTEXT_MANAGEMENT, body.get("context_management").is_some()),
        (
            PROMPT_CACHING_SCOPE,
            uses_cache_scope(body) || messages.cache_scope,
        ),
        (MID_CONVERSATION_SYSTEM, messages.system),
        (THINKING_TOKEN_COUNT, thinking),
        // 每轮表（zN）：消息级的 output_config。
        (PER_TURN_CONTROL, per_turn),
        // 请求期追加：effort、工具搜索、思考展示、缓存诊断。
        (
            EFFORT,
            body["output_config"].get("effort").is_some() || per_turn,
        ),
        (
            ADVANCED_TOOL_USE,
            items(&body["tools"]).any(|tool| tool["defer_loading"] == true),
        ),
        (
            THINKING_DISPLAY_UPDATES,
            body["thinking"]["type"] == "adaptive" && body["thinking"].get("display").is_some(),
        ),
        (CACHE_DIAGNOSIS, body.get("diagnostics").is_some()),
        // structured output（请求带输出格式时追加，titles.md §8.4）。
        (
            STRUCTURED_OUTPUTS,
            body["output_config"].get("format").is_some(),
        ),
        // Node 的 WebSearch 工具版本（AI SDK 随工具加）。
        (
            CODE_EXECUTION_WEB_TOOLS,
            items(&body["tools"])
                .any(|tool| tool["type"] == crate::anthropic_request::NODE_WEB_SEARCH),
        ),
    ];
    let mut betas: Vec<String> = rules
        .into_iter()
        .filter(|(_, used)| *used)
        .map(|(name, _)| name.to_owned())
        .collect();
    for extra in explicit {
        if !betas.contains(extra) {
            betas.push(extra.clone());
        }
    }
    betas
}

/// 只给主循环、子代理、hook agent 与 fork 出的请求写 `diagnostics`，而且只对官方端点；compact 优先走 fork。
/// 其余旁路请求不写。本工程对一方（发一方专有项的方案，model.md §3.2，用户决定）都写。
pub(crate) fn writes_diagnostics(first_party_extras: bool, source: &QuerySource) -> bool {
    first_party_extras
        && match source {
            QuerySource::MainThread | QuerySource::Subagent(_) | QuerySource::Compact => true,
            QuerySource::Side(name) => name == "hook_agent",
        }
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn scoped(item: &Value) -> bool {
    item["cache_control"].get("scope").is_some()
}

/// system 与 tools 里任一缓存断点带 `scope`（例如静态段的 `scope: global`）；消息里的见 [`MessageFlags`]。
fn uses_cache_scope(body: &Value) -> bool {
    items(&body["system"]).any(scoped) || items(&body["tools"]).any(scoped)
}

#[cfg(test)]
#[path = "anthropic_betas_tests.rs"]
mod tests;
