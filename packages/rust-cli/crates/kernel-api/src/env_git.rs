//! 执行环境端口里 git 状态操作的 DTO（docs/specs/rust-cli/context.md §12），与只经 `stat` / 读 `.git` 的 git 根
//! 判定（修复线 F58，README §4.1）。
//!
//! 只给原始值：git 命令的输出去掉首尾空白。给模型看的文本（gitStatus 段、截断提示）
//! 在内核的 context 里渲染，执行环境不拼文本。

use serde::{Deserialize, Serialize};

use crate::env::{EnvPath, WorkspaceRef};
use crate::env_files::{FileKind, PathStyle, StatFile};
use crate::machine::Files;

/// 读取 `cwd` 所在仓库的 git 状态（read）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusRequest {
    pub cwd: EnvPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitStatus {
    /// cwd 及其上层目录都没有 `.git`。
    NotRepository,
    Repository(GitSnapshot),
}

/// 一次 git 状态快照。各项是对应命令的 stdout 去掉首尾空白；命令失败时为空串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSnapshot {
    /// 当前分支；分离头为 `HEAD`。
    pub branch: String,
    /// 主分支：`origin/HEAD` 指向且存在的分支，否则 `main` / `master` 中存在的一个，都没有为 `main`。
    pub default_branch: String,
    /// `git status --short`。
    pub status: String,
    /// `git log --oneline -n 5`。
    pub recent_commits: String,
    /// `git config user.name`；没有设置时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
}

/// 读取 `cwd` 所在仓库的未提交改动（`/diff`，docs/specs/rust-cli/tui-manage.md §4，read）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiffRequest {
    pub cwd: EnvPath,
}

/// 未提交改动的原始输出；解析与显示在前端，执行环境不拼文本。命令失败时对应项为空串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitDiff {
    NotRepository,
    Repository {
        /// `git diff HEAD --numstat`。
        numstat: String,
        /// `git diff HEAD`。
        patch: String,
        /// `git ls-files --others --exclude-standard`。
        untracked: String,
    },
}

/// git 根的标记：这一层的 `.git` 是目录（普通仓库）或文件（worktree、submodule）；FIFO、socket、设备不算
/// （`stat.isDirectory() || stat.isFile()`，与 Node `hasWorktreeMarker` 相同）。
///
/// F58：以前 memory、cli、ext、agents 经端口时任何种类都算，runtime 的分支查找把别的种类当文件读；现在经端口的
/// 判定都用这里，env-local 的本机判定也用它（`meta::kind` 之后）。
pub fn marks_git_root(kind: FileKind) -> bool {
    matches!(kind, FileKind::File | FileKind::Directory)
}

/// `dot_git`（某一层的 `.git`）是不是 git 根的标记：经执行环境 `stat`，种类按 [`marks_git_root`]；不存在或
/// 出错都算不是。路径怎么拼由调用方决定。
pub async fn has_git_marker(files: &dyn Files, workspace: &WorkspaceRef, dot_git: &str) -> bool {
    let request = StatFile {
        path: EnvPath::new(dot_git),
    };
    matches!(files.stat(workspace, request).await, Ok(Some(stat)) if marks_git_root(stat.kind))
}

/// `.git` 文件的内容指向的 gitdir：去掉首尾空白、去掉 `gitdir:`、再去掉空白。不以
/// `gitdir:` 开头时为 None。指向 UNC（`\\…`、`//…` 开头）时也是 None：信任之前不按仓库内容去读网络路径，Windows
/// 上会泄露凭据。相对路径怎么解析由调用方决定。
///
/// F58：以前 context、cli、runtime 各写一份，context 不拒绝 UNC。
pub fn gitdir_pointer(text: &str) -> Option<&str> {
    let pointer = text.trim().strip_prefix("gitdir:")?.trim();
    let unc = pointer.starts_with("\\\\") || pointer.starts_with("//");
    (!unc).then_some(pointer)
}

/// 从 `cwd` 往上到 git 根（含）或 home（不含）的每一层，离 `cwd` 近的在前（git 根按
/// [`has_git_marker`]）。home 本身不在内：它的目录由用户级单独加载。
///
/// F58：以前 ext（skill 目录）与 agents（agent 目录）各写一份。
pub async fn project_levels(
    files: &dyn Files,
    workspace: &WorkspaceRef,
    style: PathStyle,
    cwd: &str,
    home: Option<&str>,
) -> Vec<String> {
    let mut levels = Vec::new();
    let mut current = Some(cwd.to_owned());
    while let Some(dir) = current {
        if home.is_some_and(|home| style.same(&dir, home)) {
            break;
        }
        let root = has_git_marker(files, workspace, &style.join(&dir, &[".git"])).await;
        current = if root { None } else { style.parent(&dir) };
        levels.push(dir);
    }
    levels
}

#[cfg(test)]
#[path = "env_git_tests.rs"]
mod tests;
