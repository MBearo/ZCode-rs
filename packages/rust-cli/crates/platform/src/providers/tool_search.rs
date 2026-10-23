//! ToolSearch 的端点判定（工作线 F52，docs/specs/rust-cli/tools-more.md §3.2，用户决定 2026-10-22：“只给 zai、bigmodel
//! 的域名下的模型开。其他的不开。”）。
//!
//! 修 bug：以前只看模型卡片（`claude-opus-5-5` 的 `tool_search`），第三方网关（后面是 AWS Bedrock）上的同一个模型也带
//! `defer_loading` 与 `anthropic-beta: advanced-tool-use-2025-11-20`，Bedrock 拒绝这个 beta。现在按端点：协议是
//! anthropic-messages、主机是 z.ai / bigmodel.cn 或它们的子域名时才开；目录项与显式覆盖的内置规则都经这里。

use rust_cli_kernel_api::provider::WireProtocol;

/// 这个端点支持延迟加载工具（`ModelCapabilities::supports_tool_search`）。`base_url` 是 Provider 配置的 `api.baseUrl`
/// （或显式覆盖的地址），解析不了时为假。智谱端点的判定只在 base 一处（`rule_url::is_zhipu_endpoint`，修复线 F58：
/// 请求方案 `zhipu-official` 用同一个判定）。
pub(crate) fn endpoint(protocol: WireProtocol, base_url: &str) -> bool {
    protocol == WireProtocol::AnthropicMessages
        && rust_cli_base::rule_url::is_zhipu_endpoint(base_url)
}

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tests;
