//! 子进程的环境变量投影（docs/specs/rust-cli/README.md §4 第二层、shell-permissions.md §2.3）。
//!
//! 入口 crate 捕获一次进程环境，这里按白名单挑出要交给用户命令的变量：shell 与终端、语言区域、
//! 路径与临时目录、代理、SSH agent、常见工具链目录。账号与模型的凭据（`ZCODE_API_KEY` 等）不在
//! 白名单里，不会进入用户命令。另外固定设置 `GIT_EDITOR=true`（git 不打开交互编辑器）与 `ZCODE=1`
//! （子进程据此知道自己在 ZCode 里运行）。

use std::collections::BTreeMap;

/// 原样传入的变量名（Windows 上按不区分大小写比较）。
const NAMES: &[&str] = &[
    rust_cli_base::names::env::PATH,
    rust_cli_base::names::env::HOME,
    rust_cli_base::names::env::USER,
    rust_cli_base::names::env::LOGNAME,
    rust_cli_base::names::env::SHELL,
    rust_cli_base::names::env::TERM,
    rust_cli_base::names::env::COLORTERM,
    rust_cli_base::names::env::LANG,
    rust_cli_base::names::env::LANGUAGE,
    rust_cli_base::names::env::TZ,
    rust_cli_base::names::env::TMPDIR,
    rust_cli_base::names::env::TEMP,
    rust_cli_base::names::env::TMP,
    rust_cli_base::names::env::SSH_AUTH_SOCK,
    rust_cli_base::names::env::DISPLAY,
    rust_cli_base::names::env::WAYLAND_DISPLAY,
    rust_cli_base::names::env::EDITOR,
    rust_cli_base::names::env::VISUAL,
    rust_cli_base::names::env::PAGER,
    rust_cli_base::names::env::HTTP_PROXY,
    rust_cli_base::names::env::HTTPS_PROXY,
    rust_cli_base::names::env::NO_PROXY,
    rust_cli_base::names::env::ALL_PROXY,
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    rust_cli_base::names::env::SSL_CERT_FILE,
    rust_cli_base::names::env::SSL_CERT_DIR,
    rust_cli_base::names::env::NODE_EXTRA_CA_CERTS,
    rust_cli_base::names::env::GOPATH,
    rust_cli_base::names::env::GOROOT,
    rust_cli_base::names::env::CARGO_HOME,
    rust_cli_base::names::env::RUSTUP_HOME,
    rust_cli_base::names::env::JAVA_HOME,
    rust_cli_base::names::env::NVM_DIR,
    rust_cli_base::names::env::PNPM_HOME,
    rust_cli_base::names::env::PYENV_ROOT,
    rust_cli_base::names::env::VIRTUAL_ENV,
    rust_cli_base::names::env::CONDA_PREFIX,
    rust_cli_base::names::env::HOMEBREW_PREFIX,
    // Windows
    rust_cli_base::names::env::SystemRoot,
    "SystemDrive",
    "windir",
    rust_cli_base::names::env::ComSpec,
    rust_cli_base::names::env::PATHEXT,
    rust_cli_base::names::env::USERPROFILE,
    rust_cli_base::names::env::USERNAME,
    rust_cli_base::names::env::APPDATA,
    rust_cli_base::names::env::LOCALAPPDATA,
    "ProgramData",
    rust_cli_base::names::env::ProgramFiles,
    "ProgramFiles(x86)",
    "ProgramW6432",
    rust_cli_base::names::env::NUMBER_OF_PROCESSORS,
    rust_cli_base::names::env::PROCESSOR_ARCHITECTURE,
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
    rust_cli_base::secret::env_var(snapshot, rust_cli_base::names::env::SHELL)
        .filter(|value| !value.is_empty())
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
        env.insert(
            rust_cli_base::names::env::SHELL.to_owned(),
            shell.to_owned(),
        );
    }
    env.insert(
        rust_cli_base::names::env::GIT_EDITOR.to_owned(),
        "true".to_owned(),
    );
    env.insert("ZCODE".to_owned(), "1".to_owned());
    env
}

/// 加上（或去掉）执行器的服务端按客户端实例推来的变量（远端执行 F96，remote-exec.md §23.5）。保护的名字已经在服务端
/// 拒绝，这里不再查。
pub(crate) fn apply(
    env: &mut BTreeMap<String, String>,
    overrides: &rust_cli_kernel_api::env_command::EnvOverrides,
) {
    for (name, value) in overrides {
        match value {
            Some(value) => {
                env.insert(name.clone(), value.clone());
            }
            None => {
                env.remove(name);
            }
        }
    }
}

#[cfg(test)]
#[path = "environ_tests.rs"]
mod tests;
