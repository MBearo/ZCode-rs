// 复制自 rust/crates/kit/src/fs.rs（feat/rs 79b6691c55）。
//! 原子写（docs/specs/rust-u0-foundation.md §3.2，缺陷 B6）、与 Node 同形的跨进程目录锁
//! （[`with_file_lock`]，docs/specs/rust-cli/platform.md §5.5），以及轮询读文件前比元数据的
//! [`FileStamp`] / [`ChangeGate`]（docs/specs/rust-provider-config-poll.md）。
//!
//! 唯一的“临时文件 + rename”实现：同目录 `create_new` 临时文件 → 写入 → `sync_all` → rename
//! （目标被短暂占用时按调用方给的表重试）→ fsync 父目录（Unix，尽力而为）。任一步失败都删除
//! 临时文件；目标文件要么是旧内容，要么是完整的新内容。
//!
//! 持久性：Node 的 `atomicWriteText` / `atomicWritePrivateTextFile` 不 fsync 临时文件，也不 fsync
//! 父目录，断电后可能留下长度为 0 的目标文件或丢失这次 rename。这里两步都做，是有意的加强；
//! 文件内容、权限与临时文件命名仍与 Node 一致，Node 与 Rust 可以交替写同一个文件。

// 文件类 IO 在 crate 级放开：本 crate 的 clippy.toml 是根配置去掉文件类的禁令（README §4，F7）。以前这里是
// crate 级的 `#![expect(clippy::disallowed_methods)]`，连进程、环境、套接字、时间一起放开了；其余类别现在逐函数豁免。

use std::ffi::{OsStr, OsString};
use std::hash::Hasher;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod exclusive;
mod lock;
mod lock_pid;
mod lock_single;
mod stamp;
pub use exclusive::{ExclusiveLock, lock_exclusive, try_lock_exclusive};
pub use lock::{FILE_LOCK_TIMEOUT_CODE, FileLock, LockOptions, acquire_file_lock, with_file_lock};
pub use lock_pid::{PidLockOptions, with_pid_lock};
pub use lock_single::{SingleLockOptions, with_single_file_lock};
pub use stamp::{ChangeGate, FileStamp};

/// 进程是否存活（Node `process.kill(pid, 0)` 的语义：能发信号或无权限都算存活）。锁文件里记的 PID 用它
/// 判断（目录锁的回收、定时任务调度锁，tools-more.md §16.4）。
pub fn process_alive(pid: u32) -> bool {
    lock::alive::is_alive(pid)
}

/// 目标文件的权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    /// 系统默认（`0o666 & !umask`）。
    Default,
    /// Unix 上恰好 `0o600`（不受 umask 影响）；Windows 上与 `Default` 相同。
    Private,
    /// Unix 上恰好 `0o755`（可执行文件：下载的执行器，remote-exec.md §26.4）；Windows 上与 `Default` 相同。
    Executable,
}

/// 临时文件名：`<name>.<pid>.<ms>.<rand>.tmp` 或在前面加 `.`。
///
/// Node `atomicWriteText` 用可见名，并清理同目录下过期的 `<name>.*.tmp`；凭据、配置等私密写入
/// 用隐藏名。同一个文件可能被 Node 与 Rust 交替写入，所以每个调用处必须沿用 Node 对应实现的命名，
/// 否则 Node 的清理规则会漏掉或误删。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempName {
    Hidden,
    Visible,
}

/// 父目录不存在时的处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentDir {
    /// 调用方已经准备好目录（例如持锁前创建并设置了权限）；目录不存在时报错。
    Existing,
    /// 递归创建（`create_dir_all`，系统默认权限）。
    Create,
}

/// Node 原子写的 rename 重试间隔（`DEFAULT_RENAME_RETRY_DELAYS_MS`：50、100、200、400、800 ms）。凭据与项目 hooks 的
/// 信任记录都用它（F55：以前 platform 的两处各写一份）。
pub const NODE_RENAME_RETRY: &[Duration] = &[
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions {
    pub mode: FileMode,
    pub temp_name: TempName,
    pub parent: ParentDir,
    /// rename 因目标被短暂占用而失败时（PermissionDenied、ResourceBusy，Windows 的共享 / 锁冲突），
    /// 依次等待这些时长后重试；用完后返回最后一次的错误。空切片表示不重试。
    pub rename_retry: &'static [Duration],
}

/// 读整个文件；文件不存在时返回 `None`（配置、凭据等“可以没有”的本机文件）。其他错误照常返回。
pub async fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 打开一个文件只读，按块读（上传到远端的执行器与它的 sha256，docs/specs/rust-cli/remote-exec.md §19.9）。
pub async fn open_read(path: &Path) -> io::Result<tokio::fs::File> {
    tokio::fs::File::open(path).await
}

/// 删掉一个文件；文件不存在时不算错（外部编辑器的临时文件，tui-input.md §4.1）。
pub async fn remove_optional(path: &Path) -> io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// 在文件末尾追加 `bytes`（按需建父目录与文件）。只给"一个写者、按顺序追加"的日志类文件用
/// （例如 TUI 输入历史）；调用方保证同一个文件只有一个写者。
pub async fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, bytes).await?;
    tokio::io::AsyncWriteExt::flush(&mut file).await
}

/// 用 `bytes` 原子地替换 `path`，见模块文档。
pub async fn atomic_write(path: &Path, bytes: &[u8], options: WriteOptions) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            format!("atomic write target has no file name: {}", path.display()),
        )
    })?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) | None => PathBuf::from("."),
    };
    let temp = directory.join(temp_name(name, options.temp_name));
    // 用 std::fs 在一个 blocking 任务里完成：tokio File 的 write_all 只是排队，写入错误要等下一次
    // flush 才会返回，直接 sync_all 会把它吞掉。
    let staged = {
        let (directory, temp, bytes) = (directory.clone(), temp.clone(), bytes.to_vec());
        rust_cli_base::blocking(move || {
            stage(&directory, &temp, &bytes, options.mode, options.parent)
        })
        .await
        .map_err(io::Error::other)?
    };
    staged?;
    if let Err(error) = rename_with_retry(&temp, path, options.rename_retry).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error);
    }
    sync_directory(directory).await;
    Ok(())
}

/// 创建并写好临时文件。打开失败时文件不是本次创建的（例如重名），不能删除；之后的失败删除它。
fn stage(
    directory: &Path,
    temp: &Path,
    bytes: &[u8],
    mode: FileMode,
    parent: ParentDir,
) -> io::Result<()> {
    match parent {
        ParentDir::Create => std::fs::create_dir_all(directory)?,
        ParentDir::Existing => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if mode == FileMode::Private {
        // 从创建起就不对其他用户可读，避免写入前的窗口期。
        std::os::unix::fs::OpenOptionsExt::mode(
            &mut options,
            rust_cli_base::proto::mode::PRIVATE_FILE,
        );
    }
    let mut file = options.open(temp)?;
    let written = restrict(&file, mode)
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all());
    drop(file);
    if written.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    written
}

/// open 时给的 mode 会被 umask 削减；再显式设置一次，私密文件恰好是 0600。
#[cfg(unix)]
fn restrict(file: &std::fs::File, mode: FileMode) -> io::Result<()> {
    use rust_cli_base::proto::mode as bits;
    use std::os::unix::fs::PermissionsExt;
    match mode {
        FileMode::Private => {
            file.set_permissions(std::fs::Permissions::from_mode(bits::PRIVATE_FILE))
        }
        FileMode::Executable => {
            file.set_permissions(std::fs::Permissions::from_mode(bits::EXECUTABLE))
        }
        FileMode::Default => Ok(()),
    }
}

#[cfg(not(unix))]
fn restrict(_file: &std::fs::File, mode: FileMode) -> io::Result<()> {
    match mode {
        FileMode::Private | FileMode::Executable | FileMode::Default => Ok(()),
    }
}

async fn rename_with_retry(temp: &Path, target: &Path, delays: &[Duration]) -> io::Result<()> {
    let mut delays = delays.iter();
    loop {
        let Err(error) = tokio::fs::rename(temp, target).await else {
            return Ok(());
        };
        match delays.next() {
            Some(delay) if retryable(&error) => tokio::time::sleep(*delay).await,
            Some(_) | None => return Err(error),
        }
    }
}

/// Windows 的 `ERROR_SHARING_VIOLATION`（系统错误码）。
const ERROR_SHARING_VIOLATION: i32 = 32;
/// Windows 的 `ERROR_LOCK_VIOLATION`（系统错误码）。
const ERROR_LOCK_VIOLATION: i32 = 33;

/// 与 Node 的 `EPERM` / `EACCES` / `EBUSY` 判定一致。libuv 把 Windows 的
/// `ERROR_SHARING_VIOLATION`（32）与 `ERROR_LOCK_VIOLATION`（33）映射为 `EBUSY`，std 不映射，单独判断。
fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::PermissionDenied | ErrorKind::ResourceBusy
    ) || (cfg!(windows)
        && matches!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        ))
}

/// rename 之后 fsync 父目录，让目录项的变更落盘。尽力而为：rename 已经成功，有的文件系统不支持
/// 目录 fsync，失败不改变写入结果。Windows 没有对应操作。
async fn sync_directory(directory: PathBuf) {
    if cfg!(unix) {
        let _ = rust_cli_base::blocking(move || std::fs::File::open(directory)?.sync_all()).await;
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "临时文件名里的毫秒是 Node 的命名格式（`<name>.<pid>.<ms>.<rand>.tmp`）；fs 在 kernel-api 之下，拿不到注入的 Clock"
)]
fn temp_name(name: &OsStr, style: TempName) -> OsString {
    // 进程内计数器保证同一进程的并发写互不冲突；RandomState 的随机种子区分 pid 复用后的进程。
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = rust_cli_base::random::hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let mut temp = OsString::new();
    match style {
        TempName::Hidden => temp.push("."),
        TempName::Visible => {}
    }
    temp.push(name);
    temp.push(format!(
        ".{}.{millis}.{:016x}.tmp",
        std::process::id(),
        hasher.finish()
    ));
    temp
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

/// 递归创建目录，已存在时成功（例如自动记忆目录，docs/specs/rust-cli/memory.md §2）。
pub async fn create_dir_all(path: &Path) -> io::Result<()> {
    tokio::fs::create_dir_all(path).await
}

/// 递归创建只有本用户能访问的目录，并把最后一层设为 0700（Node `mkdir(dir, {recursive: true, mode:
/// 0o700})` 后 `chmod(dir, 0o700)`，信任记录的 `security/`，config.md §4.2）。Windows 上同 [`create_dir_all`]。
pub async fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let path = path.to_path_buf();
    rust_cli_base::blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            builder.mode(rust_cli_base::proto::mode::PRIVATE_DIR);
            builder.create(&path)?;
            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(rust_cli_base::proto::mode::PRIVATE_DIR),
            )
        }
        #[cfg(not(unix))]
        builder.create(&path)
    })
    .await
    .map_err(io::Error::other)?
}

/// 把一个已有的文件（含 socket 文件）设成只有本用户能读写（Unix 0600；Windows 上什么都不做）：跨会话的收件箱
/// socket 绑定后由适配器调用（docs/specs/rust-cli/cross-session.md §2.2）。
pub async fn restrict_to_owner(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(rust_cli_base::proto::mode::PRIVATE_FILE),
        )
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// 文件的字节数；不存在时为 `None`（运行日志续写同一天的文件，docs/specs/rust-cli/logging.md §1）。
pub async fn file_len(path: &Path) -> io::Result<Option<u64>> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 文件的修改时间；不存在时为 `None`（模型轨迹按修改时间从旧到新淘汰文件，docs/specs/rust-cli/model-io.md §3.3）。
pub async fn file_modified(path: &Path) -> io::Result<Option<SystemTime>> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata.modified().map(Some),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 目录下普通文件的名字，按名字排序；目录不存在时为空（运行日志的保留清理，logging.md §1）。
pub async fn file_names(path: &Path) -> io::Result<Vec<String>> {
    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// 目录下的子目录名，按名字排序（Node 插件缓存的扫描，docs/specs/rust-cli/ext.md §4.1）。指向目录的
/// 符号链接不算（Node `readdir({withFileTypes: true})` 的 `isDirectory()` 不跟随链接）。目录不存在时返回
/// `NotFound`，由调用方决定是否当作空。
pub async fn subdirectories(path: &Path) -> io::Result<Vec<String>> {
    let mut entries = tokio::fs::read_dir(path).await?;
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}
