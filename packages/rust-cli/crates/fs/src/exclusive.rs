//! 操作系统的独占文件锁（docs/specs/rust-cli/remote-exec.md §19.5）：常驻执行器的单实例锁与代理之间的拉起锁。
//!
//! 与 Node 同形的几种锁（[`super::lock`]、[`super::lock_single`]、[`super::lock_pid`]）按锁文件的年龄回收，只适合
//! 短时间持有；这里的锁可以持有整个进程的生命，进程退出（包括被强杀）时由系统放开，没有“陈旧的锁”要回收。
//! 锁文件本身不删：删掉之后别的进程会锁到另一个文件上，两边都以为自己拿到了。

use std::io;
use std::path::Path;

/// 拿着的锁；丢掉时放开。
#[derive(Debug)]
pub struct ExclusiveLock {
    _file: std::fs::File,
}

fn open(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(rust_cli_base::proto::mode::PRIVATE_FILE);
    }
    options.open(path)
}

/// 试着拿锁；别人拿着时为 `None`。父目录要已经存在。
pub async fn try_lock_exclusive(path: &Path) -> io::Result<Option<ExclusiveLock>> {
    let path = path.to_path_buf();
    rust_cli_base::blocking(move || {
        let file = open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(ExclusiveLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    })
    .await
    .map_err(io::Error::other)?
}

/// 拿锁；别人拿着时等它放开（不轮询：在阻塞线程上等系统的锁）。父目录要已经存在。
pub async fn lock_exclusive(path: &Path) -> io::Result<ExclusiveLock> {
    let path = path.to_path_buf();
    rust_cli_base::blocking(move || {
        let file = open(&path)?;
        file.lock()?;
        Ok(ExclusiveLock { _file: file })
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
#[path = "exclusive_tests.rs"]
mod tests;
