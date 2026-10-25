//! ext 测试共用的替身：执行环境事实、内存里的 MCP 服务器（传输端口的替身）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, McpTransportEvent,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

pub(crate) const CWD: &str = "/Users/dev/project";
pub(crate) const HOME: &str = "/Users/dev";

pub(crate) fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new(CWD),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new(HOME)),
    }
}

pub(crate) fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(CWD),
    }
}

/// 服务器对一条请求的回答：`Ok(result)` 或 `Err((code, message))`；`None` 表示不回答（挂起）。
pub(crate) type Handler =
    dyn Fn(&str, &Value) -> Option<Result<Value, (i64, String)>> + Send + Sync;

struct Connection {
    events: Option<mpsc::UnboundedReceiver<McpTransportEvent>>,
    sender: mpsc::UnboundedSender<McpTransportEvent>,
    server: String,
}

/// 内存里的 MCP 服务器：每个服务器名一个处理函数；记录收到的消息。
#[derive(Default)]
pub(crate) struct FakeMcp {
    handlers: Mutex<HashMap<String, Arc<Handler>>>,
    connections: Mutex<HashMap<String, Connection>>,
    received: Mutex<Vec<(String, Value)>>,
    next: AtomicUsize,
    /// 连接失败的服务器。
    refuse: Mutex<Vec<String>>,
    /// [`FakeMcp::release`] 过的服务器的 `initialize` 结果：之后（含重连）的握手直接回它。
    released: Mutex<HashMap<String, Value>>,
    /// 每次连接的（服务器名，工作区根）。
    roots: Mutex<Vec<(String, String)>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 一个能握手、有给定能力与 instructions 的服务器；其余请求交给 `rest`。
pub(crate) fn server(
    capabilities: Value,
    instructions: Option<&str>,
    rest: impl Fn(&str, &Value) -> Option<Result<Value, (i64, String)>> + Send + Sync + 'static,
) -> Arc<Handler> {
    let instructions = instructions.map(str::to_owned);
    Arc::new(move |method: &str, params: &Value| match method {
        "initialize" => {
            let mut result = json!({
                "protocolVersion": "2025-06-18",
                "capabilities": capabilities,
                "serverInfo": {"name": "fake", "version": "1"},
            });
            if let Some(text) = &instructions {
                result["instructions"] = json!(text);
            }
            Some(Ok(result))
        }
        other => rest(other, params),
    })
}

/// 握手不回答的服务器（慢服务器，ext.md §3.3.1）：[`FakeMcp::release`] 时才回 `initialize`；其余请求交给 `rest`。
pub(crate) fn held(
    rest: impl Fn(&str, &Value) -> Option<Result<Value, (i64, String)>> + Send + Sync + 'static,
) -> Arc<Handler> {
    Arc::new(move |method: &str, params: &Value| match method {
        "initialize" => None,
        other => rest(other, params),
    })
}

impl FakeMcp {
    /// 完成 [`held`] 的服务器的握手：回答已经挂起的 `initialize`；还没发来时，之后发来就直接回答。
    pub(crate) fn release(&self, server: &str, capabilities: Value, instructions: Option<&str>) {
        let mut result = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": capabilities,
            "serverInfo": {"name": "fake", "version": "1"},
        });
        if let Some(text) = instructions {
            result["instructions"] = json!(text);
        }
        lock(&self.released).insert(server.to_owned(), result.clone());
        let id = lock(&self.received)
            .iter()
            .rev()
            .find(|(name, message)| name == server && message["method"] == "initialize")
            .map(|(_, message)| message["id"].clone());
        if let Some(id) = id {
            self.push(
                server,
                json!({"jsonrpc": "2.0", "id": id, "result": result}),
            );
        }
    }

    pub(crate) fn with(self, name: &str, handler: Arc<Handler>) -> Self {
        lock(&self.handlers).insert(name.to_owned(), handler);
        self
    }

    pub(crate) fn refusing(self, name: &str) -> Self {
        lock(&self.refuse).push(name.to_owned());
        self
    }

    /// 收到的消息（服务器名，消息）。
    pub(crate) fn received(&self) -> Vec<(String, Value)> {
        lock(&self.received).clone()
    }

    /// 每次连接的（服务器名，工作区根）。
    pub(crate) fn roots(&self) -> Vec<(String, String)> {
        lock(&self.roots).clone()
    }

    /// 服务器主动发一条消息（通知或请求）。
    pub(crate) fn push(&self, server: &str, message: Value) {
        for connection in lock(&self.connections).values() {
            if connection.server == server {
                let _ = connection.sender.send(McpTransportEvent::Message {
                    message: message.clone(),
                });
            }
        }
    }

    /// 关闭某个服务器的全部连接。
    pub(crate) fn drop_server(&self, server: &str, session_expired: bool) {
        for connection in lock(&self.connections).values() {
            if connection.server == server {
                let _ = connection.sender.send(McpTransportEvent::Closed {
                    reason: Some("gone".to_owned()),
                    session_expired,
                });
            }
        }
    }
}

#[async_trait]
impl McpTransport for FakeMcp {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        lock(&self.roots).push((request.server.clone(), workspace.root.as_str().to_owned()));
        if lock(&self.refuse).contains(&request.server) {
            return Err(McpError::Connect {
                message: "spawn failed".to_owned(),
            });
        }
        let id = format!("c{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (sender, events) = mpsc::unbounded_channel();
        lock(&self.connections).insert(
            id.clone(),
            Connection {
                events: Some(events),
                sender,
                server: request.server,
            },
        );
        Ok(McpConnectionId::new(id))
    }

    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError> {
        let (server, sender) = {
            let connections = lock(&self.connections);
            let found = connections
                .get(connection.as_str())
                .ok_or_else(|| McpError::Closed {
                    message: "unknown connection".to_owned(),
                })?;
            (found.server.clone(), found.sender.clone())
        };
        lock(&self.received).push((server.clone(), message.clone()));
        let (Some(id), Some(method)) = (
            message.get("id").cloned(),
            message.get("method").and_then(Value::as_str),
        ) else {
            return Ok(());
        };
        let handler = lock(&self.handlers).get(&server).cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let released = (method == "initialize")
            .then(|| lock(&self.released).get(&server).cloned())
            .flatten();
        let reply = match released {
            Some(result) => Some(Ok(result)),
            None => handler.and_then(|handler| handler(method, &params)),
        };
        let reply = match reply {
            None => return Ok(()),
            Some(Ok(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Some(Err((code, text))) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}})
            }
        };
        let _ = sender.send(McpTransportEvent::Message { message: reply });
        Ok(())
    }

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        let events = lock(&self.connections)
            .get_mut(connection.as_str())
            .and_then(|found| found.events.take());
        match events {
            Some(mut events) => {
                futures_util::stream::poll_fn(move |context| events.poll_recv(context)).boxed()
            }
            None => futures_util::stream::empty().boxed(),
        }
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        if let Some(found) = lock(&self.connections).get(connection.as_str()) {
            let _ = found.sender.send(McpTransportEvent::Closed {
                reason: None,
                session_expired: false,
            });
        }
        Ok(())
    }
}

/// 一次工具调用的上下文（替身端口）。
pub(crate) fn tool_context<E: rust_cli_kernel_api::testing::AllPorts + 'static>(
    env: Arc<E>,
    mode: rust_cli_kernel_api::permission::PermissionMode,
) -> rust_cli_kernel_api::tool::ToolUseContext {
    use rust_cli_kernel_api::env::{OperationContext, Origin};
    use rust_cli_kernel_api::ids::{
        Generation, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
    };
    use rust_cli_kernel_api::testing::{MemoryStore, RecordingSink, local_services, machine};
    let session = SessionId::new("sess_1");
    let run = RunId::new("run_1");
    rust_cli_kernel_api::tool::ToolUseContext {
        session: session.clone(),
        run: run.clone(),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: workspace(),
        mode,
        cancel: tokio_util::sync::CancellationToken::new(),
        machine: machine(env.clone()),
        local: local_services(env),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(RecordingSink::default()),
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g1"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_call"),
            origin: Origin::Agent { session, run },
        },
    }
}

/// 记下命令的 Bash 替身：输出 `out:<命令>`；命令含 `fail` 时结果是错误。
#[derive(Default)]
pub(crate) struct EchoBash {
    pub commands: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl rust_cli_kernel_api::tool::Tool for EchoBash {
    fn name(&self) -> &str {
        "Bash"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        "bash".to_owned()
    }

    async fn call(
        &self,
        input: Value,
        context: rust_cli_kernel_api::tool::ToolUseContext,
        _progress: mpsc::Sender<rust_cli_kernel_api::tool::ToolProgress>,
    ) -> Result<rust_cli_kernel_api::tool::ToolOutput, rust_cli_kernel_api::tool::ToolError> {
        let command = input["command"].as_str().unwrap_or_default().to_owned();
        lock(&self.commands).push((command.clone(), context.operation.operation.to_string()));
        Ok(rust_cli_kernel_api::tool::ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: format!("out:{command}"),
            }],
            is_error: command.contains("fail"),
            display: Value::Null,
        })
    }
}
