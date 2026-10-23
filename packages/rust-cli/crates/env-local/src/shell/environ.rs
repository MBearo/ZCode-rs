//! 子进程的环境变量投影（docs/specs/rust-cli/README.md §4 第二层、shell-permissions.md §2.3）。
//!
//! 入口 crate 捕获一次进程环境，这里按白名单挑出要交给用户命令的变量：shell 与终端、语言区域、
//! 路径与临时目录、代理、SSH agent、常见工具链目录。账号与模型的凭据（`ZCODE_API_KEY` 等）不在
//! 白名单里，不会进入用户命令。另外固定设置 `GIT_EDITOR=true`（git 不打开交互编辑器）与 `ZCODE=1`
//! （子进程据此知道自己在 ZCode 里运行）。

use std::collections::BTreeMap;

/// 原样传入的变量名（Windows 上按不区分大小写比较）。
const NAMES: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "COLORTERM",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SSH_AUTH_SOCK",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "GOPATH",
    "GOROOT",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "JAVA_HOME",
    "NVM_DIR",
    "PNPM_HOME",
    "PYENV_ROOT",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "HOMEBREW_PREFIX",
    // Windows
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "USERPROFILE",
    "USERNAME",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

/// 按前缀传入的变量。
const PREFIXES: &[&str] = &["LC_", "XDG_"];

fn allowed(name: &str) -> bool {
    let matches = |known: &str| rust_cli_base::secret::same_env_name(name, known, cfg!(windows));
    NAMES.iter().any(|known| matches(known))
        || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// 快照里的 `SHELL`（非空）。修复线 F58：`SHELL` 只有这一个来源——入口在 Windows 上找到 Git Bash 时已把快照里的值
/// 换成它，环境段、git 等子进程、MCP stdio 都读这里。以前 env-local 三处各读一次，没设时 git 写空串、
/// MCP 写 `/bin/sh`；应只有 `SHELL` 一个值，子进程继承它，没有缺省。
pub(crate) fn shell_variable(snapshot: &[(String, String)]) -> Option<&str> {
    rust_cli_base::secret::env_var(snapshot, "SHELL").filter(|value| !value.is_empty())
}

/// 用户命令的完整环境：白名单里的变量加上固定设置。`shell` 是实际执行命令的 shell（Bash 命令与 hook 命令写它）；
/// None 时 `SHELL` 照快照继承（git、pdf、worktree、MCP stdio）。
pub(crate) fn project(
    snapshot: &[(String, String)],
    shell: Option<&str>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = snapshot
        .iter()
        .filter(|(name, _)| allowed(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    if let Some(shell) = shell {
        env.insert("SHELL".to_owned(), shell.to_owned());
    }
    env.insert("GIT_EDITOR".to_owned(), "true".to_owned());
    env.insert("ZCODE".to_owned(), "1".to_owned());
    env
}

#[cfg(test)]
#[path = "environ_tests.rs"]
mod tests;
