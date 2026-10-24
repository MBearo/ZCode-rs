//! 远端机器的组装（docs/specs/rust-cli/remote-exec.md §20.2、§20.3）：读机器清单，每台机器经 R2 的连接状态机（machines 的
//! `Machine`）连，就绪时把连接包成 `env-remote` 放进注册表，重连失败或断开时拿走。
//!
//! 端点（`Endpoint`）由这里按测试变量选：产品路径是 ssh-connect（ssh 目标就是清单里的 `ssh`）；只给测试的 loopback（debug
//! 构建、`_ZCODE_ASSUME_LOOPBACK_MACHINES`）是每台机器一个 `zcode executor --stdio --standalone` 子进程，工作目录是那台机器的
//! 工作目录。TUI 在后台连（不挡启动）；`-p` 只在测试变量下连，而且同步连完。

mod endpoint;
mod registry;

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use rust_cli_base::TaskScope;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient};
use rust_cli_exec_wire::dto::core::PathStyle as WirePathStyle;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, MachineFacts, RemoteMachine};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_machines::{Machine, MachineEntry, MachineList, MachineState};

pub(crate) use registry::MachineRegistry;

/// 机器怎么连。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Connect {
    /// 产品路径：经 SSH（测试变量可以给 `-F` 的配置文件与上传用的产物）。
    Ssh {
        config: Option<String>,
        artifact: Option<String>,
    },
    /// 只给测试的 loopback：本进程的二进制以执行器模式跑在那台机器的工作目录。
    Loopback,
}

/// 什么时候连。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum When {
    /// 不连（release 的 `-p`）。
    Never,
    /// 在后台连，不挡启动（TUI）。
    Background,
    /// 连完再往下走，连不上就报错（测试变量下的 `-p`）。
    Now,
}

/// 注册表与各台机器的状态机（都属于这里；进程退出时随之结束）。
pub(crate) struct Machines {
    registry: Arc<MachineRegistry>,
    scope: TaskScope,
}

/// 启动时的输入（自己持有，不借用 `Startup`：入口之后会把 `Startup` 的一部分移走）。
pub(crate) struct Start {
    pub list: MachineList,
    pub connect: Connect,
    pub when: When,
    /// 执行器子进程与 ssh 进程的环境（入口捕获的快照）。
    pub env: Vec<(String, String)>,
    /// 本机执行器目录（上传用的产物缓存，R2 §19.4）。
    pub executor: rust_cli_paths::ExecutorPaths,
}

impl Start {
    /// 按形态与测试变量选：`surface` 是（TUI，`-p`）。
    pub(crate) fn new(
        paths: &rust_cli_paths::StoragePaths,
        (tui, print): (bool, bool),
        startup: &super::Startup,
    ) -> Self {
        let assume = startup.explicit.assume;
        let ssh = crate::options::machine_ssh(crate::options::TestDeclarations::BUILD, |name| {
            startup
                .env
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        });
        Self {
            list: MachineList::new(&paths.machines()),
            connect: if assume.loopback_machines {
                Connect::Loopback
            } else {
                Connect::Ssh {
                    config: ssh.config,
                    artifact: ssh.artifact,
                }
            },
            when: if tui {
                When::Background
            } else if print && (assume.loopback_machines || assume.ssh_machines) {
                When::Now
            } else {
                When::Never
            },
            env: startup.env.to_vec(),
            executor: crate::ssh_executor::executor_paths(startup),
        }
    }
}

impl Machines {
    pub(crate) fn new(local: Arc<dyn EnvironmentRegistry>) -> Self {
        Self {
            registry: Arc::new(MachineRegistry::new(local)),
            scope: TaskScope::root("machines", |panic| {
                tracing::error!(task = panic.task, message = %panic.message, "machine task panicked");
            }),
        }
    }

    pub(crate) fn registry(&self) -> Arc<dyn EnvironmentRegistry> {
        self.registry.clone()
    }

    /// 自动连上次连着的机器（§20.3.2）。
    pub(crate) async fn start(&self, start: Start) -> Result<()> {
        if start.when == When::Never {
            return Ok(());
        }
        let entries = match start.list.load().await {
            Ok(entries) => entries,
            Err(error) if start.when == When::Now => return Err(error.into()),
            Err(error) => {
                tracing::warn!(%error, "the machine list could not be read");
                return Ok(());
            }
        };
        for entry in entries.into_iter().filter(|entry| entry.connected) {
            let name = entry.name.clone();
            let registry = self.registry.clone();
            let spawner = self.scope.spawner();
            let machine = endpoint::machine(&start, &entry);
            match start.when {
                When::Now => connect(registry, machine?, entry, &spawner)
                    .await
                    .with_context(|| format!("connecting machine {name}"))?,
                // 后台连：连不上（包括端点建不出来）只记 warn，不挡启动。
                When::Background => {
                    drop(self.scope.spawner().spawn("machine-connect", async move {
                        let result = match machine {
                            Ok(machine) => connect(registry, machine, entry, &spawner).await,
                            Err(error) => Err(error),
                        };
                        if let Err(error) = result {
                            tracing::warn!(machine = %name, error = %format!("{error:#}"), "machine not connected");
                        }
                    }));
                }
                When::Never => {}
            }
        }
        Ok(())
    }
}

fn path_style(style: WirePathStyle) -> PathStyle {
    match style {
        WirePathStyle::Posix => PathStyle::Posix,
        WirePathStyle::Windows => PathStyle::Windows,
    }
}

/// 握手的结果 → 注册表里的一台机器（工作目录没写时是远端的 home）。
fn remote_of(entry: &MachineEntry, client: &ExecClient) -> Result<RemoteMachine> {
    let welcome = client
        .welcome()
        .ok_or_else(|| anyhow!("the executor did not finish the handshake"))?;
    let root = entry
        .working_directory
        .clone()
        .or_else(|| welcome.home.clone())
        .ok_or_else(|| anyhow!("no working directory and no remote home"))?;
    Ok(RemoteMachine {
        name: entry.name.clone(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new(entry.name.as_str()),
            root: EnvPath::new(root),
        },
        facts: MachineFacts {
            os: welcome.os,
            arch: welcome.arch,
            shell: welcome.shell.unwrap_or_default(),
            home: welcome.home.map(EnvPath::new),
            path_style: path_style(welcome.path_style),
        },
    })
}

/// 放进注册表（已经在里面时不动：重连后仍是同一个客户端）。
fn attach(registry: &MachineRegistry, entry: &MachineEntry, client: ExecClient) -> Result<()> {
    if registry.remote(&entry.name).is_some() {
        return Ok(());
    }
    let info = remote_of(entry, &client)?;
    let remote = Arc::new(RemoteEnvironment::new(client)).machine();
    registry.connected(remote.named(entry.name.as_str()), info);
    tracing::info!(event = "machine.connected", machine = %entry.name, "machine connected");
    Ok(())
}

/// 连上一台机器并跟着它的状态：就绪时放进注册表；重连中仍留着（调用等重连，暂定，§20.3.2）；重连失败或断开时拿走。
async fn connect(
    registry: Arc<MachineRegistry>,
    machine: Machine,
    entry: MachineEntry,
    spawner: &rust_cli_base::Spawner,
) -> Result<()> {
    let client = machine.connect().await?;
    attach(&registry, &entry, client)?;
    drop(spawner.spawn("machine-follow", async move {
        let mut states = machine.states();
        states.borrow_and_update();
        while states.changed().await.is_ok() {
            let state = states.borrow_and_update().clone();
            match state {
                MachineState::Ready(_) => {
                    if let Some(client) = machine.client()
                        && let Err(error) = attach(&registry, &entry, client)
                    {
                        tracing::warn!(machine = %entry.name, error = %format!("{error:#}"), "machine not attached");
                    }
                }
                MachineState::Failed { .. } | MachineState::Unconnected => {
                    registry.disconnected(&entry.name);
                    tracing::info!(event = "machine.disconnected", machine = %entry.name, state = ?state, "machine disconnected");
                }
                MachineState::Provisioning
                | MachineState::Handshaking
                | MachineState::Reconnecting { .. } => {}
            }
        }
    }));
    Ok(())
}

/// 客户端的参数：SSH 照 R2 重连；loopback 不重连（执行器断开就是这台机器断开，§18.5）。
fn options(connect: &Connect) -> ClientOptions {
    let instance = format!("{:016x}", rust_cli_base::random::u64());
    let mut options = ClientOptions::new(crate::VERSION, instance);
    if matches!(connect, Connect::Ssh { .. }) {
        options.reconnect = crate::ssh_executor::RECONNECT;
    }
    options
}
