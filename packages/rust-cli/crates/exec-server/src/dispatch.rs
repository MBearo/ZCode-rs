//! 把一个请求交给注入的 `MachineEnv`（remote-exec.md §4：远端的执行逻辑就是 env-local 的同一份代码）。

use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_exec_wire::envelope::{ReplyResult, StreamItem, WireError};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_exec_wire::{Body, BodyBuilder};
use rust_cli_kernel_api::env::{ReadFile, WriteFile};
use rust_cli_kernel_api::env_files::{ListDir, ListFiles, StatFile};
use rust_cli_kernel_api::env_pdf::PdfPageCount;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

/// 执行一个不是流的请求：回复与它的二进制段。
pub(crate) async fn unary(
    machine: &MachineEnv,
    request: Request,
    body: &Body,
) -> (ReplyResult, Body) {
    let mut out = BodyBuilder::new();
    let result = match run(machine, request, body, &mut out).await {
        Ok(reply) => ReplyResult::Ok(reply),
        Err(error) => ReplyResult::Err(error),
    };
    (result, out.finish())
}

async fn run(
    machine: &MachineEnv,
    request: Request,
    body: &Body,
    out: &mut BodyBuilder,
) -> Result<Reply, WireError> {
    let err = output::error;
    Ok(match request {
        Request::Facts { workspace } => Reply::Facts(output::facts(
            machine
                .info()
                .facts(&input::workspace(workspace))
                .await
                .map_err(err)?,
        )),
        Request::PathFacts { workspace } => Reply::PathFacts(output::path_facts(
            machine
                .info()
                .path_facts(&input::workspace(workspace))
                .await
                .map_err(err)?,
        )),
        Request::ReadFile { workspace, path } => {
            let read = ReadFile {
                path: input::path(path),
            };
            let content = machine
                .files()
                .read_file(&input::workspace(workspace), read)
                .await
                .map_err(err)?;
            Reply::ReadFile(output::file_content(content, out))
        }
        Request::WriteFile {
            workspace,
            context,
            path,
            bytes,
            precondition,
        } => {
            let write = WriteFile {
                path: input::path(path),
                bytes: body.to_vec(&bytes)?,
                precondition: input::precondition(precondition),
            };
            let receipt = machine
                .files()
                .write_file(&input::workspace(workspace), input::context(context), write)
                .await
                .map_err(err)?;
            Reply::WriteFile {
                version: receipt.version.as_str().to_owned(),
            }
        }
        Request::Stat { workspace, path } => {
            let stat = StatFile {
                path: input::path(path),
            };
            let found = machine
                .files()
                .stat(&input::workspace(workspace), stat)
                .await
                .map_err(err)?;
            Reply::Stat(found.map(output::file_stat))
        }
        Request::ListDir { workspace, path } => {
            let list = ListDir {
                path: input::path(path),
            };
            let entries = machine
                .files()
                .list_dir(&input::workspace(workspace), list)
                .await
                .map_err(err)?;
            Reply::ListDir(entries.into_iter().map(output::dir_entry).collect())
        }
        Request::ReadText { workspace, request } => {
            let read = machine
                .files()
                .read_text(&input::workspace(workspace), input::read_text(request))
                .await
                .map_err(err)?;
            Reply::ReadText(output::text_read(read, out))
        }
        Request::PutFile {
            workspace,
            context,
            request,
        } => {
            let put = input::put_file(request, body)?;
            let written = machine
                .files()
                .put_file(&input::workspace(workspace), input::context(context), put)
                .await
                .map_err(err)?;
            Reply::PutFile(output::file_written(written))
        }
        Request::Glob { workspace, request } => Reply::Glob(output::glob_matches(
            machine
                .search()
                .glob(&input::workspace(workspace), input::glob(request))
                .await
                .map_err(err)?,
        )),
        Request::ListFiles { workspace, cwd } => {
            let list = ListFiles {
                cwd: input::path(cwd),
            };
            let files = machine
                .search()
                .list_files(&input::workspace(workspace), list)
                .await
                .map_err(err)?;
            Reply::ListFiles { files: files.files }
        }
        Request::Grep { workspace, request } => Reply::Grep(output::grep_outcome(
            machine
                .search()
                .grep(&input::workspace(workspace), input::grep(request))
                .await
                .map_err(err)?,
        )),
        Request::StartCommand {
            workspace,
            context,
            request,
        } => {
            let start = input::start_command(*request, body)?;
            machine
                .commands()
                .start_command(&input::workspace(workspace), input::context(context), start)
                .await
                .map_err(err)?;
            Reply::StartCommand
        }
        Request::Cancel { operation } => {
            machine
                .commands()
                .cancel(input::operation(operation))
                .await
                .map_err(err)?;
            Reply::Cancel
        }
        Request::OperationStatus { operation } => Reply::OperationStatus(output::outcome(
            machine
                .commands()
                .operation_status(input::operation(operation))
                .await
                .map_err(err)?,
        )),
        Request::CloseSession { session, forget } => {
            machine
                .commands()
                .close_session(&input::session(session), forget)
                .await
                .map_err(err)?;
            Reply::CloseSession
        }
        Request::ShellState { workspace, session } => Reply::ShellState(output::shell_state(
            machine
                .shell()
                .shell_state(&input::workspace(workspace), &input::session(session))
                .await
                .map_err(err)?,
        )),
        Request::AckShellNotice { session, seq } => {
            machine
                .shell()
                .ack_shell_notice(&input::session(session), seq)
                .await
                .map_err(err)?;
            Reply::AckShellNotice
        }
        Request::SetShellCwd { session, cwd } => {
            machine
                .shell()
                .set_shell_cwd(&input::session(session), input::path(cwd))
                .await
                .map_err(err)?;
            Reply::SetShellCwd
        }
        Request::BackgroundCommand { task } => Reply::BackgroundCommand(
            machine
                .background()
                .background_command(input::background_task(task))
                .await
                .map_err(err)?
                .map(output::background_info),
        ),
        Request::BackgroundElapsed { task } => Reply::BackgroundElapsed(
            machine
                .background()
                .background_elapsed(input::background_task(task))
                .await
                .map_err(err)?,
        ),
        Request::StopBackgroundCommand { context, task } => {
            Reply::StopBackgroundCommand(output::background_stop(
                machine
                    .background()
                    .stop_background_command(input::context(context), input::background_task(task))
                    .await
                    .map_err(err)?,
            ))
        }
        Request::RewindPrepare {
            workspace,
            context,
            files,
        } => {
            let prepare = input::rewind_prepare(files, body)?;
            Reply::RewindPrepare(output::rewind_plan(
                machine
                    .checkpoints()
                    .rewind_prepare(
                        &input::workspace(workspace),
                        input::context(context),
                        prepare,
                    )
                    .await
                    .map_err(err)?,
            ))
        }
        Request::RewindApply { token } => Reply::RewindApply(output::rewind_status(
            machine
                .checkpoints()
                .rewind_apply(&input::operation(token))
                .await
                .map_err(err)?,
        )),
        Request::RewindStatus { token } => Reply::RewindStatus(output::rewind_status(
            machine
                .checkpoints()
                .rewind_status(&input::operation(token))
                .await
                .map_err(err)?,
        )),
        Request::RewindFinish { token } => {
            machine
                .checkpoints()
                .rewind_finish(&input::operation(token))
                .await
                .map_err(err)?;
            Reply::RewindFinish
        }
        Request::PdfPageCount { workspace, path } => {
            let count = PdfPageCount {
                path: input::path(path),
            };
            Reply::PdfPageCount(
                machine
                    .pdf()
                    .pdf_page_count(&input::workspace(workspace), count)
                    .await
                    .map_err(err)?,
            )
        }
        Request::RenderPdfPages { workspace, request } => {
            let render = machine
                .pdf()
                .render_pdf_pages(
                    &input::workspace(workspace),
                    input::render_pdf_pages(request),
                )
                .await
                .map_err(err)?;
            Reply::RenderPdfPages(output::pdf_render(render, out))
        }
        Request::WatchCommand { .. } | Request::ShellNotices { .. } => {
            return Err(WireError::BadRequest {
                message: "a stream was sent as a call".to_owned(),
            });
        }
    })
}

/// 打开一个流：每项换成线上的一项与它的二进制段；端口的流结束时结束（`end` 由调用方加）。
pub(crate) fn stream(
    machine: &MachineEnv,
    request: Request,
) -> BoxStream<'static, (StreamItem, Body)> {
    match request {
        Request::WatchCommand { operation, cursor } => machine
            .commands()
            .watch_command(input::operation(operation), cursor)
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
            .boxed(),
        Request::ShellNotices { session, after } => machine
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
            .boxed(),
        other => {
            let error = WireError::BadRequest {
                message: format!("{} is not a stream", other.op().as_str()),
            };
            futures_util::stream::iter([(StreamItem::Error { error }, Body::empty())]).boxed()
        }
    }
}
