//! `/cd <path>`（docs/specs/rust-cli/tui-manage.md §7.1）：解析与检查路径，有会话时经会话 actor
//! 判定、换工作目录并提交说明（`Command::ChangeDirectory`，工作目录的唯一 owner 是 actor）。

use rust_cli_kernel_api::env::{EnvError, EnvPath, Environment};
use rust_cli_kernel_api::env_files::{FileKind, PathStyle, StatFile};
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::manage::{ChangeDirectory, DirectoryChanged, DirectoryOutcome};
use rust_cli_kernel_api::session_api::{Ack, Command, CommandEnvelope, KernelError};

use crate::LocalKernel;

fn failed(message: String) -> KernelError {
    KernelError::Failed { message }
}

/// 显示路径：home 写成 `~`。
pub(crate) fn display(path: &str, home: Option<&EnvPath>) -> String {
    match home.map(EnvPath::as_str) {
        Some(home) if path == home => "~".to_owned(),
        Some(home) => match path.strip_prefix(home) {
            Some(rest) if rest.starts_with(['/', '\\']) => format!("~{rest}"),
            _ => path.to_owned(),
        },
        None => path.to_owned(),
    }
}

/// 解析并检查目标目录：返回绝对路径；不存在、不是目录时给出固定的文字。
async fn target(
    env: &dyn Environment,
    request: &ChangeDirectory,
    home: Option<&EnvPath>,
) -> Result<String, KernelError> {
    let workspace = &request.workspace;
    // 起点是会话 shell 的当前目录（Bash 的 cd 会改它）；没有会话时是工作区根。
    let base = match &request.session {
        Some(session) => env
            .shell_state(workspace, session)
            .await
            .map_or_else(|_| workspace.root.clone(), |state| state.cwd),
        None => workspace.root.clone(),
    };
    let style = PathStyle::infer(base.as_str());
    let target = style.resolve(
        base.as_str(),
        request.path.trim(),
        home.map(EnvPath::as_str),
    );
    let stat = StatFile {
        path: EnvPath::new(target.as_str()),
    };
    match env.stat(workspace, stat).await {
        Ok(Some(found)) if found.kind == FileKind::Directory => Ok(target),
        Ok(Some(_)) => Err(failed(format!(
            "{target} is not a directory. Did you mean {}?",
            style.dirname(&target)
        ))),
        Ok(None) | Err(EnvError::NotFound { .. }) => {
            Err(failed(format!("Couldn't find a directory at {target}.")))
        }
        Err(error) => Err(failed(error.to_string())),
    }
}

/// `/cd`（tui-manage.md §7.1）：这里解析与检查路径，有会话时交给会话 actor 判定与移动。
///
/// 修复（R1-3）：以前这里按前端给的工作区（可能过时的副本）判“已经在这里”，复制整份 `view().messages` 算新目录
/// 的规则文件，按回执文字的前缀分辨“运行中”，命令 ID 直接用 `Uuid::new_v4`。现在判定、说明与规则文件都在
/// actor（唯一 owner），回执是类型化的 [`DirectoryOutcome`]，拒绝的文字原样转给用户，命令 ID 经注入的 Ids。
pub(crate) async fn change(
    kernel: &LocalKernel,
    request: ChangeDirectory,
) -> Result<DirectoryChanged, KernelError> {
    let env = kernel.ports.env.as_ref();
    let home = env
        .path_facts(&request.workspace)
        .await
        .ok()
        .and_then(|facts| facts.home);
    let target = target(env, &request, home.as_ref()).await?;
    let shown = display(&target, home.as_ref());
    let root = EnvPath::new(target.as_str());
    let Some(session) = &request.session else {
        // 还没有会话：前端的草稿工作区就是唯一来源，只检查。
        let current = request.workspace.root.as_str();
        if PathStyle::infer(current).same(&target, current) {
            return Err(failed(format!("Already in {shown}.")));
        }
        return Ok(DirectoryChanged {
            root,
            display: shown,
        });
    };
    use rust_cli_kernel_api::session_api::Kernel;
    let handle = kernel.open_session(session).await?;
    // 命令 ID 只要在会话里唯一：取注入的 Ids 给的操作 ID。
    let envelope = CommandEnvelope {
        id: CommandId::new(format!("cmd_cd_{}", kernel.ports.ids.operation())),
        command: Command::ChangeDirectory { root: root.clone() },
    };
    match handle.send(envelope).await {
        Ack::Accepted { result } => match serde_json::from_value::<DirectoryOutcome>(result) {
            Ok(DirectoryOutcome::Moved) => Ok(DirectoryChanged {
                root,
                display: shown,
            }),
            Ok(DirectoryOutcome::Unchanged) => Err(failed(format!("Already in {shown}."))),
            Ok(DirectoryOutcome::Failed { current }) => Err(failed(format!(
                "Couldn't move to {shown} — the directory may no longer exist, or the session couldn't be moved. Staying in {}.",
                display(current.as_str(), home.as_ref())
            ))),
            Err(error) => Err(failed(format!("unreadable /cd receipt: {error}"))),
        },
        // 运行中或会话已关闭：actor 给的就是给用户看的文字。
        Ack::Rejected { reason } => Err(failed(reason)),
        Ack::Duplicate => Err(failed("the /cd command was already handled".to_owned())),
    }
}

/// `/plan open` 的计划文件（tui-manage.md §8）：`PlanFiles::in_workspace` 的位置；存在且不为空才算有计划。
pub(crate) async fn plan_file(
    env: &dyn Environment,
    query: &rust_cli_kernel_api::manage::PlanFileQuery,
) -> Result<rust_cli_kernel_api::manage::PlanFile, KernelError> {
    let facts = env
        .path_facts(&query.workspace)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let plans = rust_cli_kernel_api::plan::PlanFiles::in_workspace(
        &facts,
        &query.workspace.root,
        &query.session,
    )
    .ok_or_else(|| failed("the plan file has no location in this workspace".to_owned()))?;
    let path = plans.path(None);
    let exists = matches!(
        env.stat(&query.workspace, StatFile { path: path.clone() }).await,
        Ok(Some(stat)) if stat.kind == FileKind::File && stat.size > 0
    );
    Ok(rust_cli_kernel_api::manage::PlanFile {
        path: path.as_str().to_owned(),
        exists,
    })
}

#[cfg(test)]
#[path = "directory_tests.rs"]
mod tests;
