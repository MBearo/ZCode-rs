//! `zcode executor --stdio` 的代理（docs/specs/rust-cli/remote-exec.md §19.5）：没有常驻执行器就拉起一个（版本不同
//! 而它空闲时换掉），再把自己的 stdin / stdout 原样接到它的 socket。代理不解帧：握手在客户端与常驻进程之间。
//!
//! ```text
//! 拿 spawn.lock ─► admin status ─┬─ 连不上 ──────────────► 拉起，等它的就绪行
//!                                ├─ 版本相同 ────────────► 用它
//!                                ├─ 版本不同、空闲 ──────► stop_if_idle → 等它放开锁 → 拉起自己的版本
//!                                ├─ 版本不同、忙、协议同 ─► 用它（继续用旧的）
//!                                └─ 版本不同、忙、协议不同 ► 写一帧 refuse{protocol}，退出
//! 放开 spawn.lock ─► 连 socket，两个方向各搬各的；任一方向结束就退出
//! ```

use std::path::PathBuf;
use std::process::Stdio;

use rust_cli_exec_transport::stream::stream_link;
use rust_cli_exec_transport::{Link, MAX_FRAME};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::admin::{AdminAction, AdminOutcome};
use rust_cli_exec_wire::frame::{Frame, Kind};
use rust_cli_exec_wire::handshake::{Refuse, RefuseCode};
use rust_cli_paths::ExecutorPaths;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::admin;

/// 常驻进程的就绪行（`zcode executor --daemon` 写在 stdout 的第一行）。
pub const READY: &str = "ready";
/// 另一个常驻进程拿着单实例锁。
pub const BUSY: &str = "busy";
/// 起不来：`error <原因>`。
pub const ERROR: &str = "error";

/// 拉起常驻进程的命令（入口给：自己的路径、`executor --daemon`、环境与工作目录）。
#[derive(Debug, Clone)]
pub struct DaemonCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

pub struct ProxyConfig {
    pub paths: ExecutorPaths,
    pub version: String,
    pub protocol: u32,
    pub daemon: DaemonCommand,
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("cannot start the resident executor: {0}")]
    Start(String),
    /// 在跑的常驻执行器说别的协议主版本，而且忙（不能换）。
    #[error(
        "the running executor {version} speaks protocol {protocol} and is busy; stop it with `zcode executor stop` when its work is done"
    )]
    Incompatible { version: String, protocol: u32 },
    #[error("{0}")]
    Io(String),
}

/// 代理的入口：确保有可用的常驻执行器，再接上它。
pub async fn proxy<R, W>(stdin: R, mut stdout: W, config: ProxyConfig) -> Result<(), ProxyError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    if let Err(error) = ensure(&config).await {
        if let ProxyError::Incompatible { .. } = &error {
            // 告诉客户端为什么（它按“被拒绝、协议不对”处理，不再重连）。
            let refuse = Refuse {
                code: RefuseCode::Protocol,
                message: error.to_string(),
            };
            if let Ok(frame) = Frame::encode(Kind::Refuse, &refuse, &Body::empty()) {
                let link = stream_link(tokio::io::empty(), stdout, "stdio", MAX_FRAME);
                let (mut sink, _) = Box::new(link).split();
                let _ = sink.send(frame).await;
                let _ = sink.close().await;
            }
        }
        return Err(error);
    }
    let socket = config.paths.socket();
    let halves = rust_cli_net::uds::connect_raw(socket.as_path())
        .await
        .map_err(|error| ProxyError::Io(format!("connecting to the executor: {error}")))?;
    tracing::info!(target: "executor_host::proxy", "proxy connected");
    let (mut read, mut write) = (halves.read, halves.write);
    let mut stdin = stdin;
    let up = async move {
        let _ = tokio::io::copy(&mut stdin, &mut write).await;
        // 客户端关了：告诉常驻进程这条连接结束，等它关掉另一方向。
        let _ = write.shutdown().await;
        std::future::pending::<()>().await;
    };
    let down = async move {
        let _ = tokio::io::copy(&mut read, &mut stdout).await;
        let _ = stdout.flush().await;
    };
    tokio::select! {
        () = up => {}
        () = down => {}
    }
    Ok(())
}

async fn ensure(config: &ProxyConfig) -> Result<(), ProxyError> {
    let io = |what: &str, error: &dyn std::fmt::Display| ProxyError::Io(format!("{what}: {error}"));
    rust_cli_fs::create_private_dir_all(config.paths.dir().as_path())
        .await
        .map_err(|e| io("creating the executor directory", &e))?;
    // 同一时刻只有一个代理在判断与拉起；拉起的常驻进程写出就绪行之后才放开。
    let _guard = rust_cli_fs::lock_exclusive(config.paths.spawn_lock().as_path())
        .await
        .map_err(|e| io("taking the spawn lock", &e))?;
    let socket = config.paths.socket();
    // 最多判断三轮：换版本（停掉旧的 → 拉起）与有人手动起了一个（busy）各占一轮。
    for _ in 0..3 {
        let status = match admin::request(socket.as_path(), AdminAction::Status).await {
            Ok(Some(reply)) => reply.status,
            // 不认识管理帧的老执行器：直接用它（协议不对时客户端会被拒绝）。
            Ok(None) => return Ok(()),
            Err(error) if admin::not_running(&error) => {
                if spawn(&config.daemon).await? {
                    return Ok(());
                }
                continue;
            }
            Err(error) => return Err(io("asking the executor", &error)),
        };
        if status.version == config.version {
            return Ok(());
        }
        if status.idle {
            let reply = admin::request(socket.as_path(), AdminAction::StopIfIdle)
                .await
                .map_err(|e| io("stopping the old executor", &e))?;
            if reply.is_some_and(|reply| reply.outcome == AdminOutcome::Stopping) {
                tracing::info!(target: "executor_host::proxy", old = %status.version, new = %config.version, "replacing an idle executor");
                admin::wait_released(config.paths.lock().as_path())
                    .await
                    .map_err(|e| io("waiting for the old executor", &e))?;
                continue;
            }
        }
        if status.protocol == config.protocol {
            return Ok(());
        }
        return Err(ProxyError::Incompatible {
            version: status.version,
            protocol: status.protocol,
        });
    }
    Err(ProxyError::Start(
        "the resident executor did not settle".to_owned(),
    ))
}

/// 拉起常驻进程，读它的就绪行：`ready` → true；`busy`（另一个常驻进程拿着锁）→ false；其余是错误。
async fn spawn(daemon: &DaemonCommand) -> Result<bool, ProxyError> {
    let mut command = rust_cli_proc::command(&daemon.program);
    command
        .args(&daemon.args)
        .env_clear()
        .envs(daemon.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = &daemon.cwd {
        command.current_dir(cwd);
    }
    let mut child = rust_cli_proc::daemon::spawn(&mut command)
        .map_err(|error| ProxyError::Start(format!("{}: {error}", daemon.program.display())))?;
    let Some(stdout) = child.stdout.take() else {
        return Err(ProxyError::Start("the daemon has no stdout".to_owned()));
    };
    let mut line = String::new();
    let read = BufReader::new(stdout).read_line(&mut line).await;
    let line = line.trim();
    match read {
        Ok(_) if line == READY => Ok(true),
        Ok(_) if line == BUSY => Ok(false),
        Ok(_) => Err(ProxyError::Start(
            line.strip_prefix(ERROR)
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .unwrap_or("the daemon exited before it was ready")
                .to_owned(),
        )),
        Err(error) => Err(ProxyError::Start(error.to_string())),
    }
}
