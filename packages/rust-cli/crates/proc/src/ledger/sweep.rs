// 复制自 rust/crates/proc/src/ledger/sweep.rs（feat/rs 79b6691c55）。
//! 启动清扫（docs/specs/rust-tool-process-reaping.md 2.2）。
//!
//! 只结束身份对得上的进程：根进程的 pid 与启动时间必须同时与记录相符，结束前（取得进程表快照
//! 之后）再确认一次。查询失败时什么都不做，记录留给下一次清扫。

use std::io;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;

use super::identity::{self, Probe, Process, Start};
use super::record::{self, Record};
use crate::tree::{self, Adopted};

/// 同时结束的树的上限：每棵树要取几次进程表快照。
const CONCURRENT: usize = 8;

/// 一条记录的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// 所有者仍在运行（或无法确认它已结束）：不动。
    Owned,
    /// 根进程已结束，或 pid 已被复用：删除了记录。
    Stale,
    /// 所有者已结束、根进程仍在：结束了整棵树并删除了记录。
    Reaped,
    /// 无法确认根进程或所有者的状态，或结束失败：保留记录。
    Kept,
}

/// 清扫 `dir`。目录不存在或为空时只读一次目录。
pub(super) async fn run(dir: &Path) {
    let names = match list(dir).await {
        Ok(names) => names,
        Err(error) => {
            tracing::warn!(
                event = "process_ledger.unreadable",
                dir = %dir.display(),
                %error,
                "Cannot read the process ledger"
            );
            return;
        }
    };
    let records: Vec<_> = names
        .iter()
        .filter_map(|name| Some((dir.join(name), Record::parse(name)?)))
        .collect();
    if records.is_empty() {
        return;
    }
    let Some(this) = identity::this_process().await else {
        tracing::warn!(
            event = "process_ledger.unidentified",
            "Cannot identify this process; the process ledger is not swept"
        );
        return;
    };
    // 每个 future 持有自己的记录：借用迭代器元素的闭包会让外层任务的 future 无法证明 Send。
    let verdicts: Vec<Verdict> = futures_util::stream::iter(records)
        .map(|(path, record)| handle(path, record, this))
        .buffer_unordered(CONCURRENT)
        .collect()
        .await;
    let count = |wanted: Verdict| verdicts.iter().filter(|v| **v == wanted).count();
    let (reaped, stale) = (count(Verdict::Reaped), count(Verdict::Stale));
    if reaped + stale > 0 {
        tracing::info!(
            event = "process_ledger.swept",
            reaped,
            stale,
            kept = count(Verdict::Kept),
            owned = count(Verdict::Owned),
            "Reaped process trees left by ended runtimes"
        );
    }
}

/// 目录中本用户写下的记录文件名；目录不存在时为空。
///
/// 缺陷修复 R2-1：原来目录里的每个名字都当作记录。账本目录在共享的临时目录里时，别人能放一条伪造的记录
/// （根进程写我们自己的进程、所有者写一个已结束的进程），让清扫结束我们的进程树。现在只认属主是本 uid 的
/// 普通文件（`DirEntry::metadata` 不跟符号链接）。
#[expect(
    clippy::disallowed_methods,
    reason = "进程账本是 rust-cli-proc 的持久状态，在本用户私有的临时目录里（private::ensure 核对）"
)]
async fn list(dir: &Path) -> io::Result<Vec<String>> {
    use std::os::unix::fs::MetadataExt;

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error),
    };
    let mut names = vec![];
    while let Some(entry) = entries.next_entry().await? {
        let ours = entry
            .metadata()
            .await
            .is_ok_and(|metadata| metadata.is_file() && metadata.uid() == crate::private::uid());
        if let (true, Some(name)) = (ours, entry.file_name().to_str()) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

async fn handle(path: PathBuf, record: Record, this: &'static Process) -> Verdict {
    if owner_running(&record, this).await {
        return Verdict::Owned;
    }
    let root = &record.root;
    // 根进程不会是本进程：只有伪造的记录会这样写，不能因此结束自己。
    let probe = if root.pid == this.pid {
        Probe::Gone
    } else {
        identity::probe(root.pid).await
    };
    let verdict = match probe {
        Probe::Running(start) if start == root.start => reap(&record).await,
        Probe::Running(_) | Probe::Gone => Verdict::Stale,
        Probe::Unknown => Verdict::Kept,
    };
    match verdict {
        Verdict::Stale | Verdict::Reaped => forget(&path).await,
        Verdict::Owned | Verdict::Kept => {}
    }
    verdict
}

/// 写下记录的 runtime 仍在运行。本进程写的记录看实例是否仍已登记；其他进程的记录看所有者进程
/// 的身份。查询失败时按仍在运行处理：宁可留下进程，也不结束一个可能属于活 runtime 的树。
async fn owner_running(record: &Record, this: &Process) -> bool {
    if record.owner == *this {
        return identity::registered(record.instance);
    }
    match identity::probe(record.owner.pid).await {
        Probe::Running(start) => start == record.owner.start,
        Probe::Gone => false,
        Probe::Unknown => true,
    }
}

/// 结束记录的整棵树。
async fn reap(record: &Record) -> Verdict {
    let root = &record.root;
    let Ok(pid) = u32::try_from(root.pid) else {
        return Verdict::Kept;
    };
    let ours = || is_running(root.pid, root.start.clone());
    match tree::terminate_adopted(pid, ours).await {
        Ok(Adopted::Ended) => Verdict::Reaped,
        Ok(Adopted::Gone) => Verdict::Stale,
        Err(error) => {
            tracing::warn!(
                event = "process_ledger.reap_failed",
                purpose = record.purpose.as_str(),
                pid = root.pid,
                error = %format!("{error:#}"),
                "Cannot end a process tree left by an ended runtime"
            );
            Verdict::Kept
        }
    }
}

/// `pid` 仍是启动时间为 `start` 的那个进程，并且仍在运行。
async fn is_running(pid: i32, start: Start) -> bool {
    identity::probe(pid).await.is(&start)
}

async fn forget(path: &Path) {
    if let Err(error) = record::remove(path).await {
        tracing::warn!(
            event = "process_ledger.remove_failed",
            path = %path.display(),
            %error,
            "Cannot remove a process ledger record"
        );
    }
}

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod tests;
