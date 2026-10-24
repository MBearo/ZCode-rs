//! 合同测试的两种机器与共用的夹具。

use std::path::Path;
use std::sync::Arc;

use rust_cli_base::OwnedTask;
use rust_cli_env_local::LocalEnvironment;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient};
use rust_cli_exec_server::{AllowAll, ExecServer, MemoryLedger, ServerConfig};
use rust_cli_exec_transport::memory::network;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId,
};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::mcp::NoMcpTransport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Local,
    Loopback,
}

/// 一台被测的机器。loopback 时还持有服务端与客户端（随它一起释放）。
pub struct Machine {
    pub env: MachineEnv,
    _loopback: Option<(ExecServer, ExecClient, OwnedTask<()>)>,
}

impl std::ops::Deref for Machine {
    type Target = MachineEnv;

    fn deref(&self) -> &MachineEnv {
        &self.env
    }
}

/// 把一个 `LocalEnvironment` 装成被测的机器。
pub async fn machine(kind: Kind, local: LocalEnvironment) -> Machine {
    let local = MachineEnv::new(Arc::new(local), Arc::new(NoMcpTransport));
    match kind {
        Kind::Local => Machine {
            env: local,
            _loopback: None,
        },
        Kind::Loopback => {
            let config =
                ServerConfig::new("contract", std::env::consts::OS, std::env::consts::ARCH);
            let server = ExecServer::start(
                local,
                Arc::new(MemoryLedger::default()),
                Arc::new(AllowAll),
                config,
            )
            .await
            .unwrap();
            let (connector, listener) = network("contract");
            let serving = {
                let server = server.clone();
                rust_cli_base::owned(async move { server.serve(&listener).await })
            };
            let client = ExecClient::connect(
                Arc::new(connector),
                ClientOptions::new("contract", "client"),
            )
            .await
            .unwrap();
            let remote = Arc::new(RemoteEnvironment::new(client.clone()));
            Machine {
                env: remote.machine(),
                _loopback: Some((server, client, serving)),
            }
        }
    }
}

pub fn workspace(root: &Path) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: env_path(root),
    }
}

pub fn env_path(path: &Path) -> EnvPath {
    EnvPath::new(path.to_string_lossy())
}

pub fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(operation),
        origin: Origin::Agent {
            session: SessionId::new("s1"),
            run: RunId::new("r1"),
        },
    }
}
