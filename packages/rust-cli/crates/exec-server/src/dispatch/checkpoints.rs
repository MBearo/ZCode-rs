//! `checkpoints` 端口（代码回退事务）。

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::core::{OperationContext, Workspace};
use rust_cli_exec_wire::dto::rewind::RestoreFile;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn rewind_prepare(
    machine: MachineEnv,
    (workspace, context): (Workspace, OperationContext),
    files: Vec<RestoreFile>,
    body: Body,
) -> Result<Reply, WireError> {
    let prepare = input::rewind_prepare(files, &body)?;
    let plan = machine
        .checkpoints()
        .rewind_prepare(
            &input::workspace(workspace),
            input::context(context),
            prepare,
        )
        .await
        .map_err(output::error)?;
    Ok(Reply::RewindPrepare(output::rewind_plan(plan)))
}

pub(crate) async fn rewind_apply(machine: MachineEnv, token: String) -> Result<Reply, WireError> {
    let status = machine
        .checkpoints()
        .rewind_apply(&input::operation(token))
        .await
        .map_err(output::error)?;
    Ok(Reply::RewindApply(output::rewind_status(status)))
}

pub(crate) async fn rewind_finish(machine: MachineEnv, token: String) -> Result<Reply, WireError> {
    machine
        .checkpoints()
        .rewind_finish(&input::operation(token))
        .await
        .map_err(output::error)?;
    Ok(Reply::RewindFinish)
}
