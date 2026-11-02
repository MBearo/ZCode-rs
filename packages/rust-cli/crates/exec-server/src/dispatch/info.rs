//! `info` 端口。

use rust_cli_exec_wire::dto::core::Workspace;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn facts(machine: MachineEnv, workspace: Workspace) -> Result<Reply, WireError> {
    let facts = machine
        .info()
        .facts(&input::workspace(workspace))
        .await
        .map_err(output::error)?;
    Ok(Reply::Facts(output::facts(facts)))
}

pub(crate) async fn path_facts(
    machine: MachineEnv,
    workspace: Workspace,
) -> Result<Reply, WireError> {
    let facts = machine
        .info()
        .path_facts(&input::workspace(workspace))
        .await
        .map_err(output::error)?;
    Ok(Reply::PathFacts(output::path_facts(facts)))
}
