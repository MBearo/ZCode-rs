//! controller 的组装根（工作线 F129，审查 E1；docs/specs/rust-cli/cli-args.md §14.3）。
//!
//! 三个前端（交互界面、`-p`、app-server）各给一份 [`Controller`] 规格，组装分两个阶段：开端口（[`open`] 之后 [`ports`]）
//! 与建内核（[`kernel`]）。前端自己的步骤夹在阶段之间、写在前端的入口里（更新提示、`--session-id` 与会话库的检查、
//! 工作区信任、`storageState` 的帧、等 MCP），启动的顺序由入口的代码写明。
//!
//! 以前 TUI / `-p` 的 `session()` 453 行与 app-server 的 `serve` 165 行有 107 行逐字相同，`session()` 里按模式判断 10 处，
//! app-server 传空值 5 处，本机执行环境的组装写了 3 份（含执行器）；root 下 bypass 的拒绝只在 TUI / `-p` 的那份里。

pub(crate) mod deployment;
pub(crate) mod env_assembly;
pub(crate) mod env_layout;
pub(crate) mod limits;
pub(crate) mod loopback;
pub(crate) mod mcp;
pub(crate) mod mcp_machines;
pub(crate) mod model_io;
pub(crate) mod model_io_files;
pub(crate) mod models;
pub(crate) mod network;
pub(crate) mod options;
pub(crate) mod origins;
pub(crate) mod peers;
pub(crate) mod request_headers;
pub(crate) mod setup;
pub(crate) mod shell;
pub(crate) mod ssh_executor;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use rust_cli_base::TaskScope;
use rust_cli_base::clock::SystemClock;
use rust_cli_env_local::{LocalEnvironment, LocalMcpTransport};
use rust_cli_kernel_api::default_model::DefaultModel;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{HeaderSource, ModelDirectory, ModelLimits};
use rust_cli_net::Egress;
use rust_cli_node_store::{NodeIds, NodeStore};
use rust_cli_paths::StoragePaths;
use rust_cli_platform::Platform;
use rust_cli_runtime::{
    AutoCompactAt, ContextConfig, Defaults, LocalKernel, Ports, Surface, TitleModel,
};

use crate::assemble::deployment::Deployment;
use crate::assemble::env_layout::PrimaryPaths;
use crate::assemble::mcp::{McpFlags, Prewait};
use crate::commands::args::{Invocation, PromptFlags, cli_rules, names_search_tools};
use crate::process::facts::{Facts, ProcessFacts, TestHooks};
use crate::process::output::eprint_line;
use crate::{VERSION, commands::flags, config, machines};

/// 本机跨会话通信的登记（cross-session.md §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Peering {
    /// 不登记（app-server）。
    Off,
    /// 交互界面。
    Interactive,
    /// `-p`（登记的形态不同，cross-session.md §2.2）。
    Print,
}

/// 主 env 可以在哪（deployment.md §6、§19）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Primary {
    /// 按部署配置（缺省本进程，可以是 SSH 执行器）；debug 构建的 loopback 声明也生效。
    Configured,
    /// 只接受本进程里的主 env（app-server：远端主 env 在 M2）。
    LocalOnly,
}

/// root 下的 bypassPermissions（cli-args.md §5.2、§14.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootBypass {
    /// 执行命令的那一侧是 root 时拒绝启动（照对齐基准；TUI 与 `-p`）。
    Refuse,
    /// 不检查（照 Node 的 app-server：Node 没有这条规则；远端的 Source Server 可能以 root 运行）。
    Allow,
}

/// 命令行给组装的输入（app-server 没有命令行：都是缺省）。
#[derive(Debug, Clone, Default)]
pub(crate) struct CliInputs {
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<&'static str>,
    pub(crate) permission_mode: Option<PermissionMode>,
    pub(crate) allowed: Vec<String>,
    pub(crate) disallowed: Vec<String>,
    pub(crate) max_turns: Option<u32>,
    pub(crate) mcp: McpFlags,
    /// `--input-format stream-json`（headless.md §6）：MCP 的启动等待与 Bash 后台命令的缺省期限随之不同。
    pub(crate) streaming_input: bool,
    pub(crate) plugin_dirs: Vec<String>,
    pub(crate) system_prompt: PromptFlags,
    pub(crate) append_system_prompt: PromptFlags,
    pub(crate) add_dirs: Vec<String>,
    pub(crate) settings: Option<String>,
}

impl CliInputs {
    /// 命令行里给组装的那部分（前端自己的 prompt、续写目标、输出格式等留在入口）。
    pub(crate) fn of(invocation: &Invocation) -> Self {
        Self {
            model: invocation.model.clone(),
            effort: invocation.effort,
            permission_mode: invocation.permission_mode,
            allowed: invocation.allowed.clone(),
            disallowed: invocation.disallowed.clone(),
            max_turns: invocation.max_turns,
            mcp: invocation.mcp.clone(),
            streaming_input: invocation.format.input == rust_cli_headless::InputFormat::StreamJson,
            plugin_dirs: invocation.plugin_dirs.clone(),
            system_prompt: invocation.system_prompt.clone(),
            append_system_prompt: invocation.append_system_prompt.clone(),
            add_dirs: invocation.add_dirs.clone(),
            settings: invocation.settings.clone(),
        }
    }
}

/// 一个 controller 前端的组装规格。
pub(crate) struct Controller {
    /// 提示词与工具的形态：`-p` 是 Print，交互界面与 App 是 Interactive。
    pub(crate) surface: Surface,
    /// 会话标题（只在交互界面生成，titles.md §2）。
    pub(crate) titles: bool,
    pub(crate) peers: Peering,
    /// 远端机器（R3，remote-exec.md §20）：None 是没有机器清单（app-server：注册表只有主 env）。
    pub(crate) machines: Option<machines::When>,
    pub(crate) primary: Primary,
    /// 账号模型每请求鉴权的来源（G8：app-server 经 Host）；None 时平台层自己算。
    pub(crate) headers: Option<Arc<dyn HeaderSource>>,
    pub(crate) root_bypass: RootBypass,
    /// 组装时等不等缺省模型定下（工作线 F140，performance.md §12.3）：交互界面不等（第一帧不等模型目录），`-p` 与
    /// app-server 当场等。
    pub(crate) wait_model: bool,
    pub(crate) cli: CliInputs,
}

impl Controller {
    /// 交互界面：后台连机器（不挡启动）、生成标题。
    pub(crate) fn tui(cli: CliInputs) -> Self {
        Self {
            surface: Surface::Interactive,
            titles: true,
            peers: Peering::Interactive,
            machines: Some(machines::When::Background),
            primary: Primary::Configured,
            headers: None,
            root_bypass: RootBypass::Refuse,
            wait_model: false,
            cli,
        }
    }

    /// `-p`：只在测试变量下连机器，而且同步连完（remote-exec.md §20.10）。
    pub(crate) fn print(cli: CliInputs, hooks: &TestHooks) -> Self {
        let declared = hooks.assume.loopback_machines || hooks.assume.ssh_machines;
        Self {
            surface: Surface::Print,
            titles: false,
            peers: Peering::Print,
            machines: Some(if declared {
                machines::When::Now
            } else {
                machines::When::Never
            }),
            primary: Primary::Configured,
            headers: None,
            root_bypass: RootBypass::Refuse,
            wait_model: true,
            cli,
        }
    }

    /// app-server（app-protocol.md §3.7）：交互形态的提示词；不生成标题、不登记跨会话通信、不连远端机器、只接受本地主
    /// env；账号凭据经 Host；root 下的 bypass 照 Node 不拒绝。
    pub(crate) fn app(headers: Arc<dyn HeaderSource>) -> Self {
        Self {
            surface: Surface::Interactive,
            titles: false,
            peers: Peering::Off,
            machines: None,
            primary: Primary::LocalOnly,
            headers: Some(headers),
            root_bypass: RootBypass::Allow,
            wait_model: true,
            cli: CliInputs::default(),
        }
    }
}

/// 本机的执行环境与 MCP stdio 的管道（shell-permissions.md §2.5、ext.md §3.2）：controller 与执行器按同一条规则建（shell、
/// 网络出口、内嵌的搜索工具）。MCP stdio 服务器与 Bash 共用进程树账本，快照用 shell 改写过 `SHELL` 的那份（F58）。
pub(crate) struct LocalEnv {
    pub(crate) env: LocalEnvironment,
    pub(crate) mcp: Arc<LocalMcpTransport>,
    /// Bash 工具用的 shell（Windows 上找不到 Git Bash 时没有）。
    pub(crate) bash: Option<PathBuf>,
}

/// 建 [`LocalEnv`]。`find_and_grep`：启动参数没有点名 Glob / Grep（embedded-search.md §8.1）；`expanding_vars`：MCP 配置里的
/// `${VAR}` 在这里展开（执行器：远端的 MCP 服务器按那台机器的环境展开，remote-exec.md §22.7）。
pub(crate) async fn local_env(
    process: &ProcessFacts,
    egress: &Arc<Egress>,
    (find_and_grep, expanding_vars): (bool, bool),
) -> LocalEnv {
    let shell =
        crate::assemble::shell::config(process.env.clone(), &process.temp, find_and_grep).await;
    let bash = shell.bash.clone();
    let mcp = LocalMcpTransport::new(
        shell.ledger.clone(),
        shell.env.clone(),
        Some(egress.clone()),
    );
    let mcp = Arc::new(if expanding_vars {
        mcp.expanding_vars()
    } else {
        mcp
    });
    let env = LocalEnvironment::new(Some(process.home.clone()))
        .with_shell(shell)
        // WebFetch 的网络出口（tools-more.md §4），与模型请求共用出口策略。
        .with_web(egress.clone());
    LocalEnv { env, mcp, bash }
}

/// 第一阶段的前半：部署配置、出口、平台层、执行环境与注册表、要读文件的参数、配置、存储位置与会话库。
pub(crate) struct Opened {
    pub(crate) deployment: Deployment,
    pub(crate) egress: Arc<Egress>,
    /// 平台层（TUI 的 `/login` `/logout` 与模型端口共用，tui.md §7.2）。
    pub(crate) platform: Arc<Platform>,
    /// 模型目录；显式覆盖（`ZCODE_API_KEY`）时没有：`/model 名称` 直接换模型（tui.md §6）。
    pub(crate) directory: Option<Arc<dyn ModelDirectory>>,
    pub(crate) env: Arc<LocalEnvironment>,
    /// Monitor 开不开（工作线 F83）。
    pub(crate) monitor: bool,
    /// 会话的工作区：主 env 上的工作目录（deployment.md §10）；controller 没有自己的工作区。
    pub(crate) workspace: WorkspaceRef,
    pub(crate) environments: Arc<dyn EnvironmentRegistry>,
    /// 远端机器（R3）与它们的启动输入；没有机器清单时没有。
    pub(crate) machines: Option<(machines::Machines, machines::Start)>,
    pub(crate) primary: MachineEnv,
    pub(crate) primary_paths: PrimaryPaths,
    pub(crate) resolved: setup::ResolvedFlags,
    pub(crate) config: config::ConfigSnapshot,
    /// 读配置之后的存储位置（`storage.*`）。
    pub(crate) paths: StoragePaths,
    /// 时间与 ID 只有一份（README §4，F7）：内核、会话库的记录、模型、定时任务存储与 TUI 共用。
    pub(crate) ids: Arc<NodeIds>,
    pub(crate) store: Arc<NodeStore>,
}

/// 开端口的前半。
pub(crate) async fn open(
    spec: &Controller,
    facts: &Facts,
    (paths, clock): (&StoragePaths, &Arc<SystemClock>),
) -> Result<Opened> {
    let deployment = env_assembly::deployment(facts).await?;
    if spec.primary == Primary::LocalOnly {
        anyhow::ensure!(
            deployment.primary_is_local(),
            "app-server does not support a remote primary env yet ({})",
            crate::assemble::deployment::VAR
        );
    }
    let egress = setup::egress(&facts.process).await?;
    let platform = setup::platform(facts, paths, egress.clone(), clock.clone())?;
    let platform = Arc::new(match &spec.headers {
        // 缺口 G8：账号模型的凭据向 Host 要（与 Node 相同经反向请求）；API key 的 provider 不受影响。
        Some(headers) => platform.with_header_source(headers.clone()),
        None => platform,
    });
    let explicit = facts
        .controller
        .explicit
        .api_key
        .as_ref()
        .is_some_and(|key| !key.expose().is_empty());
    let directory = (!explicit).then(|| {
        let directory: Arc<dyn ModelDirectory> = platform.clone();
        directory
    });
    let find_and_grep = !names_search_tools(&spec.cli.allowed);
    let local = local_env(&facts.process, &egress, (find_and_grep, false)).await;
    let monitor = options::monitor(&facts.hooks.assume, local.bash.is_some());
    let workspace = env_assembly::workspace(&deployment, &facts.process);
    let env = Arc::new(local.env);
    let executor_source = machines::artifact_source(facts, &egress);
    let loopback = match spec.primary {
        Primary::Configured => facts
            .hooks
            .assume
            .loopback
            .then_some(env_assembly::Loopback { find_and_grep }),
        Primary::LocalOnly => None,
    };
    let environments = env_assembly::registry(
        &deployment,
        &facts.process,
        (env.clone(), local.mcp),
        loopback,
        executor_source.clone(),
    )
    .await?;
    // 额外机器（R3，remote-exec.md §20.2）：主 env 的注册表外面包一层，连上的机器放进来、断开时拿走。
    let (environments, fleet) = match spec.machines {
        Some(_) => {
            let machines = machines::Machines::new(environments);
            (machines.registry(), Some(machines))
        }
        None => (environments, None),
    };
    let primary = environments.primary_machine();
    // 要读文件、看目录的参数（cli-args.md §4、§6、§8）：路径一律指主 env，连上之后经它解析（deployment.md §21）。
    let primary_paths = PrimaryPaths::of(&primary, &workspace).await;
    let cli = &spec.cli;
    let resolved = setup::resolve_flags(
        (&primary, &workspace, &primary_paths),
        setup::FlagInputs {
            system: &cli.system_prompt,
            append: &cli.append_system_prompt,
            add_dirs: &cli.add_dirs,
            settings: cli.settings.as_deref(),
        },
    )
    .await?;
    for warning in &resolved.warnings {
        eprint_line(warning);
    }
    // hooks、权限规则与 MCP 读 Node 的配置文件（docs/specs/rust-cli/config.md）；模式按 Node 的顺序
    //（命令行 → 本机项目的持久模式 → 配置 → default，config.md §2.2）。用户级配置在 controller 本机，项目级配置在主 env。
    let mut config = config::assemble(&config::ConfigInputs {
        files: primary.files().as_ref(),
        workspace: &workspace,
        paths,
        process_env: &facts.process.env,
        flag: resolved.settings.as_ref(),
        project_is_local: deployment.primary_is_local(),
    })
    .await;
    env_assembly::drop_remote_plugins(&deployment, &mut config.plugins);
    // 会话库与产物目录按 Node 的 `storage.sessionDbPath` / `storage.dir`（用户级 → 项目级 → 环境变量，
    // config.md §2.3）：先读配置再开库。修复线 F2：以前总在缺省位置，Node 设了这些值时两边各用一个库。
    let paths = facts.storage_paths(&config.storage.settings());
    let machines = match (fleet, spec.machines) {
        (Some(fleet), Some(when)) => {
            let start = machines::Start::new(&paths, when, (facts, executor_source));
            Some((fleet, start))
        }
        (Some(_) | None, _) => None,
    };
    let ids = Arc::new(NodeIds::new(clock.clone()));
    let store = Arc::new(NodeStore::new(paths.clone(), ids.clone()).with_version(VERSION));
    Ok(Opened {
        deployment,
        egress,
        platform,
        directory,
        env,
        monitor,
        workspace,
        environments,
        machines,
        primary,
        primary_paths,
        resolved,
        config,
        paths,
        ids,
        store,
    })
}

/// 第一阶段的后半：模型端口（设备身份、显式覆盖或平台层、入口级选项、模型轨迹）与内核的端口。
pub(crate) struct Ported {
    pub(crate) opened: Opened,
    pub(crate) ports: Ports,
    /// 新会话的模型选择（`--model`、`--effort` 已作用）：交互界面里可能还没定下（工作线 F140）。
    pub(crate) selection: DefaultModel,
    /// 等模型目录、定下缺省模型的任务（[`models::default_model`]）：持有到会话结束。
    pub(crate) choosing: TaskScope,
    pub(crate) limits: Arc<dyn ModelLimits>,
}

/// 开端口的后半。
pub(crate) async fn ports(
    spec: &Controller,
    facts: &Facts,
    opened: Opened,
    (clock, model_io): (&Arc<SystemClock>, &model_io::ModelIo),
) -> Result<Ported> {
    // 设备身份（platform.md §10，工作线 F52）：每个请求的 `metadata.user_id`；第一次请求时才读文件。
    let device = setup::device_id(&opened.paths, opened.ids.clone());
    let assembled = models::assemble(
        opened.platform.clone(),
        &facts.controller.explicit,
        &facts.hooks.assume,
        (spec.cli.model.clone(), spec.cli.effort),
    )
    .await;
    let options = setup::route_options(
        (&facts.process.env, facts.controller.max_output_tokens),
        opened.ids.clone(),
        (device, facts.hooks.assumed_base_url.clone()),
    )?;
    // 模型轨迹（工作线 F91，model-io.md §3）：目录跟随合并后的 `storage.dir`，全量保留读 App 的设置。
    let options = rust_cli_model::RouteOptions {
        model_io: model_io.sink_for(model_io::Target {
            env: &facts.process.env,
            paths: &opened.paths,
            storage_dir: opened.config.storage.dir.as_deref(),
            base: &facts.process.cwd,
            settings: opened.platform.settings().clone(),
        }),
        ..options
    };
    let ports = Ports {
        model: assembled.port(opened.egress.clone(), clock.clone(), options),
        environments: opened.environments.clone(),
        store: opened.store.clone(),
        artifacts: opened.store.clone(),
        clock: clock.clone(),
        ids: opened.ids.clone(),
    };
    // 缺省模型（工作线 F140，performance.md §12.3）：组装完就取（不等信任页）；交互界面不等它画第一帧，`-p` 与 App 当场等。
    let (selection, choosing) = models::default_model(
        assembled.selection,
        opened.paths.default_model(),
        spec.wait_model,
        facts.process.started,
    )
    .await;
    Ok(Ported {
        selection,
        choosing,
        limits: assembled.limits,
        opened,
        ports,
    })
}

/// 第二阶段的结果：内核与前端还要的东西。
pub(crate) struct Built {
    pub(crate) opened: Opened,
    pub(crate) kernel: Arc<LocalKernel>,
    pub(crate) selection: DefaultModel,
    /// 定下缺省模型的任务（[`Ported::choosing`]）：只为持有，作用域丢掉时任务就停了（前端把 `Built` 留到会话结束）。
    pub(crate) _choosing: TaskScope,
    /// 内核读的自动压缩位置（配置的 `compact.autoThresholdTokens`，compact.md §2.3）：交互界面的设置页与 `/autocompact`
    /// 改的就是这一份。
    pub(crate) compact_at: AutoCompactAt,
    /// 新会话的权限模式（命令行 → 本机项目的持久模式 → 配置 → default）。
    pub(crate) mode: PermissionMode,
    /// 本机跨会话通信（`Peering::Off` 时没有）：退出时删掉登记与 socket。
    pub(crate) peers: Option<peers::Peers>,
    /// 远端机器（没有机器清单时没有）。
    pub(crate) machines: Option<machines::Machines>,
    /// 绑定到远端机器的 MCP 服务器（服务器名，机器名；含停用的）：TUI 的工具行标出机器（R5，remote-exec.md §22.8）。
    pub(crate) mcp_machines: Vec<(String, String)>,
}

/// 建内核：权限模式与 root 下的规则、缺省、上下文与压缩、扩展（MCP、插件、跨会话通信、定时任务），内核、hooks 与权限
/// 规则；之后连远端机器（先于 MCP，R5）、开始连接 MCP 服务器。
pub(crate) async fn kernel(
    spec: &Controller,
    facts: &Facts,
    ported: Ported,
    clock: &Arc<SystemClock>,
) -> Result<Built> {
    let Ported {
        opened,
        ports,
        selection,
        choosing,
        limits: model_limits,
    } = ported;
    let Opened {
        primary,
        workspace,
        config,
        ..
    } = &opened;
    let cli = &spec.cli;
    let env = &facts.process.env;
    // 命令行模式只来自 `--permission-mode`（工作线 F63 删掉了排在它前面的 `--dangerously-skip-permissions`，
    // cli-args.md §5.1）。
    let mode = config::initial_mode(
        cli.permission_mode,
        config.mode,
        opened.store.as_ref(),
        workspace,
    )
    .await;
    match spec.root_bypass {
        // root 不能用 bypassPermissions：看执行命令的那一侧，即主 env 报告的 uid（deployment.md §21，Q10）。
        RootBypass::Refuse => {
            let executing_uid = primary
                .info()
                .facts(workspace)
                .await
                .ok()
                .and_then(|facts| facts.uid);
            flags::refuse_root_bypass(
                mode == PermissionMode::BypassPermissions,
                executing_uid,
                env,
            )?;
        }
        RootBypass::Allow => {}
    }
    let defaults = Defaults {
        selection: selection.clone(),
        mode,
        max_turns: cli.max_turns,
        search_tools: names_search_tools(&cli.allowed),
        task_tools: options::task_tools(env),
        limits: limits::limits(env, facts.controller.max_output_tokens),
    };
    // 自动记忆目录先建好，提示词的记忆段才能说“目录已存在”（memory.md §2）；目录在主 env 上（deployment.md §8）。
    let mut context = ContextConfig::new(spec.surface);
    context.custom_system_prompt = opened.resolved.system_prompt.clone();
    context.append_system_prompt = opened.resolved.append_system_prompt.clone();
    // 附加目录只归会话（deployment.md §21）：环境段、嵌套规则文件与权限规则读这一份。
    let additional: Vec<EnvPath> = opened.resolved.add_dirs.iter().map(EnvPath::new).collect();
    context.additional_directories = additional.clone();
    let layout = env_assembly::layout(&opened.deployment, &opened.primary_paths);
    context.memory_dir = options::memory_dir(primary, &layout, &opened.paths, workspace, env).await;
    // 压缩的上限两种模式都接（显式覆盖按内置规则，修复线 F61：以前显式覆盖时是“没有目录”，按 200,000）。
    let compact = options::compact(env, facts.controller.max_output_tokens, model_limits);
    // 所有模式都按配置的位置压缩；设置页与 `/autocompact` 写回后换掉的就是内核读的这一份（compact.md §2.3）。
    compact.at.set(config.compact_at);
    let compact_at = compact.at.clone();
    // stream-json 输入的 MCP prewait 看能不能用工具搜索（headless.md §6.2）：乐观判断，`ENABLE_TOOL_SEARCH` 不是关（工作线
    // F89，tools-more.md §3.2.1）。修改：F84 时看 `tool_search_mode`（禁用表里的模型不算）。
    let prewait = Prewait {
        streaming_input: cli.streaming_input,
        tool_search: limits::tool_search(env).allows(),
    };
    // `--mcp-config` 与 `--plugin-dir` 经主 env 读、解析（deployment.md §21）；机器绑定的旁路文件是 controller 的配置。
    let mut extensions = setup::extensions(
        (primary, workspace, &opened.primary_paths),
        opened.env.as_ref(),
        env,
        (&cli.mcp, &cli.allowed, prewait),
        (&cli.plugin_dirs, &opened.paths.mcp_machines()),
        config,
    )
    .await;
    extensions.monitor = opened.monitor;
    let mut opened = opened;
    // 绑定到远端机器的 MCP 服务器（R5，remote-exec.md §22.5、§22.8）：连上机器时的期望集合，TUI 的标记。
    let machines = opened
        .machines
        .take()
        .map(|(fleet, start)| (fleet, start.with_mcp(&extensions.config)));
    let mcp_machines = machines
        .as_ref()
        .map(|(_, start)| start.mcp_marks.clone())
        .unwrap_or_default();
    // 本机跨会话通信（cross-session.md §2）：进程启动时绑定收件箱、写登记；退出时删除。
    let peers = match spec.peers {
        Peering::Off => None,
        Peering::Interactive | Peering::Print => Some(
            peers::start(
                peers::Startup {
                    paths: &opened.paths,
                    temp: &facts.process.temp,
                    cwd: &facts.process.cwd,
                    env,
                    version: VERSION,
                    print: spec.peers == Peering::Print,
                },
                clock.clone(),
                opened.ids.clone(),
            )
            .await,
        ),
    };
    if let Some(peers) = &peers {
        extensions.peers = peers.deps.clone();
    }
    // 持久定时任务：启动工作区（项目根）下的 `.zcode/scheduled_tasks.json`（tools-more.md §16）。
    extensions.cron_store =
        env_assembly::cron_store(&opened.deployment, &opened.workspace, clock.clone());
    // 任务清单（Task v2）放在本地存储：文件是事实源，写后投影到 Node 的 todo 表（agents.md §7）。
    let kernel = LocalKernel::with_extensions(
        ports,
        defaults,
        context,
        compact,
        opened.store.clone(),
        extensions,
    );
    // 会话标题只在交互界面生成（titles.md §2），模型经小模型槽位从会话当前的主模型得出。
    let kernel = Arc::new(if spec.titles {
        let titles = TitleModel {
            model: kernel.ports().model.clone(),
            directory: opened.directory.clone(),
        };
        kernel.with_titles(titles)
    } else {
        kernel
    });
    kernel.replace_hooks(opened.config.hooks.clone());
    let local = config::local_rules(kernel.ports().store.as_ref(), &opened.workspace).await;
    kernel.replace_permission_rules(
        opened
            .config
            .permission_rules(local, cli_rules(&cli.allowed, &cli.disallowed, additional)),
    );
    // 自动连上次连着的机器（§20.3.2）：TUI 在后台；`-p` 只在测试变量下、同步连完（§20.10）。R5（remote-exec.md
    // §22.6）：先于 MCP 启动，绑定到远端机器的服务器看得到“正在连”、等它连完。没有清单里的机器时两者都不做事。
    let machines = match machines {
        Some((fleet, start)) => {
            fleet.start(start).await?;
            Some(fleet)
        }
        None => None,
    };
    // 开始连接 MCP 服务器、加载插件；等不等由前端决定（`-p` 短等，TUI 与 App 不等，ext.md §3.3.1）。
    kernel.start_extensions().await;
    Ok(Built {
        opened,
        kernel,
        selection,
        _choosing: choosing,
        compact_at,
        mode,
        peers,
        machines,
        mcp_machines,
    })
}
