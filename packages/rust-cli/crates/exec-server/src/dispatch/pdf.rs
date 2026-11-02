//! `pdf` 端口。

use rust_cli_exec_wire::dto::core::Workspace;
use rust_cli_exec_wire::dto::files::RenderPdfPages;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::{Body, BodyBuilder};
use rust_cli_kernel_api::env_pdf::PdfPageCount;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::convert_in as input;
use crate::convert_out as output;

pub(crate) async fn pdf_page_count(
    machine: MachineEnv,
    workspace: Workspace,
    path: String,
) -> Result<Reply, WireError> {
    let count = PdfPageCount {
        path: input::path(path),
    };
    let pages = machine
        .pdf()
        .pdf_page_count(&input::workspace(workspace), count)
        .await
        .map_err(output::error)?;
    Ok(Reply::PdfPageCount(pages))
}

pub(crate) async fn render_pdf_pages(
    machine: MachineEnv,
    workspace: Workspace,
    request: RenderPdfPages,
) -> Result<(Reply, Body), WireError> {
    let render = machine
        .pdf()
        .render_pdf_pages(
            &input::workspace(workspace),
            input::render_pdf_pages(request),
        )
        .await
        .map_err(output::error)?;
    let mut out = BodyBuilder::new();
    let reply = Reply::RenderPdfPages(output::pdf_render(render, &mut out));
    Ok((reply, out.finish()))
}
