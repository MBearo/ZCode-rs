//! 远端机器的组装（docs/specs/rust-cli/remote-exec.md §20.2、§20.3）：读机器清单，每台机器经 R2 的连接状态机（machines 的
//! `Machine`）连，就绪时把连接包成 `env-remote` 放进注册表，重连失败或断开时拿走。
//!
//! 端点（`Endpoint`）由这里按测试变量选：产品路径是 ssh-connect（ssh 目标就是清单里的 `ssh`）；只给测试的 loopback（debug
//! 构建、`_ZCODE_ASSUME_LOOPBACK_MACHINES`）是每台机器一个 `zcode executor --stdio --standalone` 子进程，工作目录是那台机器的
//! 工作目录。TUI 在后台连（不挡启动）；`-p` 只在测试变量下连，而且同步连完。

mod endpoint;
mod executor_dist;
mod fleet;
mod registry;

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow};
use rust_cli_base::TaskScope;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient};
use rust_cli_exec_wire::dto::core::PathStyle as WirePathStyle;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_registry::{EnvironmentRegistry, MachineFacts, RemoteMachine};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_machines::{MachineEntry, MachineList};

pub(crate) use fleet::{Changed, Fleet, FleetError, Seen};
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

/// 注册表、机器清单与各台机器的状态机（都属于这里；进程退出时随之结束）。
pub(crate) struct Machines {
    registry: Arc<MachineRegistry>,
    scope: TaskScope,
    /// 清单与连接（R4，remote-exec.md §21.2）：`start` 时建（`-p` 不连机器时没有）。
    fleet: OnceLock<Arc<Fleet>>,
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
    /// 执行器的 npm 来源（F100，remote-exec.md §26.7）：所有机器共用一个（单飞靠它）；debug 构建没给假注册表时没有。
    pub source: Option<Arc<dyn rust_cli_ssh_connect::ArtifactSource>>,
    /// 每台机器上期望的 MCP 服务器（R5，remote-exec.md §22.5）：入口在组装好 MCP 设置之后填。
    pub mcp: std::collections::BTreeMap<String, Vec<rust_cli_kernel_api::mcp::McpConnect>>,
    /// 绑定到远端机器的全部 MCP 服务器（服务器名，机器名；含停用的）：TUI 的工具行标出机器（§22.8）。
    pub mcp_marks: Vec<(String, String)>,
}

impl Start {
    /// 按形态与测试变量选：`surface` 是（TUI，`-p`）；`egress` 给执行器的 npm 来源（F100）。
    pub(crate) fn new(
        paths: &rust_cli_paths::StoragePaths,
        (tui, print): (bool, bool),
        (startup, egress): (&super::Startup, &Arc<rust_cli_net::Egress>),
    ) -> Self {
        let assume = startup.explicit.assume;
        let ssh = crate::options::machine_ssh(crate::options::TestDeclarations::BUILD, |name| {
            startup
                .env
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        });
        let executor = crate::ssh_executor::executor_paths(startup);
        let registry =
            executor_dist::registry(crate::options::TestDeclarations::BUILD, ssh.npm_registry);
        Self {
            source: executor_dist::source(registry, egress, &executor),
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
            executor,
            mcp: std::collections::BTreeMap::new(),
            mcp_marks: Vec::new(),
        }
    }

    /// 绑定到远端机器的 MCP 服务器：启用的按机器分组成期望集合（§22.5；停用的不算，执行器里还在跑的会被结束）；
    /// 启用的与停用的都给 TUI 标出机器（§22.8）。
    pub(crate) fn with_mcp(mut self, config: &rust_cli_ext::ExtConfig) -> Self {
        let all = config.mcp.servers.iter().chain(&config.mcp_disabled);
        self.mcp_marks = all
            .filter_map(|server| Some((server.name.clone(), server.machine.clone()?)))
            .collect();
        for server in &config.mcp.servers {
            if let Some(machine) = &server.machine {
                self.mcp.entry(machine.clone()).or_default().push(
                    rust_cli_kernel_api::mcp::McpConnect {
                        server: server.name.clone(),
                        transport: server.transport.clone(),
                        env: Default::default(),
                    },
                );
            }
        }
        self
    }
}

impl Machines {
    pub(crate) fn new(local: Arc<dyn EnvironmentRegistry>) -> Self {
        Self {
            registry: Arc::new(MachineRegistry::new(local)),
            scope: TaskScope::root("machines", |panic| {
                tracing::error!(task = panic.task, message = %panic.message, "machine task panicked");
            }),
            fleet: OnceLock::new(),
        }
    }

    /// TUI 的机器服务（R4，§21.2）；`start` 之前或不连机器时没有。
    pub(crate) fn service(&self) -> Option<Arc<dyn rust_cli_tui::machines::Machines>> {
        self.fleet
            .get()
            .map(|fleet| crate::tui_machines::service(fleet.clone()))
    }

    pub(crate) fn registry(&self) -> Arc<dyn EnvironmentRegistry> {
        self.registry.clone()
    }

    /// 读清单、建 Fleet，自动连上次连着的机器（§20.3.2）：TUI 在后台连（不挡启动，连不上只记 warn）；测试变量下的
    /// `-p` 同步连完，连不上就报错。
    pub(crate) async fn start(&self, mut start: Start) -> Result<()> {
        let when = start.when;
        if when == When::Never {
            return Ok(());
        }
        let entries = match start.list.load().await {
            Ok(entries) => entries,
            Err(error) if when == When::Now => return Err(error.into()),
            Err(error) => {
                tracing::warn!(%error, "the machine list could not be read");
                Vec::new()
            }
        };
        let list = start.list.clone();
        let mcp = std::mem::take(&mut start.mcp);
        let make = move |entry: &MachineEntry| endpoint::machine(&start, entry);
        let fleet = Arc::new(
            Fleet::new(
                (list, Box::new(make)),
                entries,
                self.registry.clone(),
                self.scope.spawner(),
            )
            .with_mcp(mcp),
        );
        let fleet = self.fleet.get_or_init(|| fleet).clone();
        for name in fleet.remembered() {
            match when {
                When::Now => fleet
                    .connect(&name)
                    .await
                    .map_err(anyhow::Error::from)
                    .with_context(|| format!("connecting machine {name}"))
                    .map(drop)?,
                When::Background => {
                    let fleet = fleet.clone();
                    // 返回之前就记上“正在连”（R5，§22.6）：之后启动的 MCP 连接任务看得到，等它连完。
                    let connecting = self.registry.connecting(&name);
                    drop(self.scope.spawner().spawn("machine-connect", async move {
                        let _connecting = connecting;
                        if let Err(error) = fleet.connect(&name).await {
                            tracing::warn!(machine = %name, %error, "machine not connected");
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
pub(super) fn remote_of(entry: &MachineEntry, client: &ExecClient) -> Result<RemoteMachine> {
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
    let link = client.states();
    // 机器名也交给远端环境：失败时给模型的说明写它（F96，remote-exec.md §23.6）。
    let remote = Arc::new(RemoteEnvironment::new(client).with_name(entry.name.as_str())).machine();
    registry.connected(remote.named(entry.name.as_str()), info, link);
    tracing::info!(event = "machine.connected", machine = %entry.name, "machine connected");
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
