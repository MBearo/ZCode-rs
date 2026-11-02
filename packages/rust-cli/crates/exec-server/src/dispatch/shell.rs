//! `shell` 端口。

use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::core::Workspace;
use rust_cli_exec_wire::envelope::{StreamItem, WireError};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn shell_state(
    machine: MachineEnv,
    workspace: Workspace,
    session: String,
) -> Result<Reply, WireError> {
    let state = machine
        .shell()
        .shell_state(&input::workspace(workspace), &input::session(session))
        .await
        .map_err(output::error)?;
    Ok(Reply::ShellState(output::shell_state(state)))
}

pub(crate) async fn ack_shell_notice(
    machine: MachineEnv,
    session: String,
    seq: u64,
) -> Result<Reply, WireError> {
    machine
        .shell()
        .ack_shell_notice(&input::session(session), seq)
        .await
        .map_err(output::error)?;
    Ok(Reply::AckShellNotice)
}

pub(crate) async fn set_shell_cwd(
    machine: MachineEnv,
    session: String,
    cwd: String,
) -> Result<Reply, WireError> {
    machine
        .shell()
        .set_shell_cwd(&input::session(session), input::path(cwd))
        .await
        .map_err(output::error)?;
    Ok(Reply::SetShellCwd)
}

/// `shell_notices`：后台命令的终态通知（流）。
pub(crate) fn notices(
    machine: &MachineEnv,
    session: String,
    after: u64,
) -> BoxStream<'static, (StreamItem, Body)> {
    machine
        .shell()
        .shell_notices(&input::session(session), after)
        .map(|item| match item {
            Ok(notice) => (
                StreamItem::Notice {
                    notice: output::shell_notice(notice),
                },
                Body::empty(),
            ),
            Err(error) => (
                StreamItem::Error {
                    error: output::error(error),
                },
                Body::empty(),
            ),
        })
        .boxed()
}
