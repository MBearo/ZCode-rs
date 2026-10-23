//! 执行环境端口里 git worktree 操作的 DTO（docs/specs/rust-cli/tools-more.md §11）。
//!
//! EnterWorktree / ExitWorktree 的语义（校验、权限、结果文本、会话目录的切换）在内核；git 命令在
//! 执行环境里跑。路径都是执行环境里的路径（[`EnvPath`]）。

use serde::{Deserialize, Serialize};

use crate::env::EnvPath;

/// 新建或续用一个 worktree（mutate）：`cwd` 所在主仓库根下 `.zcode/worktrees/<flat>`，分支
/// `worktree-<flat>`（`flat` 是 `name` 把 `/` 换成 `+`）。名字已由内核校验。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreate {
    pub cwd: EnvPath,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorktreeCreated {
    Ready {
        path: EnvPath,
        branch: String,
        /// 创建时的基点（续用时是 worktree 现在的 HEAD）：ExitWorktree 据此数新提交。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_commit: Option<String>,
        /// 目录已经是 worktree，原样续用。
        existed: bool,
    },
    /// `cwd` 不在 git 仓库里。
    NotARepository,
    /// git 失败；`message` 是给模型的原因。
    Failed { message: String },
}

/// `cwd` 所在仓库的 `git worktree list`（read）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeList {
    /// 主仓库根（canonical git root）；不在仓库里时为 None，`entries` 为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<EnvPath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<WorktreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub path: EnvPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// 查 worktree 里还没有保存的工作（read）：`base` 是创建时的基点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInspect {
    pub path: EnvPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// worktree 里还没有保存的工作：`git status --porcelain` 的非空行数与 `<基点>..HEAD` 的提交数。
/// 查不到时（git 失败、没有基点）执行环境返回 None。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeChanges {
    pub changed_files: u64,
    pub commits: u64,
}

/// 删除 worktree 与它的分支（mutate）：`git worktree remove --force <path>`、`git branch -D <branch>`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemove {
    pub path: EnvPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}
