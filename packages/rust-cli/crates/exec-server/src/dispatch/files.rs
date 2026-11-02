//! `files` 端口与 `dirs.ensure_dir`（F110：kernel-api 里是 `Files::ensure_dir`，线上单独一个能力位）。

use rust_cli_exec_wire::dto::core::{OperationContext, Workspace};
use rust_cli_exec_wire::dto::files::{PutFile, ReadText, WritePrecondition};
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::{Blob, Body, BodyBuilder};
use rust_cli_kernel_api::env::{ReadFile, WriteFile};
use rust_cli_kernel_api::env_files::{EnsureDir, ListDir, StatFile, StatMany};
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn read_file(
    machine: MachineEnv,
    workspace: Workspace,
    path: String,
) -> Result<(Reply, Body), WireError> {
    let read = ReadFile {
        path: input::path(path),
    };
    let content = machine
        .files()
        .read_file(&input::workspace(workspace), read)
        .await
        .map_err(output::error)?;
    let mut out = BodyBuilder::new();
    let reply = Reply::ReadFile(output::file_content(content, &mut out));
    Ok((reply, out.finish()))
}

pub(crate) async fn write_file(
    machine: MachineEnv,
    (workspace, context): (Workspace, OperationContext),
    (path, bytes, precondition): (String, Blob, WritePrecondition),
    body: Body,
) -> Result<Reply, WireError> {
    let write = WriteFile {
        path: input::path(path),
        bytes: body.to_vec(&bytes)?,
        precondition: input::precondition(precondition),
    };
    let receipt = machine
        .files()
        .write_file(&input::workspace(workspace), input::context(context), write)
        .await
        .map_err(output::error)?;
    Ok(Reply::WriteFile {
        version: receipt.version.as_str().to_owned(),
    })
}

pub(crate) async fn stat(
    machine: MachineEnv,
    workspace: Workspace,
    path: String,
) -> Result<Reply, WireError> {
    let stat = StatFile {
        path: input::path(path),
    };
    let found = machine
        .files()
        .stat(&input::workspace(workspace), stat)
        .await
        .map_err(output::error)?;
    Ok(Reply::Stat(found.map(output::file_stat)))
}

pub(crate) async fn stat_many(
    machine: MachineEnv,
    workspace: Workspace,
    paths: Vec<String>,
) -> Result<Reply, WireError> {
    let request = StatMany {
        paths: paths.into_iter().map(input::path).collect(),
    };
    let found = machine
        .files()
        .stat_many(&input::workspace(workspace), request)
        .await
        .map_err(output::error)?;
    Ok(Reply::StatMany(
        found.into_iter().map(output::stat_entry).collect(),
    ))
}

pub(crate) async fn list_dir(
    machine: MachineEnv,
    workspace: Workspace,
    path: String,
) -> Result<Reply, WireError> {
    let list = ListDir {
        path: input::path(path),
    };
    let entries = machine
        .files()
        .list_dir(&input::workspace(workspace), list)
        .await
        .map_err(output::error)?;
    Ok(Reply::ListDir(
        entries.into_iter().map(output::dir_entry).collect(),
    ))
}

pub(crate) async fn read_text(
    machine: MachineEnv,
    workspace: Workspace,
    request: ReadText,
) -> Result<(Reply, Body), WireError> {
    let read = machine
        .files()
        .read_text(&input::workspace(workspace), input::read_text(request))
        .await
        .map_err(output::error)?;
    let mut out = BodyBuilder::new();
    let reply = Reply::ReadText(output::text_read(read, &mut out));
    Ok((reply, out.finish()))
}

pub(crate) async fn put_file(
    machine: MachineEnv,
    (workspace, context): (Workspace, OperationContext),
    request: PutFile,
    body: Body,
) -> Result<Reply, WireError> {
    let put = input::put_file(request, &body)?;
    let written = machine
        .files()
        .put_file(&input::workspace(workspace), input::context(context), put)
        .await
        .map_err(output::error)?;
    Ok(Reply::PutFile(output::file_written(written)))
}

pub(crate) async fn ensure_dir(
    machine: MachineEnv,
    workspace: Workspace,
    path: String,
) -> Result<Reply, WireError> {
    let request = EnsureDir {
        path: input::path(path),
    };
    machine
        .files()
        .ensure_dir(&input::workspace(workspace), request)
        .await
        .map_err(output::error)?;
    Ok(Reply::EnsureDir)
}
