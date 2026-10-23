//! 内核组装（docs/specs/rust-cli/README.md §3）。
//!
//! 把各能力 crate 的实现接到端口上，得到实现 [`Kernel`] 的 [`LocalKernel`]；不含任何适配器，
//! 适配器（模型、执行环境、存储）由入口 crate 构造后经 [`Ports`] 传入。时间与 ID 也经 [`Ports`] 注入：本机实现是
//! node-store 的 `SystemClock` / `NodeIds`（F7 从这里移走，内核 crate 不碰系统时钟与随机源，README §4）。
//!
//! 每个会话一个 actor，运行在自己的子任务作用域里；内核释放时根作用域中止全部会话任务。

mod agents;
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
use rust_cli_agents::{AgentCatalog, Agents, AgentsConfig, AgentsPorts, MemoryTaskLists};
use rust_cli_base::TaskScope;
use rust_cli_compact::{CompactPorts, Compaction, DenyTools, Disabled};
use rust_cli_context::{ContextPorts, PromptAssembler};
use rust_cli_ext::{Ext, ExtPorts};
use rust_cli_hooks::{HookEngine, HookPorts};
use rust_cli_kernel_api::agents::{AgentSignals, TaskListStore};
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::env::Environment;
use rust_cli_kernel_api::ids::{Generation, OwnerEpoch, SessionId, TraceId};
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::orchestration::{Compactor, QueryRunner};
use rust_cli_kernel_api::peers::ModeClass;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    CreateSession, Kernel, KernelError, SessionHandle, StartSource,
};
use rust_cli_kernel_api::store::{
    ArtifactStore, Fact, FactBatch, SessionFilter, SessionKind, SessionMeta, SessionStore,
    SessionSummary, StoreError,
};
use rust_cli_kernel_api::tool::ReadFileStates;
use rust_cli_policy::PolicyGate;
use rust_cli_query::{QueryLoop, QueryParts};
use rust_cli_session::{Opened, Session, SessionState};
use rust_cli_tools::Pipeline;
use table::{Closer, OpenSessions};

pub use ext::{ExtConfig, Extensions};
pub use permissions::{format_rule as format_permission_rule, parse_rule as parse_permission_rule};
pub use rust_cli_compact::{CompactConfig, Thresholds as CompactThresholds};
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
    pub env: Arc<dyn Environment>,
    pub store: Arc<dyn SessionStore>,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
}

/// 冷加载的会话没有记录模型或模式时使用的默认值。
#[derive(Debug, Clone)]
pub struct Defaults {
    pub selection: ModelSelection,
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

/// 跑分用的开关（cli-args.md §9.1–§9.3，修复线 F42）：主循环的输出上限与 thinking、Bash 的超时。缺省是都没设。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// 主循环、子代理与压缩摘要的 `max_tokens` 与 thinking（query.md §1.4）。
    pub request: RequestLimits,
    /// Bash 工具（模型的调用与 bash 模式）的缺省与最大超时。
    pub bash: BashTimeouts,
}

pub struct LocalKernel {
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
    generation: Generation,
    scope: TaskScope,
    /// 会话表（session-lifecycle.md §3.3.1）：每个会话归谁的唯一来源。
    sessions: OpenSessions,
    /// 关闭时标成关闭中、释放按会话的表（[`Closer`]）。
    closer: Arc<Closer>,
    /// 本机跨会话通信（cross-session.md §2.4）：每个会话 actor 共用这一份端口。
    peers: rust_cli_session::PeerDeps,
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
        let generation = ports.ids.generation();
        // hooks 的配置快照由入口层组装后经 `HookEngine::replace` 交进来（hooks.md §2.2）。
        let hooks = Arc::new(HookEngine::new(HookPorts {
            env: ports.env.clone(),
            ids: ports.ids.clone(),
            model: Some(ports.model.clone()),
            http: None,
            generation: generation.clone(),
            // 大输出与工具结果落在同一个存储（hooks.md §4.5，修复线 F15）。
            artifacts: ports.artifacts.clone(),
        }));
        let non_interactive = matches!(context.surface, Surface::Print);
        let late = Arc::new(late::LateCompactor::default());
        // 一个判决链实例：工具注册（Edit / Write 的先读规则）与执行管线共用，会话内“总是允许”的
        // 规则因此对两边都生效。规则快照由入口层接入配置后替换（shell-permissions.md §5）。
        let gate = Arc::new(PolicyGate::default());
        // ToolSearch 与延迟工具清单看同一个主线程注册表，建好注册表后绑定（tools-more.md §3.4）。
        let catalog = Arc::new(rust_cli_tools::ToolCatalog::default());
        // 会话内定时任务（L11b）：任务表在这里，触发由会话 actor 在空闲时取走。
        let cron =
            rust_cli_tools::CronBoard::new(ports.clock.clone(), extensions.cron_store.clone());
        // 基础工具：文件与搜索（L3）、Bash（L4）、交互与 Web 工具（L11a）；Skill 与 MCP 工具由 ext
        // 加在后面（L9），Agent 等由 agents 再加（L7）。
        // Read 整读 PDF 前按会话当前模型的生效配置查 `supports_pdf`（tools-files.md §1.5，修复线 F24）。
        let pdf_model = rust_cli_tools::SideModel {
            model: ports.model.clone(),
            selection: defaults.selection.clone(),
        };
        let builtin = rust_cli_tools::builtin_with(gate.clone(), Some(pdf_model));
        // 压缩时清空的 readFileState 与文件工具是同一份（compact.md §3.4）；关闭会话时也释放它（[`Closer`]）。
        let read_files: Arc<rust_cli_tools::ReadFileState> =
            builtin.file_state().cloned().unwrap_or_default();
        let file_states: Arc<dyn ReadFileStates> = read_files.clone();
        let worktrees = Arc::new(rust_cli_tools::WorktreeBoard::default());
        let base: Arc<dyn rust_cli_kernel_api::orchestration::ToolRegistry> = Arc::new(
            builtin
                .with_search_opt_in(defaults.search_tools)
                // 后台命令的期限只在 `-p`（shell-permissions.md §1.1，修复线 F9）；启动时是 bypass 类别时
                // 描述去掉“不要用 cat”一行（§4.9，修复线 F43），整个进程不变。
                .with_leading(Arc::new({
                    // `BASH_*_TIMEOUT_MS` 的缺省与上限（cli-args.md §9.3，修复线 F42）；进程启动时是 bypass 类别时
                    // 去掉“不要用 cat”一行（修复线 F43，shell-permissions.md §4.9）。
                    let bash = rust_cli_tools::BashTool::new()
                        .with_timeouts(defaults.limits.bash)
                        .with_background_deadline(non_interactive)
                        .with_bash_first_trimmed(ModeClass::of(defaults.mode) == ModeClass::Bypass);
                    // stream-json 输入：后台命令的缺省期限 max(1800000, 前台缺省)（headless.md §6.1）。
                    if extensions.streaming_input {
                        bash.with_streaming_input()
                    } else {
                        bash
                    }
                }))
                .with_tools(rust_cli_tools::interaction_tools(
                    &catalog,
                    !non_interactive,
                ))
                .with_tools(rust_cli_tools::web_tools(
                    rust_cli_tools::SideModel {
                        model: ports.model.clone(),
                        selection: defaults.selection.clone(),
                    },
                    ports.clock.clone(),
                ))
                .with_tools(rust_cli_tools::cron_tools(&cron))
                // `/loop` 动态模式的自定节奏（L17，tools-more.md §17.2）：默认注册、不延迟。
                .with_tools(vec![Arc::new(rust_cli_tools::ScheduleWakeupTool::new(
                    &cron,
                ))])
                .with_tools(vec![Arc::new(rust_cli_tools::ReportFindingsTool::new())])
                .with_tools(rust_cli_tools::worktree_tools(&worktrees)),
        );
        // 用户输入的提及与 bash 模式用基础注册表里的 Read 与 Bash（与模型的调用共用 readFileState 与 shell 状态）。
        let inputs: Arc<dyn rust_cli_kernel_api::user_input::UserInputs> = Arc::new(
            rust_cli_tools::UserInputTools::new(base.as_ref(), gate.clone())
                .with_bash_timeouts(defaults.limits.bash),
        );
        let ext = Arc::new(Ext::new(
            ExtPorts {
                env: ports.env.clone(),
                ids: ports.ids.clone(),
                gate: gate.clone(),
                mcp: extensions.mcp,
                spawner: scope.spawner(),
                clock: ports.clock.clone(),
            },
            base,
            extensions.config,
        ));
        ext.set_print_session(non_interactive);
        // agent 清单（context）与 Agent 工具共用一个目录：插件 agent 设置一次，两处同时看到（L12）。
        let agent_catalog = Arc::new(AgentCatalog::new(ports.env.clone()));
        let context = PromptAssembler::new(
            ContextPorts {
                env: ports.env.clone(),
                clock: ports.clock.clone(),
                ids: ports.ids.clone(),
                // agent 清单由 agents 给出（L7），延迟工具由 ToolCatalog 给出（L11a），skill 清单与 MCP
                // instructions 由 ext 给出（L9）。
                listings: Arc::new(listings::CombinedListings(vec![
                    agent_catalog.clone(),
                    catalog.clone(),
                    ext.listings(),
                ])),
            },
            context,
        );
        let context = Arc::new(context);
        let assembler = context.clone();
        let sessions = OpenSessions::default();
        let closer = Arc::new(Closer {
            sessions: Arc::downgrade(&sessions),
            context: context.clone(),
            read_files,
            worktrees,
            gate: gate.clone(),
            mcp: ext.mcp().clone(),
        });
        let parts = QueryParts {
            model: ports.model.clone(),
            context: context.clone(),
            // 整个工具的 deny 规则从清单里去掉（tools-files.md §1.1.1，工作线 F69）：主线程、子代理、延迟清单与
            // 压缩摘要都从这一层往外取，规则用判决链当前的快照。
            tools: Arc::new(rust_cli_tools::PolicyFiltered::new(
                ext.registry(),
                gate.clone(),
            )),
            executor: Arc::new(Pipeline::new(
                Arc::new(permissions::SharedRules {
                    gate: gate.clone(),
                    store: ports.store.clone(),
                }),
                hooks.clone(),
            )),
            // 先交出“稍后接上”的句柄：子循环（agents）与主循环共用，压缩器建好后再设置（late.rs）。
            compactor: late.clone(),
            hooks: hooks.clone(),
            ids: ports.ids.clone(),
            clock: ports.clock.clone(),
            env: ports.env.clone(),
            artifacts: ports.artifacts.clone(),
            generation,
            non_interactive,
            limits: defaults.limits.request,
        };
        // 子代理与任务清单（工作线 L7）：主线程的工具注册表换成 agents 的（基础工具 + Agent 等），
        // 主循环包一层运行登记；子循环由 `LoopFactory` 用同一套能力组装。
        let agents = Agents::new(
            AgentsPorts {
                store: ports.store.clone(),
                ids: ports.ids.clone(),
                clock: ports.clock.clone(),
                env: ports.env.clone(),
                tasks,
                queries: Arc::new(agents::LoopFactory {
                    parts: parts.clone(),
                }),
                context: context.clone(),
                catalog: agent_catalog.clone(),
                spawner: scope.spawner(),
                shell_notice: agents::shell_notice,
                peers: extensions.peers.port.clone(),
            },
            parts.tools.clone(),
            AgentsConfig {
                task_tools: defaults.task_tools.unwrap_or_else(|| {
                    rust_cli_agents::task_tools_enabled(&defaults.selection.model, false)
                }),
                // 只有交互模式停放还有后台子代理的子代理（agents.md §5.1）。
                keep_alive: !non_interactive,
                // 修复线 F67：交互界面里后台子代理也能询问（agents.md §4.1.1）。
                background_prompts: !non_interactive,
            },
        );
        let tools = agents.registry();
        catalog.bind(&tools);
        let usage = Arc::new(usage::UsageAnalyzer {
            model: ports.model.clone(),
            context: assembler,
            tools: tools.clone(),
            agents: agent_catalog.clone(),
            skills: ext.skills().clone(),
            compact: compact.clone(),
            cache: usage::Cache::default(),
        });
        // 摘要请求的子循环：同一个上下文与主线程的工具清单（共享缓存前缀），工具一律拒绝，自己不再压缩
        // （compact.md §3.2）。
        let summary = QueryLoop::new(QueryParts {
            executor: Arc::new(DenyTools),
            tools: tools.clone(),
            compactor: Arc::new(Disabled),
            ..parts.clone()
        });
        let compactor: Arc<dyn Compactor> = Arc::new(Compaction::new(
            CompactPorts {
                runner: Arc::new(summary),
                tools: tools.clone(),
                files: file_states,
                context,
                hooks: hooks.clone(),
                ids: ports.ids.clone(),
                env: ports.env.clone(),
                artifacts: ports.artifacts.clone(),
                generation: parts.generation.clone(),
            },
            compact,
        ));
        late.set(compactor.clone());
        let generation = parts.generation.clone();
        let parts = QueryParts { tools, ..parts };
        let runner = agents.track(Arc::new(QueryLoop::new(parts)));
        Self {
            ports,
            defaults,
            runner,
            agents: agents.signals(),
            cron: if non_interactive {
                Arc::new(rust_cli_kernel_api::cron::NoCron)
            } else {
                cron.signals()
            },
            compactor,
            gate,
            ext,
            hooks,
            agent_catalog,
            titles: None,
            usage,
            inputs,
            generation,
            scope,
            sessions,
            closer,
            peers: extensions.peers,
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
