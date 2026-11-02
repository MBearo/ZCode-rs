//! Git 端口的远端实现（docs/specs/rust-cli/deployment.md §19.1）：每个方法一条 `git.*` 请求，执行器那一头是同一份
//! env-local（换算在 `to_wire`、`from_wire`）。执行器没报告 `git` 能力位时，exec-client
//! 不发请求、直接回 `Unsupported`。

use async_trait::async_trait;
use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::dto::git as wg;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::env::{EnvError, EnvPath, OperationContext, WorkspaceRef};
use rust_cli_kernel_api::env_git::{GitDiff, GitDiffRequest, GitStatus, GitStatusRequest};
use rust_cli_kernel_api::env_worktree::{
    WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeInspect, WorktreeList, WorktreeRemove,
};
use rust_cli_kernel_api::machine::Git;

use crate::{RemoteEnvironment, from_wire as from, mismatch, to_wire as to};

#[async_trait]
impl Git for RemoteEnvironment {
    async fn git_status(
        &self,
        ws: &WorkspaceRef,
        request: GitStatusRequest,
    ) -> Result<GitStatus, EnvError> {
        let GitStatusRequest { cwd } = request;
        let request = Request::GitStatus {
            workspace: to::workspace(ws),
            cwd: cwd.as_str().to_owned(),
        };
        match self.call(request, BodyBuilder::new()).await?.0 {
            Reply::GitStatus(status) => Ok(from::git_status(status)),
            other => Err(mismatch("git.status", other.op().as_str())),
        }
    }

    async fn git_diff(
        &self,
        ws: &WorkspaceRef,
        request: GitDiffRequest,
    ) -> Result<GitDiff, EnvError> {
        let GitDiffRequest { cwd } = request;
        let request = Request::GitDiff {
            workspace: to::workspace(ws),
            cwd: cwd.as_str().to_owned(),
        };
        match self.call(request, BodyBuilder::new()).await?.0 {
            Reply::GitDiff(diff) => Ok(from::git_diff(diff)),
            other => Err(mismatch("git.diff", other.op().as_str())),
        }
    }

    async fn worktree_create(
        &self,
        context: OperationContext,
        request: WorktreeCreate,
    ) -> Result<WorktreeCreated, EnvError> {
        let WorktreeCreate { cwd, name } = request;
        let request = Request::WorktreeCreate {
            context: to::context(context),
            request: wg::WorktreeCreate {
                cwd: cwd.as_str().to_owned(),
                name,
            },
        };
        match self.call(request, BodyBuilder::new()).await?.0 {
            Reply::WorktreeCreate(created) => Ok(from::worktree_created(created)),
            other => Err(mismatch("git.worktree_create", other.op().as_str())),
        }
    }

    async fn worktree_list(&self, cwd: EnvPath) -> Result<WorktreeList, EnvError> {
        let request = Request::WorktreeList {
            cwd: cwd.as_str().to_owned(),
        };
        match self.call(request, BodyBuilder::new()).await?.0 {
            Reply::WorktreeList(list) => Ok(from::worktree_list(list)),
            other => Err(mismatch("git.worktree_list", other.op().as_str())),
        }
    }

    async fn worktree_changes(
        &self,
        request: WorktreeInspect,
    ) -> Result<Option<WorktreeChanges>, EnvError> {
        let WorktreeInspect { path, base } = request;
        let request = Request::WorktreeChanges {
            request: wg::WorktreeInspect {
                path: path.as_str().to_owned(),
                base,
            },
        };
        match self.call(request, BodyBuilder::new()).await?.0 {
            Reply::WorktreeChanges(changes) => Ok(changes.map(from::worktree_changes)),
            other => Err(mismatch("git.worktree_changes", other.op().as_str())),
        }
    }

    async fn worktree_remove(
        &self,
        context: OperationContext,
        request: WorktreeRemove,
    ) -> Result<bool, EnvError> {
        let WorktreeRemove { path, branch } = request;
        let request = Request::WorktreeRemove {
            context: to::context(context),
            request: wg::WorktreeRemove {
                path: path.as_str().to_owned(),
                branch,
            },
        };
        match self.call(request, BodyBuilder::new()).await?.0 {
            Reply::WorktreeRemove { removed } => Ok(removed),
            other => Err(mismatch("git.worktree_remove", other.op().as_str())),
        }
    }
}
