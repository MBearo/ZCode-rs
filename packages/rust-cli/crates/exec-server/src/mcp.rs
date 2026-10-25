//! MCP 的管道在执行器一侧（docs/specs/rust-cli/remote-exec.md §22.4、§22.5）：连接表、四种消息与期望集合。
//!
//! 服务器进程由注入的 `MachineEnv::mcp()`（执行器里是 env-local 的 `LocalMcpTransport`，打开了 `${VAR}` 展开）起，
//! 与 Bash 同一条进程启动路径。线上的连接 ID 是 `<执行器实例>-<本地 ID>`：执行器重启后不会与旧 ID 重名。
//!
//! 幂等（§22.5，暂定）：键是“服务器名 + 配置指纹”。同一个键已经在跑、归这个客户端实例或一个没连着的实例时接回，
//! 不重启进程；同名而指纹变了时结束旧的、起新的；归一个正连着的别的实例时另起一个（一条 stdio 上不能有两个客户端）。
//! 控制面连上机器时发期望集合（`mcp.declare`），不在里面的结束。传输的事件流只取一次，由连接表拿着，`mcp.events`
//! 订阅它；接回时旧的订阅停掉。
//!
//! 断线不杀：连接表与事件流都属于服务端，ssh 断开时服务器进程照常，事件按 `seq` 留在流里等补发（calls.rs 的流规则）。

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use futures_util::StreamExt;
use futures_util::stream::{BoxStream, Stream};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::core::Workspace;
use rust_cli_exec_wire::dto::mcp as wm;
use rust_cli_exec_wire::envelope::{StreamItem, WireError};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransportConfig, McpTransportEvent,
};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::convert_in as input;
use crate::ledger::CallKey;
use crate::server::Inner;

/// 传输的事件流：只取一次，放在这里；订阅时借走，订阅的流被丢掉时放回（接回的新订阅接着读）。
struct Slot {
    events: Mutex<Option<BoxStream<'static, McpTransportEvent>>>,
    returned: Notify,
    /// 流里出过 Closed 或已经结束：服务器不在了，同一配置再连时另起（不接回）。
    ended: AtomicBool,
}

type Events = Arc<Slot>;

/// 借走的事件流：丢掉时放回去，叫醒等着的下一个订阅。锁不跨 await：只在取与放的那一下拿锁。
struct Lease {
    slot: Events,
    events: Option<BoxStream<'static, McpTransportEvent>>,
}

impl Stream for Lease {
    type Item = McpTransportEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = match self.events.as_mut() {
            Some(events) => events.poll_next_unpin(context),
            None => Poll::Ready(None),
        };
        if let Poll::Ready(None | Some(McpTransportEvent::Closed { .. })) = &next {
            self.slot.ended.store(true, Ordering::SeqCst);
        }
        next
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(events) = self.events.take() {
            *self
                .slot
                .events
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(events);
            self.slot.returned.notify_one();
        }
    }
}

/// 等上一个订阅放回事件流（接回时旧的生产者正被结束），再借走。
async fn lease(slot: Events) -> Lease {
    let events = loop {
        let returned = slot.returned.notified();
        tokio::pin!(returned);
        returned.as_mut().enable();
        let taken = slot
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match taken {
            Some(events) => break events,
            None => returned.await,
        }
    };
    Lease {
        slot,
        events: Some(events),
    }
}

/// 一条连接。
struct Entry {
    /// 现在归哪个客户端实例（接回时换成接回的实例）。
    owner: String,
    server: String,
    fingerprint: String,
    local: McpConnectionId,
    events: Events,
    /// 现在订阅事件的 `mcp.events` 调用。
    subscriber: Option<CallKey>,
}

/// 线上的连接 ID → 连接。
#[derive(Default)]
pub(crate) struct McpTable(Mutex<HashMap<String, Entry>>);

impl McpTable {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 配置指纹：服务器名、工作区根与配置原文（命令、参数、环境变量的名字与值、cwd、URL、headers）的 sha256。只存摘要：
/// 秘密值不以原文留在表里。
fn fingerprint(server: &str, workspace: &Workspace, transport: &wm::McpTransport) -> String {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&(server, &workspace.root, transport)).unwrap_or_default());
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 执行器的 `McpError` → 线上的错误（§22.4）：`Connect` 与 `Io` 是 `io`，`Closed` 是 `not_found`，原文不变。
fn error(error: McpError) -> WireError {
    match error {
        McpError::Connect { message } | McpError::Io { message } => WireError::Io { message },
        McpError::Closed { message } => WireError::NotFound { path: message },
        McpError::Unsupported { what } => WireError::Unsupported { what },
    }
}

fn unknown(connection: &str) -> WireError {
    WireError::NotFound {
        path: format!("MCP connection {connection}"),
    }
}

fn transport(transport: wm::McpTransport) -> McpTransportConfig {
    match transport {
        wm::McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => McpTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        },
        wm::McpTransport::Sse { url, headers } => McpTransportConfig::Sse { url, headers },
        wm::McpTransport::Http { url, headers } => McpTransportConfig::Http { url, headers },
    }
}

fn event(event: McpTransportEvent) -> wm::McpEvent {
    match event {
        McpTransportEvent::Message { message } => wm::McpEvent::Message { message },
        McpTransportEvent::Closed {
            reason,
            session_expired,
        } => wm::McpEvent::Closed {
            reason,
            session_expired,
        },
    }
}

/// 现在连着的客户端实例（先取出来：不在连接表的锁里拿路由表的锁）。
fn online(inner: &Inner) -> HashSet<String> {
    inner.routes().keys().cloned().collect()
}

/// 这条连接这个实例能动：归它，或者归一个没连着的实例。
fn ours(entry: &Entry, instance: &str, online: &HashSet<String>) -> bool {
    entry.owner == instance || !online.contains(&entry.owner)
}

/// 停掉一个旧的事件订阅（接回、换订阅时）：它的流从服务端删掉，生产者随之结束。
async fn drop_subscriber(inner: &Inner, key: Option<CallKey>) {
    if let Some(key) = key {
        let removed = inner.streams().remove(&key);
        drop(removed);
        let _ = inner.ledger.forget(&key).await;
    }
}

async fn close_all(inner: &Inner, entries: Vec<Entry>) {
    for entry in entries {
        if let Err(error) = inner.machine.mcp().close(&entry.local).await {
            tracing::warn!(target: "exec_server", server = %entry.server, %error, "closing an MCP server failed");
        }
    }
}

/// 计划：接回哪条、结束哪些。
struct Plan {
    reattach: Option<(String, Option<CallKey>)>,
    replaced: Vec<Entry>,
}

/// `env` 是这个客户端实例推来的环境变量（F96）：只交给新起的服务器，不进指纹（变了也接回同一个进程）。
async fn connect(
    inner: &Inner,
    instance: &str,
    (workspace, server): (Workspace, String),
    config: wm::McpTransport,
    env: rust_cli_kernel_api::env_command::EnvOverrides,
) -> Result<Reply, WireError> {
    let wanted = fingerprint(&server, &workspace, &config);
    let online = online(inner);
    let plan = {
        let mut table = inner.mcp.lock();
        // 缺陷修复：服务器进程自己退出之后条目还在表里，同一配置再连会接回一个已经退出的进程（握手必然失败）。看过
        // Closed 的条目不接回，按“过期”结束、另起。
        let same = table.iter_mut().find(|(_, entry)| {
            entry.server == server
                && entry.fingerprint == wanted
                && !entry.events.ended.load(Ordering::SeqCst)
                && ours(entry, instance, &online)
        });
        match same {
            Some((connection, entry)) => {
                entry.owner = instance.to_owned();
                Plan {
                    reattach: Some((connection.clone(), entry.subscriber.take())),
                    replaced: Vec::new(),
                }
            }
            None => {
                let stale: Vec<String> = table
                    .iter()
                    .filter(|(_, entry)| entry.server == server && ours(entry, instance, &online))
                    .map(|(connection, _)| connection.clone())
                    .collect();
                Plan {
                    reattach: None,
                    replaced: stale.iter().filter_map(|key| table.remove(key)).collect(),
                }
            }
        }
    };
    if let Some((connection, old)) = plan.reattach {
        tracing::info!(target: "exec_server", %server, %connection, "MCP server reattached");
        drop_subscriber(inner, old).await;
        return Ok(Reply::McpConnect { connection });
    }
    close_all(inner, plan.replaced).await;
    let mcp = inner.machine.mcp();
    let connect = McpConnect {
        server: server.clone(),
        transport: transport(config),
        env,
    };
    let local = mcp
        .connect(&input::workspace(workspace), connect)
        .await
        .map_err(error)?;
    let connection = format!("{}-{}", inner.instance, local.as_str());
    let events = Arc::new(Slot {
        events: Mutex::new(Some(mcp.events(&local))),
        returned: Notify::new(),
        ended: AtomicBool::new(false),
    });
    inner.mcp.lock().insert(
        connection.clone(),
        Entry {
            owner: instance.to_owned(),
            server,
            fingerprint: wanted,
            local,
            events,
            subscriber: None,
        },
    );
    Ok(Reply::McpConnect { connection })
}

/// 期望集合：归这个实例或没连着的实例、但不在集合里的连接结束（配置删了、改了的）；在集合里的不动，缺的不起。
async fn declare(
    inner: &Inner,
    instance: &str,
    workspace: &Workspace,
    servers: Vec<wm::McpDeclared>,
) -> Reply {
    let wanted: HashSet<(String, String)> = servers
        .into_iter()
        .map(|declared| {
            let print = fingerprint(&declared.server, workspace, &declared.transport);
            (declared.server, print)
        })
        .collect();
    let online = online(inner);
    let gone: Vec<Entry> = {
        let mut table = inner.mcp.lock();
        let keys: Vec<String> = table
            .iter()
            .filter(|(_, entry)| {
                ours(entry, instance, &online)
                    && !wanted.contains(&(entry.server.clone(), entry.fingerprint.clone()))
            })
            .map(|(key, _)| key.clone())
            .collect();
        keys.iter().filter_map(|key| table.remove(key)).collect()
    };
    let mut closed: Vec<String> = gone.iter().map(|entry| entry.server.clone()).collect();
    closed.sort();
    if !closed.is_empty() {
        tracing::info!(target: "exec_server", ?closed, "MCP servers not declared any more were closed");
    }
    close_all(inner, gone).await;
    Reply::McpDeclare { closed }
}

/// 执行一条不是流的 MCP 消息。
pub(crate) async fn unary(
    inner: &Inner,
    instance: &str,
    request: Request,
    env: rust_cli_kernel_api::env_command::EnvOverrides,
) -> Result<Reply, WireError> {
    match request {
        Request::McpConnect {
            workspace,
            server,
            transport,
        } => connect(inner, instance, (workspace, server), transport, env).await,
        Request::McpSend {
            connection,
            message,
        } => {
            let local = inner
                .mcp
                .lock()
                .get(&connection)
                .filter(|entry| entry.owner == instance)
                .map(|entry| entry.local.clone())
                .ok_or_else(|| unknown(&connection))?;
            inner
                .machine
                .mcp()
                .send(&local, message)
                .await
                .map_err(error)?;
            Ok(Reply::McpSend)
        }
        Request::McpClose { connection } => {
            // 关掉不认识的（已经关过的、别的实例的）连接不做事。
            let removed = {
                let mut table = inner.mcp.lock();
                let owned = table
                    .get(&connection)
                    .is_some_and(|entry| entry.owner == instance);
                if owned {
                    table.remove(&connection)
                } else {
                    None
                }
            };
            if let Some(entry) = removed {
                inner
                    .machine
                    .mcp()
                    .close(&entry.local)
                    .await
                    .map_err(error)?;
            }
            Ok(Reply::McpClose)
        }
        Request::McpDeclare { workspace, servers } => {
            Ok(declare(inner, instance, &workspace, servers).await)
        }
        other => Err(WireError::BadRequest {
            message: format!("{} is not an MCP call", other.op().as_str()),
        }),
    }
}

/// `mcp.events`：订阅这条连接的事件（`key` 是这个流的调用）；不认识的连接是一项错误。
pub(crate) async fn events(
    inner: &Inner,
    key: &CallKey,
    request: Request,
) -> BoxStream<'static, (StreamItem, Body)> {
    let found = match &request {
        Request::McpEvents { connection } => {
            let mut table = inner.mcp.lock();
            match table
                .get_mut(connection)
                .filter(|entry| entry.owner == key.instance)
            {
                Some(entry) => {
                    let old = entry.subscriber.replace(key.clone());
                    Ok((entry.events.clone(), old.filter(|old| old != key)))
                }
                None => Err(unknown(connection)),
            }
        }
        other => Err(WireError::BadRequest {
            message: format!("{} is not an MCP stream", other.op().as_str()),
        }),
    };
    match found {
        Ok((shared, old)) => {
            drop_subscriber(inner, old).await;
            futures_util::stream::once(lease(shared))
                .flatten()
                .map(|item| (StreamItem::Mcp { event: event(item) }, Body::empty()))
                .boxed()
        }
        Err(error) => {
            futures_util::stream::iter([(StreamItem::Error { error }, Body::empty())]).boxed()
        }
    }
}

/// 一个客户端实例被清掉（断开太久，§19.7）：还归它的连接关掉（服务器进程结束）。
pub(crate) async fn forget(inner: &Inner, instance: &str) {
    let gone: Vec<Entry> = {
        let mut table = inner.mcp.lock();
        let keys: Vec<String> = table
            .iter()
            .filter(|(_, entry)| entry.owner == instance)
            .map(|(key, _)| key.clone())
            .collect();
        keys.iter().filter_map(|key| table.remove(key)).collect()
    };
    close_all(inner, gone).await;
}
