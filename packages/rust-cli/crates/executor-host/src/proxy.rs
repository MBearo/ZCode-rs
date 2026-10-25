//! `zcode executor --stdio` 的代理（docs/specs/rust-cli/remote-exec.md §19.5）：没有常驻执行器就拉起一个（版本不同
//! 而它空闲时换掉），再把自己的 stdin / stdout 原样接到它的 socket。握手在客户端与常驻进程之间。
//!
//! 远端执行 F96（§23.5）：代理只认第一帧——是 hello 时在 JSON 头里加上这条 ssh 会话的环境（[`SESSION_ENV`] 里的名字，
//! 值取代理自己的环境，就是这条会话在远端的环境），常驻执行器按客户端实例让之后起的进程用它。之后照旧原样搬字节。
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

use bytes::Bytes;
use rust_cli_exec_transport::stream::stream_link;
use rust_cli_exec_transport::{Link, MAX_FRAME};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::admin::{AdminAction, AdminOutcome};
use rust_cli_exec_wire::dto::env::EnvVars;
use rust_cli_exec_wire::frame::{Decoded, Frame, Kind, decode};
use rust_cli_exec_wire::handshake::{Refuse, RefuseCode};
use rust_cli_paths::ExecutorPaths;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::admin;

/// 常驻进程的就绪行（`zcode executor --daemon` 写在 stdout 的第一行）。
pub const READY: &str = "ready";
/// 另一个常驻进程拿着单实例锁。
pub const BUSY: &str = "busy";
/// 起不来：`error <原因>`。
pub const ERROR: &str = "error";

/// 代理带给常驻执行器的这条 ssh 会话的变量（F96，§23.2 的验证）：sshd 按会话设置、交给命令的只有 agent 转发的
/// socket。会话结束时 sshd 删掉它，常驻执行器继承的那一个就失效了。
pub const SESSION_ENV: &[&str] = &["SSH_AUTH_SOCK"];

/// 这条会话的环境：白名单里每个名字的值，没有时是 None（执行器去掉它）。
pub fn session_env(lookup: impl Fn(&str) -> Option<String>) -> EnvVars {
    SESSION_ENV
        .iter()
        .map(|name| ((*name).to_owned(), lookup(name)))
        .collect()
}

/// 第一帧是 hello 时在 JSON 头里加上 `session_env`（头里别的字段原样留着）；别的帧、解不开的字节原样交回。
pub fn with_session_env(payload: Bytes, session: &EnvVars) -> Bytes {
    let Ok(Decoded::Frame(frame)) = decode(payload.clone()) else {
        return payload;
    };
    if frame.kind != Kind::Hello {
        return payload;
    }
    let Ok(mut header) = frame.header::<serde_json::Map<String, serde_json::Value>>() else {
        return payload;
    };
    let Ok(value) = serde_json::to_value(session) else {
        return payload;
    };
    header.insert("session_env".to_owned(), value);
    Frame::encode(Kind::Hello, &header, &frame.body).unwrap_or(payload)
}

/// 读客户端的第一帧（u32 BE 长度 + 一帧）；连接在这之前结束时 None。
async fn first_frame<R: AsyncRead + Unpin>(stdin: &mut R) -> std::io::Result<Option<Bytes>> {
    let mut length = [0u8; 4];
    match stdin.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let size = u32::from_be_bytes(length) as usize;
    if size > MAX_FRAME {
        return Err(std::io::Error::other(format!(
            "the first frame is {size} bytes, more than {MAX_FRAME}"
        )));
    }
    let mut payload = vec![0u8; size];
    stdin.read_exact(&mut payload).await?;
    Ok(Some(Bytes::from(payload)))
}

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
    /// 这条 ssh 会话的环境（[`session_env`]，入口从自己的环境取，F96）。
    pub session_env: EnvVars,
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
    // 第一帧：hello 时加上这条 ssh 会话的环境（F96）。客户端在这之前就走了时不再往下搬。
    let first = first_frame(&mut stdin)
        .await
        .map_err(|error| ProxyError::Io(format!("reading the first frame: {error}")))?;
    let Some(first) = first else {
        return Ok(());
    };
    let first = with_session_env(first, &config.session_env);
    let length = u32::try_from(first.len())
        .map_err(|_| ProxyError::Io("the first frame is too large".to_owned()))?;
    let forward = async {
        write.write_all(&length.to_be_bytes()).await?;
        write.write_all(&first).await
    };
    forward
        .await
        .map_err(|error| ProxyError::Io(format!("forwarding the first frame: {error}")))?;
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

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;

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
