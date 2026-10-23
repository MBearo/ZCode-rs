//! stdio 传输（docs/specs/rust-cli/ext.md §3.2；MCP SDK `StdioClientTransport`）：服务器进程经账本启动
//! （`Purpose::Mcp`、`Isolation::Group`、`Containment::ProcessJob`），stdin 一行一条消息，stdout 按行读，
//! 不是 JSON 的行丢弃并记日志，stderr 只进日志。
//!
//! 一个连接任务拥有进程与管道：写 stdin、读 stdout、关闭都在这里，没有共享的锁。
//!
//! 读与写是同一个任务里并发的两半（审查 R1-18）：读的一半在事件通道满时停读 stdout（背压到服务器）；写的
//! 一半在服务器不读 stdin 时停在写入上。两半互不等待，关闭是单独的信号，任何一半卡住都能关。

use std::collections::BTreeMap;
use std::process::Stdio;

use rust_cli_kernel_api::mcp::{McpError, McpTransportEvent};
use rust_cli_proc::ledger::{Containment, Isolation, Ledger, Purpose, Tracked};
use rust_cli_proc::tree::Shutdown;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{EventSender, OUTBOX_CAPACITY};
use crate::shell::environ;

/// 发往一个 stdio 服务器的入口：有界的发件箱与关闭信号。
pub(crate) struct StdioLink {
    outbox: mpsc::Sender<Value>,
    closing: CancellationToken,
}

impl StdioLink {
    /// 交给连接任务写进 stdin。发件箱满（服务器不读输入）时立即报错，不等待：等待会把调用方（工具调用、读者对
    /// 服务器请求的回答）卡在一个不读输入的服务器上。
    pub(crate) fn send(&self, message: Value) -> Result<(), McpError> {
        self.outbox.try_send(message).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => McpError::Io {
                message: format!(
                    "the MCP server is not reading its input ({OUTBOX_CAPACITY} messages are waiting)"
                ),
            },
            mpsc::error::TrySendError::Closed(_) => McpError::Closed {
                message: "the server process has exited".to_owned(),
            },
        })
    }

    pub(crate) fn close(&self) {
        self.closing.cancel();
    }
}

/// 启动一个 stdio 服务器需要的东西。
pub(crate) struct Launch<'a> {
    pub server: &'a str,
    pub command: &'a str,
    pub args: &'a [String],
    pub env: &'a BTreeMap<String, String>,
    pub cwd: &'a str,
    /// 入口捕获的进程环境（按白名单投影）。
    pub snapshot: &'a [(String, String)],
}

/// 服务器进程的环境：快照的白名单投影，再加配置的 `env`。
pub(crate) fn child_env(
    snapshot: &[(String, String)],
    configured: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    // 修复线 F58：`SHELL` 照快照继承（以前没设时写 `/bin/sh`，不该有这个缺省）。
    let mut env = environ::project(snapshot, None);
    env.extend(
        configured
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    env
}

fn spawn_error(server: &str, error: &std::io::Error) -> McpError {
    McpError::Connect {
        message: format!("MCP server \"{server}\" failed to start: {error}"),
    }
}

/// 启动进程与连接任务。
pub(crate) async fn start(
    ledger: &Ledger,
    launch: Launch<'_>,
    events: EventSender,
) -> Result<(std::sync::Arc<StdioLink>, rust_cli_base::OwnedTask<()>), McpError> {
    let env = child_env(launch.snapshot, launch.env);
    let mut command = rust_cli_proc::command(launch.command);
    command
        .args(launch.args)
        .current_dir(launch.cwd)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut tracked = ledger
        .spawn(
            &mut command,
            Purpose::Mcp,
            Isolation::Group,
            Containment::ProcessJob,
        )
        .await
        .map_err(|error| spawn_error(launch.server, &error))?;
    let child = tracked.child();
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(McpError::Connect {
            message: format!("MCP server \"{}\" has no pipes", launch.server),
        });
    };
    let (outbox, inbox) = mpsc::channel(OUTBOX_CAPACITY);
    let closing = CancellationToken::new();
    let server = launch.server.to_owned();
    let task = rust_cli_base::owned(run(Pipes {
        server,
        tracked,
        stdin,
        stdout,
        stderr,
        inbox,
        events,
        closing: closing.clone(),
    }));
    Ok((std::sync::Arc::new(StdioLink { outbox, closing }), task))
}

struct Pipes {
    server: String,
    tracked: Tracked,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    inbox: mpsc::Receiver<Value>,
    events: EventSender,
    closing: CancellationToken,
}

/// 为什么停下。
enum Stop {
    Closed,
    Exited,
    WriteFailed(String),
}

/// 读的一半：stdout 一行一条消息交给读者，stderr 只进日志。事件通道满时在这里等（背压）；读者已经释放事件流时
/// 继续读、丢掉消息（不让服务器卡在输出上）。stdout 结束即服务器退出。
async fn read(
    server: &str,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    events: &EventSender,
) -> Stop {
    let mut lines = BufReader::new(stdout).lines();
    let mut errors = BufReader::new(stderr).lines();
    let mut errors_open = true;
    let mut reader_gone = false;
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&line) {
                        Ok(message) if !reader_gone => {
                            let event = McpTransportEvent::Message { message };
                            reader_gone = events.send(event).await.is_err();
                        }
                        Ok(_) => {}
                        Err(error) => tracing::debug!(server, %error, "ignored a non-JSON line from an MCP server"),
                    }
                }
                Ok(None) | Err(_) => return Stop::Exited,
            },
            line = errors.next_line(), if errors_open => match line {
                Ok(Some(line)) => tracing::debug!(server, line, "MCP server stderr"),
                Ok(None) | Err(_) => errors_open = false,
            },
        }
    }
}

/// 写的一半：发件箱里的消息逐条写进 stdin；发件箱的发送端全部释放（连接已移除）时当作关闭。
async fn write(mut stdin: tokio::process::ChildStdin, mut inbox: mpsc::Receiver<Value>) -> Stop {
    while let Some(message) = inbox.recv().await {
        let mut text = message.to_string();
        text.push('\n');
        if let Err(error) = stdin.write_all(text.as_bytes()).await {
            return Stop::WriteFailed(error.to_string());
        }
        if let Err(error) = stdin.flush().await {
            return Stop::WriteFailed(error.to_string());
        }
    }
    Stop::Closed
}

async fn run(pipes: Pipes) {
    let Pipes {
        server,
        mut tracked,
        stdin,
        stdout,
        stderr,
        inbox,
        events,
        closing,
    } = pipes;
    // 先关 stdin（MCP SDK 关闭时也是先结束输入），再结束进程树：select! 结束时写的一半连同 stdin 一起释放。
    let stop = tokio::select! {
        stop = read(&server, stdout, stderr, &events) => stop,
        stop = write(stdin, inbox) => stop,
        () = closing.cancelled() => Stop::Closed,
    };
    // 进程自己退出时直接收拾残留；主动关闭与写失败时先 TERM 再 KILL。
    let (reason, shutdown) = match stop {
        Stop::Closed => (None, Shutdown::Graceful),
        Stop::Exited => (
            Some(format!("MCP server \"{server}\" process exited")),
            Shutdown::Immediate,
        ),
        Stop::WriteFailed(error) => (
            Some(format!(
                "writing to MCP server \"{server}\" failed: {error}"
            )),
            Shutdown::Graceful,
        ),
    };
    if let Err(error) = tracked.terminate(shutdown).await {
        tracing::warn!(server, %error, "could not confirm the MCP server process tree ended");
    }
    // 读者已经释放事件流时发送失败，忽略。
    let _ = events
        .send(McpTransportEvent::Closed {
            reason,
            session_expired: false,
        })
        .await;
}
