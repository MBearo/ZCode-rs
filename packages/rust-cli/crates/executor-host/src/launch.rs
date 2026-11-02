//! 本机执行环境的起进程入口（F96，remote-exec.md §23.5；F128）：exec-server 按请求所属的客户端实例带上推来的环境变量，
//! 这里交给 env-local 的 `start_command_with_env` / `connect_with_env`。端口本身（`Commands`、`McpTransport`）不带变量。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_env_local::{LocalEnvironment, LocalMcpTransport};
use rust_cli_exec_server::Launcher;
use rust_cli_kernel_api::env::{EnvError, OperationContext, StartCommand, WorkspaceRef};
use rust_cli_kernel_api::env_command::EnvOverrides;
use rust_cli_kernel_api::machine::{CommandEvents, MachineEnv};
use rust_cli_kernel_api::mcp::{McpConnect, McpConnectionId, McpError};

/// 本机的执行环境与 MCP 的管道（与 Bash 同一份进程树账本与环境）。
pub struct LocalLauncher {
    env: Arc<LocalEnvironment>,
    mcp: Arc<LocalMcpTransport>,
}

impl LocalLauncher {
    /// 执行器服务的那台机器：端口与起进程的入口是同一个本机执行环境（`ExecServer::start` 的第一个参数）。
    pub fn served(
        env: Arc<LocalEnvironment>,
        mcp: Arc<LocalMcpTransport>,
    ) -> (MachineEnv, Arc<dyn Launcher>) {
        let machine = MachineEnv::new(env.clone(), mcp.clone());
        (machine, Arc::new(Self { env, mcp }))
    }
}

#[async_trait]
impl Launcher for LocalLauncher {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
        env: &EnvOverrides,
    ) -> Result<CommandEvents, EnvError> {
        self.env.start_command_with_env(context, request, env).await
    }

    async fn connect_mcp(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
        env: &EnvOverrides,
    ) -> Result<McpConnectionId, McpError> {
        self.mcp.connect_with_env(workspace, request, env).await
    }
}
