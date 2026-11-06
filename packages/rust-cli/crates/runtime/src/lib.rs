//! 内核组装（docs/specs/rust-cli/README.md §3）。
//!
//! 把各能力 crate 的实现接到端口上，得到实现 [`Kernel`] 的 [`LocalKernel`]；不含任何适配器，
//! 适配器（模型、执行环境、存储）由入口 crate 构造后经 [`Ports`] 传入。时间与 ID 也经 [`Ports`] 注入：本机实现是
//! node-store 的 `SystemClock` / `NodeIds`（F7 从这里移走，内核 crate 不碰系统时钟与随机源，README §4）。
//!
//! 每个会话一个 actor，运行在自己的子任务作用域里；内核释放时根作用域中止全部会话任务。

mod agents;
mod assemble;
mod catalog;
mod config;
mod directory;
mod export;
mod ext;
mod kernel;
mod late;
mod listings;
mod permissions;
mod projects;
mod table;
mod usage;
mod workspace_sessions;

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_agents::{AgentCatalog, MemoryTaskLists};
use rust_cli_base::TaskScope;
use rust_cli_ext::Ext;
use rust_cli_hooks::HookEngine;
use rust_cli_kernel_api::agents::{AgentSignals, TaskListStore};
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::default_model::DefaultModel;
use rust_cli_kernel_api::env::Operations;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::{OwnerEpoch, SessionId, TraceId};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::orchestration::{Compactor, QueryRunner};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    CreateSession, Kernel, KernelError, SessionHandle, StartSource,
};
use rust_cli_kernel_api::store::{
    ArtifactStore, Fact, FactBatch, SessionFilter, SessionKind, SessionMeta, SessionSummary, Store,
};
use rust_cli_kernel_api::tool::ReadFileStates;
use rust_cli_policy::PolicyGate;
use rust_cli_query::{QueryLoop, QueryParts};
use rust_cli_session::{Opened, Session, SessionState};
use table::{Closer, OpenSessions};

use crate::assemble::SurfaceTraits;

pub use ext::{ExtConfig, Extensions};
pub use rust_cli_compact::{AutoCompactAt, CompactConfig, Thresholds as CompactThresholds};
pub use rust_cli_context::{Attribution, ContextConfig, OutputStyle, Surface};
// hooks 快照（`LocalKernel::replace_hooks` 的参数）：前端的测试组装它不必依赖 hooks crate（修复线 F37 的 TUI 测试）。
pub use rust_cli_hooks::{HookSource, HooksConfig, HooksSettings};
pub use rust_cli_query::{RequestLimits, ThinkingTokens};
pub use rust_cli_session::{PeerDeps, TitleModel};
pub use rust_cli_tools::BashTimeouts;

/// 内核需要的全部端口。
#[derive(Clone)]
pub struct Ports {
    pub model: Arc<dyn ModelPort>,
    /// 执行环境的注册表（R0 只有本机，remote-exec.md §15.8.1）：内核建好时取本机的机器与本机服务分给各能力。
    pub environments: Arc<dyn EnvironmentRegistry>,
    /// 会话存储的三个端口（生命周期、本机设置、只读查询，审查 K7）：一个适配器实现全部；runtime 按能力分给会话、子代理、
    /// 判决链的读回与前端的查询。
    pub store: Arc<dyn Store>,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
}

/// 冷加载的会话没有记录模型或模式时使用的默认值。
#[derive(Debug, Clone)]
pub struct Defaults {
    /// 新会话的缺省模型。TUI 里它可能在内核建好之后才定下（工作线 F140，performance.md §12.3）：建会话与开会话先等它。
    pub selection: DefaultModel,
    pub mode: PermissionMode,
    /// 每次提问最多几轮（`--max-turns`）；None 不限制。
    pub max_turns: Option<u32>,
    /// 启动参数点名了 Glob 或 Grep：有 Bash 时也暴露这两个工具
    /// （tools-files.md §1.1）。
    pub search_tools: bool,
    /// 任务工具的显式开关（入口读 `ZCODE_ENABLE_TODO_TOOLS` / `ZCODE_ENABLE_TASKS`，
    /// 修复线 F42）：Some(true) 打开，Some(false) 关闭，None 按默认模型（agents.md §7）。
    pub task_tools: Option<bool>,
    /// 入口按环境变量给的上限（cli-args.md §9，修复线 F42）。
    pub limits: Limits,
}

/// 入口按环境变量给的开关（cli-args.md §9.1–§9.3，修复线 F42）：主循环的输出上限与 thinking、Bash 的超时；工作线 F89 加
/// `ENABLE_TOOL_SEARCH`（`request.tool_search`：ToolSearch 工具启用与否、主循环与 `/context` 每一步开不开，tools-more.md
/// §3.2.1）。缺省是都没设。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// 主循环、子代理与压缩摘要的 `max_tokens` 与 thinking（query.md §1.4），ToolSearch 的设置（工作线 F89）。
    pub request: RequestLimits,
    /// Bash 工具（模型的调用与 bash 模式）的缺省与最大超时。
    pub bash: BashTimeouts,
    /// `ZCODE_FORK_SUBAGENT`（对照实现有同义的开关，修复线 L-A，agents.md §8.1.1）：Some(true) 打开 fork
    /// 子代理（`-p` 也开），Some(false) 关掉，None 按形态（交互会话开，`-p` 关）。
    pub fork_subagent: Option<bool>,
}

pub struct LocalKernel {
    /// 端口；会话的主 env 与 controller 的服务都从 `environments` 取（[`LocalKernel::machine`]；工作线 F131：以前另存一份）。
    ports: Ports,
    defaults: Defaults,
    runner: Arc<dyn QueryRunner>,
    /// 子代理的信号来源（后台子代理的通知、任务清单变化），交给每个会话 actor。
    agents: Arc<dyn AgentSignals>,
    /// 定时任务的唤醒与到期任务（tools-more.md §10.4）；`-p` 不触发，是 `NoCron`。
    cron: Arc<dyn rust_cli_kernel_api::cron::CronSignals>,
    compactor: Arc<dyn Compactor>,
    gate: Arc<PolicyGate>,
    /// skill、MCP 与插件（docs/specs/rust-cli/ext.md）。
    ext: Arc<Ext>,
    /// 会话侧 hooks 与回退事务的执行环境实例（L10，docs/specs/rust-cli/session-lifecycle.md）。
    hooks: Arc<HookEngine>,
    /// agent 类型的来源（context 的清单与 Agent 工具共用）；插件 agent 在插件加载后设置。
    agent_catalog: Arc<AgentCatalog>,
    /// 会话标题的生成：入口在 TUI 下经 `with_titles` 打开（`-p` 不生成，docs/specs/rust-cli/titles.md §2）。
    titles: Option<TitleModel>,
    /// `/context` 的分析（context-usage.md），与主循环共用同一批实例。
    usage: Arc<usage::UsageAnalyzer>,
    /// 用户输入的 `@` 提及与 bash 模式（tui-input.md §1.1、§2.2）：基础注册表里的 Read 与 Bash。
    inputs: Arc<dyn rust_cli_kernel_api::user_input::UserInputs>,
    /// 操作上下文的工厂（执行环境的实例 + 注入的 Ids，F134）：会话 actor 与 `/export` 用它。
    operations: Operations,
    scope: TaskScope,
    /// 会话表（session-lifecycle.md §3.3.1）：每个会话归谁的唯一来源。
    sessions: OpenSessions,
    /// 关闭时标成关闭中、释放按会话的表（[`Closer`]）。
    closer: Arc<Closer>,
    /// 本机跨会话通信（cross-session.md §2.4）：每个会话 actor 共用这一份端口。
    peers: rust_cli_session::PeerDeps,
    /// 父会话关闭之后旧会话的询问入口（进程级，agents.md §4.1.2，工作线 F72）：每个会话 actor 共用这一份。
    detached: Arc<rust_cli_session::DetachedAsks>,
}

impl LocalKernel {
    /// 上下文按 TUI 形态组装；`-p` 用 [`LocalKernel::with_context`] 传入 `Surface::Print`。
    pub fn new(ports: Ports, defaults: Defaults) -> Self {
        Self::with_context(ports, defaults, ContextConfig::new(Surface::Interactive))
    }

    /// 任务清单放在内存里（测试、嵌入）；入口 crate 用 [`LocalKernel::with_task_lists`] 接本地存储。
    pub fn with_context(ports: Ports, defaults: Defaults, context: ContextConfig) -> Self {
        Self::with_options(
            ports,
            defaults,
            context,
            CompactConfig::default(),
            Arc::new(MemoryTaskLists::default()),
        )
    }

    /// 同 [`LocalKernel::with_context`]，另给扩展（前端的集成测试接跨会话的端口，cross-session.md §6）。
    pub fn with_context_and_extensions(
        ports: Ports,
        defaults: Defaults,
        context: ContextConfig,
        extensions: Extensions,
    ) -> Self {
        Self::with_extensions(
            ports,
            defaults,
            context,
            CompactConfig::default(),
            Arc::new(MemoryTaskLists::default()),
            extensions,
        )
    }

    /// 另给压缩配置（自动压缩开关、阈值覆盖、Provider 目录；docs/specs/rust-cli/compact.md §2）与
    /// 任务清单（Task v2）的存储（docs/specs/rust-cli/agents.md §7）。
    pub fn with_options(
        ports: Ports,
        defaults: Defaults,
        context: ContextConfig,
        compact: CompactConfig,
        tasks: Arc<dyn TaskListStore>,
    ) -> Self {
        Self::with_extensions(ports, defaults, context, compact, tasks, Extensions::none())
    }

    /// 另给 MCP 传输与扩展配置（docs/specs/rust-cli/ext.md §1）。内核建好后由入口调用
    /// [`LocalKernel::start_extensions`] 开始连接 MCP 服务器。
    pub fn with_extensions(
        ports: Ports,
        defaults: Defaults,
        context: ContextConfig,
        compact: CompactConfig,
        tasks: Arc<dyn TaskListStore>,
        extensions: Extensions,
    ) -> Self {
        let scope = TaskScope::root("rust-cli", |panic| {
            tracing::error!(scope = %panic.scope, task = panic.task, message = %panic.message, "task panicked");
        });
        // 本地执行环境的实例随进程变化（spec §6 规则 3）；hook 命令与工具共用同一个实例。ID 经注入的 Ids
        // （F7，审查 R2-11：以前直接调用 `Uuid::new_v4`）。
        let operations = Operations::new(ports.ids.generation(), ports.ids.clone());
        // 各能力拿会话主 env 的端口：规则文件、记忆、插件、skills 与 git 都跟着主 env（deployment.md §8、§10）。
        let machine = ports.environments.primary_machine();
        let hooks = assemble::hooks(&ports, &operations);
        let traits = SurfaceTraits::new(
            matches!(context.surface, Surface::Print),
            extensions.streaming_input,
        );
        let late = Arc::new(late::LateCompactor::default());
        let gate = assemble::gate(&ports);
        let base = assemble::base_tools(&ports, &defaults, &extensions, (traits, &gate));
        let ext = assemble::ext(
            &ports,
            (&gate, scope.spawner()),
            base.registry.clone(),
            (extensions.config.clone(), traits),
        );
        // agent 清单（context）与 Agent 工具共用一个目录：插件 agent 设置一次，两处同时看到（L12）。
        let agent_catalog = Arc::new(AgentCatalog::new(
            machine.info().clone(),
            machine.files().clone(),
        ));
        let assembler = assemble::context(&ports, context, (&agent_catalog, &base, &ext));
        let sessions = OpenSessions::default();
        let closer = Arc::new(Closer {
            sessions: Arc::downgrade(&sessions),
            scoped: table::SessionScoped {
                context: assembler.clone(),
                read_files: base.read_files.clone(),
                worktrees: base.worktrees.clone(),
                gate: gate.clone(),
                mcp: ext.mcp().clone(),
            },
        });
        let parts = assemble::query_parts(
            &ports,
            (&assembler, &ext, &gate, &hooks),
            (&late, operations),
            (&defaults, traits),
        );
        let agents = assemble::agents(
            &ports,
            (&parts, tasks, &agent_catalog),
            (&late, scope.spawner(), &extensions.peers),
            (&defaults, traits),
        );
        // 两处晚绑定（late.rs）：ToolSearch 的目录看 agents 的注册表；任务提醒读 agents 的清单（agents 要用上下文）。
        let tools = agents.registry();
        base.catalog.bind(&tools);
        assembler.bind_tasks(agents.task_reader());
        let usage = Arc::new(usage::UsageAnalyzer {
            model: ports.model.clone(),
            context: assembler.clone(),
            tools: tools.clone(),
            agents: agent_catalog.clone(),
            skills: ext.skills().clone(),
            compact: compact.clone(),
            tool_search: defaults.limits.request.tool_search,
        });
        let file_states: Arc<dyn ReadFileStates> = base.read_files.clone();
        let compactor = assemble::compactor(
            &ports,
            (&parts, &tools),
            (file_states, assembler, &hooks),
            compact,
        );
        // 第三处晚绑定：主循环与子循环拿到的压缩器句柄。
        late.bind(compactor.clone());
        let operations = parts.operations.clone();
        let parts = QueryParts { tools, ..parts };
        let runner = agents.track(Arc::new(QueryLoop::new(parts)));
        let detached = Arc::new(rust_cli_session::DetachedAsks::new(
            ports.store.clone(),
            ports.ids.clone(),
        ));
        Self {
            ports,
            defaults,
            runner,
            agents: agents.signals(),
            cron: if traits.interactive {
                base.cron.signals()
            } else {
                Arc::new(rust_cli_kernel_api::cron::NoCron)
            },
            compactor,
            gate,
            ext,
            hooks,
            agent_catalog,
            titles: None,
            usage,
            inputs: base.inputs,
            operations,
            scope,
            sessions,
            closer,
            peers: extensions.peers,
            detached,
        }
    }

    /// 加载插件、开始连接 MCP 服务器（不等待连接完成，ext.md §3.3）。插件的 agent 文件交给 agent
    /// 目录、hooks 登记到 hooks 引擎（ext.md §4，L12）；返回时两者都已生效，之后才开会话。
    pub async fn start_extensions(&self) {
        let plugins = self.ext.start().await;
        let (agents, hooks) = ext::plugin_parts(&plugins);
        self.agent_catalog.set_plugins(agents);
        self.hooks.register_plugins(hooks);
    }

    /// `-p` 在第一个请求前的 MCP 等待（ext.md §3.3.1，修复线 F36）：alwaysLoad 的服务器与 prewait，到期就开跑，
    /// 没连上的服务器在后台继续连。
    pub async fn wait_for_extensions(&self) {
        self.ext.wait_for_print().await;
    }

    pub fn ports(&self) -> &Ports {
        &self.ports
    }

    /// 会话的主 env（注册表的 `primary_machine()`）：会话、`/cd`、`/diff`、导出、项目清单、`@` 清单都在它上面。
    fn machine(&self) -> MachineEnv {
        self.ports.environments.primary_machine()
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "metering_tests.rs"]
mod metering_tests;

#[cfg(test)]
#[path = "closing_tests.rs"]
mod closing_tests;
