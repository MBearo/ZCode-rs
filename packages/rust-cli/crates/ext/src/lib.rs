//! 扩展：skill、MCP、插件（docs/specs/rust-cli/ext.md）。
//!
//! - skill（[`skills`]）：来源与优先级、SKILL.md frontmatter、清单与预算、Skill 工具（参数替换、变量、
//!   `!` 内联命令）、`paths` 条件激活；
//! - MCP（[`mcp`]）：连接管理（握手、工具清单、调用、通知、重连）、工具命名与包装、结果转换与截断、
//!   instructions、resources；传输经 kernel-api 的 [`McpTransport`]，本地实现在 env-local；
//! - 插件（[`plugins`]）：来源与启用判定、目录与 manifest，提供 skill、MCP 服务器、agent 与 hooks。
//!
//! 不做 IO：文件经执行环境端口，MCP 消息经传输端口，任务经 base 的 [`Spawner`]。runtime 组装：
//! [`Ext::registry`] 包住基础工具注册表（加上 Skill 与 MCP 工具），[`Ext::listings`] 是 skill 清单与
//! MCP instructions 的来源，[`Ext::start`] 在内核建好后开始连接 MCP 服务器。

#[cfg(test)]
mod clock_tests;
pub mod mcp;
pub mod plugins;
pub mod skills;
#[cfg(test)]
mod test_support;

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::Spawner;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::machine::{EnvInfo, Files};
use rust_cli_kernel_api::mcp::McpTransport;
use rust_cli_kernel_api::mcp_state::{McpConnections, WAIT_FOR_MCP_SERVERS};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::orchestration::{
    ContextRequest, OrchestrationError, ToolRegistry, ToolSnapshot,
};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_policy::PolicyGate;

pub use mcp::{McpManager, McpScope, McpServer, McpServers, McpSettings};
pub use plugins::{Plugin, PluginAgentFile, PluginSources, Plugins};
pub use skills::{Skill, SkillCatalog, SkillCommands, SkillSource, SkillTool};

/// ext 用到的端口，由 runtime 注入。
#[derive(Clone)]
pub struct ExtPorts {
    /// skill、插件都是本机的文件（工作区、用户目录、插件缓存），经本机的端口读（runtime 从注册表的本机取，
    /// remote-exec.md §3、§15.8.1）。
    pub info: Arc<dyn EnvInfo>,
    pub files: Arc<dyn Files>,
    pub ids: Arc<dyn Ids>,
    /// Skill 的 `!` 命令用判决链（带 skill 的 `allowed-tools`，ext.md §2.5）。
    pub gate: Arc<PolicyGate>,
    pub mcp: Arc<dyn McpTransport>,
    /// 远端机器的注册表（R5，remote-exec.md §22.6）：绑定到远端机器的 MCP 服务器从这里取那台机器的管道；None 时
    /// （只给测试的组装）它们都连不上。
    pub machines: Option<Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>>,
    /// MCP 的连接任务在这里启动。
    pub spawner: Spawner,
    /// MCP 等待的截止时刻（ext.md §3.3.1、§3.8）。
    pub clock: Arc<dyn Clock>,
}

/// 入口层给出的配置快照。
#[derive(Debug, Clone, Default)]
pub struct ExtConfig {
    /// MCP 服务器、连接与调用的超时、输出上限（ext.md §3.1）。
    pub mcp: McpSettings,
    /// 插件目录（`--plugin-dir`，执行环境里的路径，ext.md §4）。
    pub plugin_dirs: Vec<EnvPath>,
    /// Node 配置与已安装插件给出的候选目录与启用设置（ext.md §4.1、config.md §2.1）。
    pub plugins: PluginSources,
    /// MCP 服务器与插件所在的工作区（stdio 服务器的 cwd）；None 时不连接 MCP、不加载插件。
    pub workspace: Option<WorkspaceRef>,
    /// 配置里停用的 MCP 服务器（`enabled: false`）：登记为 disabled、不连接，`/mcp enable` 时才连
    /// （docs/specs/rust-cli/tui-manage.md §6.1）。
    pub mcp_disabled: Vec<McpServer>,
}

pub struct Ext {
    skills: Arc<SkillCatalog>,
    commands: Arc<SkillCommands>,
    mcp: Arc<McpManager>,
    registry: Arc<ExtRegistry>,
    /// 插件目录（`--plugin-dir` 与入口给的候选，都在本机）。
    files: Arc<dyn Files>,
    config: ExtConfig,
}

impl Ext {
    /// `base` 是基础工具注册表（文件、Bash 等）；Skill 的 `!` 命令用其中的 `Bash`。
    pub fn new(ports: ExtPorts, base: Arc<dyn ToolRegistry>, config: ExtConfig) -> Self {
        let skills = Arc::new(SkillCatalog::new(ports.info.clone(), ports.files.clone()));
        let runner = base
            .find("Bash")
            .map(|bash| skills::runner(bash, ports.gate.clone(), ports.ids.clone()));
        // 用户调用（`/<名字>`，ext.md §2.7）与 Skill 工具共用目录与 `!` 命令的执行者。
        let commands = Arc::new(SkillCommands::new(skills.clone(), runner.clone()));
        let skill_tool: Arc<dyn Tool> =
            Arc::new(SkillTool::new(skills.clone(), ports.ids.clone(), runner));
        let mcp = Arc::new(McpManager::new(
            ports.mcp.clone(),
            ports.spawner.clone(),
            config.mcp.clone(),
            ports.clock.clone(),
        ));
        if let Some(machines) = ports.machines.clone() {
            mcp.set_machines(machines);
        }
        let registry = Arc::new(ExtRegistry {
            base,
            skill: skill_tool,
            wait: Arc::new(mcp::WaitForMcpServers::new(Arc::downgrade(&mcp))),
            mcp: mcp.clone(),
        });
        Self {
            skills,
            commands,
            mcp,
            registry,
            files: ports.files,
            config,
        }
    }

    /// 主线程的工具注册表：基础工具、Skill、MCP 工具（ext.md §1）。
    /// 单次的 `-p`：内置 `/loop` 的 Monitor 超时随它（tools-more.md §17.1）。
    pub fn set_print_session(&self, print: bool) {
        self.skills.set_print_session(print);
        self.mcp.set_print_session(print);
    }

    pub fn registry(&self) -> Arc<dyn ToolRegistry> {
        self.registry.clone()
    }

    /// skill 清单与 MCP instructions 的来源。
    pub fn listings(&self) -> Arc<dyn ListingSource> {
        Arc::new(ExtListings {
            skills: self.skills.clone(),
            mcp: self.mcp.clone(),
        })
    }

    pub fn skills(&self) -> &Arc<SkillCatalog> {
        &self.skills
    }

    /// 用户调用 skill 的展开与斜杠补全（ext.md §2.7）。
    pub fn user_commands(&self) -> Arc<dyn rust_cli_kernel_api::user_command::UserCommands> {
        self.commands.clone()
    }

    pub fn mcp(&self) -> &Arc<McpManager> {
        &self.mcp
    }

    /// 加载插件、开始连接 MCP 服务器（每个服务器一个连接任务，不等待）。返回加载好的插件：其中的
    /// agent 文件与 hooks 由 runtime 交给 agents 与 hooks（ext.md §4）。
    pub async fn start(&self) -> Plugins {
        let Some(workspace) = self.config.workspace.clone() else {
            return Plugins::default();
        };
        let loaded = plugins::load_all(
            self.files.as_ref(),
            &workspace,
            &self.config.plugin_dirs,
            &self.config.plugins,
        )
        .await
        .plugins;
        self.skills.set_plugin_roots(loaded.skill_roots());
        let mut servers = self.config.mcp.servers.clone();
        servers.extend(loaded.mcp_servers());
        self.mcp.start(workspace, servers);
        self.mcp.register_disabled(self.config.mcp_disabled.clone());
        loaded
    }

    /// `-p` 在第一个请求前的等待（ext.md §3.3.1）：alwaysLoad 的服务器与第一条命令前的 prewait，到期就开跑。
    pub async fn wait_for_print(&self) {
        self.mcp.wait_for_print().await;
    }
}

/// 基础工具在前，Skill 其次（有连接中的服务器时再加 WaitForMcpServers），MCP 工具最后（MCP 工具是连接任务发布的
/// 快照）。发给模型前按名字排序（tools-more.md §3.3）。
struct ExtRegistry {
    base: Arc<dyn ToolRegistry>,
    skill: Arc<dyn Tool>,
    /// WaitForMcpServers（ext.md §3.8）：有 Pending 的服务器、或这个会话带过它（declared）时在工具表里。启用 ToolSearch
    /// 时由主循环去掉（tools-more.md §3.3 第 4 条）。
    wait: Arc<dyn Tool>,
    mcp: Arc<McpManager>,
}

impl ExtRegistry {
    fn assemble(&self, wait: bool, mcp: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base.tools();
        tools.push(self.skill.clone());
        if wait {
            tools.push(self.wait.clone());
        }
        tools.extend(mcp);
        tools
    }
}

impl ToolRegistry for ExtRegistry {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.assemble(self.mcp.has_pending(), self.mcp.tools())
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if name == self.skill.name() {
            return Some(self.skill.clone());
        }
        // 快照里带过它、之后服务器连上了，这次调用照常执行（按请求时的工具表找）：有服务器就找得到。
        if name == WAIT_FOR_MCP_SERVERS && self.mcp.has_servers() {
            return Some(self.wait.clone());
        }
        self.base.find(name).or_else(|| self.mcp.find(name))
    }

    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.base.withheld(name)
    }

    /// MCP 工具、连接中的服务器与 instructions 在同一把锁里取（query.md §1.3，修复线 F36）。
    fn snapshot(&self, session: &SessionId) -> ToolSnapshot {
        let (tools, view) = self.mcp.snapshot();
        let wait = !view.pending().is_empty() || self.mcp.declared(session);
        ToolSnapshot {
            tools: self.assemble(wait, tools),
            mcp: Some(view),
        }
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        Some(self.mcp.clone())
    }
}

struct ExtListings {
    skills: Arc<SkillCatalog>,
    mcp: Arc<McpManager>,
}

#[async_trait]
impl ListingSource for ExtListings {
    async fn listings(&self, request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        self.listings_for(request, &[]).await
    }

    async fn listings_for(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Result<Listings, OrchestrationError> {
        let skills = self
            .skills
            .listing(&request.workspace, history, &request.selection.model)
            .await;
        Ok(Listings {
            skills,
            ..Listings::default()
        })
    }

    /// 调用过的 skill（compact.md §3.4）。
    async fn invoked_skills(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Result<Vec<rust_cli_kernel_api::listing::InvokedSkill>, OrchestrationError> {
        Ok(self.skills.invoked(&request.workspace, history).await)
    }

    /// 输入中间的 skill 提及（ext.md §2.8）。
    async fn skill_mentions(
        &self,
        request: &ContextRequest,
        input: &str,
    ) -> Result<Vec<String>, OrchestrationError> {
        Ok(self.skills.mentions(&request.workspace, input).await)
    }

    /// 有工具快照时用快照里的（与工具同一时点，query.md §1.3）；否则取现在的。
    async fn mcp_instructions(
        &self,
        request: &ContextRequest,
    ) -> Result<Vec<ListingEntry>, OrchestrationError> {
        Ok(match &request.mcp {
            Some(view) => view.instructions.clone(),
            None => self.mcp.instructions(),
        })
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
