//! SSH 的组装根（docs/specs/rust-cli/remote-exec.md §19.11；deployment.md §19.1）：ssh-connect 的部署器与连接器装成
//! machines 的 `Endpoint`，经状态机连上；之后的断线由 exec-client 重连，状态写进运行日志。
//!
//! 两个用途：部署配置把会话的主 env 放在一个 SSH 执行器上（[`primary`]，debug 构建的 `_ZCODE_ASSUME_SSH_EXECUTOR`
//! 也换成同一份部署配置）；机器清单里的额外机器（[`endpoint`]，R3）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rust_cli_env_local::LocalEnvironment;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, Reconnect};
use rust_cli_exec_transport::{Connector, LinkError};
use rust_cli_exec_wire::handshake::PROTOCOL;
use rust_cli_kernel_api::controller_services::ControllerServices;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, PrimaryRegistry};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_machines::{Endpoint, Machine};
use rust_cli_machines::{Provisioned, Provisioner};
use rust_cli_paths::ExecutorPaths;
use rust_cli_ssh_connect::{
    ArtifactSource, Platform, SshConnector, SshProvisioner, SshTarget, Wanted,
};

use crate::assemble::deployment::SshPrimary;

/// SSH 下的重连：0.5 s 起翻倍、最多 60 s、次数不限，直到连上、被拒绝（ssh 认证失败等，ssh-connect 标成 `Rejected`）或
/// 用户断开（远端执行 F96，remote-exec.md §23.4；R2 原来是最多 15 s、12 次）。机器清单里的机器同样用它（R3）。
pub(crate) const RECONNECT: Reconnect = Reconnect {
    attempts: None,
    delay: Duration::from_millis(500),
    max_delay: Duration::from_secs(60),
};

pub(crate) struct SshEndpoint {
    target: SshTarget,
    provisioner: SshProvisioner,
}

impl Endpoint for SshEndpoint {
    fn describe(&self) -> String {
        self.target.describe()
    }

    fn provisioner(&self) -> &dyn Provisioner {
        &self.provisioner
    }

    fn connector(&self, provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        Ok(Arc::new(SshConnector::new(
            self.target.clone(),
            provisioned,
        )?))
    }
}

/// 上传用的静态产物：测试变量给的文件，再加本机缓存里同版本的两种 Linux 架构（§19.4 的 `dist/` 的 musl 目录）。下载
/// 的产物在各自平台的 target 目录，归下载来源管（F100、F107，§26.3、§28.8），互不覆盖；macOS 没有静态产物的目录。
fn artifacts(executor: &ExecutorPaths, artifact: Option<&str>) -> Vec<PathBuf> {
    artifact
        .into_iter()
        .map(PathBuf::from)
        .chain(Platform::LINUX.into_iter().filter_map(|platform| {
            Some(
                executor
                    .artifact(crate::VERSION, platform.static_triple()?)
                    .as_path()
                    .to_path_buf(),
            )
        }))
        .collect()
}

/// 会话的主 env 是经 SSH 连到的执行器（deployment.md §19.1）：9 个执行端口与 MCP 的管道都经这条连接（MCP 服务器起在
/// 那台机器上）；controller 的服务（取网页）仍是本机的 `controller`。连接的状态机随注册表活着。`source` 是执行器的 npm
/// 来源（部署配置没给产物时用，F100）。
pub(crate) async fn primary(
    process: &crate::process::facts::ProcessFacts,
    ssh: &SshPrimary,
    controller: Arc<LocalEnvironment>,
    source: Option<Arc<dyn ArtifactSource>>,
) -> Result<Arc<dyn EnvironmentRegistry>> {
    let endpoint = endpoint(
        (&process.env, &process.executor_paths()),
        &ssh.destination,
        (ssh.ssh_config.as_deref(), ssh.artifact.as_deref()),
        source,
    );
    let instance = format!("{:016x}", rust_cli_base::random::u64());
    let mut options = ClientOptions::new(crate::VERSION, instance);
    options.reconnect = RECONNECT;
    let machine = Arc::new(Machine::new(endpoint, options));
    let client = machine
        .connect()
        .await
        .with_context(|| format!("connecting to the ssh executor at {}", ssh.destination))?;
    // 主 env 的名字是身份（`primary`），不是 ssh 的目标（deployment.md §23）。
    let remote = Arc::new(RemoteEnvironment::new(client));
    Ok(Arc::new(SshRegistry {
        inner: PrimaryRegistry::new(remote.machine(), ControllerServices::new(controller)),
        _machine: machine,
    }))
}

/// 到 `destination` 的 SSH 端点（R2 的测试入口与 R3 的机器清单共用）：`extra` 是测试变量给的 ssh 配置文件与上传用的产物；
/// `source` 是执行器的 npm 来源（F100，§26.7；R2 的测试入口不给）。
pub(crate) fn endpoint(
    (env, executor): (&rust_cli_base::secret::EnvSnapshot, &ExecutorPaths),
    destination: &str,
    (config, artifact): (Option<&str>, Option<&str>),
    source: Option<Arc<dyn ArtifactSource>>,
) -> Arc<SshEndpoint> {
    let target = SshTarget {
        destination: destination.to_owned(),
        program: PathBuf::from("ssh"),
        config: config.map(PathBuf::from),
        options: Vec::new(),
        env: env.vars().to_vec(),
    };
    let wanted = Wanted {
        version: crate::VERSION.to_owned(),
        protocol: PROTOCOL,
    };
    let provisioner = SshProvisioner::new(target.clone(), wanted, artifacts(executor, artifact));
    let provisioner = match source {
        Some(source) => provisioner.with_source(source),
        None => provisioner,
    };
    Arc::new(SshEndpoint {
        target,
        provisioner,
    })
}

/// 主 env 在 SSH 执行器上的注册表加上它的连接状态机（状态机订阅连接状态，要和注册表活得一样久）。
struct SshRegistry {
    inner: PrimaryRegistry,
    _machine: Arc<Machine>,
}

impl EnvironmentRegistry for SshRegistry {
    fn primary_machine(&self) -> MachineEnv {
        self.inner.primary_machine()
    }

    fn controller(&self) -> ControllerServices {
        self.inner.controller()
    }
}
