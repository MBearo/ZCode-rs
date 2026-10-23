//! `ListMcpResourcesTool` 与 `ReadMcpResourceTool`（docs/specs/rust-cli/ext.md §3.7）。有 resources 能力的服务器
//! 连上后才注册。描述与 schema 在 `prompts/tools/*.json`。

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use super::connection;
use super::manager::{Inner, resource_servers};
use super::rpc::RpcError;

pub(crate) const LIST: &str = "ListMcpResourcesTool";
pub(crate) const READ: &str = "ReadMcpResourceTool";
pub(crate) const READ_DIR: &str = "ReadMcpResourceDirTool";
/// MCP SDK 的 `ResourceSchema` 字段顺序：结果是 schema 解析后的对象，键按 schema 的顺序，
/// 未知的键在后。
const RESOURCE_KEYS: &[&str] = &[
    "name",
    "title",
    "uri",
    "description",
    "mimeType",
    "annotations",
    "size",
    "_meta",
];
const MAX_RESULT_CHARS: usize = 100_000;

/// 三个资源工具，按名字排序（和其他内置工具一起按名字排）。
pub(crate) fn tools(manager: Weak<Inner>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListResources {
            manager: manager.clone(),
        }),
        Arc::new(ReadResourceDir),
        Arc::new(ReadResource { manager }),
    ]
}

/// 资源对象按 SDK schema 的顺序重排键，再加上 `server`。
fn with_server(resource: Value, server: &str) -> Value {
    let Value::Object(mut fields) = resource else {
        return resource;
    };
    let mut out = Map::new();
    for key in RESOURCE_KEYS {
        if let Some(value) = fields.remove(*key) {
            out.insert((*key).to_owned(), value);
        }
    }
    out.extend(fields);
    out.insert("server".to_owned(), Value::String(server.to_owned()));
    Value::Object(out)
}

fn prompt_schema(name: &str) -> Value {
    rust_cli_prompts::tool(name)
        .map(|prompt| prompt.input_schema.clone())
        .unwrap_or(Value::Null)
}

fn prompt_description(name: &str) -> String {
    rust_cli_prompts::tool(name)
        .map(|prompt| prompt.description.clone())
        .unwrap_or_default()
}

fn text_output(text: String) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error: false,
        display: Value::Null,
    }
}

fn failed(message: String) -> ToolError {
    ToolError::Failed { message }
}

fn manager(weak: &Weak<Inner>) -> Result<Arc<Inner>, ToolError> {
    weak.upgrade()
        .ok_or_else(|| failed("MCP is shut down".to_owned()))
}

struct ListResources {
    manager: Weak<Inner>,
}

#[async_trait]
impl Tool for ListResources {
    fn name(&self) -> &str {
        LIST
    }

    /// 延迟加载（`shouldDefer: true`，tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("list resources from connected MCP servers")
    }

    fn aliases(&self) -> &[&str] {
        &["ListMcpResources"]
    }

    fn input_schema(&self) -> Value {
        prompt_schema(LIST)
    }

    fn description(&self, _context: &PromptContext) -> String {
        prompt_description(LIST)
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let manager = manager(&self.manager)?;
        let wanted = input.get("server").and_then(Value::as_str);
        let servers = resource_servers(&manager);
        if let Some(wanted) = wanted
            && !servers.iter().any(|(name, _)| name == wanted)
        {
            let names: Vec<&str> = servers.iter().map(|(name, _)| name.as_str()).collect();
            return Err(failed(format!(
                "Server \"{wanted}\" not found. Available servers: {}",
                names.join(", ")
            )));
        }
        let mut all = Vec::new();
        for (name, client) in servers {
            if wanted.is_some_and(|wanted| wanted != name) {
                continue;
            }
            let options = manager.options(&name);
            let timeout = Duration::from_millis(manager.settings().connect_timeout_for(options));
            match connection::list(&client, "resources/list", "resources", timeout).await {
                Ok(resources) => all.extend(
                    resources
                        .into_iter()
                        .map(|resource| with_server(resource, &name)),
                ),
                Err(error) => tracing::warn!(server = name, %error, "MCP resources/list failed"),
            }
        }
        if all.is_empty() {
            return Ok(text_output(
                "No resources found. MCP servers may still provide tools even if they have no resources."
                    .to_owned(),
            ));
        }
        Ok(text_output(Value::Array(all).to_string()))
    }
}

struct ReadResource {
    manager: Weak<Inner>,
}

/// 目录资源的列出：缺省构建没有打开（`isMcpSkillsEnabled` 为假），总是回答未启用。
struct ReadResourceDir;

#[async_trait]
impl Tool for ReadResourceDir {
    fn name(&self) -> &str {
        READ_DIR
    }

    /// 延迟加载（`shouldDefer: true`，tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("list the children of an MCP directory resource")
    }

    fn aliases(&self) -> &[&str] {
        &["ReadMcpResourceDir"]
    }

    fn input_schema(&self) -> Value {
        prompt_schema(READ_DIR)
    }

    fn description(&self, _context: &PromptContext) -> String {
        prompt_description(READ_DIR)
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(text_output(
            "Directory listing is not enabled in this build.".to_owned(),
        ))
    }
}

/// 一项内容：文本原样；二进制不落盘（ext.md §8 D9）。
fn content(item: &Value) -> Value {
    let mut out = Map::new();
    for key in ["uri", "mimeType"] {
        if let Some(value) = item.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(text) = item.get("text") {
        out.insert("text".to_owned(), text.clone());
    } else if item.get("blob").is_some_and(Value::is_string) {
        out.insert(
            "text".to_owned(),
            Value::String("Binary content could not be saved to disk: not supported".to_owned()),
        );
    }
    Value::Object(out)
}

#[async_trait]
impl Tool for ReadResource {
    fn name(&self) -> &str {
        READ
    }

    /// 延迟加载（`shouldDefer: true`，tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("read a specific MCP resource by URI")
    }

    fn aliases(&self) -> &[&str] {
        &["ReadMcpResource"]
    }

    fn input_schema(&self) -> Value {
        prompt_schema(READ)
    }

    fn description(&self, _context: &PromptContext) -> String {
        prompt_description(READ)
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let manager = manager(&self.manager)?;
        let server = input
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let uri = input.get("uri").and_then(Value::as_str).unwrap_or_default();
        let servers = resource_servers(&manager);
        let Some((_, client)) = servers.iter().find(|(name, _)| name == server) else {
            return match manager.client(server) {
                Some(_) => Err(failed(format!(
                    "Server \"{server}\" does not support resources"
                ))),
                None => Err(failed(format!("Server \"{server}\" is not connected"))),
            };
        };
        let options = manager.options(server);
        let timeout = Duration::from_millis(manager.settings().tool_timeout_for(options));
        let read = client
            .request(
                "resources/read",
                json!({"uri": uri}),
                timeout,
                &context.cancel,
            )
            .await;
        let result = match read {
            Ok(result) => result,
            Err(RpcError::Cancelled) => return Err(ToolError::Cancelled),
            Err(RpcError::Server { code: -32601, .. }) => {
                return Ok(text_output(format!(
                    "Server \"{server}\" advertises resource support but does not implement resource reads."
                )));
            }
            Err(error) => return Err(failed(error.to_string())),
        };
        let contents: Vec<Value> = result
            .get("contents")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(content).collect())
            .unwrap_or_default();
        Ok(text_output(json!({"contents": contents}).to_string()))
    }
}
