//! 本用户私有的临时目录（docs/specs/rust-cli/shell-permissions.md §2.5：建好并核对是本用户私有的
//! 目录）。Bash 的输出文件与进程账本都在这里。
//!
//! 缺陷修复 R2-1：原来放在共享临时目录下的 `zcode-shell`，没有 uid，按默认权限建。Linux 的 `/tmp` 由所有用户
//! 共享：别的用户能读输出、抢先建好这个目录，或放一条伪造的账本记录，让我们的清扫结束自己的进程。现在的
//! 做法：目录名带 uid（Windows 的临时目录本来就按用户分开，不带），用 0700 建；每次用之前核对它不是符号
//! 链接、属主是本 uid，权限不是 0700 时改回 0700。属主不对时拒绝使用，不换位置。

use std::io;
use std::path::{Path, PathBuf};

/// 拒绝时给用户的建议（本工程的临时目录取自 `TMPDIR`）。
#[cfg(unix)]
const HINT: &str = "Set TMPDIR to a directory you control, or ask an administrator to remove it.";

/// 本用户的私有临时目录：Unix 上 `<temp>/zcode-<uid>`，Windows 上 `<temp>/zcode`。
pub fn user_dir(temp: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        temp.join(format!("zcode-{}", uid()))
    }
    #[cfg(windows)]
    {
        temp.join("zcode")
    }
}

/// 本进程的 uid（`process.getuid()`）。入口的 bypass 模式的 root 判断也用它（cli-args.md §5.2；F55：以前
/// cli 另调了一次 getuid）。
#[cfg(unix)]
pub fn uid() -> u32 {
    // SAFETY: getuid(2) 没有参数，总是成功，不读写任何内存。
    #[expect(unsafe_code, reason = "getuid(2) 是 libc 的 FFI")]
    unsafe {
        libc::getuid()
    }
}

#[cfg(unix)]
fn refused(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(unix)]
fn owned_by(dir: &Path, owner: u32) -> io::Error {
    refused(format!(
        "Temp directory {} is owned by uid {owner}, expected {}. Refusing to use it \u{2014} another user may have pre-created it. {HINT}",
        dir.display(),
        uid()
    ))
}

/// 建好 `dir`（缺的上级一起建，权限 0700），再核对它是本用户私有的目录：不是符号链接、属主是本 uid、
/// 权限是 0700（不是时改回）。Windows 上只建目录。
#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "proc 是进程账本与 Bash 输出所在私有临时目录的唯一核对者（spec §4）"
)]
pub async fn ensure(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(rust_cli_base::proto::mode::PRIVATE_DIR)
        .create(dir)
        .await?;
    // O_NOFOLLOW：目录本身是符号链接时打开失败（ELOOP），不跟到别人指定的位置。
    let opened = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .await;
    let file = match opened {
        Ok(file) => file,
        Err(error) if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
            return Err(refused(format!(
                "Temp directory {} is not a directory (may be an attacker-planted symlink). Refusing to use it. {HINT}",
                dir.display()
            )));
        }
        Err(error) => {
            if error.kind() == io::ErrorKind::PermissionDenied
                && let Ok(metadata) = tokio::fs::symlink_metadata(dir).await
                && metadata.uid() != uid()
            {
                return Err(owned_by(dir, metadata.uid()));
            }
            return Err(error);
        }
    };
    let metadata = file.metadata().await?;
    if metadata.uid() != uid() {
        return Err(owned_by(dir, metadata.uid()));
    }
    if metadata.permissions().mode() & rust_cli_base::proto::mode::PERMISSION_BITS
        != rust_cli_base::proto::mode::PRIVATE_DIR
    {
        file.set_permissions(std::fs::Permissions::from_mode(
            rust_cli_base::proto::mode::PRIVATE_DIR,
        ))
        .await?;
    }
    Ok(())
}

/// Windows：临时目录本来就按用户分开，只建目录。
#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "proc 是进程账本与 Bash 输出所在私有临时目录的唯一核对者（spec §4）"
)]
pub async fn ensure(dir: &Path) -> io::Result<()> {
    tokio::fs::create_dir_all(dir).await
}

#[cfg(test)]
#[path = "private_tests.rs"]
mod tests;
