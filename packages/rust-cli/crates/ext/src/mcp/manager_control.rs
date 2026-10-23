//! `/mcp reconnect|enable|disable`（docs/specs/rust-cli/tui-manage.md §6.1）：连接任务的唯一 owner 是
//! [`McpManager`]。重连与停用时代号加一、发取消消息并关闭旧客户端，旧任务之后的更新因代号不符被丢弃；
//! 等待用状态变化的 watch，不用超时。

use std::sync::PoisonError;

use rust_cli_kernel_api::manage::{McpAction, McpServerInfo};
use tokio_util::sync::CancellationToken;

use super::{McpManager, McpServer, McpStatus, Server};

impl McpManager {
    /// 启动时停用的服务器（配置里 `enabled: false`）：登记为 disabled，不连接；`/mcp enable` 时才连。
    pub fn register_disabled(&self, servers: Vec<McpServer>) {
        {
            let mut known = self.0.servers();
            for config in servers {
                known
                    .entry(config.name.clone())
                    .or_insert_with(|| Server::new(config, McpStatus::Disabled));
            }
        }
        self.0.changed.send_modify(|version| *version += 1);
    }

    /// 停掉当前连接任务并换成 `status`；返回旧客户端（由调用方关闭）与新的代号和取消消息。
    fn retire(
        &self,
        name: &str,
        status: McpStatus,
    ) -> Option<(
        McpServer,
        Option<std::sync::Arc<super::Client>>,
        u64,
        CancellationToken,
    )> {
        let mut servers = self.0.servers();
        let server = servers.get_mut(name)?;
        server.cancel.cancel();
        server.generation += 1;
        server.cancel = CancellationToken::new();
        server.status = status;
        server.tools.clear();
        server.prompts.clear();
        server.instructions = None;
        server.resources = false;
        server.expired = false;
        server.reconnecting = false;
        let client = server.client.take();
        Some((
            server.config.clone(),
            client,
            server.generation,
            server.cancel.clone(),
        ))
    }

    /// 重新连一个服务器（重连与启用共用）。
    async fn restart(&self, name: &str) {
        let Some((config, client, generation, cancel)) = self.retire(name, McpStatus::Pending)
        else {
            return;
        };
        self.0.changed.send_modify(|version| *version += 1);
        if let Some(client) = client {
            client.close().await;
        }
        let workspace = self
            .0
            .workspace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(workspace) = workspace else {
            return;
        };
        let task = self.0.clone().run(workspace, config, generation, cancel);
        self.0.spawner.spawn_resident("mcp-connection", task);
    }

    async fn stop(&self, name: &str) {
        let Some((_, client, _, _)) = self.retire(name, McpStatus::Disabled) else {
            return;
        };
        self.0.changed.send_modify(|version| *version += 1);
        if let Some(client) = client {
            client.close().await;
        }
    }

    fn status_of(&self, name: &str) -> Option<McpStatus> {
        self.0
            .servers()
            .get(name)
            .map(|server| server.status.clone())
    }

    /// 对这些服务器做一个动作，等重连与启用的服务器离开 connecting，返回它们改动后的状态。
    /// reconnect 跳过停用的；enable 只动停用的；disable 跳过已经停用的。不认识的名字不在结果里。
    pub async fn control(&self, action: McpAction, names: &[String]) -> Vec<McpServerInfo> {
        for name in names {
            let Some(status) = self.status_of(name) else {
                continue;
            };
            match (action, status) {
                (McpAction::Reconnect, McpStatus::Disabled)
                | (McpAction::Enable, McpStatus::Pending | McpStatus::Connected)
                | (McpAction::Enable, McpStatus::Failed { .. })
                | (McpAction::Disable, McpStatus::Disabled) => {}
                (McpAction::Reconnect | McpAction::Enable, _) => self.restart(name).await,
                (McpAction::Disable, _) => self.stop(name).await,
            }
        }
        let mut changes = self.0.changed.subscribe();
        loop {
            let pending = names
                .iter()
                .any(|name| self.status_of(name) == Some(McpStatus::Pending));
            if !pending || changes.changed().await.is_err() {
                break;
            }
        }
        self.server_infos()
            .into_iter()
            .filter(|info| names.contains(&info.name))
            .collect()
    }
}

#[cfg(test)]
#[path = "manager_control_tests.rs"]
mod tests;
