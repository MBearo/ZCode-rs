//! 工作区文件：元数据、目录、读取与条件写（docs/specs/rust-cli/tools-files.md §3）。
//!
//! 都是阻塞调用，由调用方放进 `rust_cli_base::blocking`。写入是原地写：保留 inode、权限、硬链接，
//! 写穿符号链接；写完 fsync 并核对大小。

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use rust_cli_kernel_api::env::{EnvError, FileContent, WritePrecondition};
use rust_cli_kernel_api::env_files::{
    DirEntry, FileKind, FileStat, FileWritten, PutFile, ReadText, TextRead, TextSlice,
};

use crate::meta::{display, io_error, kind, mtime_ms, version};
use crate::text::select;

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
fn metadata(path: &Path) -> Result<Option<fs::Metadata>, EnvError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path, &error)),
    }
}

pub(crate) fn stat(path: &Path) -> Result<Option<FileStat>, EnvError> {
    Ok(metadata(path)?.map(|metadata| FileStat {
        kind: kind(&metadata),
        size: metadata.len(),
        mtime_ms: mtime_ms(&metadata),
        version: version(&metadata),
    }))
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
pub(crate) fn list_dir(path: &Path) -> Result<Vec<DirEntry>, EnvError> {
    let entries = fs::read_dir(path).map_err(|error| io_error(path, &error))?;
    let mut listed = Vec::new();
    for entry in entries.flatten() {
        // 符号链接按它指向的东西分类（与 Node 的 stat 一致）；断链算 Other。
        let kind = fs::metadata(entry.path()).map_or(FileKind::Other, |metadata| kind(&metadata));
        listed.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
        });
    }
    Ok(listed)
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
pub(crate) fn read_file(path: &Path) -> Result<FileContent, EnvError> {
    let bytes = fs::read(path).map_err(|error| io_error(path, &error))?;
    let metadata = metadata(path)?.ok_or_else(|| EnvError::NotFound {
        path: display(path),
    })?;
    Ok(FileContent {
        bytes,
        version: version(&metadata),
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
pub(crate) fn read_text(path: &Path, request: &ReadText) -> Result<TextRead, EnvError> {
    let Some(before) = metadata(path)? else {
        return Err(EnvError::NotFound {
            path: display(path),
        });
    };
    if before.is_dir() {
        return Ok(TextRead::Directory);
    }
    if let Some(limit) = request.max_bytes
        && before.is_file()
        && before.len() > limit
    {
        return Ok(TextRead::TooLarge { size: before.len() });
    }
    let bytes = fs::read(path).map_err(|error| io_error(path, &error))?;
    if let Some(limit) = request.max_bytes
        && bytes.len() as u64 > limit
    {
        return Ok(TextRead::TooLarge {
            size: bytes.len() as u64,
        });
    }
    let text = String::from_utf8_lossy(&bytes);
    let selected = select(
        &text,
        bytes.len() as u64,
        request.start_line,
        request.max_lines,
    );
    let read_bytes = selected.content.len() as u64;
    Ok(TextRead::Text(TextSlice {
        content: selected.content,
        line_count: selected.line_count,
        total_lines: selected.total_lines,
        total_bytes: selected.total_bytes,
        read_bytes,
        mtime_ms: mtime_ms(&before),
        version: version(&before),
    }))
}

fn admits(current: Option<&fs::Metadata>, precondition: &WritePrecondition) -> bool {
    match (precondition, current) {
        (WritePrecondition::Any, _) | (WritePrecondition::Absent, None) => true,
        (WritePrecondition::Absent, Some(_)) | (WritePrecondition::Version(_), None) => false,
        (WritePrecondition::Version(expected), Some(metadata)) => &version(metadata) == expected,
    }
}

/// 条件写：检查前置条件与写入在同一个执行侧操作里完成。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
pub(crate) fn put_file(path: &Path, request: &PutFile) -> Result<FileWritten, EnvError> {
    let current = metadata(path)?;
    if !admits(current.as_ref(), &request.precondition) {
        return Err(EnvError::PreconditionFailed {
            path: display(path),
        });
    }
    if current.as_ref().is_some_and(fs::Metadata::is_dir) {
        return Err(EnvError::Io {
            message: format!(
                "EISDIR: illegal operation on a directory, open '{}'",
                display(path)
            ),
        });
    }
    if request.create_parents
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| io_error(parent, &error))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| io_error(path, &error))?;
    file.write_all(&request.bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error(path, &error))?;
    drop(file);
    let written = metadata(path)?.ok_or_else(|| EnvError::NotFound {
        path: display(path),
    })?;
    // 写后核对：网络盘、云同步目录可能静默截断。
    if written.len() != request.bytes.len() as u64 {
        return Err(EnvError::Io {
            message: format!(
                "Write verification failed: {} is {} bytes on disk, expected {}. The filesystem may have silently truncated the write (network drive / cloud sync).",
                display(path),
                written.len(),
                request.bytes.len()
            ),
        });
    }
    Ok(FileWritten {
        version: version(&written),
        mtime_ms: mtime_ms(&written),
        created: current.is_none(),
    })
}
