//! 按游标读命令输出（[`rust_cli_kernel_api::env::Environment::watch_command`]）。
//!
//! 先发 `Started`（输出文件），再从 `cursor` 起读文件：有新字节就发 `Output`；命令结束后把剩下的
//! 读完再发 `Finished`。命令还在跑、文件暂时没有新内容时，等终态变化或 50 ms 后再读——输出由
//! 子进程直接写文件（合并的 fd），文件增长没有别的通知来源。
//!
//! hook 命令的 stderr 在单独的文件里（`StartCommand::hook`）：stdout 读完、命令结束后，再把 stderr
//! 作为 `OutputStream::Stderr` 的事件送出，最后发 `Finished`。这些事件的 `cursor` 仍是 stdout 的位置。

use std::io::SeekFrom;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{CommandEvent, EnvError, EnvPath, OutputStream};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::Running;

/// 一次最多读这么多字节，作为一个 `Output` 事件。
const CHUNK: usize = 64 * 1024;
/// 命令还在跑、文件没有新内容时，再读之前等多久。
const IDLE: Duration = Duration::from_millis(50);

enum Phase {
    Start,
    Reading,
    Done,
}

struct Cursor {
    running: Arc<Running>,
    position: u64,
    /// stderr 文件已送达的位置（只有 hook 命令有 stderr 文件）。
    errors: u64,
    phase: Phase,
    /// 已经发过 `Backgrounded`（前台命令超时转后台，只发一次）。
    announced: bool,
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程 IO 的入口：输出文件在执行侧（docs/specs/rust-cli/README.md §4）"
)]
async fn read_from(path: &std::path::Path, position: u64) -> std::io::Result<Vec<u8>> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    file.seek(SeekFrom::Start(position)).await?;
    let mut buffer = vec![0; CHUNK];
    let read = file.read(&mut buffer).await?;
    buffer.truncate(read);
    Ok(buffer)
}

async fn next(mut cursor: Cursor) -> Option<(Result<CommandEvent, EnvError>, Cursor)> {
    loop {
        match cursor.phase {
            Phase::Done => return None,
            Phase::Start => {
                cursor.phase = Phase::Reading;
                let output = EnvPath::new(cursor.running.output.to_string_lossy());
                return Some((Ok(CommandEvent::Started { output }), cursor));
            }
            Phase::Reading => {}
        }
        // 先取终态再读：读到空且此前已结束，说明输出已经读完。
        let mut finished = cursor.running.finished.subscribe();
        let ended = finished.borrow_and_update().clone();
        let mut moved = cursor.running.backgrounded.subscribe();
        let backgrounded = *moved.borrow_and_update();
        let data = match read_from(&cursor.running.output, cursor.position).await {
            Ok(data) => data,
            Err(error) => {
                cursor.phase = Phase::Done;
                let error = EnvError::Io {
                    message: error.to_string(),
                };
                return Some((Err(error), cursor));
            }
        };
        if !data.is_empty() {
            cursor.position += data.len() as u64;
            let event = CommandEvent::Output {
                stream: OutputStream::Stdout,
                data,
                cursor: cursor.position,
            };
            return Some((Ok(event), cursor));
        }
        // 已经写出的输出读完之后再说转到了后台（shell-permissions.md §1.2.1）；之后照常读输出、发终态。
        if backgrounded && !cursor.announced {
            cursor.announced = true;
            return Some((Ok(CommandEvent::Backgrounded), cursor));
        }
        if let Some(finish) = ended {
            if let Some(errors) = cursor.running.errors.clone() {
                match read_from(&errors, cursor.errors).await {
                    Ok(data) if !data.is_empty() => {
                        cursor.errors += data.len() as u64;
                        let event = CommandEvent::Output {
                            stream: OutputStream::Stderr,
                            data,
                            cursor: cursor.position,
                        };
                        return Some((Ok(event), cursor));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        cursor.phase = Phase::Done;
                        let error = EnvError::Io {
                            message: error.to_string(),
                        };
                        return Some((Err(error), cursor));
                    }
                }
            }
            cursor.phase = Phase::Done;
            return Some((Ok(finish.event()), cursor));
        }
        tokio::select! {
            _ = finished.changed() => {}
            _ = moved.changed() => {}
            () = tokio::time::sleep(IDLE) => {}
        }
    }
}

pub(crate) fn watch(
    running: Arc<Running>,
    cursor: u64,
) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
    let start = Cursor {
        running,
        position: cursor,
        errors: 0,
        phase: Phase::Start,
        announced: false,
    };
    stream::unfold(start, next).boxed()
}
