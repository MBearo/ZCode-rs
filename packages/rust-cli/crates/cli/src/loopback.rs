//! 只给测试的 loopback（docs/specs/rust-cli/remote-exec.md §18.6）：debug 构建设了 `_ZCODE_ASSUME_LOOPBACK_EXECUTOR` 时，
//! 本机的执行端口（`MachineEnv` 的 8 个端口）换成经 stdio 连到子进程 `zcode executor --stdio --standalone` 的
//! `RemoteEnvironment`（每个会话一个执行器，不经常驻进程，remote-exec.md §19.3）。
//! MCP 仍走本机（远端 MCP 在 R5）；只在本机的 Git、取网页仍是本机的 `LocalEnvironment`。用来验证远端实现与本机结果相同
//! （同一批场景、同一份轨迹）。

use std::sync::Arc;

use anyhow::{Context, Result};
use rust_cli_env_local::{LocalEnvironment, LocalMcpTransport};
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient};
use rust_cli_exec_transport::stdio::{ChildCommand, StdioConnector};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, LocalRegistry};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;

use crate::executor::child_args;
use crate::executor_run::this_executable;

/// 注册表：本机的机器经 loopback，本机服务仍是 `env`。
pub(super) async fn environments(
    startup: &super::Startup,
    env: Arc<LocalEnvironment>,
    mcp: Arc<LocalMcpTransport>,
    additional: &[EnvPath],
    find_and_grep: bool,
) -> Result<Arc<dyn EnvironmentRegistry>> {
    let dirs: Vec<String> = additional
        .iter()
        .map(|dir| dir.as_str().to_owned())
        .collect();
    let command = ChildCommand {
        program: this_executable()?,
        args: child_args(&dirs, find_and_grep),
        // 子进程用本进程捕获的完整环境与同一个工作目录：执行器算出的 home、临时目录、shell 与出口与本进程相同。
        env: startup.env.to_vec(),
        cwd: Some(startup.cwd.clone()),
        kill_on_drop: false,
    };
    let instance = format!("{:016x}", rust_cli_base::random::u64());
    let client = ExecClient::connect(
        Arc::new(StdioConnector::new(command)),
        ClientOptions::new(super::VERSION, instance),
    )
    .await
    .context("starting the loopback executor")?;
    let remote = Arc::new(RemoteEnvironment::new(client));
    Ok(Arc::new(LocalRegistry::new(
        MachineEnv::new(remote, mcp),
        LocalServices::new(env),
    )))
}
