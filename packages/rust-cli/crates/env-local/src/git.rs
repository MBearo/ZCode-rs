//! git 状态（`Git::git_status`，docs/specs/rust-cli/context.md §12）。
//!
//! 命令见 context.md §12：
//! 各命令并发执行，非零退出时该项输出为空（`preserveOutputOnError: false`），stdout 去掉首尾空白。
//! 进程经 `rust-cli-proc` 启动，环境是白名单投影加上受限的 git 变量。

use rust_cli_base::path::DEV_NULL;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rust_cli_kernel_api::env::EnvError;
use rust_cli_kernel_api::env_git::{GitDiff, GitSnapshot, GitStatus};

use crate::host::git_root;

/// 命令的默认超时（10 分钟）。
const TIMEOUT: Duration = Duration::from_secs(600);

/// 读分支的命令不跑仓库的 hook 与 fsmonitor。
const NO_HOOKS: [&str; 4] = ["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor="];

/// 挡住 remote helper 的配置项（经 `GIT_CONFIG_PARAMETERS` 传入）。
const GUARD: &str = "'alias.remote-none=version' 'alias.remote-none.command=version' \
                     'alias.remote-/.command=version' 'fetch.bundleURI=' 'help.autocorrect=0'";

/// git 不交互地要凭据（git 的安全环境里有；worktree 也用，F55：以前写了两次）。
pub(crate) const NO_TERMINAL_PROMPT: (&str, &str) =
    (rust_cli_base::names::env::GIT_TERMINAL_PROMPT, "0");

/// 白名单投影后再加受限的变量：不提示输入、不走网络取对象、不用 ssh、
/// PATH 只留绝对路径。
fn restricted(projected: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = projected;
    for (name, value) in [
        (rust_cli_base::names::env::GIT_ALLOW_PROTOCOL, "/"),
        (rust_cli_base::names::env::GIT_CONFIG_PARAMETERS, GUARD),
        (rust_cli_base::names::env::GIT_NO_LAZY_FETCH, "1"),
        (rust_cli_base::names::env::GIT_NO_REPLACE_OBJECTS, "1"),
        NO_TERMINAL_PROMPT,
        (rust_cli_base::names::env::GIT_LFS_SKIP_SMUDGE, "1"),
        (rust_cli_base::names::env::GIT_SSH_COMMAND, "false"),
    ] {
        env.insert(name.to_owned(), value.to_owned());
    }
    if cfg!(unix)
        && let Some(path) = env.get_mut(rust_cli_base::names::env::PATH)
    {
        let kept: Vec<&str> = path
            .split(':')
            .filter(|entry| entry.starts_with('/'))
            .collect();
        *path = if kept.is_empty() {
            DEV_NULL.to_owned()
        } else {
            kept.join(":")
        };
    }
    env
}

struct Git {
    cwd: PathBuf,
    env: BTreeMap<String, String>,
}

impl Git {
    /// 跑一条 git 命令：成功时为 stdout（去首尾空白），否则为 None。
    async fn run(&self, args: &[&str]) -> Option<String> {
        let mut command = rust_cli_proc::command("git");
        command
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(TIMEOUT, command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            }
            Ok(Ok(_) | Err(_)) | Err(_) => None,
        }
    }

    async fn output(&self, args: &[&str]) -> String {
        self.run(args).await.unwrap_or_default()
    }

    async fn ref_exists(&self, name: &str) -> bool {
        let reference = format!("refs/remotes/origin/{name}");
        let args = [
            &NO_HOOKS[..],
            &["show-ref", "--verify", "--quiet", &reference],
        ]
        .concat();
        self.run(&args).await.is_some()
    }

    /// 当前分支；分离头或读不到时为 `HEAD`。用 symbolic-ref：没有提交的分支也有名字（与读 HEAD 文件
    /// 相同；`rev-parse --abbrev-ref` 在这种仓库上会失败）。
    async fn branch(&self) -> String {
        let args = [&NO_HOOKS[..], &["symbolic-ref", "--short", "-q", "HEAD"]].concat();
        self.run(&args)
            .await
            .filter(|branch| !branch.is_empty())
            .unwrap_or_else(|| "HEAD".to_owned())
    }

    /// `origin/HEAD` 指向且存在的分支，否则 main / master 中存在的一个，都没有为 main。
    async fn default_branch(&self) -> String {
        let args = [
            &NO_HOOKS[..],
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        ]
        .concat();
        if let Some(target) = self.run(&args).await {
            let name = target.strip_prefix("origin/").unwrap_or(&target);
            if !name.is_empty() && self.ref_exists(name).await {
                return name.to_owned();
            }
        }
        for name in ["main", "master"] {
            if self.ref_exists(name).await {
                return name.to_owned();
            }
        }
        "main".to_owned()
    }
}

pub(crate) async fn status(
    cwd: &Path,
    projected: BTreeMap<String, String>,
) -> Result<GitStatus, EnvError> {
    let start = cwd.to_path_buf();
    let root = rust_cli_base::blocking(move || git_root(&start))
        .await
        .map_err(|error| EnvError::Io {
            message: error.to_string(),
        })?;
    if root.is_none() {
        return Ok(GitStatus::NotRepository);
    }
    let git = Git {
        cwd: cwd.to_path_buf(),
        env: restricted(projected),
    };
    let (branch, default_branch, status, recent_commits, user_name) = tokio::join!(
        git.branch(),
        git.default_branch(),
        git.output(&[
            "--no-optional-locks",
            "status",
            "--short",
            "--ignore-submodules=dirty",
        ]),
        git.output(&["--no-optional-locks", "log", "--oneline", "-n", "5"]),
        git.output(&["config", "user.name"]),
    );
    Ok(GitStatus::Repository(GitSnapshot {
        branch,
        default_branch,
        status,
        recent_commits,
        user_name: (!user_name.is_empty()).then_some(user_name),
    }))
}

/// 未提交改动（`Git::git_diff`，docs/specs/rust-cli/tui-manage.md §4.1）：diff、hunk 与未跟踪文件
/// 三组命令并发执行，原始输出交给前端解析。
pub(crate) async fn diff(
    cwd: &Path,
    projected: BTreeMap<String, String>,
) -> Result<GitDiff, EnvError> {
    let start = cwd.to_path_buf();
    let root = rust_cli_base::blocking(move || git_root(&start))
        .await
        .map_err(|error| EnvError::Io {
            message: error.to_string(),
        })?;
    if root.is_none() {
        return Ok(GitDiff::NotRepository);
    }
    let git = Git {
        cwd: cwd.to_path_buf(),
        env: restricted(projected),
    };
    let (numstat, patch, untracked) = tokio::join!(
        git.output(&["--no-optional-locks", "diff", "HEAD", "--numstat"]),
        git.output(&["--no-optional-locks", "diff", "HEAD"]),
        git.output(&[
            "--no-optional-locks",
            "ls-files",
            "--others",
            "--exclude-standard"
        ]),
    );
    Ok(GitDiff::Repository {
        numstat,
        patch,
        untracked,
    })
}

#[cfg(test)]
#[path = "git_tests.rs"]
pub(crate) mod tests;
