//! 本地命令（`/usage`、`/context`、`/rename`）的输出（docs/specs/rust-cli/headless.md §3.6）：text 是命令的文字
//! （补换行）；json / stream-json 是 init、一条合成的 assistant 与 `num_turns: 0` 的 result。

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rust_cli_kernel_api::catalog::CatalogQuery;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::Kernel;
use tokio::io::AsyncWrite;

use crate::printer::Printer;
use crate::{PrintOptions, SessionTarget, events};

/// 本地命令的会话 ID：`/rename` 与 `-r <id>` 是那个会话，`--session-id` 给出的新会话用它；其余不建会话行，用一个
/// 不落库的新 ID（本地命令同样有会话 ID）。
pub(crate) fn session_id(options: &PrintOptions) -> String {
    match &options.target {
        SessionTarget::Resume(id) => id.as_str().to_owned(),
        // `--session-id <uuid>`（cli-args.md §7）：新会话用它。
        SessionTarget::New if options.session_id.is_some() => options
            .session_id
            .as_ref()
            .map(|id| id.as_str().to_owned())
            .unwrap_or_default(),
        _ => options
            .output
            .stamps
            .as_ref()
            .map(|stamps| stamps.ids.session().as_str().to_owned())
            .unwrap_or_default(),
    }
}

/// 权限模式的名字。
pub(crate) fn mode_name(mode: rust_cli_kernel_api::permission::PermissionMode) -> String {
    match serde_json::to_value(mode) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "default".to_owned(),
    }
}

/// 写本地命令的输出并结束。`command` 是命令名（result 的 `local_command`）。
pub(crate) async fn finish<W: AsyncWrite + Unpin>(
    kernel: &Arc<dyn Kernel>,
    options: &PrintOptions,
    printer: &mut Printer<'_, W>,
    (command, session_id): (&str, &str),
    text: String,
    started: Instant,
) -> Result<()> {
    if printer.format().streams() || printer.format().collects() {
        let catalog = kernel
            .session_catalog(CatalogQuery {
                session: SessionId::new(session_id),
                workspace: options.workspace.clone(),
                selection: options.selection.clone(),
            })
            .await
            .unwrap_or_default();
        let servers = kernel.mcp_servers().await;
        let mode = mode_name(options.mode);
        let uuid = printer.uuid();
        let init = events::init(
            &events::Init {
                cwd: options.workspace.root.as_str(),
                session_id,
                catalog: &catalog,
                mcp_servers: &servers,
                model: &options.selection.model,
                permission_mode: &mode,
                api_key_source: &options.output.api_key_source,
                version: &options.output.version,
            },
            &uuid,
        );
        printer.emit(init).await?;
        let stamp = printer.stamp();
        printer
            .emit(events::synthetic_assistant(&text, session_id, stamp))
            .await?;
    }
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let uuid = printer.uuid();
    let value = events::local_result(events::LocalResult {
        session_id,
        duration_api_ms: 0,
        model_usage: &printer.model_usage,
        result: &text,
        command,
        duration_ms,
        uuid,
        result_index: printer.result_index(),
    });
    // text 输出：结果不以换行结尾时补一个。
    let output = if text.ends_with('\n') {
        text
    } else {
        format!("{text}\n")
    };
    printer.result(value, output, false).await?;
    printer.finish().await?;
    Ok(())
}
