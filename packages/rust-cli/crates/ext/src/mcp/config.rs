//! MCP 服务器的配置快照（docs/specs/rust-cli/ext.md §3.1）与名字规范化（§3.4）。
//!
//! 入口层读配置文件、展开 `${VAR}`、按作用域合并，交给 ext 的只是这份快照。

use rust_cli_base::units;
use rust_cli_kernel_api::mcp::McpTransportConfig;

/// 配置作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpScope {
    Enterprise,
    User,
    Local,
    Project,
    /// `--mcp-config`。
    Dynamic,
    /// 插件提供（ext.md §4）。
    Plugin,
}

/// 一个服务器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub scope: McpScope,
    pub transport: McpTransportConfig,
    pub options: McpServerOptions,
    /// 服务器绑定的远端机器（R5，remote-exec.md §22.2：入口按 rust-cli 的旁路文件设）；None 或 `local` 是本机。绑定到
    /// 远端时 `transport` 是配置的原文（`${VAR}` 由那台机器的执行器展开，§22.7）。
    pub machine: Option<String>,
}

/// 按服务器的选项（Node 配置的 `timeoutMs`、`alwaysLoad`，ext.md §3.1）；`--mcp-config` 与插件的服务器用缺省值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpServerOptions {
    /// 这个服务器的连接（握手与清单）、工具调用与资源读取的期限；None 时用 [`McpSettings`] 的缺省值。
    pub timeout_ms: Option<u64>,
    /// 这个服务器的工具都不延迟（工具自己的 `_meta` 标记也不延迟，`tool.rs` 的 `ALWAYS_LOAD_KEYS`）。
    pub always_load: bool,
}

pub type McpServers = Vec<McpServer>;

/// 连接与调用的设置（入口层从环境变量读，缺省值见 ext.md §3.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSettings {
    pub servers: McpServers,
    /// `MCP_TIMEOUT`：连接与握手。
    pub connect_timeout_ms: u64,
    /// `MCP_TOOL_TIMEOUT`：一次工具调用。
    pub tool_timeout_ms: u64,
    /// `MAX_MCP_OUTPUT_TOKENS`：结果截断的上限。
    pub max_output_tokens: u64,
    /// `initialize` 的 `clientInfo.version`：入口给的本工程版本（docs/specs/rust-cli/version.md §4）；
    /// ext 不读自己的包版本。缺省为空串。
    pub client_version: String,
    /// `-p` 的启动等待（ext.md §3.3.1，修复线 F36）：入口按环境变量与参数算好。
    pub startup: super::startup::McpStartup,
}

impl McpSettings {
    /// 这个服务器的连接期限：`timeoutMs`，没有时 `MCP_TIMEOUT`。
    pub fn connect_timeout_for(&self, options: McpServerOptions) -> u64 {
        options.timeout_ms.unwrap_or(self.connect_timeout_ms)
    }

    /// 这个服务器的工具调用与资源读取期限：`timeoutMs`，没有时 `MCP_TOOL_TIMEOUT`。
    pub fn tool_timeout_for(&self, options: McpServerOptions) -> u64 {
        options.timeout_ms.unwrap_or(self.tool_timeout_ms)
    }
}

/// 连接一个服务器的缺省超时（`MCP_TIMEOUT`）。
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 30 * units::MS_PER_SECOND;
/// 一次工具调用的缺省超时（`MCP_TOOL_TIMEOUT`，约 27.8 小时，实际不限）。
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 100_000_000;
/// 工具结果的缺省 token 上限（`MAX_MCP_OUTPUT_TOKENS`）。
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 25_000;

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            tool_timeout_ms: DEFAULT_TOOL_TIMEOUT_MS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            client_version: String::new(),
            startup: super::startup::McpStartup::default(),
        }
    }
}

/// `[^a-zA-Z0-9_-]` 换成 `_`。规则只有
/// 一份，在 kernel-api（ToolSearch 与直接调用时的等待也用它，F36）。
pub use rust_cli_kernel_api::mcp_state::normalize_name;

/// `mcp__<server>__<tool>`。
pub fn tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", normalize_name(server), normalize_name(tool))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
