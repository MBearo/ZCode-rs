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
use rust_cli_paths::StorageSettings;
use rust_cli_ssh_connect::{Arch, SshConnector, SshProvisioner, SshTarget, Wanted};

use crate::options::SshAssumption;

/// SSH 下的重连（remote-exec.md §19.8）：0.5 s 起翻倍、最多 15 s，12 次（合计约 2 分钟）。
const RECONNECT: Reconnect = Reconnect {
    attempts: 12,
    delay: Duration::from_millis(500),
    max_delay: Duration::from_secs(15),
};

struct SshEndpoint {
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

/// 上传用的产物：测试变量给的文件，再加本机缓存里同版本的两种架构（§19.4 的 `dist/`）。
fn artifacts(startup: &super::Startup, ssh: &SshAssumption) -> Vec<PathBuf> {
    let executor = super::setup::storage_paths(startup, &StorageSettings::default()).executor();
    ssh.artifact
        .iter()
        .map(PathBuf::from)
        .chain([Arch::X86_64, Arch::Aarch64].map(|arch| {
            executor
                .artifact(super::VERSION, arch.triple())
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
    let target = SshTarget {
        destination: ssh.destination.clone(),
        program: PathBuf::from("ssh"),
        config: ssh.config.as_ref().map(PathBuf::from),
        options: Vec::new(),
        env: startup.env.to_vec(),
    };
    let wanted = Wanted {
        version: super::VERSION.to_owned(),
        protocol: PROTOCOL,
    };
    let provisioner = SshProvisioner::new(target.clone(), wanted, artifacts(startup, ssh));
    let endpoint = Arc::new(SshEndpoint {
        target,
        provisioner,
    });
    let instance = format!("{:016x}", rust_cli_base::random::u64());
    let mut options = ClientOptions::new(super::VERSION, instance);
    options.reconnect = RECONNECT;
    let machine = Arc::new(Machine::new(endpoint, options));
    let client = machine
        .connect()
        .await
        .with_context(|| format!("connecting to the ssh executor at {}", ssh.destination))?;
    let remote = Arc::new(RemoteEnvironment::new(client));
    Ok(Arc::new(SshRegistry {
        inner: LocalRegistry::new(MachineEnv::new(remote, mcp), LocalServices::new(env)),
        _machine: machine,
    }))
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
