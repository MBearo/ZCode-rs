//! MCP 的语义部分（docs/specs/rust-cli/ext.md §3）：连接管理、握手、工具包装、结果转换、instructions、
//! resources。传输经 kernel-api 的 `McpTransport`（本地实现在 env-local）。

mod config;
mod connection;
mod manager;
mod node_config;
mod notices;
mod parse;
mod resources;
mod result;
mod rpc;
mod startup;
mod tool;
mod wait_tool;

pub use config::{
    McpScope, McpServer, McpServerOptions, McpServers, McpSettings, normalize_name, tool_name,
};
pub use manager::{McpManager, McpStatus};
pub use node_config::{NodeMcpServer, parse_node_servers};
pub use parse::{expand_vars, merge_scopes, parse_servers};
pub use startup::{
    DEFAULT_CONNECT_WAIT_MS, DEFAULT_PREWAIT_MS, McpStartup, prewait_ms, required_servers,
    wait_for_deferrable,
};
pub(crate) use wait_tool::WaitForMcpServers;
