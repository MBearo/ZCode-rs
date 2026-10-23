//! MCP 工具的包装（docs/specs/rust-cli/ext.md §3.4）：名字
//! `mcp__<server>__<tool>`、描述截断、只读标记、权限交给判决链、调用与结果转换。

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::mcp::McpToolInfo;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::config::tool_name;
use super::connection::truncate_description;
use super::manager::Inner;
use super::result;
use super::rpc::RpcError;

/// 结果的字符上限。
const MAX_RESULT_CHARS: usize = 100_000;

/// 发给模型的工具名最长 128 个 UTF-16 码元：API 拒绝更长的名字，整个请求失败（ext.md §3.4）。
pub(crate) const MAX_TOOL_NAME_UNITS: usize = 128;
/// 名字太长的工具每个名字只记一次日志，最多记这么多个名字。
const NOTED_NAMES: usize = 256;

/// 这个工具不发给模型：名字超过 [`MAX_TOOL_NAME_UNITS`]。
///
/// 修复线 F67：以前只把名字规范化、不看长度，服务器列出一个名字太长的工具，之后每个请求都被 API 拒绝。
pub(crate) fn name_too_long(name: &str) -> bool {
    name.encode_utf16().count() > MAX_TOOL_NAME_UNITS
}

/// 服务器列出的工具里名字太长的，各记一次 warn（`noted` 是管理器里记过的名字）。
pub(crate) fn note_long_names(
    server: &str,
    tools: &[Arc<dyn Tool>],
    noted: &Mutex<HashSet<String>>,
) {
    let mut noted = noted.lock().unwrap_or_else(PoisonError::into_inner);
    for tool in tools.iter().filter(|tool| name_too_long(tool.name())) {
        let name = tool.name();
        if noted.len() >= NOTED_NAMES || !noted.insert(name.to_owned()) {
            continue;
        }
        let length = name.encode_utf16().count();
        tracing::warn!(
            event = "mcp.tool.skipped",
            server,
            tool = name,
            length,
            "Tool \"{name}\" is not sent to the model: its name has {length} characters and the API accepts at most {MAX_TOOL_NAME_UNITS}"
        );
    }
}

pub(crate) struct McpTool {
    name: String,
    info: McpToolInfo,
    description: String,
    schema: Value,
    read_only: bool,
    manager: Weak<Inner>,
}

impl McpTool {
    /// `tools/list` 的一项；没有名字的项为 None。不延迟：服务器配置的 `alwaysLoad`（`server_always_load`）
    /// 或工具的 `_meta["zcode/alwaysLoad"] === true`（ext.md §3.1、D35）。
    pub(crate) fn from_listed(
        server: &str,
        listed: &Value,
        server_always_load: bool,
        manager: Weak<Inner>,
    ) -> Option<Self> {
        let original = listed.get("name").and_then(Value::as_str)?.to_owned();
        let always_load = server_always_load
            || listed
                .pointer("/_meta/zcode~1alwaysLoad")
                .and_then(Value::as_bool)
                == Some(true);
        Some(Self {
            name: tool_name(server, &original),
            description: listed
                .get("description")
                .and_then(Value::as_str)
                .map(truncate_description)
                .unwrap_or_default(),
            schema: listed
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"})),
            read_only: listed
                .pointer("/annotations/readOnlyHint")
                .and_then(Value::as_bool)
                == Some(true),
            info: McpToolInfo {
                server: server.to_owned(),
                tool: original,
                always_load,
            },
            manager,
        })
    }
}

fn failed(message: String) -> ToolError {
    ToolError::Failed { message }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.description.clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        self.read_only
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        self.read_only
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    fn mcp_info(&self) -> Option<McpToolInfo> {
        Some(self.info.clone())
    }

    /// MCP 工具除 `alwaysLoad` 外一律延迟（tools-more.md §3.1）。
    fn is_mcp(&self) -> bool {
        true
    }

    /// 与 `mcp_info().always_load` 同源（服务器的 `alwaysLoad` 或工具的 `_meta["zcode/alwaysLoad"]`）。
    fn always_load(&self) -> bool {
        self.info.always_load
    }

    /// 交给判决链（返回 `passthrough`，询问文本由判决链生成；建议的整个工具名规则只用于
    /// 界面，`Passthrough` 不带建议）。
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Passthrough
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| failed("MCP is shut down".to_owned()))?;
        let server = &self.info.server;
        let settings = manager.settings();
        // 这个服务器的 `timeoutMs`，没有时 `MCP_TOOL_TIMEOUT`（ext.md §3.1）。
        let timeout_ms = settings.tool_timeout_for(manager.options(server));
        let params = json!({
            "name": self.info.tool,
            "arguments": input,
            // 只带本工程的键：tool_use 的 ID（ext.md D35）。
            "_meta": {"zcode/toolUseId": context.tool_use},
        });
        let timeout = Duration::from_millis(timeout_ms);
        let mut retried = false;
        let value = loop {
            let client = manager
                .client(server)
                .ok_or_else(|| failed(format!("MCP server \"{server}\" is not connected")))?;
            match client
                .request("tools/call", params.clone(), timeout, &context.cancel)
                .await
            {
                Ok(value) => break value,
                Err(RpcError::Cancelled) => return Err(ToolError::Cancelled),
                Err(RpcError::TimedOut) => {
                    return Err(failed(format!(
                        "MCP server \"{server}\" tool \"{}\" timed out after {}s",
                        self.info.tool,
                        timeout_ms / 1000
                    )));
                }
                // 会话过期（HTTP 404）：等连接任务重连后重试一次。
                Err(RpcError::Closed(_)) if !retried && manager.expired(server) => {
                    retried = true;
                    manager.wait_reconnected(server, &client).await;
                }
                Err(error) => return Err(failed(error.to_string())),
            }
        };
        // 服务器报告的错误（`isError`）：结果是原文、标记为错误，不加 `<tool_use_error>`。
        if let Some(message) = result::error_text(&value) {
            return Ok(ToolOutput {
                content: vec![ToolResultContent::Text { text: message }],
                is_error: true,
                display: Value::Null,
            });
        }
        let content = result::transform(&value, server).ok_or_else(|| {
            failed(format!(
                "MCP server \"{server}\" tool \"{}\": unexpected response format",
                self.info.tool
            ))
        })?;
        Ok(ToolOutput {
            content: result::truncate(content, settings.max_output_tokens),
            is_error: false,
            display: Value::Null,
        })
    }
}
