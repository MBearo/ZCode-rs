//! 内核组装的各步（`LocalKernel::with_extensions` 调它们，docs/specs/rust-cli/architecture.md §2）。
//!
//! 工作线 F131：以前 `with_extensions` 一个函数 311 行，10 天里被 16 个提交在中间插行；现在每一步一个函数、返回有名字的
//! 结构，会话形态推出的开关一次算进 [`SurfaceTraits`]。组装的顺序与产物不变（三处晚绑定见 `late.rs`）。

use std::sync::Arc;

use rust_cli_agents::{AgentCatalog, Agents, AgentsConfig, AgentsPorts, TaskTools};
use rust_cli_base::Spawner;
use rust_cli_compact::{CompactConfig, CompactPorts, Compaction, DenyTools, Disabled};
use rust_cli_context::{ContextConfig, ContextPorts, PromptAssembler};
use rust_cli_ext::{Ext, ExtPorts};
use rust_cli_hooks::{HookEngine, HookPorts};
use rust_cli_kernel_api::agents::TaskListStore;
use rust_cli_kernel_api::env::Operations;
use rust_cli_kernel_api::orchestration::{Compactor, ToolRegistry};
use rust_cli_kernel_api::peers::ModeClass;
use rust_cli_kernel_api::tool::ReadFileStates;
use rust_cli_kernel_api::user_input::UserInputs;
use rust_cli_policy::PolicyGate;
use rust_cli_query::{QueryLoop, QueryParts};
use rust_cli_tools::{
    BashSettings, BashTool, CronBoard, Pipeline, ReadFileState, ToolCatalog, WorktreeBoard,
};

use crate::late::LateCompactor;
use crate::{Defaults, Extensions, Ports, agents, listings, permissions};

/// 由会话形态推出的开关：组装时算一次，各能力只看开关，不看形态（app-protocol.md §9.3）。`-p` 与 stream-json 是非交互，
/// 其余是交互（TUI、App）。以前这些分支散在 `with_extensions` 的 9 处。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceTraits {
    /// 非交互：主线程的断流续写、Bash 后台命令的期限（shell-permissions.md §1.1）、MCP 的 print 会话规则（ext.md §3.3.3）。
    pub(crate) non_interactive: bool,
    /// 交互工具（AskUserQuestion 等）能问用户；子代理跑完、还有后台子代理时停放等通知（agents.md §5.1）；后台子代理
    /// 能询问（agents.md §4.1.1）；fork 子代理缺省开（环境变量可以改，agents.md §8.1.1）；定时任务会触发（tools-more.md §10.4）。
    pub(crate) interactive: bool,
    /// Monitor 的上限按会话（30 分钟）算：交互会话或 stream-json 输入；单次的 `-p` 是 10 分钟（tools-more.md §19）。
    pub(crate) monitor_session_cap: bool,
}

impl SurfaceTraits {
    pub(crate) fn new(non_interactive: bool, streaming_input: bool) -> Self {
        Self {
            non_interactive,
            interactive: !non_interactive,
            monitor_session_cap: !non_interactive || streaming_input,
        }
    }
}

/// 会话侧 hooks 与回退事务的执行环境实例：hook 命令在主 env 上跑，与工具共用同一个实例（`operations` 的 generation）。配置快照由入口层
/// 组装后经 `HookEngine::replace` 交进来（hooks.md §2.2）。
pub(crate) fn hooks(ports: &Ports, operations: &Operations) -> Arc<HookEngine> {
    let machine = ports.environments.primary_machine();
    Arc::new(HookEngine::new(HookPorts {
        commands: machine.commands().clone(),
        shell: machine.shell().clone(),
        ids: ports.ids.clone(),
        model: Some(ports.model.clone()),
        http: None,
        operations: operations.clone(),
        // 大输出与工具结果落在同一个存储（hooks.md §4.5，修复线 F15）。
        artifacts: ports.artifacts.clone(),
    }))
}

/// 一个判决链实例：工具注册（Edit / Write 的先读规则）与执行管线共用，会话内“总是允许”的规则因此对两边都生效。规则
/// 快照由入口层接入配置后替换；“总是允许”落库后从会话存储读回 LocalSettings 层（shell-permissions.md §5）。
pub(crate) fn gate(ports: &Ports) -> Arc<PolicyGate> {
    Arc::new(
        PolicyGate::default()
            .with_local_rules(Arc::new(permissions::StoredRules(ports.store.clone()))),
    )
}

/// 基础工具与它们共用的状态。
pub(crate) struct BaseTools {
    /// 文件与搜索（L3）、Bash（L4）、交互与 Web 工具（L11a）等；Skill 与 MCP 工具由 ext 加在后面（L9），Agent 等由
    /// agents 再加（L7）。
    pub(crate) registry: Arc<dyn ToolRegistry>,
    /// ToolSearch 与延迟工具清单看的主线程注册表，建好之后绑定（tools-more.md §3.4）。
    pub(crate) catalog: Arc<ToolCatalog>,
    /// 会话内定时任务（L11b）：任务表在这里，触发由会话 actor 在空闲时取走。
    pub(crate) cron: CronBoard,
    /// 文件工具、Bash 与压缩共用的 readFileState（compact.md §3.4）；关闭会话时也释放它。
    pub(crate) read_files: Arc<ReadFileState>,
    pub(crate) worktrees: Arc<WorktreeBoard>,
    /// 用户输入的 `@` 提及与 bash 模式（tui-input.md §1.1、§2.2）：基础注册表里的 Read 与 Bash。
    pub(crate) inputs: Arc<dyn UserInputs>,
}

/// Bash 的进程级设置（shell-permissions.md §1.1，F132 的 T12）：模型的 Bash 与 bash 模式共用这一份。
/// `BASH_*_TIMEOUT_MS` 的缺省与上限（cli-args.md §9.3，修复线 F42），stream-json 输入时后台命令的缺省期限是
/// max(1800000, 前台缺省)（headless.md §6.1）；后台命令的期限只在 `-p`（修复线 F9）；进程启动时是 bypass 类别时描述
/// 去掉“不要用 cat”一行（§4.9，修复线 F43）；Monitor 开着时描述多一句、前台长 `sleep` 拦下（tools-more.md §19.9）。
fn bash_settings(
    defaults: &Defaults,
    extensions: &Extensions,
    traits: SurfaceTraits,
) -> BashSettings {
    BashSettings {
        timeouts: if extensions.streaming_input {
            defaults.limits.bash.with_streaming_input()
        } else {
            defaults.limits.bash
        },
        background_deadline: traits.non_interactive,
        bash_first_trimmed: ModeClass::of(defaults.mode) == ModeClass::Bypass,
        monitor: extensions.monitor,
    }
}

/// 基础工具的注册表（tools 的工具，ext 与 agents 之前的那一层）。
pub(crate) fn base_tools(
    ports: &Ports,
    defaults: &Defaults,
    extensions: &Extensions,
    (traits, gate): (SurfaceTraits, &Arc<PolicyGate>),
) -> BaseTools {
    // `ENABLE_TOOL_SEARCH` 为关时 ToolSearch 不启用（工作线 F89，tools-more.md §3.2.1）。
    let catalog = Arc::new(ToolCatalog::new(defaults.limits.request.tool_search));
    let cron = CronBoard::new(ports.clock.clone(), extensions.cron_store.clone());
    // Read 整读 PDF 前按会话当前模型的生效配置查 `supports_pdf`（tools-files.md §1.5，修复线 F24）。
    let side_model = || rust_cli_tools::SideModel {
        model: ports.model.clone(),
        selection: defaults.selection.clone(),
    };
    let builtin = rust_cli_tools::builtin_with(gate.clone(), Some(side_model()));
    let read_files: Arc<ReadFileState> = builtin.file_state().cloned().unwrap_or_default();
    let worktrees = Arc::new(WorktreeBoard::default());
    // Monitor（工作线 F83，tools-more.md §19）：单次的 `-p` 上限 10 分钟，其余 30 分钟。
    let monitor: Vec<Arc<dyn rust_cli_kernel_api::tool::Tool>> = if extensions.monitor {
        let tool = rust_cli_tools::MonitorTool::new().with_session_cap(traits.monitor_session_cap);
        vec![Arc::new(tool)]
    } else {
        Vec::new()
    };
    let bash = bash_settings(defaults, extensions, traits);
    let registry: Arc<dyn ToolRegistry> = Arc::new(
        builtin
            .with_search_opt_in(defaults.search_tools)
            // 读文件的命令记进文件工具的同一份 readFileState（tools-files.md §4.2）。
            .with_leading(Arc::new(
                BashTool::new(bash).with_file_state(read_files.clone()),
            ))
            .with_tools(rust_cli_tools::interaction_tools(
                &catalog,
                traits.interactive,
            ))
            .with_tools(rust_cli_tools::web_tools(side_model(), ports.clock.clone()))
            .with_tools(rust_cli_tools::cron_tools(&cron, extensions.monitor))
            // `/loop` 动态模式的自定节奏（L17，tools-more.md §17.2）：默认注册、不延迟。
            .with_tools(vec![Arc::new(rust_cli_tools::ScheduleWakeupTool::new(
                &cron,
            ))])
            .with_tools(monitor)
            .with_tools(vec![Arc::new(rust_cli_tools::ReportFindingsTool::new())])
            .with_tools(rust_cli_tools::worktree_tools(&worktrees)),
    );
    // 用户输入的提及与 bash 模式用基础注册表里的 Read 与 Bash（与模型的调用共用 readFileState 与 shell 状态）。
    let inputs: Arc<dyn UserInputs> = Arc::new(rust_cli_tools::UserInputTools::new(
        registry.as_ref(),
        gate.clone(),
        bash,
    ));
    BaseTools {
        registry,
        catalog,
        cron,
        read_files,
        worktrees,
        inputs,
    }
}

/// skill、MCP 与插件（ext.md §1）：工具加在基础注册表之后；绑定到远端机器的 MCP 服务器经注册表取那台机器的管道（R5，
/// remote-exec.md §22.6）。
pub(crate) fn ext(
    ports: &Ports,
    (gate, spawner): (&Arc<PolicyGate>, Spawner),
    base: Arc<dyn ToolRegistry>,
    (config, traits): (rust_cli_ext::ExtConfig, SurfaceTraits),
) -> Arc<Ext> {
    let machine = ports.environments.primary_machine();
    let ext = Arc::new(Ext::new(
        ExtPorts {
            info: machine.info().clone(),
            files: machine.files().clone(),
            ids: ports.ids.clone(),
            gate: gate.clone(),
            mcp: machine.mcp().clone(),
            machines: Some(ports.environments.clone()),
            spawner,
            clock: ports.clock.clone(),
        },
        base,
        config,
    ));
    ext.set_print_session(traits.non_interactive);
    ext
}

/// 上下文组装（context.md）：agent 清单由 agents 给出（L7），延迟工具由 ToolCatalog 给出（L11a），skill 清单与 MCP
/// instructions 由 ext 给出（L9）。读过的文件被改动（tools-files.md §4.1）用文件工具的同一份 readFileState，远端机器上
/// 读过的文件每台每步一次批量 stat（R3，remote-exec.md §20.7.4）；连上、断开远端机器时发机器清单提醒（§20.5.3）。
pub(crate) fn context(
    ports: &Ports,
    config: ContextConfig,
    (agent_catalog, tools, ext): (&Arc<AgentCatalog>, &BaseTools, &Ext),
) -> Arc<PromptAssembler> {
    let machine = ports.environments.primary_machine();
    let assembler = PromptAssembler::new(
        ContextPorts {
            info: machine.info().clone(),
            files: machine.files().clone(),
            shell: machine.shell().clone(),
            git: machine.git().clone(),
            clock: ports.clock.clone(),
            ids: ports.ids.clone(),
            listings: Arc::new(listings::CombinedListings(vec![
                agent_catalog.clone(),
                tools.catalog.clone(),
                ext.listings(),
            ])),
        },
        config,
    )
    .with_changed_files(Arc::new(
        rust_cli_tools::FileChanges::new(tools.read_files.clone(), machine.files().clone())
            .with_machines(ports.environments.clone()),
    ))
    .with_machines(ports.environments.clone());
    Arc::new(assembler)
}

/// 主循环用到的能力。工具是 ext 的注册表外面去掉整个工具被 deny 的（tools-files.md §1.1.1，工作线 F69）：主线程、子代理、
/// 延迟清单与压缩摘要都从这一层往外取；agents 建好后主线程换成它的注册表。压缩器先交出“稍后接上”的句柄（late.rs）。
pub(crate) fn query_parts(
    ports: &Ports,
    (context, ext, gate, hooks): (
        &Arc<PromptAssembler>,
        &Ext,
        &Arc<PolicyGate>,
        &Arc<HookEngine>,
    ),
    (late, operations): (&Arc<LateCompactor>, Operations),
    (defaults, traits): (&Defaults, SurfaceTraits),
) -> QueryParts {
    QueryParts {
        model: ports.model.clone(),
        context: context.clone(),
        tools: Arc::new(rust_cli_tools::PolicyFiltered::new(
            ext.registry(),
            gate.clone(),
        )),
        // 管线直接拿判决链（F125：缺陷 D1 是以前包在外面的一层漏转发了“只批准这一次”）；按调用的 `machine` 选机器
        // （R3，remote-exec.md §20.6）。
        executor: Arc::new(
            Pipeline::new(gate.clone(), hooks.clone()).with_machines(ports.environments.clone()),
        ),
        compactor: late.clone(),
        hooks: hooks.clone(),
        ids: ports.ids.clone(),
        clock: ports.clock.clone(),
        machines: ports.environments.clone(),
        artifacts: ports.artifacts.clone(),
        operations,
        non_interactive: traits.non_interactive,
        limits: defaults.limits.request,
    }
}

/// 子代理与任务清单（工作线 L7）：主线程的工具注册表换成 agents 的（基础工具 + Agent 等），主循环包一层运行登记；子循环
/// 由 `LoopFactory` 用同一套能力组装。只读查看的子代理快照算 ctx 与上下文提示时用同一个压缩器（修复线 F112）。
pub(crate) fn agents(
    ports: &Ports,
    (parts, tasks, agent_catalog): (&QueryParts, Arc<dyn TaskListStore>, &Arc<AgentCatalog>),
    (late, spawner, peers): (&Arc<LateCompactor>, Spawner, &rust_cli_session::PeerDeps),
    (defaults, traits): (&Defaults, SurfaceTraits),
) -> Agents {
    Agents::new(
        AgentsPorts {
            store: ports.store.clone(),
            ids: ports.ids.clone(),
            clock: ports.clock.clone(),
            machines: ports.environments.clone(),
            tasks,
            queries: Arc::new(agents::LoopFactory {
                parts: parts.clone(),
            }),
            context: parts.context.clone(),
            compactor: late.clone(),
            catalog: agent_catalog.clone(),
            spawner,
            peers: peers.port.clone(),
            operations: parts.operations.clone(),
        },
        parts.tools.clone(),
        AgentsConfig {
            task_tools: match defaults.task_tools {
                Some(true) => TaskTools::On,
                Some(false) => TaskTools::Off,
                None => TaskTools::ByDefaultModel(defaults.selection.clone()),
            },
            keep_alive: traits.interactive,
            // 修复线 F67：交互界面里后台子代理也能询问（agents.md §4.1.1）。
            background_prompts: traits.interactive,
            // 修复线 L-A：交互会话缺省打开 fork 子代理，`-p` 关；环境变量可以改（agents.md §8.1.1）。
            fork: defaults.limits.fork_subagent.unwrap_or(traits.interactive),
        },
    )
}

/// 压缩器（compact.md §3.2）：摘要请求的子循环用同一个上下文与主线程的工具清单（共享缓存前缀），工具一律拒绝，自己不再
/// 压缩。压缩时清空的 readFileState 与文件工具是同一份（§3.4）。
pub(crate) fn compactor(
    ports: &Ports,
    (parts, tools): (&QueryParts, &Arc<dyn ToolRegistry>),
    (files, context, hooks): (
        Arc<dyn ReadFileStates>,
        Arc<PromptAssembler>,
        &Arc<HookEngine>,
    ),
    compact: CompactConfig,
) -> Arc<dyn Compactor> {
    let summary = QueryLoop::new(QueryParts {
        executor: Arc::new(DenyTools),
        tools: tools.clone(),
        compactor: Arc::new(Disabled),
        ..parts.clone()
    });
    Arc::new(Compaction::new(
        CompactPorts {
            runner: Arc::new(summary),
            tools: tools.clone(),
            files,
            context,
            hooks: hooks.clone(),
            ids: ports.ids.clone(),
            machines: ports.environments.clone(),
            artifacts: ports.artifacts.clone(),
            operations: parts.operations.clone(),
        },
        compact,
    ))
}
