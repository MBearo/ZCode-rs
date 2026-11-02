//! `background` 端口。

use rust_cli_exec_wire::dto::core::OperationContext;
use rust_cli_exec_wire::dto::shell::BackgroundTask;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn background_command(
    machine: MachineEnv,
    task: BackgroundTask,
) -> Result<Reply, WireError> {
    let info = machine
        .background()
        .background_command(input::background_task(task))
        .await
        .map_err(output::error)?;
    Ok(Reply::BackgroundCommand(info.map(output::background_info)))
}

pub(crate) async fn background_elapsed(
    machine: MachineEnv,
    task: BackgroundTask,
) -> Result<Reply, WireError> {
    let elapsed = machine
        .background()
        .background_elapsed(input::background_task(task))
        .await
        .map_err(output::error)?;
    Ok(Reply::BackgroundElapsed(elapsed))
}

pub(crate) async fn stop_background_command(
    machine: MachineEnv,
    context: OperationContext,
    task: BackgroundTask,
) -> Result<Reply, WireError> {
    let stop = machine
        .background()
        .stop_background_command(input::context(context), input::background_task(task))
        .await
        .map_err(output::error)?;
    Ok(Reply::StopBackgroundCommand(output::background_stop(stop)))
}
