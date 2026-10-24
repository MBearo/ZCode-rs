//! 执行器模式的组装（docs/specs/rust-cli/remote-exec.md §12、§15.3、§18.6、§19）：`zcode executor` 子命令由 cli 转到这里。
//!
//! - [`resident`]：常驻执行器（单实例锁、本用户私有的 unix socket、管理帧、文件版账本），一直跑到 `stop`；
//! - [`proxy`]：`--stdio` 的代理（没有常驻进程就拉起，版本不同而空闲时换掉，再把 stdio 接到 socket）；
//! - [`admin`]：管理帧的客户端一侧（`status` / `stop` 与代理用）；
//! - [`file_ledger`]：文件版 `ExecLedger` 与保留清理；
//! - [`serve_stdio`]：`--standalone`（R1 的行为，只给 loopback）：在一对字节流上直接服务一条连接，连接结束就返回。
//!
//! 执行环境（env-local）由 cli 按同样的规则建好再交进来：入口才读进程环境。

pub mod admin;
pub mod file_ledger;
mod journal;
pub mod proxy;
pub mod resident;

use std::sync::Arc;

use rust_cli_exec_server::{AllowAll, ExecServer, MemoryLedger, ServerConfig, ServerError};
use rust_cli_exec_transport::MAX_FRAME;
use rust_cli_exec_transport::stream::stream_link;
use rust_cli_kernel_api::machine::MachineEnv;
use tokio::io::{AsyncRead, AsyncWrite};

/// 在一对字节流（执行器进程的 stdin / stdout）上服务一条连接，直到它结束。
pub async fn serve_stdio<R, W>(
    reader: R,
    writer: W,
    machine: MachineEnv,
    config: ServerConfig,
) -> Result<(), ServerError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let link = stream_link(reader, writer, "stdio", MAX_FRAME);
    let server = ExecServer::start(
        machine,
        Arc::new(MemoryLedger::default()),
        Arc::new(AllowAll),
        config,
    )
    .await?;
    server.serve_link(Box::new(link)).await
}
