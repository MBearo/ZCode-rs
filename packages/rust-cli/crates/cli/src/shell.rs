//! 本机 shell 的配置（docs/specs/rust-cli/shell-permissions.md §2.5）：入口 crate 是唯一读进程环境
//! 与本机文件系统事实的位置，这里把它们换成 [`ShellConfig`] 交给 env-local。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rust_cli_base::secret::EnvSnapshot;
use rust_cli_env_local::{EmbeddedSearch, ShellConfig};

/// 固定的候选目录。
const SHELL_DIRS: [&str; 4] = ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"];
/// Git Bash 的固定位置。
const GIT_BASH_DIRS: [&str; 2] = [
    r"C:\Program Files\Git\bin\bash.exe",
    r"C:\Program Files (x86)\Git\bin\bash.exe",
];
/// `ZCODE_GIT_BASH_PATH` 认的文件名（比较时转小写）。
const GIT_BASH_NAMES: [&str; 4] = ["bash.exe", "sh.exe", "bash", "sh"];
/// 探测时跑 `<shell> --version` 的时限。
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// Windows 上没有 `PATHEXT` 时试的扩展名。
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// 进程环境快照里的一个变量。
fn lookup<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    rust_cli_base::secret::env_var(env, name).filter(|value| !value.is_empty())
}

/// 找 shell 要问的本机事实；测试换成表（shell_tests.rs）。
pub(crate) trait Host {
    /// 能执行：`access(X_OK)` 通过，或者 `<path> --version` 在 1 s 内退出码为 0。
    async fn executable(&self, path: &str) -> bool;
    /// PATH 里第一个能执行的同名文件（同 `Bun.which`）。
    fn which(&self, name: &str) -> Option<String>;
    /// 文件存在（找 Git Bash 时只看存在）。
    fn exists(&self, path: &str) -> bool;
}

fn supported(shell: &str) -> bool {
    shell.contains("bash") || shell.contains("zsh")
}

/// 找 shell：`ZCODE_SHELL`
/// 是能执行的 bash / zsh 时用它；否则按候选表取第一个能执行的。`shell` 是 `SHELL`（Windows 上是 Git Bash）。
async fn find(overridden: Option<&str>, shell: Option<&str>, host: &impl Host) -> Option<String> {
    if let Some(chosen) = overridden {
        if supported(chosen) && host.executable(chosen).await {
            tracing::debug!(shell = chosen, "using shell override ZCODE_SHELL");
            return Some(chosen.to_owned());
        }
        tracing::debug!(
            shell = chosen,
            "ZCODE_SHELL is not a valid bash/zsh path, falling back to detection"
        );
    }
    let prefer_bash = shell.is_some_and(|value| value.contains("bash"));
    let (zsh, bash) = (host.which("zsh"), host.which("bash"));
    let order = if prefer_bash {
        ["bash", "zsh"]
    } else {
        ["zsh", "bash"]
    };
    let mut candidates: VecDeque<String> = order
        .iter()
        .flat_map(|name| SHELL_DIRS.iter().map(move |dir| format!("{dir}/{name}")))
        .collect();
    let (first, last) = if prefer_bash {
        (bash, zsh)
    } else {
        (zsh, bash)
    };
    if let Some(path) = first {
        candidates.push_front(path);
    }
    if let Some(path) = last {
        candidates.push_back(path);
    }
    if let Some(value) = shell.filter(|value| supported(value))
        && host.executable(value).await
    {
        candidates.push_front(value.to_owned());
    }
    for candidate in candidates {
        if host.executable(&candidate).await {
            return Some(candidate);
        }
    }
    tracing::warn!("no suitable shell found (bash or zsh); Bash commands will fail");
    None
}

/// Windows 路径的最后一段（`path.win32.basename`：`\` 与 `/` 都是分隔符）。
fn windows_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Windows 路径的上一级（去掉最后一段）。
fn windows_parent(path: &str) -> Option<&str> {
    path.rfind(['\\', '/']).map(|at| &path[..at])
}

/// Windows 上的 Git Bash。
fn git_bash(configured: Option<&str>, host: &impl Host) -> Option<String> {
    if let Some(path) = configured {
        let named = GIT_BASH_NAMES.contains(&windows_name(path).to_lowercase().as_str());
        if named && host.exists(path) {
            return Some(path.to_owned());
        }
        tracing::warn!(
            path,
            reason = if named {
                "not found"
            } else {
                "is not a bash/sh binary"
            },
            "ZCODE_GIT_BASH_PATH ignored; falling back to auto-detection"
        );
    }
    if let Some(found) = GIT_BASH_DIRS.iter().find(|path| host.exists(path)) {
        return Some((*found).to_owned());
    }
    // `<git 目录>\..\..\bin\bash.exe`：git.exe 所在目录的上一级。
    let git = host.which("git")?;
    let root = windows_parent(windows_parent(&git)?)?;
    let candidate = format!(r"{root}\bin\bash.exe");
    host.exists(&candidate).then_some(candidate)
}

/// Bash 工具用的 shell（shell-permissions.md §2.5）。Windows 上先找 Git Bash，找到时把它当作 `SHELL`（启动时
/// 把 `process.env.SHELL` 设成它），找不到时 Bash 工具不可用。
///
/// 缺陷修复（F44）：以前 Unix 上固定 `/bin/bash`，没有 bash 的镜像里命令都启动失败（`No such file or directory`）。
/// 入口经 [`resolve`] 一并拿到 Git Bash（F58）；这个包装只给测试看 shell 的选法。
#[cfg(test)]
async fn bash(env: &[(String, String)], windows: bool, host: &impl Host) -> Option<PathBuf> {
    resolve(env, windows, host).await.1
}

/// （Windows 上找到的 Git Bash，Bash 工具用的 shell）。Git Bash 只找一次，入口用它改写快照里的 `SHELL`。
async fn resolve(
    env: &[(String, String)],
    windows: bool,
    host: &impl Host,
) -> (Option<String>, Option<PathBuf>) {
    let git_bash = if windows {
        git_bash(
            lookup(env, rust_cli_base::names::env::ZCODE_GIT_BASH_PATH),
            host,
        )
    } else {
        None
    };
    let shell = if windows {
        let Some(found) = git_bash.as_deref() else {
            return (None, None);
        };
        Some(found.to_owned())
    } else {
        lookup(env, rust_cli_base::names::env::SHELL).map(str::to_owned)
    };
    let found = find(
        lookup(env, rust_cli_base::names::env::ZCODE_SHELL),
        shell.as_deref(),
        host,
    )
    .await
    .map(PathBuf::from);
    (git_bash, found)
}

/// `SHELL` 的唯一取值（修复线 F58，README §4.1）：Windows 上找到 Git Bash 时快照里的 `SHELL` 换成它（启动时
/// 改写 `process.env.SHELL`），否则不变。执行环境（环境段、git、pdf、worktree）与 MCP stdio 都读这份快照。以前
/// Windows 上快照里没有这个值，环境段写 `unknown`（不是 `bash`）。
fn shell_snapshot(env: EnvSnapshot, git_bash: Option<&str>) -> EnvSnapshot {
    match git_bash {
        Some(found) => env.with_var(rust_cli_base::names::env::SHELL, found),
        None => env,
    }
}

/// 本机的事实：按入口快照的 `PATH` 找，探测进程经 proc 起。
struct System {
    path: Vec<String>,
    /// Windows 上 which 试的扩展名（`PATHEXT`）；其他平台为空。
    extensions: Vec<String>,
    windows: bool,
}

impl System {
    fn new(env: &[(String, String)], windows: bool) -> Self {
        let separator = if windows { ';' } else { ':' };
        let path = lookup(env, rust_cli_base::names::env::PATH)
            .unwrap_or_default()
            .split(separator)
            .map(str::to_owned)
            .collect();
        let extensions = if windows {
            lookup(env, rust_cli_base::names::env::PATHEXT)
                .unwrap_or(DEFAULT_PATHEXT)
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(str::to_lowercase)
                .collect()
        } else {
            Vec::new()
        };
        Self {
            path,
            extensions,
            windows,
        }
    }

    /// `PATH` 原文（探测进程的环境只有它）。
    fn path_value(&self) -> String {
        self.path.join(if self.windows { ";" } else { ":" })
    }
}

/// 执行位的判断（`access(X_OK)` 的近似）：`None` 是文件不存在。Windows 上存在即可（Node 的 X_OK 在 Windows 上
/// 同 F_OK）。
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 判断本机的 shell 是否存在、能否执行（spec §4）"
)]
fn mode_executable(path: &Path) -> Option<bool> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(false),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // 属主、属组、其他人任一可执行的权限位。
        const ANY_EXECUTE: u32 = 0o111;
        Some(metadata.permissions().mode() & ANY_EXECUTE != 0)
    }
    // Windows 交叉 clippy 报 `drop_non_drop`（F66）：`Metadata` 没有 Drop，用 `let _` 表示不用它。
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Some(true)
    }
}

/// 是能执行的普通文件（which 用）。
#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 判断本机的 shell 是否存在（spec §4）"
)]
fn executable_file(path: &Path) -> bool {
    path.is_file() && mode_executable(path) == Some(true)
}

#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 判断本机的 shell 与 bwrap 是否存在（spec §4）"
)]
fn present(path: &Path) -> bool {
    path.is_file()
}

impl Host for System {
    async fn executable(&self, path: &str) -> bool {
        match mode_executable(Path::new(path)) {
            Some(true) => return true,
            // 绝对路径不存在：跑它同样是 ENOENT，省掉一次进程。
            None if Path::new(path).is_absolute() => return false,
            _ => {}
        }
        let mut command = rust_cli_proc::command(path);
        command
            .arg("--version")
            .env_clear()
            .env(rust_cli_base::names::env::PATH, self.path_value())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        matches!(
            tokio::time::timeout(PROBE_TIMEOUT, command.status()).await,
            Ok(Ok(status)) if status.success()
        )
    }

    fn which(&self, name: &str) -> Option<String> {
        let names: Vec<String> = if self.windows {
            self.extensions
                .iter()
                .map(|extension| format!("{name}{extension}"))
                .collect()
        } else {
            vec![name.to_owned()]
        };
        // 空项与相对目录跳过（shell-permissions.md D15：不让工作区里的文件冒充 shell）。
        self.path
            .iter()
            .filter(|dir| !dir.is_empty() && Path::new(dir).is_absolute())
            .flat_map(|dir| names.iter().map(move |name| Path::new(dir).join(name)))
            .find(|candidate| executable_file(candidate))
            .map(|found| found.to_string_lossy().into_owned())
    }

    fn exists(&self, path: &str) -> bool {
        present(Path::new(path))
    }
}

/// Linux 上的 bwrap：在 PATH 里找。
fn bwrap(env: &[(String, String)]) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    lookup(env, rust_cli_base::names::env::PATH)?
        .split(':')
        .map(|dir| Path::new(dir).join("bwrap"))
        .find(|candidate| present(candidate))
}

/// 输出文件与进程账本放在本用户私有的临时目录。
///
/// 缺陷修复 R2-1：原来是共享的 `<tmp>/zcode-shell/{sessions,ledger}`，没有 uid，别的用户能读输出、伪造账本
/// 记录。现在是 `<tmp>/zcode-<uid>`（Windows `<tmp>/zcode`），两者共用它：执行环境与账本每次用之前都核对
/// 它是本用户私有的目录（核对的是根本身，所以不再分子目录）。
///
/// `find_and_grep`：启动参数没有点名 Glob / Grep（embedded-search.md §8.1）。取不到本进程二进制的路径
/// 时没有内嵌的搜索工具（Bash 里用系统的 find / grep / rg）。
pub(crate) async fn config(env: EnvSnapshot, temp: &Path, find_and_grep: bool) -> ShellConfig {
    let root = rust_cli_proc::private::user_dir(temp);
    let windows = cfg!(windows);
    let (git_bash, shell) = resolve(&env, windows, &System::new(&env, windows)).await;
    let env = shell_snapshot(env, git_bash.as_deref());
    ShellConfig {
        scratch: root.clone(),
        ledger: root,
        bash: shell,
        bwrap: bwrap(&env),
        env,
        embedded: crate::embedded::executable().map(|executable| EmbeddedSearch {
            executable,
            find_and_grep,
        }),
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
