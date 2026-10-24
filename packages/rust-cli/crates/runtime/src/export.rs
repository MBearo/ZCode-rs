//! `/export <文件名>` 的写入（docs/specs/rust-cli/tui-manage.md §3）：前端渲染好会话文字，内核解析路径
//! （没有扩展名时加 `.txt`，相对路径相对工作区根，`~` 展开到 home），经执行环境写文件。

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WritePrecondition};
use rust_cli_kernel_api::env_files::{PathStyle, PutFile};
use rust_cli_kernel_api::env_path;
use rust_cli_kernel_api::ids::{Generation, OwnerEpoch};
use rust_cli_kernel_api::machine::{EnvInfo, Files};
use rust_cli_kernel_api::manage::ExportFile;
use rust_cli_kernel_api::session_api::KernelError;

/// 写入的绝对路径。
pub(crate) fn target(input: &str, root: &EnvPath, home: Option<&EnvPath>) -> String {
    let input = input.trim();
    let named = if env_path::extension(input).is_empty() {
        format!("{input}.txt")
    } else {
        input.to_owned()
    };
    PathStyle::infer(root.as_str()).resolve(root.as_str(), &named, home.map(EnvPath::as_str))
}

pub(crate) async fn write(
    info: &dyn EnvInfo,
    files: &dyn Files,
    ids: &dyn Ids,
    generation: &Generation,
    file: ExportFile,
) -> Result<String, KernelError> {
    let workspace = &file.workspace;
    let style = PathStyle::infer(workspace.root.as_str());
    let home = if style.needs_home(file.path.trim()) {
        info.path_facts(workspace)
            .await
            .ok()
            .and_then(|facts| facts.home)
    } else {
        None
    };
    let path = target(&file.path, &workspace.root, home.as_ref());
    let context = OperationContext {
        environment: workspace.environment.clone(),
        generation: generation.clone(),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: ids.operation(),
        origin: Origin::User { session: None },
    };
    let request = PutFile {
        path: EnvPath::new(path.as_str()),
        bytes: file.text.into_bytes(),
        precondition: WritePrecondition::Any,
        create_parents: true,
    };
    files
        .put_file(workspace, context, request)
        .await
        .map_err(|error| KernelError::Failed {
            message: error.to_string(),
        })?;
    Ok(path)
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
