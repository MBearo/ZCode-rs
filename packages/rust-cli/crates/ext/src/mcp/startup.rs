//! `-p` 的启动等待的设置（docs/specs/rust-cli/ext.md §3.3.1，修复线 F36）。入口按环境变量与参数算好
//! [`McpStartup`]，等待本身在 [`super::McpManager::wait_for_print`]。
//!
//! 启动只等 alwaysLoad 的服务器（非阻塞时），第一条命令前的 prewait 再等到截止；缺省值见下面的常量。

use std::collections::BTreeSet;

use super::config::normalize_name;

/// `MCP_CONNECT_TIMEOUT_MS` 的缺省。
pub const DEFAULT_CONNECT_WAIT_MS: u64 = 5_000;
/// prewait 的缺省截止；allow 规则点名的服务器至少等这么久。
pub const DEFAULT_PREWAIT_MS: u64 = 2_000;

/// `-p` 的启动等待。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStartup {
    /// `MCP_CONNECTION_NONBLOCKING` 不是“假”：启动只等 alwaysLoad 的服务器。
    pub nonblocking: bool,
    /// 启动等待的上限（`MCP_CONNECT_TIMEOUT_MS`）。
    pub connect_wait_ms: u64,
    /// 第一条命令前的 prewait 截止（[`prewait_ms`]）。
    pub prewait_ms: u64,
    /// allow 规则点名的服务器（[`required_servers`]，按 [`segment`] 比较）。
    pub required: BTreeSet<String>,
    /// ② 也等可延迟的服务器（`waitForDeferrable`）。stream-json 输入的 `lifted` 为假：② 只等 alwaysLoad
    /// 与 allow 规则点名的服务器（docs/specs/rust-cli/headless.md §6，修复线 F39）。
    pub wait_for_deferrable: bool,
}

impl Default for McpStartup {
    fn default() -> Self {
        Self {
            nonblocking: true,
            connect_wait_ms: DEFAULT_CONNECT_WAIT_MS,
            prewait_ms: DEFAULT_PREWAIT_MS,
            required: BTreeSet::new(),
            wait_for_deferrable: true,
        }
    }
}

/// prewait 截止（`deadlineMs`，没有时用缺省）：设了 `ZCODE_MCP_STARTUP_WAIT_MS` 用它；
/// 否则显式给了 `--mcp-config` 的服务器时用 `MCP_TIMEOUT`；否则 2000。
pub fn prewait_ms(startup_wait_ms: Option<u64>, explicit: bool, mcp_timeout_ms: u64) -> u64 {
    match startup_wait_ms {
        Some(wait) => wait,
        None if explicit => mcp_timeout_ms,
        None => DEFAULT_PREWAIT_MS,
    }
}

/// ② 等不等可延迟的服务器（`firstTurnHold`）：stream-json 输入、能用工具搜索、没有显式 `--mcp-config`、
/// 没有设 `ZCODE_MCP_STARTUP_WAIT_MS` 时是 `lifted`，不等（headless.md §6）。单次 `-p "…"` 是 `single_shot`，等。
pub fn wait_for_deferrable(
    streaming_input: bool,
    tool_search: bool,
    explicit: bool,
    startup_wait_ms: Option<u64>,
) -> bool {
    !(streaming_input && tool_search && !explicit && startup_wait_ms.is_none())
}

/// allow 规则点名的服务器：规则名（`(` 之前）按 `__` 拆开，第一段是 `mcp`、第二段不为空时，
/// 第二段规范化后就是服务器。
pub fn required_servers<'a>(rules: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    rules
        .into_iter()
        .filter_map(|rule| {
            let name = rule.split('(').next().unwrap_or_default().trim();
            let mut parts = name.split("__");
            (parts.next() == Some("mcp"))
                .then(|| parts.next())
                .flatten()
                .filter(|server| !server.is_empty())
                .map(normalize_name)
        })
        .collect()
}

/// 服务器名比较用的段：规范化后到第一个 `__` 为止；那一段为空时是整个规范化的名字。
pub fn segment(name: &str) -> String {
    let normalized = normalize_name(name);
    match normalized.split("__").next() {
        Some(first) if !first.is_empty() => first.to_owned(),
        _ => normalized,
    }
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
