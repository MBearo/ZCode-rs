//! `search` 端口。

use rust_cli_exec_wire::dto::core::Workspace;
use rust_cli_exec_wire::dto::files::{GlobFiles, GrepSearch};
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_kernel_api::env_files::ListFiles;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn glob(
    machine: MachineEnv,
    workspace: Workspace,
    request: GlobFiles,
) -> Result<Reply, WireError> {
    let matches = machine
        .search()
        .glob(&input::workspace(workspace), input::glob(request))
        .await
        .map_err(output::error)?;
    Ok(Reply::Glob(output::glob_matches(matches)))
}

pub(crate) async fn list_files(
    machine: MachineEnv,
    workspace: Workspace,
    cwd: String,
) -> Result<Reply, WireError> {
    let list = ListFiles {
        cwd: input::path(cwd),
    };
    let files = machine
        .search()
        .list_files(&input::workspace(workspace), list)
        .await
        .map_err(output::error)?;
    Ok(Reply::ListFiles { files: files.files })
}

pub(crate) async fn grep(
    machine: MachineEnv,
    workspace: Workspace,
    request: GrepSearch,
) -> Result<Reply, WireError> {
    let outcome = machine
        .search()
        .grep(&input::workspace(workspace), input::grep(request))
        .await
        .map_err(output::error)?;
    Ok(Reply::Grep(output::grep_outcome(outcome)))
}
