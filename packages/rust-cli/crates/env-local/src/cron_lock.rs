//! 定时任务的调度锁 `.zcode/scheduled_tasks.lock`（docs/specs/rust-cli/tools-more.md §16.4）。
//!
//! `O_EXCL` 创建即持锁。已存在时：持有者是自己就持锁（PID 变了改写成本进程）；PID 存活且启动时间相同或
//! 未知就让给它；否则（坏文件、PID 不在、启动时间不同）删除后再 `O_EXCL` 一次。存活只看 PID 与启动时间。

use std::io::{self, ErrorKind};
use std::path::Path;

use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt;

use crate::cron_store::Identity;

/// 锁文件的内容（`{sessionId, pid, procStart?, acquiredAt}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LockRecord {
    pub(crate) holder: String,
    pub(crate) pid: u32,
    pub(crate) proc_start: Option<String>,
    pub(crate) acquired_at: u64,
}

impl LockRecord {
    /// `JSON.stringify` 的紧凑写法；没有启动时间时省略 `procStart`。
    pub(crate) fn render(&self) -> String {
        let mut record = Map::new();
        record.insert("sessionId".to_owned(), json!(self.holder));
        record.insert("pid".to_owned(), json!(self.pid));
        if let Some(start) = &self.proc_start {
            record.insert("procStart".to_owned(), json!(start));
        }
        record.insert("acquiredAt".to_owned(), json!(self.acquired_at));
        Value::Object(record).to_string()
    }

    /// 按 schema 校验：字段类型不对就当作坏文件。
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let record: Value = serde_json::from_str(raw).ok()?;
        let record = record.as_object()?;
        let start = match record.get("procStart") {
            None => None,
            Some(value) => Some(value.as_str()?.to_owned()),
        };
        Some(Self {
            holder: record.get("sessionId")?.as_str()?.to_owned(),
            pid: record.get("pid")?.as_f64()? as u32,
            proc_start: start,
            acquired_at: record.get("acquiredAt")?.as_f64()? as u64,
        })
    }
}

/// 只在锁不存在时创建（`O_EXCL`）；目录不在时建好再试一次。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）：锁要 O_EXCL 创建"
)]
async fn create_exclusive(path: &Path, body: &str) -> io::Result<bool> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let open = || options.open(path);
    let mut file = match open().await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                rust_cli_fs::create_dir_all(parent).await?;
            }
            match open().await {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    file.write_all(body.as_bytes()).await?;
    file.flush().await?;
    Ok(true)
}

async fn read(path: &Path) -> io::Result<Option<LockRecord>> {
    let bytes = rust_cli_fs::read_optional(path).await?;
    Ok(bytes.and_then(|bytes| LockRecord::parse(&String::from_utf8_lossy(&bytes))))
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）：删除失效的锁"
)]
async fn remove(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

/// PID 存活，且启动时间相同或任一方未知。
async fn held_by_live_process(record: &LockRecord) -> bool {
    if !rust_cli_fs::process_alive(record.pid) {
        return false;
    }
    let Some(recorded) = &record.proc_start else {
        return true;
    };
    match rust_cli_proc::process_started(record.pid).await {
        Some(current) => &current == recorded,
        None => true,
    }
}

/// 抢锁：拿到或本来就是 `holder` 的返回 true。
pub(crate) async fn acquire(
    path: &Path,
    holder: &str,
    me: &Identity,
    now_ms: u64,
) -> io::Result<bool> {
    let mine = LockRecord {
        holder: holder.to_owned(),
        pid: me.pid,
        proc_start: me.started.clone(),
        acquired_at: now_ms,
    };
    let body = mine.render();
    if create_exclusive(path, &body).await? {
        tracing::debug!(pid = me.pid, "acquired the scheduler lock");
        return Ok(true);
    }
    let existing = read(path).await?;
    if let Some(existing) = &existing {
        if existing.holder == holder {
            // resume 后会话 ID 不变、进程换了：改写成本进程，别的会话才看得到活着的 PID。
            if existing.pid != me.pid {
                rust_cli_fs::atomic_write(
                    path,
                    body.as_bytes(),
                    crate::cron_store::write_options(),
                )
                .await?;
            }
            return Ok(true);
        }
        if held_by_live_process(existing).await {
            return Ok(false);
        }
        tracing::debug!(pid = existing.pid, "recovering a stale scheduler lock");
    }
    remove(path).await;
    // 两个会话同时回收时只有一个能创建成功。
    create_exclusive(path, &body).await
}

/// 释放：只删除持有者是 `holder` 的锁。
pub(crate) async fn release(path: &Path, holder: &str) -> io::Result<()> {
    if read(path)
        .await?
        .is_some_and(|record| record.holder == holder)
    {
        remove(path).await;
    }
    Ok(())
}

#[cfg(test)]
#[path = "cron_lock_tests.rs"]
mod tests;
