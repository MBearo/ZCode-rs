//! MCP 连接管理（docs/specs/rust-cli/ext.md §3.3）：每个服务器一个连接任务，状态与工具快照只在这里，
//! 注册表每步读当前快照；`list_changed` 重新拉取；HTTP / SSE 断开后退避重连。

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rust_cli_base::Spawner;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::listing::ListingEntry;
use rust_cli_kernel_api::mcp::{McpTransport, McpTransportConfig};
use rust_cli_kernel_api::tool::Tool;
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::config::{McpServer, McpServerOptions, McpSettings};
use super::connection::{self, Session, has_capability};
use super::resources;
use super::rpc::{Client, Notice};
use super::tool::{McpTool, name_too_long, note_long_names};

/// 断开后重连的次数与退避。
const MAX_RECONNECTS: u32 = 5;
const INITIAL_BACKOFF_MS: u64 = 1_000;
const MAX_BACKOFF_MS: u64 = 30_000;

/// 一个服务器的连接状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpStatus {
    Pending,
    Connected,
    Failed {
        reason: String,
    },
    /// 停用（配置里 `enabled: false`，或 `/mcp disable`，docs/specs/rust-cli/tui-manage.md §6）：不连接。
    Disabled,
}

struct Server {
    config: McpServer,
    status: McpStatus,
    client: Option<Arc<Client>>,
    tools: Vec<Arc<dyn Tool>>,
    instructions: Option<String>,
    resources: bool,
    prompts: Vec<Value>,
    /// 最近一次断开是 HTTP 会话过期（工具调用据此重试一次）。
    expired: bool,
    /// 连上过、断开后在退避重连（HTTP / SSE）；状态是 Pending（直接调用时不等它，ext.md §3.8）。
    reconnecting: bool,
    /// 连接任务的代号：重连、停用时加一；旧任务的状态更新因代号不符被丢弃（tui-manage.md §6.1）。
    generation: u64,
    /// 停掉当前连接任务的取消消息。
    cancel: CancellationToken,
}

impl Server {
    fn new(config: McpServer, status: McpStatus) -> Self {
        Self {
            config,
            status,
            client: None,
            tools: Vec::new(),
            instructions: None,
            resources: false,
            prompts: Vec::new(),
            expired: false,
            reconnecting: false,
            generation: 0,
            cancel: CancellationToken::new(),
        }
    }
}

pub(crate) struct Inner {
    transport: Arc<dyn McpTransport>,
    spawner: Spawner,
    settings: McpSettings,
    servers: Mutex<BTreeMap<String, Server>>,
    /// 每次状态变化加一；等待方据此醒来重新检查。
    changed: watch::Sender<u64>,
    /// `start` 给的工作区（重连与启用时用）。
    workspace: Mutex<Option<WorkspaceRef>>,
    /// 等待的截止时刻用注入的时钟算（ext.md §3.3.1、§3.8）。
    clock: Arc<dyn Clock>,
    /// 主线程的请求带过 WaitForMcpServers 的会话（declared，ext.md §3.8）；会话关闭时去掉。
    declared: Mutex<HashSet<SessionId>>,
    /// 名字太长、记过日志的工具（每个名字只记一次，ext.md §3.4）。
    noted_names: Mutex<HashSet<String>>,
}

impl Inner {
    fn servers(&self) -> MutexGuard<'_, BTreeMap<String, Server>> {
        self.servers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 只改仍是这个代号的服务器：重连或停用之后，旧连接任务的更新不覆盖新的状态。
    fn update(&self, name: &str, generation: u64, change: impl FnOnce(&mut Server)) {
        if let Some(server) = self
            .servers()
            .get_mut(name)
            .filter(|server| server.generation == generation)
        {
            change(server);
        }
        self.changed.send_modify(|version| *version += 1);
    }

    pub(crate) fn settings(&self) -> &McpSettings {
        &self.settings
    }

    /// 这个服务器的选项（Node 配置的 `timeoutMs`、`alwaysLoad`，ext.md §3.1）；不认识的服务器用缺省值。
    pub(crate) fn options(&self, name: &str) -> McpServerOptions {
        self.servers()
            .get(name)
            .map(|server| server.config.options)
            .unwrap_or_default()
    }

    /// 已连接的服务器的客户端。
    pub(crate) fn client(&self, name: &str) -> Option<Arc<Client>> {
        self.servers()
            .get(name)
            .filter(|server| server.status == McpStatus::Connected)
            .and_then(|server| server.client.clone())
    }

    pub(crate) fn expired(&self, name: &str) -> bool {
        self.servers()
            .get(name)
            .is_some_and(|server| server.expired)
    }

    /// 等服务器换上新客户端（重连成功）或不再重连。
    pub(crate) async fn wait_reconnected(&self, name: &str, old: &Arc<Client>) {
        let mut changes = self.changed.subscribe();
        loop {
            let settled = match self.servers().get(name) {
                Some(server) => match &server.status {
                    McpStatus::Connected => server
                        .client
                        .as_ref()
                        .is_some_and(|client| !Arc::ptr_eq(client, old)),
                    McpStatus::Failed { .. } | McpStatus::Disabled => true,
                    McpStatus::Pending => false,
                },
                None => true,
            };
            if settled || changes.changed().await.is_err() {
                return;
            }
        }
    }

    async fn tools_of(self: &Arc<Self>, server: &str, client: &Client) -> Vec<Arc<dyn Tool>> {
        let options = self.options(server);
        let timeout = Duration::from_millis(self.settings.connect_timeout_for(options));
        match connection::list(client, "tools/list", "tools", timeout).await {
            Ok(listed) => {
                let tools: Vec<Arc<dyn Tool>> = listed
                    .iter()
                    .filter_map(|item| {
                        McpTool::from_listed(
                            server,
                            item,
                            options.always_load,
                            Arc::downgrade(self),
                        )
                    })
                    .map(|tool| Arc::new(tool) as Arc<dyn Tool>)
                    .collect();
                note_long_names(server, &tools, &self.noted_names);
                tools
            }
            Err(error) => {
                tracing::warn!(server, %error, "MCP tools/list failed");
                Vec::new()
            }
        }
    }

    async fn prompts_of(&self, server: &str, client: &Client) -> Vec<Value> {
        let timeout =
            Duration::from_millis(self.settings.connect_timeout_for(self.options(server)));
        connection::list(client, "prompts/list", "prompts", timeout)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(server, %error, "MCP prompts/list failed");
                Vec::new()
            })
    }

    /// 连上之后：拉取清单、发布，处理通知直到连接关闭；返回关闭原因与是否会话过期。
    async fn serve(
        self: &Arc<Self>,
        name: &str,
        generation: u64,
        session: Session,
    ) -> (Option<String>, bool) {
        let Session {
            client,
            mut notices,
            capabilities,
            instructions,
        } = session;
        let tools = if has_capability(&capabilities, "tools") {
            self.tools_of(name, &client).await
        } else {
            Vec::new()
        };
        let prompts = if has_capability(&capabilities, "prompts") {
            self.prompts_of(name, &client).await
        } else {
            Vec::new()
        };
        let resources = has_capability(&capabilities, "resources");
        let published = client.clone();
        self.update(name, generation, move |server| {
            server.status = McpStatus::Connected;
            server.client = Some(published);
            server.tools = tools;
            server.prompts = prompts;
            server.instructions = instructions;
            server.resources = resources;
            server.expired = false;
            server.reconnecting = false;
        });
        tracing::info!(server = name, "MCP server connected");
        while let Some(notice) = notices.recv().await {
            match notice {
                Notice::ListChanged(kind) if kind == "tools" => {
                    let tools = self.tools_of(name, &client).await;
                    self.update(name, generation, |server| server.tools = tools);
                }
                Notice::ListChanged(kind) if kind == "prompts" => {
                    let prompts = self.prompts_of(name, &client).await;
                    self.update(name, generation, |server| server.prompts = prompts);
                }
                // resources 不缓存清单：ListMcpResourcesTool 每次实时拉取。
                Notice::ListChanged(_) => {}
                Notice::Closed {
                    reason,
                    session_expired,
                } => return (reason, session_expired),
            }
        }
        (None, false)
    }

    /// 一个服务器的连接任务：收到取消消息（重连、停用）时停下（tui-manage.md §6.1）。
    async fn run(
        self: Arc<Self>,
        workspace: WorkspaceRef,
        config: McpServer,
        generation: u64,
        cancel: CancellationToken,
    ) {
        tokio::select! {
            () = cancel.cancelled() => {}
            () = self.clone().connect(workspace, config, generation) => {}
        }
    }

    /// 连接与重连（ext.md §3.3）：首次连接失败即失败；连上后断开时 stdio 失败，HTTP / SSE 退避重连。
    async fn connect(self: Arc<Self>, workspace: WorkspaceRef, config: McpServer, generation: u64) {
        let remote = !matches!(config.transport, McpTransportConfig::Stdio { .. });
        let timeout = Duration::from_millis(self.settings.connect_timeout_for(config.options));
        let name = config.name.clone();
        let mut connected_once = false;
        let mut attempts = 0;
        loop {
            let reason = match connection::open(
                &self.transport,
                &self.spawner,
                &workspace,
                &config,
                timeout,
                &self.settings.client_version,
            )
            .await
            {
                Ok(session) => {
                    connected_once = true;
                    attempts = 0;
                    let (reason, expired) = self.serve(&name, generation, session).await;
                    self.update(&name, generation, |server| {
                        server.status = McpStatus::Pending;
                        server.client = None;
                        server.tools.clear();
                        server.expired = expired;
                        // stdio 不重连：下面马上记为失败。
                        server.reconnecting = remote;
                    });
                    reason.unwrap_or_else(|| "connection closed".to_owned())
                }
                Err(reason) => reason,
            };
            if !remote || !connected_once || attempts >= MAX_RECONNECTS {
                tracing::warn!(
                    server = name,
                    reason = logged_reason(&reason),
                    "MCP server failed"
                );
                self.update(&name, generation, |server| {
                    server.status = McpStatus::Failed { reason };
                    server.client = None;
                    server.tools.clear();
                    server.reconnecting = false;
                });
                return;
            }
            let backoff = (INITIAL_BACKOFF_MS << attempts).min(MAX_BACKOFF_MS);
            attempts += 1;
            tracing::info!(
                server = name,
                attempts,
                backoff,
                "reconnecting the MCP server"
            );
            tokio::time::sleep(Duration::from_millis(backoff)).await;
        }
    }
}

/// MCP 连接管理。
pub struct McpManager(Arc<Inner>);

impl McpManager {
    pub fn new(
        transport: Arc<dyn McpTransport>,
        spawner: Spawner,
        settings: McpSettings,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (changed, _) = watch::channel(0);
        Self(Arc::new(Inner {
            transport,
            spawner,
            settings,
            servers: Mutex::default(),
            changed,
            workspace: Mutex::default(),
            clock,
            declared: Mutex::default(),
            noted_names: Mutex::default(),
        }))
    }

    /// 开始连接（每个服务器一个常驻任务，不等待）。同名的服务器只连一次。
    pub fn start(&self, workspace: WorkspaceRef, servers: Vec<McpServer>) {
        *self
            .0
            .workspace
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(workspace.clone());
        for config in servers {
            let cancel = {
                let mut known = self.0.servers();
                if known.contains_key(&config.name) {
                    continue;
                }
                let server = Server::new(config.clone(), McpStatus::Pending);
                let cancel = server.cancel.clone();
                known.insert(config.name.clone(), server);
                cancel
            };
            let task = self.0.clone().run(workspace.clone(), config, 0, cancel);
            self.0.spawner.spawn_resident("mcp-connection", task);
        }
        self.0.changed.send_modify(|version| *version += 1);
    }

    /// 有 resources 能力的服务器时先是三个资源工具，然后是已连接的服务器的工具（按名字排序）。
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        tools_of(&self.0, &self.0.servers())
    }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        // 旧名也能找到（资源工具的 `ListMcpResources` 等，与内置工具的查找相同）。
        self.tools()
            .into_iter()
            .find(|tool| tool.name() == name || tool.aliases().contains(&name))
    }

    /// 已连接、有 instructions 的服务器，按名字排序：`## <server>\n<instructions>`（ext.md §3.6）。
    pub fn instructions(&self) -> Vec<ListingEntry> {
        instructions_of(&self.0.servers())
    }

    /// 每个服务器的状态（按名字排序）。
    pub fn status(&self) -> Vec<(String, McpStatus)> {
        self.0
            .servers()
            .values()
            .map(|server| (server.config.name.clone(), server.status.clone()))
            .collect()
    }

    /// `/mcp` 与 `/status` 的快照（docs/specs/rust-cli/tui-display.md §3.4）：状态、失败原因、已连接时的工具数。
    pub fn server_infos(&self) -> Vec<rust_cli_kernel_api::manage::McpServerInfo> {
        use rust_cli_kernel_api::manage::{McpServerInfo, McpServerState};
        self.0
            .servers()
            .values()
            .map(|server| {
                let (state, reason) = match &server.status {
                    McpStatus::Pending => (McpServerState::Pending, None),
                    McpStatus::Connected => (McpServerState::Connected, None),
                    McpStatus::Failed { reason } => (McpServerState::Failed, Some(reason.clone())),
                    McpStatus::Disabled => (McpServerState::Disabled, None),
                };
                McpServerInfo {
                    name: server.config.name.clone(),
                    state,
                    reason,
                    tools: server.tools.len(),
                }
            })
            .collect()
    }

    /// 服务器的 prompts（`prompts/list` 的原样结果）。
    pub fn prompts(&self, server: &str) -> Vec<Value> {
        self.0
            .servers()
            .get(server)
            .map(|server| server.prompts.clone())
            .unwrap_or_default()
    }

    /// 等全部服务器离开 `Pending`（连上或失败）。
    pub async fn wait_ready(&self) {
        let mut changes = self.0.changed.subscribe();
        loop {
            let pending = self
                .0
                .servers()
                .values()
                .any(|server| server.status == McpStatus::Pending);
            if !pending || changes.changed().await.is_err() {
                return;
            }
        }
    }
}

/// 有 resources 能力的服务器时先是三个资源工具，然后是已连接的服务器的工具（按名字排序）。`servers` 是调用方
/// 持有的锁里的表：快照（manager_state.rs）与工具、状态在同一把锁里取。这是给模型的工具表：名字太长的工具不在里面
/// （主线程、子代理、ToolSearch 与直接调用都从这里取，ext.md §3.4）；`/mcp` 的工具数仍按服务器列出的全部工具。
fn tools_of(inner: &Arc<Inner>, servers: &BTreeMap<String, Server>) -> Vec<Arc<dyn Tool>> {
    let connected = || {
        servers
            .values()
            .filter(|server| server.status == McpStatus::Connected)
    };
    let mut tools: Vec<Arc<dyn Tool>> = if connected().any(|server| server.resources) {
        resources::tools(Arc::downgrade(inner))
    } else {
        Vec::new()
    };
    let mut own: Vec<Arc<dyn Tool>> = connected()
        .flat_map(|server| server.tools.iter())
        .filter(|tool| !name_too_long(tool.name()))
        .cloned()
        .collect();
    own.sort_by(|left, right| left.name().cmp(right.name()));
    tools.extend(own);
    tools
}

/// 已连接、有 instructions 的服务器，按名字排序：`## <server>\n<instructions>`（ext.md §3.6）。
fn instructions_of(servers: &BTreeMap<String, Server>) -> Vec<ListingEntry> {
    servers
        .values()
        .filter(|server| server.status == McpStatus::Connected)
        .filter_map(|server| {
            let text = server.instructions.as_ref()?;
            Some(ListingEntry {
                name: server.config.name.clone(),
                line: format!("## {}\n{text}", server.config.name),
            })
        })
        .collect()
}

/// 已连接、有 resources 能力的服务器与它们的客户端（资源工具用）。
pub(crate) fn resource_servers(inner: &Inner) -> Vec<(String, Arc<Client>)> {
    inner
        .servers()
        .values()
        .filter(|server| server.status == McpStatus::Connected && server.resources)
        .filter_map(|server| Some((server.config.name.clone(), server.client.clone()?)))
        .collect()
}

#[path = "manager_control.rs"]
mod control;

#[path = "manager_state.rs"]
mod state;

/// 失败原因写进日志的部分：前 300 个字符（与 `tool.failed` 相同，logging.md §4）。
///
/// 缺陷修复 R2-14：原来整段按 warn 记日志；HTTP 错误的原因带最多 64 KiB 的响应体（env-local
/// `ERROR_BODY_LIMIT`）。界面上的原因（`McpStatus::Failed`）不截断。
///
/// 截取只定义在 base 的 `log_text::head`（F58：以前这里与 query 各写一份）。
pub(crate) fn logged_reason(reason: &str) -> String {
    rust_cli_base::log_text::head(reason)
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "manager_names_tests.rs"]
mod names_tests;
