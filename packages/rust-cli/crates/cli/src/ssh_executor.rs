//! 只给测试的 SSH 入口（docs/specs/rust-cli/remote-exec.md §19.11）：debug 构建设了 `_ZCODE_ASSUME_SSH_EXECUTOR` 时，本机的
//! 执行端口（`MachineEnv` 的 8 个端口）换成经 SSH 连到那个目标的远端环境。组装同 loopback：MCP、Git、取网页仍是本机；
//! 工作区仍是本机的路径（远端的工作目录在 R3），所以只用于用绝对路径的测试。
//!
//! 这里是 SSH 的组装根：ssh-connect 的部署器与连接器装成 machines 的 `Endpoint`，经状态机连上；之后的断线由 exec-client
//! 重连，状态写进运行日志。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rust_cli_env_local::{LocalEnvironment, LocalMcpTransport};
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, Reconnect};
use rust_cli_exec_transport::{Connector, LinkError, Provisioned, Provisioner};
use rust_cli_exec_wire::handshake::PROTOCOL;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, LocalRegistry};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_machines::{Endpoint, Machine};
use rust_cli_paths::{ExecutorPaths, StorageSettings};
use rust_cli_ssh_connect::{Arch, ArtifactSource, SshConnector, SshProvisioner, SshTarget, Wanted};

use crate::options::SshAssumption;

/// SSH 下的重连：0.5 s 起翻倍、最多 60 s、次数不限，直到连上、被拒绝（ssh 认证失败等，ssh-connect 标成 `Rejected`）或
/// 用户断开（远端执行 F96，remote-exec.md §23.4；R2 原来是最多 15 s、12 次）。机器清单里的机器同样用它（R3）。
pub(super) const RECONNECT: Reconnect = Reconnect {
    attempts: None,
    delay: Duration::from_millis(500),
    max_delay: Duration::from_secs(60),
};

pub(super) struct SshEndpoint {
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

/// 本机执行器目录（上传用的产物缓存在它下面，§19.4）：在用户根下，不随 `storage.dir`。
pub(super) fn executor_paths(startup: &super::Startup) -> ExecutorPaths {
    super::setup::storage_paths(startup, &StorageSettings::default()).executor()
}

/// 上传用的静态产物：测试变量给的文件，再加本机缓存里同版本的两种架构（§19.4 的 `dist/` 的 musl 目录）。下载的 glibc
/// 产物在 gnu 目录，归下载来源管（F100，§26.3），两种互不覆盖。
fn artifacts(executor: &ExecutorPaths, artifact: Option<&str>) -> Vec<PathBuf> {
    artifact
        .into_iter()
        .map(PathBuf::from)
        .chain([Arch::X86_64, Arch::Aarch64].map(|arch| {
            executor
                .artifact(super::VERSION, arch.musl_triple())
                .as_path()
                .to_path_buf()
        }))
        .collect()
}

/// 注册表：本机的机器经 SSH 连到的远端，本机服务仍是 `env`；连接的状态机随注册表活着。
pub(super) async fn environments(
    startup: &super::Startup,
    ssh: &SshAssumption,
    env: Arc<LocalEnvironment>,
    mcp: Arc<LocalMcpTransport>,
) -> Result<Arc<dyn EnvironmentRegistry>> {
    let endpoint = endpoint(
        (&startup.env, &executor_paths(startup)),
        &ssh.destination,
        (ssh.config.as_deref(), ssh.artifact.as_deref()),
        None,
    );
    let instance = format!("{:016x}", rust_cli_base::random::u64());
    let mut options = ClientOptions::new(super::VERSION, instance);
    options.reconnect = RECONNECT;
    let machine = Arc::new(Machine::new(endpoint, options));
    let client = machine
        .connect()
        .await
        .with_context(|| format!("connecting to the ssh executor at {}", ssh.destination))?;
    let remote = Arc::new(RemoteEnvironment::new(client).with_name(ssh.destination.as_str()));
    Ok(Arc::new(SshRegistry {
        inner: LocalRegistry::new(MachineEnv::new(remote, mcp), LocalServices::new(env)),
        _machine: machine,
    }))
}

/// 到 `destination` 的 SSH 端点（R2 的测试入口与 R3 的机器清单共用）：`extra` 是测试变量给的 ssh 配置文件与上传用的产物；
/// `source` 是执行器的 npm 来源（F100，§26.7；R2 的测试入口不给）。
pub(super) fn endpoint(
    (env, executor): (&[(String, String)], &ExecutorPaths),
    destination: &str,
    (config, artifact): (Option<&str>, Option<&str>),
    source: Option<Arc<dyn ArtifactSource>>,
) -> Arc<SshEndpoint> {
    let target = SshTarget {
        destination: destination.to_owned(),
        program: PathBuf::from("ssh"),
        config: config.map(PathBuf::from),
        options: Vec::new(),
        env: env.to_vec(),
    };
    let wanted = Wanted {
        version: super::VERSION.to_owned(),
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

/// 本机的注册表加上它的连接状态机（状态机订阅连接状态，要和注册表活得一样久）。
struct SshRegistry {
    inner: LocalRegistry,
    _machine: Arc<Machine>,
}

impl EnvironmentRegistry for SshRegistry {
    fn local_machine(&self) -> MachineEnv {
        self.inner.local_machine()
    }

    fn local(&self) -> LocalServices {
        self.inner.local()
    }
}
