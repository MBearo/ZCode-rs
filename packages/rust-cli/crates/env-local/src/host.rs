//! 环境事实（`Environment::facts`，docs/specs/rust-cli/context.md §12）：平台、系统版本、shell 与
//! git 仓库判断。写法见 context.md §12 的环境段。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rust_cli_kernel_api::env_git::marks_git_root;

/// Node `process.platform` 的写法（定义在 base，F55）。
pub(crate) use rust_cli_base::js::node_platform as platform;

/// shell 名：`$SHELL` 含 zsh 为 `zsh`，含 bash 为 `bash`，否则原样，没有为 `unknown`；
/// Windows 上没有 Git Bash（Bash 工具不可用）时为 `PowerShell`。
pub(crate) fn describe_shell(shell: Option<&str>, bash: bool) -> String {
    let shell = shell.filter(|value| !value.is_empty()).unwrap_or("unknown");
    if cfg!(windows) && !bash {
        return "PowerShell".to_owned();
    }
    if shell.contains("zsh") {
        "zsh".to_owned()
    } else if shell.contains("bash") {
        "bash".to_owned()
    } else {
        shell.to_owned()
    }
}

/// `path`（某一层的 `.git`）是不是 git 根的标记：本机 `metadata`，种类按 kernel-api 的 `marks_git_root`（与经端口的
/// 判定同一条规则，F58）。
#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
fn git_marker(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| marks_git_root(crate::meta::kind(&metadata)))
}

/// 最近的含 `.git`（目录或文件）的祖先目录。阻塞调用。
pub(crate) fn git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| git_marker(&dir.join(".git")))
        .map(Path::to_path_buf)
}

/// Node 的 `${os.type()} ${os.release()}`，Unix 上就是 `uname -sr`。失败时为空串（环境段不出这一行）。
/// Windows 的 `os.version()` 是产品名，暂不给出（context.md §7）。
pub(crate) async fn os_version(env: &[(String, String)]) -> String {
    if cfg!(windows) {
        return String::new();
    }
    let mut command = rust_cli_proc::command("uname");
    command
        .arg("-sr")
        .env_clear()
        .envs(env.iter().filter(|(name, _)| name == "PATH").cloned())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    match command.output().await {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        Ok(_) | Err(_) => String::new(),
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
