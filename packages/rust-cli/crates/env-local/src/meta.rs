//! 文件元数据、版本与 IO 错误的映射。

use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rust_cli_kernel_api::env::{EnvError, FileVersion};
use rust_cli_kernel_api::env_files::FileKind;

pub(crate) fn kind(metadata: &Metadata) -> FileKind {
    if metadata.is_file() {
        FileKind::File
    } else if metadata.is_dir() {
        FileKind::Directory
    } else {
        FileKind::Other
    }
}

fn since_epoch(time: io::Result<SystemTime>) -> u128 {
    time.ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

/// 修改时间，毫秒向下取整（`Math.floor(stat.mtimeMs)`）。
pub(crate) fn mtime_ms(metadata: &Metadata) -> u64 {
    u64::try_from(since_epoch(metadata.modified()) / 1_000_000).unwrap_or(u64::MAX)
}

/// 文件版本：修改时间（纳秒）+ 大小，Unix 上再加设备号与 inode。
///
/// 每次写入都会改变修改时间；同一纳秒内写出相同长度的内容才会撞上，概率远低于
/// 毫秒级的 mtime 比较。
pub(crate) fn version(metadata: &Metadata) -> FileVersion {
    let modified = since_epoch(metadata.modified());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileVersion::new(format!(
            "{modified}-{}-{}-{}",
            metadata.len(),
            metadata.dev(),
            metadata.ino()
        ))
    }
    #[cfg(not(unix))]
    {
        FileVersion::new(format!("{modified}-{}", metadata.len()))
    }
}

pub(crate) fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn io_error(path: &Path, error: &io::Error) -> EnvError {
    match error.kind() {
        io::ErrorKind::NotFound => EnvError::NotFound {
            path: display(path),
        },
        io::ErrorKind::PermissionDenied => EnvError::PermissionDenied {
            message: format!("{error}: {}", display(path)),
        },
        _ => EnvError::Io {
            message: format!("{error}: {}", display(path)),
        },
    }
}
