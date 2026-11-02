//! 执行器一侧起进程（F96，remote-exec.md §23.5）：命令与 MCP stdio 服务器带上请求所属的客户端实例推来的环境变量
//! （`env.update` 与 `hello.session_env`）。推来的变量只在执行器里有，所以它们是这里的参数，不在 kernel-api 的
//! `StartCommand` / `McpConnect` 里（F128，审查 K10）。执行器的入口用本机的执行环境实现它（executor-host 的
//! `LocalLauncher`，背后是 env-local 的 `start_command_with_env` / `connect_with_env`）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvError, OperationContext, StartCommand, WorkspaceRef};
use rust_cli_kernel_api::env_command::EnvOverrides;
use rust_cli_kernel_api::machine::{CommandEvents, MachineEnv};
use rust_cli_kernel_api::mcp::{McpConnect, McpConnectionId, McpError};

/// 起进程的入口：同 `Commands::start_command` 与 `McpTransport::connect`，多一组推来的环境变量。
#[async_trait]
pub trait Launcher: Send + Sync {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
        env: &EnvOverrides,
    ) -> Result<CommandEvents, EnvError>;

    async fn connect_mcp(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
        env: &EnvOverrides,
    ) -> Result<McpConnectionId, McpError>;
}

/// 起进程直接经机器的端口：给不报告 `env` 能力位的执行器（客户端不会推变量）。真的收到推来的变量时拒绝，不悄悄丢掉。
pub struct PortLauncher(pub MachineEnv);

fn refused() -> String {
    "this executor does not apply pushed environment variables".to_owned()
}

#[async_trait]
impl Launcher for PortLauncher {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
        env: &EnvOverrides,
    ) -> Result<CommandEvents, EnvError> {
        if !env.is_empty() {
            return Err(EnvError::Unsupported { what: refused() });
        }
        self.0
            .commands()
            .start_command(workspace, context, request)
            .await
    }

    async fn connect_mcp(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
        env: &EnvOverrides,
    ) -> Result<McpConnectionId, McpError> {
        if !env.is_empty() {
            return Err(McpError::Unsupported { what: refused() });
        }
        self.0.mcp().connect(workspace, request).await
    }
}
