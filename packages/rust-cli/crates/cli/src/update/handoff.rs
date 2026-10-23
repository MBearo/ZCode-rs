//! Windows 的更新交接（docs/specs/rust-cli/update.md §8、§8.1，工作线 F66）。
//!
//! 缺陷修复（update.md 以前的 D7）：Windows 上正在运行的可执行文件（平台包里的 `zcode.exe`）不能被删除或覆盖，以前二进制自己起 npm，
//! npm 换平台包时报 EPERM / EBUSY。现在 Windows 的 npm 启动器给一个交接文件的路径（[`VARIABLE`]），二进制只往里写
//! 目标版本、照常退出；启动器在本进程退出之后自己拼命令执行。没有交接变量时 Unix 照旧在本进程执行，Windows 只打印
//! 命令（有标记却不是经 Windows 的启动器启动，没有启动器替它执行）。

use std::path::{Path, PathBuf};

use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};

/// 交接变量：启动器给的交接文件的路径（只在 Windows 上设，每次启动一个新路径）。
pub(crate) const VARIABLE: &str = "ZCODE_MANAGED_UPDATE_HANDOFF";
/// `zcode update` 的目标：npm 的 dist-tag（不查版本，与 Unix 的 `npm install -g <包>` 一样装 latest）。
pub(crate) const LATEST: &str = "latest";

/// 执行更新的做法（update.md §8 的表）。只看交接变量有没有与平台，不看安装方式（安装方式在这之前已经判定）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// 本进程起包管理器（Unix，没有交接变量）。
    Run,
    /// 写交接文件、照常退出，由启动器在本进程退出后执行。
    Handoff(PathBuf),
    /// Windows 上没有交接变量：不执行，打印要用户自己执行的命令。
    Manual,
}

/// 按入口的环境快照选做法：有交接变量（非空）时交接，不论平台（Unix 的启动器不设它，所以 macOS / Linux 不变）。
pub(crate) fn route(env: &[(String, String)], windows: bool) -> Route {
    match rust_cli_base::secret::env_var(env, VARIABLE).filter(|path| !path.is_empty()) {
        Some(path) => Route::Handoff(PathBuf::from(path)),
        None if windows => Route::Manual,
        None => Route::Run,
    }
}

/// 交接文件的内容：一行 JSON 加换行，只有目标版本（只写数据，不写命令、包名与包管理器）。
pub(crate) fn render(version: &str) -> String {
    let mut text = serde_json::json!({ "version": version }).to_string();
    text.push('\n');
    text
}

/// 原子写交接文件。目录要已经存在（启动器给的是系统临时目录下的路径）；不存在时报错，不替启动器建目录。
pub(crate) async fn write(path: &Path, version: &str) -> std::io::Result<()> {
    let options = WriteOptions {
        mode: FileMode::Private,
        temp_name: TempName::Hidden,
        parent: ParentDir::Existing,
        rename_retry: &[],
    };
    rust_cli_fs::atomic_write(path, render(version).as_bytes(), options).await
}

/// Windows 上没有交接变量时的报错：`command_line` 是安装命令（update.md §2）。
pub(crate) fn manual(command_line: &str) -> String {
    format!(
        "Error: Cannot update ZCode while it is running on Windows. Please run `{command_line}` after it exits."
    )
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
