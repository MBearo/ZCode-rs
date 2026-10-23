// 复制自 rust/crates/kit/src/fs/stamp.rs（feat/rs 79b6691c55）。
//! 轮询读文件前先比元数据（docs/specs/rust-provider-config-poll.md）。
//!
//! 每秒轮询的一方先取每个输入文件的指纹（存在与否、长度、修改时间；Unix 上还有设备号 + inode 与
//! ctime），与上次成功读取前取的指纹相同就不读。指纹看不出的改写（同一时间粒度内、长度不变、
//! 同一个 inode 的原地改写；Windows 上没有 inode 与 ctime）由兜底放行覆盖：距上次读取满
//! `full_read_every` 时无条件读一次。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// 一个文件在某一时刻的元数据指纹（含路径：路径变了指纹也不同）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp {
    path: PathBuf,
    /// `None`：元数据读不到（不存在，或权限等原因）。之后能读到时指纹随之变化，调用方会去读文件。
    meta: Option<Meta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    len: u64,
    modified: Option<SystemTime>,
    /// 设备号与 inode：rename 覆盖（原子写）后是另一个 inode，即使长度与 mtime 相同也能看出。
    #[cfg(unix)]
    inode: (u64, u64),
    /// ctime（秒、纳秒）：任何写入、chmod、改 mtime 都会更新，工具无法把它改回去。
    #[cfg(unix)]
    changed: (i64, i64),
}

impl FileStamp {
    /// 取 `path` 的指纹（跟随符号链接）。
    pub async fn of(path: &Path) -> Self {
        let meta = tokio::fs::metadata(path).await.ok().map(|metadata| Meta {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
            #[cfg(unix)]
            changed: {
                use std::os::unix::fs::MetadataExt;
                (metadata.ctime(), metadata.ctime_nsec())
            },
        });
        Self {
            path: path.to_path_buf(),
            meta,
        }
    }
}

/// 轮询读文件的闸门，由轮询循环独占（不共享，不加锁）。
///
/// 用法：每次轮询先取全部输入文件的指纹（读文件之前取，读的过程中文件再变，下一轮指纹就不同），
/// [`ChangeGate::should_read`] 为 false 就跳过；读完且结果可以沿用时 [`ChangeGate::record`]。
/// 读失败、结果需要重试时不记录，下一轮照常读（与不设闸门时每轮都读的行为相同）。
#[derive(Debug)]
pub struct ChangeGate {
    full_read_every: Duration,
    last: Option<(Vec<FileStamp>, Instant)>,
}

impl ChangeGate {
    /// `full_read_every`：指纹不变时两次读取之间的最长间隔（兜底放行）。
    pub fn new(full_read_every: Duration) -> Self {
        Self {
            full_read_every,
            last: None,
        }
    }

    /// 还没有成功读过、指纹（含路径与顺序）变了，或距上次读取已满兜底间隔时为 true。
    pub fn should_read(&self, stamps: &[FileStamp], now: Instant) -> bool {
        match &self.last {
            None => true,
            Some((last, at)) => {
                last.as_slice() != stamps
                    || now.saturating_duration_since(*at) >= self.full_read_every
            }
        }
    }

    /// 记下这次读取之前取的指纹与时刻。
    pub fn record(&mut self, stamps: Vec<FileStamp>, now: Instant) {
        self.last = Some((stamps, now));
    }
}

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
