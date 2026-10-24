//! stdio 传输（docs/specs/rust-cli/remote-exec.md §4、§18.6）：经 proc 拉起子进程，帧走它的 stdin / stdout。本机的
//! loopback 拉起 `zcode executor --stdio`；以后的 ssh 拉起 `ssh -T <目标> 'zcode executor --stdio'`。
//!
//! 子进程的 stderr 逐行写进日志，留最后几行，子进程异常退出时随日志给出。关闭发送方向（或丢掉连接）时子进程
//! 读到 stdin 结束、自己退出；loopback 的执行器不强杀——强杀会留下它起的命令的进程树（§18.6）。ssh 进程在连接被
//! 丢掉时结束（`kill_on_drop`，§19.2）：远端的代理读到结束自己退出，常驻执行器不受影响；不结束的话，网络卡住时
//! 旧的 ssh 进程会一直留着。接收方向读到结束时先等子进程退出，再报告结束，所以读到结束就说明子进程已经收尾。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::link::{Connector, FrameSink, FrameSource, Link, LinkError};
use crate::stream::stream_link;

/// stderr 留下的字节数。
const TAIL: usize = 4096;

/// 拉起执行器的命令：环境与工作目录由组装根给（子进程不继承本进程的环境，只用给的这份）。
#[derive(Debug, Clone)]
pub struct ChildCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// 连接被丢掉时结束子进程（ssh：true；loopback 的执行器：false）。
    pub kill_on_drop: bool,
}

pub struct StdioConnector {
    command: ChildCommand,
    max_frame: usize,
}

impl StdioConnector {
    pub fn new(command: ChildCommand) -> Self {
        Self {
            command,
            max_frame: crate::MAX_FRAME,
        }
    }
}

#[async_trait]
impl Connector for StdioConnector {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        let mut command = rust_cli_proc::command(&self.command.program);
        command
            .args(&self.command.args)
            .env_clear()
            .envs(self.command.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(self.command.kill_on_drop);
        if let Some(cwd) = &self.command.cwd {
            command.current_dir(cwd);
        }
        // 终端的 Ctrl-C 只给前台进程组：执行器自成一组，不随父进程的终端信号一起退出（它按 stdin 结束退出）。
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| LinkError::Connect(format!("{}: {error}", self.describe())))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(LinkError::Connect("child pipes are missing".to_owned()));
        };
        let tail = Arc::new(Mutex::new(String::new()));
        let forwarder = rust_cli_base::owned(forward_stderr(stderr, tail.clone()));
        let inner = stream_link(stdout, stdin, self.describe(), self.max_frame);
        Ok(Box::new(ChildLink {
            sink: inner.sink,
            source: Box::new(ChildSource {
                inner: inner.source,
                child: Some(child),
                tail,
                _forwarder: forwarder,
            }),
            peer: inner.peer,
        }))
    }

    fn describe(&self) -> String {
        format!("stdio:{}", self.command.program.display())
    }
}

async fn forward_stderr(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<String>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::warn!(target: "exec_transport::stdio", line = %line, "executor stderr");
        if let Ok(mut tail) = tail.lock() {
            tail.push_str(&line);
            tail.push('\n');
            if tail.len() > TAIL {
                let cut = tail.len() - TAIL;
                let cut = (cut..tail.len())
                    .find(|index| tail.is_char_boundary(*index))
                    .unwrap_or(tail.len());
                tail.drain(..cut);
            }
        }
    }
}

struct ChildLink {
    sink: Box<dyn FrameSink>,
    source: Box<dyn FrameSource>,
    peer: String,
}

impl Link for ChildLink {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>) {
        (self.sink, self.source)
    }

    fn peer(&self) -> String {
        self.peer.clone()
    }
}

struct ChildSource {
    inner: Box<dyn FrameSource>,
    child: Option<tokio::process::Child>,
    tail: Arc<Mutex<String>>,
    _forwarder: rust_cli_base::OwnedTask<()>,
}

#[async_trait]
impl FrameSource for ChildSource {
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError> {
        let received = self.inner.recv().await;
        if matches!(received, Ok(Some(_))) {
            return received;
        }
        // 结束或出错：等子进程退出（收尾），异常退出时把 stderr 的最后几行写进日志与错误。
        if let Some(mut child) = self.child.take() {
            let status = child.wait().await;
            let tail = self
                .tail
                .lock()
                .map(|tail| tail.clone())
                .unwrap_or_default();
            match status {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    tracing::warn!(target: "exec_transport::stdio", %status, tail = %tail, "executor exited");
                    if received.is_ok() && !tail.is_empty() {
                        return Err(LinkError::Io(format!("executor exited ({status}): {tail}")));
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "exec_transport::stdio", %error, "executor wait failed")
                }
            }
        }
        received
    }
}

#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
