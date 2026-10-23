//! git worktree 操作（`Environment::worktree_*`，docs/specs/rust-cli/tools-more.md §11.2、§11.3）。
//!
//! 命令见 tools-more.md §11.2、§11.3（新建或续用、`git worktree list --porcelain`、未保存的工作、删除）；目录与
//! 分支名在 zcode 的 `.zcode/worktrees/`。git 经
//! `rust-cli-proc` 启动，环境是白名单投影加 `GIT_TERMINAL_PROMPT=0`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rust_cli_base::names::{DOT_DIR, WORKTREES_DIR};
use rust_cli_kernel_api::env::{EnvError, EnvPath};
use rust_cli_kernel_api::env_worktree::{
    WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeEntry, WorktreeInspect, WorktreeList,
    WorktreeRemove,
};

/// 读分支的命令不跑仓库的 hook 与 fsmonitor。
const NO_HOOKS: [&str; 4] = ["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor="];

struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

struct Git {
    env: BTreeMap<String, String>,
}

impl Git {
    fn new(projected: BTreeMap<String, String>) -> Self {
        let mut env = projected;
        let (name, value) = crate::git::NO_TERMINAL_PROMPT;
        env.insert(name.to_owned(), value.to_owned());
        env.insert("GIT_ASKPASS".to_owned(), String::new());
        env.insert("LC_ALL".to_owned(), "C".to_owned());
        Self { env }
    }

    async fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut command = rust_cli_proc::command("git");
        command
            .args(NO_HOOKS)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match command.output().await {
            Ok(output) => Output {
                ok: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            },
            Err(error) => Output {
                ok: false,
                stdout: String::new(),
                stderr: error.to_string(),
            },
        }
    }

    async fn stdout(&self, cwd: &Path, args: &[&str]) -> Option<String> {
        let output = self.run(cwd, args).await;
        output.ok.then_some(output.stdout)
    }

    /// 主仓库根：在 worktree 里也解析到主仓库。
    async fn canonical_root(&self, cwd: &Path) -> Option<PathBuf> {
        let common = self
            .stdout(
                cwd,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await?;
        let common = PathBuf::from(common);
        if common.file_name().is_some_and(|name| name == ".git") {
            return common.parent().map(Path::to_path_buf);
        }
        self.stdout(cwd, &["rev-parse", "--show-toplevel"])
            .await
            .map(PathBuf::from)
    }

    /// `origin/HEAD` 指向的分支，否则 main / master 中存在的一个，都没有为 main（同 git 状态的规则）。
    async fn default_branch(&self, root: &Path) -> String {
        if let Some(target) = self
            .stdout(
                root,
                &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            )
            .await
        {
            let name = target.strip_prefix("origin/").unwrap_or(&target);
            if !name.is_empty() && !name.starts_with('-') {
                return name.to_owned();
            }
        }
        for name in ["main", "master"] {
            if self.remote_ref(root, name).await.is_some() {
                return name.to_owned();
            }
        }
        "main".to_owned()
    }

    async fn remote_ref(&self, root: &Path, name: &str) -> Option<String> {
        let reference = format!("refs/remotes/origin/{name}");
        self.stdout(root, &["rev-parse", "--verify", "--quiet", &reference])
            .await
            .filter(|sha| !sha.is_empty())
    }
}

fn env_path(path: &Path) -> EnvPath {
    EnvPath::new(path.to_string_lossy())
}

/// 名字的展平：嵌套的名字换成 `+`，目录与分支都不嵌套。
pub(crate) fn flatten(name: &str) -> String {
    name.replace('/', "+")
}

pub(crate) async fn create(
    request: WorktreeCreate,
    projected: BTreeMap<String, String>,
) -> Result<WorktreeCreated, EnvError> {
    let git = Git::new(projected);
    let cwd = PathBuf::from(request.cwd.as_str());
    let Some(root) = git.canonical_root(&cwd).await else {
        return Ok(WorktreeCreated::NotARepository);
    };
    let flat = flatten(&request.name);
    let path = root.join(DOT_DIR).join(WORKTREES_DIR).join(&flat);
    let branch = format!("worktree-{flat}");
    // 已经是 worktree（它自己的顶层目录就是这个路径）：原样续用（fast resume）。目录只是主仓库里
    // 的普通目录时，rev-parse 会解析到主仓库，所以要比对顶层目录。
    let top = git.stdout(&path, &["rev-parse", "--show-toplevel"]).await;
    if top.is_some_and(|top| Path::new(&top) == path)
        && let Some(head) = git.stdout(&path, &["rev-parse", "HEAD"]).await
    {
        return Ok(WorktreeCreated::Ready {
            path: env_path(&path),
            branch,
            head_commit: Some(head),
            existed: true,
        });
    }
    let default = git.default_branch(&root).await;
    let remote = format!("origin/{default}");
    let (base, head) = match git.remote_ref(&root, &default).await {
        Some(sha) => (remote, Some(sha)),
        None => {
            let fetched = git.run(&root, &["fetch", "origin", &default]).await.ok;
            (if fetched { remote } else { "HEAD".to_owned() }, None)
        }
    };
    let head = match head {
        Some(head) => head,
        None => match git.stdout(&root, &["rev-parse", &base]).await {
            Some(head) => head,
            None => {
                return Ok(WorktreeCreated::Failed {
                    message: format!(
                        "Failed to resolve base branch \"{base}\": git rev-parse failed"
                    ),
                });
            }
        },
    };
    let path_text = path.to_string_lossy().into_owned();
    let added = git
        .run(
            &root,
            &[
                "worktree",
                "add",
                "--no-track",
                "-B",
                &branch,
                &path_text,
                &base,
            ],
        )
        .await;
    if !added.ok {
        return Ok(WorktreeCreated::Failed {
            message: format!("Failed to create worktree: {}", added.stderr),
        });
    }
    Ok(WorktreeCreated::Ready {
        path: env_path(&path),
        branch,
        head_commit: Some(head),
        existed: false,
    })
}

pub(crate) async fn list(
    cwd: &EnvPath,
    projected: BTreeMap<String, String>,
) -> Result<WorktreeList, EnvError> {
    let git = Git::new(projected);
    let cwd = PathBuf::from(cwd.as_str());
    let Some(root) = git.canonical_root(&cwd).await else {
        return Ok(WorktreeList {
            root: None,
            entries: Vec::new(),
        });
    };
    let listed = git.run(&root, &["worktree", "list", "--porcelain"]).await;
    if !listed.ok {
        return Err(EnvError::Io {
            message: format!(
                "`git -C {} worktree list` failed: {}",
                root.display(),
                listed.stderr
            ),
        });
    }
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for line in listed.stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            entries.push(WorktreeEntry {
                path: EnvPath::new(path),
                branch: None,
            });
        } else if let (Some(branch), Some(entry)) =
            (line.strip_prefix("branch "), entries.last_mut())
        {
            entry.branch = Some(branch.trim_start_matches("refs/heads/").to_owned());
        }
    }
    Ok(WorktreeList {
        root: Some(env_path(&root)),
        entries,
    })
}

/// 未保存的工作：`git status --porcelain` 的非空行数与 `<基点>..HEAD` 的提交数；任一步失败为 None。
pub(crate) async fn changes(
    request: WorktreeInspect,
    projected: BTreeMap<String, String>,
) -> Result<Option<WorktreeChanges>, EnvError> {
    let git = Git::new(projected);
    let path = PathBuf::from(request.path.as_str());
    let Some(status) = git.stdout(&path, &["status", "--porcelain"]).await else {
        return Ok(None);
    };
    let changed_files = status
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64;
    let Some(base) = request.base else {
        return Ok(None);
    };
    let range = format!("{base}..HEAD");
    let Some(count) = git.stdout(&path, &["rev-list", "--count", &range]).await else {
        return Ok(None);
    };
    Ok(Some(WorktreeChanges {
        changed_files,
        commits: count.trim().parse().unwrap_or(0),
    }))
}

/// `git worktree remove --force <path>`（在主仓库根里跑），成功后 `git branch -D <branch>`。
pub(crate) async fn remove(
    request: WorktreeRemove,
    projected: BTreeMap<String, String>,
) -> Result<bool, EnvError> {
    let git = Git::new(projected);
    let path = PathBuf::from(request.path.as_str());
    let Some(root) = git.canonical_root(&path).await else {
        return Ok(false);
    };
    let path_text = path.to_string_lossy().into_owned();
    if !git
        .run(&root, &["worktree", "remove", "--force", &path_text])
        .await
        .ok
    {
        return Ok(false);
    }
    if let Some(branch) = &request.branch {
        // 分支删不掉不影响结果（只记日志）。
        let _ = git.run(&root, &["branch", "-D", branch]).await;
    }
    Ok(true)
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
