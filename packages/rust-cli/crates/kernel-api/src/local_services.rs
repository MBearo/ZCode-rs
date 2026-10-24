//! 只在本机的服务（docs/specs/rust-cli/remote-exec.md §15.8、§15.8.1）：Git 与 worktree、取网页（WebFetch）。
//!
//! 这些工具没有 `machine` 参数（R3 起也没有），所以不在 [`crate::machine::MachineEnv`] 里，远端执行器不实现。
//! 本机的实现是 env-local；规则与 [`crate::machine`] 相同，后加的方法默认返回 `Unsupported`。

use std::sync::Arc;

use async_trait::async_trait;

use crate::env::{EnvError, EnvPath, OperationContext, WorkspaceRef};
use crate::env_git::{GitDiff, GitDiffRequest, GitStatus, GitStatusRequest};
use crate::env_web::{FetchUrl, FetchedUrl};
use crate::env_worktree::{
    WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeInspect, WorktreeList, WorktreeRemove,
};
use crate::machine::not_provided;

/// git 状态、未提交改动与 worktree。
#[async_trait]
pub trait Git: Send + Sync {
    /// `cwd` 所在仓库的 git 状态（read，docs/specs/rust-cli/context.md §12）。
    async fn git_status(
        &self,
        _workspace: &WorkspaceRef,
        _request: GitStatusRequest,
    ) -> Result<GitStatus, EnvError> {
        Err(not_provided("git_status"))
    }

    /// 未提交改动（`/diff`，read）。
    async fn git_diff(
        &self,
        _workspace: &WorkspaceRef,
        _request: GitDiffRequest,
    ) -> Result<GitDiff, EnvError> {
        Err(not_provided("git_diff"))
    }

    /// 新建或续用 worktree（mutate，docs/specs/rust-cli/tools-more.md §11.2）。
    async fn worktree_create(
        &self,
        _context: OperationContext,
        _request: WorktreeCreate,
    ) -> Result<WorktreeCreated, EnvError> {
        Err(not_provided("worktree_create"))
    }

    /// `cwd` 所在仓库的主仓库根与 `git worktree list`（read）。
    async fn worktree_list(&self, _cwd: EnvPath) -> Result<WorktreeList, EnvError> {
        Err(not_provided("worktree_list"))
    }

    /// worktree 里未保存的工作；查不到时 None（read）。
    async fn worktree_changes(
        &self,
        _request: WorktreeInspect,
    ) -> Result<Option<WorktreeChanges>, EnvError> {
        Err(not_provided("worktree_changes"))
    }

    /// 删除 worktree 与分支（mutate）；返回是否删掉了目录。
    async fn worktree_remove(
        &self,
        _context: OperationContext,
        _request: WorktreeRemove,
    ) -> Result<bool, EnvError> {
        Err(not_provided("worktree_remove"))
    }
}

/// 取一个网页。
#[async_trait]
pub trait Fetch: Send + Sync {
    /// 取一个网页（read，WebFetch；docs/specs/rust-cli/tools-more.md §4）：一次 GET，不跟随重定向。
    async fn fetch_url(
        &self,
        _workspace: &WorkspaceRef,
        _request: FetchUrl,
    ) -> Result<FetchedUrl, EnvError> {
        Err(not_provided("fetch_url"))
    }
}

/// 只在本机的服务：Git 与取网页。只经 [`LocalServices::new`] 建，两个端口是同一个对象。
#[derive(Clone)]
pub struct LocalServices {
    git: Arc<dyn Git>,
    fetch: Arc<dyn Fetch>,
}

impl LocalServices {
    /// `services` 实现两个端口（本机是 env-local 的 `LocalEnvironment`）。
    pub fn new<S>(services: Arc<S>) -> Self
    where
        S: Git + Fetch + 'static,
    {
        Self {
            git: services.clone(),
            fetch: services,
        }
    }

    pub fn git(&self) -> &Arc<dyn Git> {
        &self.git
    }

    pub fn fetch(&self) -> &Arc<dyn Fetch> {
        &self.fetch
    }
}
