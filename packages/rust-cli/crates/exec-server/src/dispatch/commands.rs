//! `commands` 端口（起命令不在这里：在 started.rs，执行器要拿着端口给的流，工作线 F119）。

use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_exec_wire::envelope::{StreamItem, WireError};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::{Body, BodyBuilder};
use rust_cli_kernel_api::machine::{CommandEvents, MachineEnv};

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn cancel(machine: MachineEnv, operation: String) -> Result<Reply, WireError> {
    machine
        .commands()
        .cancel(input::operation(operation))
        .await
        .map_err(output::error)?;
    Ok(Reply::Cancel)
}

pub(crate) async fn close_session(
    machine: MachineEnv,
    session: String,
    forget: bool,
) -> Result<Reply, WireError> {
    machine
        .commands()
        .close_session(&input::session(session), forget)
        .await
        .map_err(output::error)?;
    Ok(Reply::CloseSession)
}

/// `watch_command`：经端口接回还在跑的命令。
pub(crate) fn watch(
    machine: &MachineEnv,
    operation: String,
    cursor: u64,
) -> BoxStream<'static, (StreamItem, Body)> {
    items(
        machine
            .commands()
            .watch_command(input::operation(operation), cursor),
    )
}

/// 命令的事件换成线上的项（`watch_command`，或 `start_command` 留下的流，工作线 F119）。
pub(crate) fn items(events: CommandEvents) -> BoxStream<'static, (StreamItem, Body)> {
    events
        .map(|item| match item {
            Ok(event) => {
                let mut body = BodyBuilder::new();
                let event = output::command_event(event, &mut body);
                (StreamItem::Command { event }, body.finish())
            }
            Err(error) => (
                StreamItem::Error {
                    error: output::error(error),
                },
                Body::empty(),
            ),
        })
        .boxed()
}
