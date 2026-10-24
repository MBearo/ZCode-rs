//! Monitor 的 ws 来源（docs/specs/rust-cli/tools-more.md §19.7）：不起进程，经 net 开 WebSocket。
//!
//! 与命令一样登记成后台命令（任务 ID、输出文件、TaskStop、终态通知都同一套）：每个文本帧加换行写进输出文件（事件流
//! 按游标读它），二进制帧写成 `[binary frame, <N> bytes]`；生命周期按行写成 `SocketEvent` 的 JSON，进
//! `<任务>.stderr`（终态之前作为 stderr 事件送达）。被停掉（取消）时发 close、不写生命周期；结束时照命令记终态通知。

use std::path::Path;
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, Outcome};
use rust_cli_kernel_api::env_shell::{MonitorSocket, ShellNotice, SocketEvent};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_net::Egress;
use rust_cli_net::websocket::{WsError, WsFrame};
use tokio::io::AsyncWriteExt;

use super::supervise::Plan;
use super::{Finish, Running, Shared};

/// 一个 ws 来源的 monitor。
pub(crate) struct SocketJob {
    pub operation: OperationId,
    pub session: SessionId,
    pub plan: Plan,
    pub request: rust_cli_kernel_api::env::StartCommand,
    pub socket: MonitorSocket,
    pub running: Arc<Running>,
    pub egress: Option<Arc<Egress>>,
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：输出文件在执行侧（docs/specs/rust-cli/README.md §4）"
)]
async fn open(dir: &Path, path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::create_dir_all(dir).await?;
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).await
}

async fn lifecycle(errors: &mut Option<tokio::fs::File>, event: &SocketEvent) {
    if let (Some(file), Ok(mut line)) = (errors.as_mut(), serde_json::to_string(event)) {
        line.push('\n');
        let _ = file.write_all(line.as_bytes()).await;
        let _ = file.flush().await;
    }
}

/// 连接失败：`ws` 库的写法是 `WebSocket connection to '<url>' failed: <原因>`，关闭的原因是那段原因。
async fn failed(errors: &mut Option<tokio::fs::File>, url: &str, error: &WsError) {
    let cause = error.to_string();
    let message = format!("WebSocket connection to '{url}' failed: {cause}");
    lifecycle(errors, &SocketEvent::Error { message }).await;
    let closed = SocketEvent::Closed {
        code: 1006,
        reason: cause,
    };
    lifecycle(errors, &closed).await;
}

/// 读到 close、出错或被取消为止。返回是不是被取消的。
async fn pump(
    job: &SocketJob,
    output: &mut tokio::fs::File,
    errors: &mut Option<tokio::fs::File>,
) -> bool {
    let cancel = job.running.cancel.clone();
    let url = job.socket.url.clone();
    let Some(egress) = job.egress.clone() else {
        let error = WsError::Protocol("network egress is not configured".to_owned());
        failed(errors, &url, &error).await;
        return false;
    };
    let connected = tokio::select! {
        () = cancel.cancelled() => return true,
        connected = egress.websocket(&url, &job.socket.protocols) => connected,
    };
    let mut socket = match connected {
        Ok(socket) => socket,
        Err(WsError::Rejected { status }) => {
            lifecycle(errors, &SocketEvent::Rejected { status }).await;
            let closed = SocketEvent::Closed {
                code: 1006,
                reason: String::new(),
            };
            lifecycle(errors, &closed).await;
            return false;
        }
        Err(error) => {
            failed(errors, &url, &error).await;
            return false;
        }
    };
    loop {
        let frame = tokio::select! {
            () = cancel.cancelled() => {
                socket.close().await;
                return true;
            }
            frame = socket.next() => frame,
        };
        match frame {
            Ok(WsFrame::Text(text)) => {
                let _ = output.write_all(format!("{text}\n").as_bytes()).await;
                let _ = output.flush().await;
            }
            Ok(WsFrame::Binary(bytes)) => {
                let line = format!("[binary frame, {bytes} bytes]\n");
                let _ = output.write_all(line.as_bytes()).await;
                let _ = output.flush().await;
            }
            Ok(WsFrame::Close { code, reason }) => {
                lifecycle(errors, &SocketEvent::Closed { code, reason }).await;
                return false;
            }
            Err(WsError::TooBig(bytes)) => {
                lifecycle(errors, &SocketEvent::Dropped { bytes }).await;
                socket.close().await;
                return false;
            }
            Err(error) => {
                let message = error.to_string();
                lifecycle(
                    errors,
                    &SocketEvent::Error {
                        message: message.clone(),
                    },
                )
                .await;
                let closed = SocketEvent::Closed {
                    code: 1006,
                    reason: message,
                };
                lifecycle(errors, &closed).await;
                return false;
            }
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：输出文件在执行侧（docs/specs/rust-cli/README.md §4）"
)]
pub(crate) async fn run(shared: Arc<Shared>, job: SocketJob) {
    let output = open(&job.plan.dir, &job.plan.output).await;
    let mut errors = match &job.plan.errors {
        Some(path) => open(&job.plan.dir, path).await.ok(),
        None => None,
    };
    let cancelled = match output {
        Ok(mut output) => pump(&job, &mut output, &mut errors).await,
        Err(error) => {
            let message = error.to_string();
            lifecycle(
                &mut errors,
                &SocketEvent::Error {
                    message: message.clone(),
                },
            )
            .await;
            lifecycle(
                &mut errors,
                &SocketEvent::Closed {
                    code: 1006,
                    reason: message,
                },
            )
            .await;
            false
        }
    };
    let output_bytes = tokio::fs::metadata(&job.plan.output)
        .await
        .map_or(0, |meta| meta.len());
    let finish = Finish {
        outcome: if cancelled {
            Outcome::Cancelled
        } else {
            Outcome::Completed
        },
        exit_code: None,
        timed_out: false,
        cwd_reset: None,
        output_bytes,
        output_limit: None,
    };
    if let Some(background) = &job.request.background {
        let mut state = shared.state();
        let shell = state.sessions.entry(job.session.clone()).or_default();
        shell.background.remove(&job.request.task);
        // 被停掉的不记通知（同命令，shell-permissions.md §2.6）。
        if !shell.stopped.remove(&job.request.task) {
            shell.next_seq = shell.next_seq.max(shell.acked) + 1;
            shell.notices.push(ShellNotice {
                seq: shell.next_seq,
                operation: job.operation.clone(),
                task: job.request.task.clone(),
                tool_use: background.tool_use.clone(),
                description: background.description.clone(),
                output: EnvPath::new(job.plan.output.to_string_lossy()),
                outcome: finish.outcome.clone(),
                exit_code: None,
                kind: background.kind,
                output_bytes,
            });
        }
    }
    job.running.finished.send_replace(Some(finish));
    shared.notices.send_modify(|count| *count += 1);
}

#[cfg(test)]
#[path = "socket_tests.rs"]
mod tests;
