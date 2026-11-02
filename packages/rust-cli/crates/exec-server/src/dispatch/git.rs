//! `git` 端口（docs/specs/rust-cli/deployment.md §19.1）：交给注入的 `MachineEnv` 的 Git 端口（就是 env-local 的同一份
//! 代码）。

use rust_cli_exec_wire::dto::core::{OperationContext, Workspace};
use rust_cli_exec_wire::dto::git as wg;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_kernel_api::env_git::{GitDiffRequest, GitStatusRequest};
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn status(
    machine: MachineEnv,
    workspace: Workspace,
    cwd: String,
) -> Result<Reply, WireError> {
    let request = GitStatusRequest {
        cwd: input::path(cwd),
    };
    let status = machine
        .git()
        .git_status(&input::workspace(workspace), request)
        .await
        .map_err(output::error)?;
    Ok(Reply::GitStatus(output::git_status(status)))
}

pub(crate) async fn diff(
    machine: MachineEnv,
    workspace: Workspace,
    cwd: String,
) -> Result<Reply, WireError> {
    let request = GitDiffRequest {
        cwd: input::path(cwd),
    };
    let diff = machine
        .git()
        .git_diff(&input::workspace(workspace), request)
        .await
        .map_err(output::error)?;
    Ok(Reply::GitDiff(output::git_diff(diff)))
}

pub(crate) async fn worktree_create(
    machine: MachineEnv,
    context: OperationContext,
    request: wg::WorktreeCreate,
) -> Result<Reply, WireError> {
    let request = input::worktree_create(request);
    let created = machine
        .git()
        .worktree_create(input::context(context), request)
        .await
        .map_err(output::error)?;
    Ok(Reply::WorktreeCreate(output::worktree_created(created)))
}

pub(crate) async fn worktree_list(machine: MachineEnv, cwd: String) -> Result<Reply, WireError> {
    let list = machine
        .git()
        .worktree_list(input::path(cwd))
        .await
        .map_err(output::error)?;
    Ok(Reply::WorktreeList(output::worktree_list(list)))
}

pub(crate) async fn worktree_changes(
    machine: MachineEnv,
    request: wg::WorktreeInspect,
) -> Result<Reply, WireError> {
    let request = input::worktree_inspect(request);
    let changes = machine
        .git()
        .worktree_changes(request)
        .await
        .map_err(output::error)?;
    Ok(Reply::WorktreeChanges(
        changes.map(output::worktree_changes),
    ))
}

pub(crate) async fn worktree_remove(
    machine: MachineEnv,
    context: OperationContext,
    request: wg::WorktreeRemove,
) -> Result<Reply, WireError> {
    let request = input::worktree_remove(request);
    let removed = machine
        .git()
        .worktree_remove(input::context(context), request)
        .await
        .map_err(output::error)?;
    Ok(Reply::WorktreeRemove { removed })
}
