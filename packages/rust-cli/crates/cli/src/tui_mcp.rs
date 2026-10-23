//! `/mcp enable/disable` 的持久化（docs/specs/rust-cli/tui-manage.md §6.1）：写这个服务器最终生效的定义所在的
//! 配置文件的 `enabled`（Node 的字段）。用户级文件经 platform（paths + fs），项目级文件是工作区文件，经执行
//! 环境读写（条件写：读到的版本没变才写）。

use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::{
    EnvPath, Environment, OperationContext, Origin, ReadFile, WorkspaceRef, WritePrecondition,
};
use rust_cli_kernel_api::env_files::PutFile;
use rust_cli_kernel_api::ids::{Generation, OwnerEpoch};
use rust_cli_paths::StoragePaths;

use crate::config::merge::McpOrigin;

pub(crate) struct McpConfigFiles {
    pub(crate) origins: BTreeMap<String, McpOrigin>,
    pub(crate) env: Arc<dyn Environment>,
    pub(crate) ids: Arc<dyn Ids>,
    pub(crate) workspace: WorkspaceRef,
}

impl McpConfigFiles {
    /// 写回 `enabled`；不在配置文件里的服务器返回 `Ok(None)`。
    pub(crate) async fn save(
        &self,
        paths: &StoragePaths,
        server: &str,
        enabled: bool,
    ) -> Result<Option<String>, String> {
        let Some(origin) = self.origins.get(server) else {
            return Ok(None);
        };
        if origin.user {
            return rust_cli_platform::write_mcp_enabled(paths, server, enabled)
                .await
                .map(|file| Some(file.as_path().display().to_string()))
                .map_err(|error| error.to_string());
        }
        let path = EnvPath::new(origin.path.as_str());
        let read = self
            .env
            .read_file(&self.workspace, ReadFile { path: path.clone() })
            .await
            .map_err(|error| error.to_string())?;
        let text =
            rust_cli_platform::mcp_enabled_text(Some(&read.bytes), &origin.path, server, enabled)
                .map_err(|error| error.to_string())?;
        let context = OperationContext {
            environment: self.workspace.environment.clone(),
            generation: Generation::new("cli-config"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: self.ids.operation(),
            origin: Origin::User { session: None },
        };
        let request = PutFile {
            path,
            bytes: text.into_bytes(),
            precondition: WritePrecondition::Version(read.version),
            create_parents: false,
        };
        self.env
            .put_file(&self.workspace, context, request)
            .await
            .map(|_| Some(origin.path.clone()))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "tui_mcp_tests.rs"]
mod tests;
