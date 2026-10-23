//! bwrap 挂载点的清理。
//!
//! 保护表里不存在的路径在 bwrap 里绑 `/dev/null`（sandbox.rs `protected_binds`）。工作目录是宿主目录的可写
//! 绑定，bwrap 建挂载点时会在宿主上留下一个空文件。这里记下这些挂载点，最后一个 sandbox 结束时删掉还是空文件
//! 的那些。进程退出时不另清一次（进程被强杀时留下的空文件下次也不会清）。

use std::collections::BTreeSet;
use std::path::PathBuf;

/// 正在跑的 Linux sandbox 数与它们建的挂载点（在 [`super::State`] 的锁里）。
#[derive(Default)]
pub(crate) struct Mounts {
    active: usize,
    created: BTreeSet<PathBuf>,
}

impl Mounts {
    /// 一个 sandbox 开始；`created` 是它的 bwrap 会建的挂载点。
    pub(crate) fn enter(&mut self, created: Vec<PathBuf>) {
        self.active += 1;
        self.created.extend(created);
    }

    /// 一个 sandbox 结束。还有别的在跑时返回空（它们的挂载可能还用着这些路径），否则交出全部挂载点。
    pub(crate) fn leave(&mut self) -> Vec<PathBuf> {
        self.active = self.active.saturating_sub(1);
        if self.active > 0 {
            return Vec::new();
        }
        std::mem::take(&mut self.created).into_iter().collect()
    }
}

/// 删掉还是空的普通文件；命令写过内容的、目录与已经不在的都不动。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程与 sandbox 的入口：清理 bwrap 在工作目录里留下的挂载点（spec §4）"
)]
pub(crate) async fn clean(paths: Vec<PathBuf>) {
    for path in paths {
        let empty = tokio::fs::symlink_metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0);
        if empty {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

#[cfg(test)]
#[path = "mounts_tests.rs"]
mod tests;
