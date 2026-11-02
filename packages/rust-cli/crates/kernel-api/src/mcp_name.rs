//! MCP 工具名 `mcp__<服务器>__<工具>` 的拼与拆（docs/specs/rust-cli/ext.md §3.4，工作线 F132 的 T10）：前缀与分隔符只
//! 写在这里。以前 9 个 crate 16 处各自拼、各自按前缀拆。
//!
//! 拆的规则：前缀之后到第一个 `__` 是服务器，其余（可能还含 `__`）是工具。规范化的服务器名里也可能有 `__`，所以
//! “这个工具属于哪个已知的服务器”按 [`server_prefix`] 的最长前缀找，不用 [`parse`]。

/// 前缀。
pub const PREFIX: &str = "mcp__";
/// 服务器与工具之间的分隔符。
pub const SEPARATOR: &str = "__";

/// `mcp__<server>__<tool>`。调用方给已规范化的两段（`mcp_state::normalize_name`）。
pub fn compose(server: &str, tool: &str) -> String {
    format!("{PREFIX}{server}{SEPARATOR}{tool}")
}

/// 一个服务器的工具名前缀 `mcp__<server>__`。
pub fn server_prefix(server: &str) -> String {
    format!("{PREFIX}{server}{SEPARATOR}")
}

/// 名字是 MCP 的写法（以 `mcp__` 开头）：MCP 工具，或指向 MCP 服务器的规则名。
pub fn is_mcp(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// 拆开的名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpName<'a> {
    /// 前缀之后到第一个 `__`（没有 `__` 时是其余全部；可能为空）。
    pub server: &'a str,
    /// 第一个 `__` 之后的部分；没有 `__` 时为 None（可能为空串）。
    pub tool: Option<&'a str>,
}

/// 按前缀与第一个分隔符拆开；不以 `mcp__` 开头时为 None。
pub fn parse(name: &str) -> Option<McpName<'_>> {
    let rest = name.strip_prefix(PREFIX)?;
    Some(match rest.split_once(SEPARATOR) {
        Some((server, tool)) => McpName {
            server,
            tool: Some(tool),
        },
        None => McpName {
            server: rest,
            tool: None,
        },
    })
}

#[cfg(test)]
#[path = "mcp_name_tests.rs"]
mod tests;
