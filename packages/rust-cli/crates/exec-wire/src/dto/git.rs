//! git 与 worktree（docs/specs/rust-cli/deployment.md §19.1）：kernel-api `env_git`、`env_worktree` 的线上形状。
//! 路径是字符串；字段名与 kernel-api 相同，换算只在 env-remote 与 exec-server。

use serde::{Deserialize, Serialize};

/// `cwd` 所在仓库的 git 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitStatus {
    NotRepository,
    Repository(GitSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSnapshot {
    pub branch: String,
    pub default_branch: String,
    pub status: String,
    pub recent_commits: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
}

/// 未提交改动（`/diff`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitDiff {
    NotRepository,
    Repository {
        numstat: String,
        patch: String,
        untracked: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreate {
    pub cwd: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorktreeCreated {
    Ready {
        path: String,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_commit: Option<String>,
        existed: bool,
    },
    NotARepository,
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<WorktreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInspect {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeChanges {
    pub changed_files: u64,
    pub commits: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemove {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}
