//! 会话主 env 的组装（docs/specs/rust-cli/deployment.md §6、§19.1）：读部署配置，定下两个工作区（controller 本机的、
//! 会话的），按主 env 在哪建执行环境的注册表。TUI 与 `-p` 共用；app-server 现在只接受本地主 env。
//!
//! debug 构建的 SSH 测试入口（`_ZCODE_ASSUME_SSH_EXECUTOR`）折算成同一份部署配置（主 env 是那台机器，工作目录取 cwd），
//! 不再是一条单独的组装路径。

use std::sync::Arc;

use anyhow::Result;
use rust_cli_base::clock::SystemClock;
use rust_cli_env_local::{LocalCronStore, LocalEnvironment, LocalMcpTransport};
use rust_cli_ext::PluginSources;
use rust_cli_ext::plugins::PluginOrigin;
use rust_cli_kernel_api::cron_store::CronStore;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_node_store::LOCAL_ENVIRONMENT;
use rust_cli_ssh_connect::ArtifactSource;

use crate::assemble::deployment::{self, Deployment, PrimaryEnv, SshPrimary};
use crate::assemble::env_layout::{EnvLayout, PrimaryPaths};
use crate::process::facts::{Facts, ProcessFacts};
use crate::{assemble::loopback, assemble::setup, assemble::ssh_executor, commands::flags};

/// 读部署配置。
pub(crate) async fn deployment(facts: &Facts) -> Result<Deployment> {
    let mut deployment = deployment::load(&facts.process.env)
        .await
        .map_err(|error| flags::Fatal(format!("Error: {error}")))?;
    if let Some(ssh) = &facts.hooks.ssh
        && deployment.primary_is_local()
    {
        deployment.primary_env = PrimaryEnv::Ssh(SshPrimary {
            destination: ssh.destination.clone(),
            workdir: facts.process.cwd.to_string_lossy().into_owned(),
            ssh_config: ssh.config.clone(),
            artifact: ssh.artifact.clone(),
        });
    }
    Ok(deployment)
}

/// 会话的工作区：主 env 上的工作目录（本进程里是 cwd，SSH 时是部署配置的 `workdir`）。命令行的路径参数也对着它
/// 解析（deployment.md §21），controller 没有自己的工作区。
pub(crate) fn workspace(deployment: &Deployment, process: &ProcessFacts) -> WorkspaceRef {
    let root = match &deployment.primary_env {
        PrimaryEnv::Local => EnvPath::new(process.cwd.to_string_lossy()),
        PrimaryEnv::Ssh(ssh) => EnvPath::new(ssh.workdir.as_str()),
    };
    WorkspaceRef {
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
        root,
    }
}

/// 只给测试的 loopback（remote-exec.md §18.6）的输入：要不要内嵌 find / grep。
pub(crate) struct Loopback {
    pub find_and_grep: bool,
}

/// 执行环境的注册表：主 env 是本进程里的 env-local（`controller` 本身）、经 SSH 连到的执行器，或测试用的子进程执行器。
/// controller 的服务（取网页）总是本机的 `controller`。
pub(crate) async fn registry(
    deployment: &Deployment,
    process: &ProcessFacts,
    (controller, mcp): (Arc<LocalEnvironment>, Arc<LocalMcpTransport>),
    loopback: Option<Loopback>,
    source: Option<Arc<dyn ArtifactSource>>,
) -> Result<Arc<dyn EnvironmentRegistry>> {
    Ok(match (&deployment.primary_env, loopback) {
        (PrimaryEnv::Ssh(ssh), _) => {
            ssh_executor::primary(process, ssh, controller, source).await?
        }
        (PrimaryEnv::Local, Some(test)) => {
            loopback::environments(process, controller, mcp, test.find_and_grep).await?
        }
        (PrimaryEnv::Local, None) => setup::environments(controller, mcp),
    })
}

/// 主 env 上文件类数据的布局（自动记忆目录）：本进程里用 controller 的存储路径，别处按那台机器报告的 home。
pub(crate) fn layout(deployment: &Deployment, primary: &PrimaryPaths) -> EnvLayout {
    if deployment.primary_is_local() {
        EnvLayout::Controller
    } else {
        EnvLayout::Remote(primary.clone())
    }
}

/// 官方与已安装插件的包在 controller 本机的存储里，主 env 在别处时还没有让它们出现在那台机器上的办法（deployment.md
/// §24）：先去掉这两种候选，不拿 controller 的路径去那台机器上读。配置里的 `plugins.dirs` 是执行侧的目录（§21），照常。
pub(crate) fn drop_remote_plugins(deployment: &Deployment, plugins: &mut PluginSources) {
    if !deployment.primary_is_local() {
        plugins
            .candidates
            .retain(|candidate| candidate.origin == PluginOrigin::ConfigDir);
    }
}

/// 持久定时任务的存储：任务文件经本机的 fs 读写（项目根下的 `.zcode/scheduled_tasks.json`）。主 env 在别处时那条路径
/// 不在本机，所以先关掉，直到 cron 端口做完（deployment.md §10、§20）。
pub(crate) fn cron_store(
    deployment: &Deployment,
    workspace: &WorkspaceRef,
    clock: Arc<SystemClock>,
) -> Option<Arc<dyn CronStore>> {
    deployment.primary_is_local().then(|| {
        let store: Arc<dyn CronStore> = Arc::new(LocalCronStore::new(&workspace.root, clock));
        store
    })
}
