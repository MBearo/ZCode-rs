//! `zcode`：rust-cli 的组装根（docs/specs/rust-cli/README.md §3）。
//!
//! 只有这里读取进程环境与 cwd，读到的值显式传给下层。这里构造平台层（Provider registry、账号）与适配器
//! （存储、执行环境、模型），交给 runtime 组装成内核，再按命令行进入 TUI 或 `-p`。

mod app_server;
mod args;
// build.rs 的计数与版本规则（读写计数文件、拼接与校验版本），只在单测里引入（docs/specs/rust-cli/version.md §3）。
#[cfg(test)]
mod build_number;
mod clipboard;
mod commander;
mod config;
mod editor;
mod effort;
mod embedded;
mod executor;
mod executor_run;
mod flags;
mod help;
mod hooks_trust;
mod limits;
mod log;
mod log_writer;
mod login;
mod loopback;
mod machines;
mod mcp;
mod mcp_machines;
mod model_io;
mod model_io_files;
mod models;
mod network;
mod options;
mod origins;
mod output;
mod peers;
mod print_format;
mod request_headers;
mod signals;
mod ssh_executor;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::args::{Invocation, Mode, cli_rules, names_search_tools, parse, version_line};
use crate::output::{eprint_line, eprint_raw, help_width, print_line, print_raw, write_lossy};
use rust_cli_base::TaskScope;
use rust_cli_base::secret::{EnvSnapshot, RedactedString};
use rust_cli_env_local::{LocalCronStore, LocalEnvironment, LocalMcpTransport};
use rust_cli_headless::PrintOptions;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::ModelDirectory;
use rust_cli_net::Egress;
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings, expand_home, home_dir_with, user_name_with};
use rust_cli_platform::{AccountServices, CredentialSecret, History, Platform, node_platform};
use rust_cli_runtime::{
    ContextConfig, Defaults, ExtConfig, Extensions, LocalKernel, Ports, Surface, TitleModel,
};

mod os_release;
mod shell;
mod stdin_prompt;
mod temp_dir;
mod time_zone;
mod tui;
mod tui_account;
mod tui_machines;
mod tui_mcp;
mod update;
mod workspace_trust;

#[path = "main_setup.rs"]
mod setup;

/// Linux musl 版的全局分配器（docs/specs/rust-cli/linux.md §8.3，工作线 F44）：musl 自带的 malloc 在多线程下慢，
/// Rust 代码的分配换成 mimalloc；C 代码（SQLite、ring）仍用 musl 的 malloc。macOS 与 glibc 不变。
#[cfg(all(target_os = "linux", target_env = "musl"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// 本工程的版本：唯一来源，由 build.rs 在编译期给出（release 第三位是编译次数，debug 是 Cargo.toml 的版本，
/// docs/specs/rust-cli/version.md）。运行时显式传给 TUI、会话库、MCP 握手与账号服务，其他 crate 不读包版本。
const VERSION: &str = env!("ZCODE_BUILD_VERSION");
/// 本机执行环境的 ID。
const LOCAL_ENVIRONMENT: &str = "local";

/// 启动时从进程捕获的事实。
struct Startup {
    /// Node `os.homedir()`（paths `home_dir_with`，config.md §2.3）：本机存储、`~` 展开、凭据密钥与执行环境的 home
    /// 都用它。F57 之前这里在所有平台先看 `HOME`，凭据密钥另取一份（Windows 上 `USERPROFILE`），Windows 上两者不同。
    home: PathBuf,
    cwd: PathBuf,
    /// 进程环境快照（用户命令按白名单投影，shell-permissions.md §2.3；`Debug` 只列变量名）与临时目录。
    env: EnvSnapshot,
    temp: PathBuf,
    /// 系统时区（time-zone.md §1）：启动时读一次。
    zone: Arc<dyn rust_cli_node_store::TimeZone>,
    user_name: String,
    /// `ZCODE_DATA_BASE_DIR`：账号与 Provider 配置的根（Node 相同）。
    data_base_dir: Option<String>,
    /// `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`：显式的内置 Provider 配置。
    builtin_provider_config: Option<String>,
    /// `ZCODE_CREDENTIAL_SECRET`。
    credential_secret: Option<RedactedString>,
    explicit: models::Override,
    /// `ZCODE_MAX_OUTPUT_TOKENS` 的有效值：入口读一次，模型端口、平台层、压缩与内核的缺省共用（F58）。
    max_output_tokens: Option<u32>,
    /// 进程的 uid（Unix；bypass 模式的 root 检查，cli-args.md §5.2）。Windows 没有。
    uid: Option<u32>,
    /// 运行的平台与 Windows 的系统版本（TUI 的按键规则与颜色级别，修复线 F65）。
    os: rust_cli_tui::Os,
}

/// 非空（去掉首尾空白）的环境变量值。
fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取 cwd 的位置（spec §4）"
)]
fn capture() -> Result<Startup> {
    // 修复线 F58：以前除了快照，这里又按名字直接读进程环境（约 13 个变量，`std::env::var` 在 Windows 上不分大小写，
    // 快照按原样比较）。现在先捕获快照，所有变量都从它取，名字的比较只有 `EnvSnapshot::get` 一种（Windows 上不分
    // 大小写，照 Node）。值是快照里的 UTF-8 文本（非 UTF-8 的字节换成 U+FFFD，Node 的 `process.env` 相同）。
    let env: EnvSnapshot = setup::process_env().into();
    let os = |name: &str| env.get(name).map(std::ffi::OsString::from);
    let home = home_dir_with(os, setup::windows_profile)?;
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let var = |name: &str| env.get(name).map(str::to_owned);
    // 修复（R2-6）：以前只取 `TZ` 的名字，时钟一律按 UTC；现在读系统时区，时钟按它给本地日期与偏移。
    let zone = time_zone::system(var("TZ").as_deref());
    let max_output_tokens = limits::max_output_tokens(&env);
    Ok(Startup {
        user_name: user_name_with(os),
        data_base_dir: non_empty(var("ZCODE_DATA_BASE_DIR")),
        builtin_provider_config: non_empty(var("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE")),
        credential_secret: non_empty(var("ZCODE_CREDENTIAL_SECRET")).map(RedactedString::new),
        // 显式覆盖的名字（修复线 F42，config.md §2.5）：只读 `ZCODE_*`；端点不叫 `ZCODE_BASE_URL`（它是
        // Node 的 ZCode 服务地址，`origins.rs`）。
        explicit: models::Override {
            base_url: var("ZCODE_MODEL_BASE_URL"),
            api_key: var("ZCODE_API_KEY").map(RedactedString::new),
            model: non_empty(var("ZCODE_MODEL")),
            // 测试声明只在 debug 构建读（工作线 F52b，model.md §6）。
            assume: options::assumptions(options::TestDeclarations::BUILD, var),
            ssh: options::ssh_assumption(options::TestDeclarations::BUILD, var),
        },
        temp: temp_dir::node_tmpdir(&env, cfg!(windows)),
        max_output_tokens,
        home,
        cwd,
        env,
        zone,
        uid: process_uid(),
        os: os_release::current(),
    })
}

/// 进程的真实 uid（同 `process.getuid()`，proc 的唯一实现，F55）。
#[cfg(unix)]
fn process_uid() -> Option<u32> {
    Some(rust_cli_proc::private::uid())
}

#[cfg(not(unix))]
fn process_uid() -> Option<u32> {
    None
}

/// 一次会话运行：先开运行日志（docs/specs/rust-cli/logging.md），记进程的开始与结束，退出前等日志写完。
/// 返回进程的退出码（工作区信任的对话框里拒绝时不是 0，workspace-trust.md §2.4）。
async fn run(invocation: Invocation) -> Result<u8> {
    let startup = capture()?;
    let paths = setup::storage_paths(&startup, &StorageSettings::default());
    let clock = Arc::new(SystemClock::new(startup.zone.clone()));
    // 只写文件：TUI 占着终端，`-p` 的 stdout 是输出（logging.md §6）。
    let log = log::RunLog::start(paths.logs().as_path().to_path_buf(), clock.clone());
    let mode = match &invocation.mode {
        Mode::Print => "print",
        Mode::Tui => "tui",
        Mode::Version | Mode::Help => "version",
    };
    tracing::info!(
        event = "process.start",
        version = VERSION,
        mode,
        pid = std::process::id(),
        "process started"
    );
    // 退出信号（session-lifecycle.md §3.5）：前端收到令牌后走已有的退出路径，退出码按信号（0 / 143 / 129）。
    let stop = signals::Stop::listen();
    let tokens = (stop.token.clone(), stop.cancel.clone());
    // 模型轨迹（model-io.md §6）：会话组装时打开写者；退出前等它写完（在 `process.exit` 之前）。
    let model_io = model_io::ModelIo::default();
    let result = session(invocation, startup, paths, clock, tokens, &model_io).await;
    model_io.flush().await;
    let result = result.map(|code| stop.code().unwrap_or(code));
    match &result {
        Ok(code) => tracing::info!(
            event = "process.exit",
            status = "completed",
            code = *code,
            "process exiting"
        ),
        Err(error) => tracing::info!(
            event = "process.exit",
            status = "failed",
            error = %format!("{error:#}"),
            "process exiting"
        ),
    }
    log.flush().await;
    result
}

async fn session(
    invocation: Invocation,
    startup: Startup,
    paths: StoragePaths,
    clock: Arc<SystemClock>,
    (stop, cancel): (
        tokio_util::sync::CancellationToken,
        tokio_util::sync::CancellationToken,
    ),
    model_io: &model_io::ModelIo,
) -> Result<u8> {
    let Invocation {
        mode,
        prompt,
        model: model_id,
        target,
        permission_mode,
        allowed,
        disallowed,
        max_turns,
        effort,
        warnings: _,
        mcp: mut mcp_flags,
        plugin_dirs,
        resume_pick,
        fork_session,
        system_prompt,
        append_system_prompt,
        add_dirs,
        session_id,
        settings,
        format,
    } = invocation;
    // `-p` 的输入：prompt 参数与管道里的文字（cli-args.md §2.3）；TUI 不接受 prompt 参数（D8）。stream-json 输入时
    // stdin 是逐行的 user 消息（headless.md §4），不读成 prompt，prompt 参数忽略。
    let prompt = match mode {
        Mode::Print if format.input == rust_cli_headless::InputFormat::StreamJson => {
            Some(prompt.unwrap_or_default())
        }
        Mode::Print => Some(stdin_prompt::print_prompt(prompt).await?),
        _ if prompt.is_some() => {
            return Err(flags::Fatal(
                "Error: a prompt argument needs -p (the interactive interface does not take one)"
                    .to_owned(),
            )
            .into());
        }
        _ => None,
    };
    // 读到输入之后的校验（headless.md §1.1 第 5、6 条）：只有空白的输入、stream-json 输出没有 `--verbose`。
    if let Some(prompt) = &prompt {
        print_format::check_input(format, prompt).map_err(flags::Fatal)?;
    }
    if session_id.is_some() && !matches!(mode, Mode::Print) {
        return Err(flags::Fatal(
            "Error: --session-id needs -p (the interactive interface does not take it)".to_owned(),
        )
        .into());
    }
    let egress = setup::egress(&startup).await?;
    let platform = Arc::new(setup::platform(
        &startup,
        &paths,
        egress.clone(),
        clock.clone(),
    )?);
    // 显式覆盖（ZCODE_API_KEY）时没有模型目录：`/model 名称` 直接换模型（tui.md §6）。
    let explicit = startup
        .explicit
        .api_key
        .as_ref()
        .is_some_and(|key| !key.expose().is_empty());
    let directory: Option<Arc<dyn ModelDirectory>> = (!explicit).then(|| {
        let directory: Arc<dyn ModelDirectory> = platform.clone();
        directory
    });
    // TUI 的输入历史在 `<dataBaseDir>/.zcode/v2`，不随 `storage.*`。
    let history = History::new(&paths);
    // TUI 的 `/login` `/logout` 与模型端口共用同一个平台层（tui.md §7.2）。
    let account = platform.clone();
    let shell = shell::config(
        startup.env.clone(),
        &startup.temp,
        !names_search_tools(&allowed),
    )
    .await;
    let monitor = options::monitor(startup.explicit.assume, shell.bash.is_some());
    // MCP stdio 服务器与 Bash 共用进程树账本（docs/specs/rust-cli/ext.md §3.2）；快照用 shell 改写过 `SHELL` 的那份（F58）。
    let mcp_transport = Arc::new(LocalMcpTransport::new(
        shell.ledger.clone(),
        shell.env.clone(),
        Some(egress.clone()),
    ));
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
        root: EnvPath::new(startup.cwd.to_string_lossy()),
    };
    // 要读文件、看目录的参数（cli-args.md §4、§6、§8）：经一个只做文件操作的执行环境，结果再交给真正的执行环境。
    let resolved = setup::resolve_flags(
        &startup,
        &workspace,
        setup::FlagInputs {
            system: &system_prompt,
            append: &append_system_prompt,
            add_dirs: &add_dirs,
            settings: settings.as_deref(),
        },
    )
    .await?;
    for warning in &resolved.warnings {
        eprint_line(warning);
    }
    let additional: Vec<EnvPath> = resolved.add_dirs.iter().map(EnvPath::new).collect();
    let env = Arc::new(
        LocalEnvironment::new(Some(startup.home.clone()))
            .with_shell(shell)
            // WebFetch 的网络出口（tools-more.md §4），与模型请求共用出口策略。
            .with_web(egress.clone())
            .with_additional_directories(additional.clone()),
    );
    // 入口自己读的本机文件（配置、`.mcp.json`、工作区信任、记忆目录）也经注册表的本机取（remote-exec.md §3）。
    // 只给测试的 loopback（remote-exec.md §18.6）：本机的执行端口经子进程执行器。
    let environments = if let Some(ssh) = &startup.explicit.ssh {
        ssh_executor::environments(&startup, ssh, env, mcp_transport).await?
    } else if startup.explicit.assume.loopback {
        let find_and_grep = !names_search_tools(&allowed);
        loopback::environments(&startup, env, mcp_transport, &additional, find_and_grep).await?
    } else {
        setup::environments(env, mcp_transport)
    };
    // 远端机器（R3，remote-exec.md §20.2）：本机的注册表外面包一层，连上的远端机器放进来、断开时拿走。
    let machines = machines::Machines::new(environments);
    let environments = machines.registry();
    let local_machine = environments.local_machine();
    // hooks、权限规则与 MCP 读 Node 的配置文件（docs/specs/rust-cli/config.md）；模式按 Node 的顺序
    //（命令行 → 本机项目的持久模式 → 配置 → default，config.md §2.2）。
    let mut config = config::assemble(&config::ConfigInputs {
        files: local_machine.files().as_ref(),
        workspace: &workspace,
        paths: &paths,
        process_env: &startup.env,
        flag: resolved.settings.as_ref(),
    })
    .await;
    // 会话库与产物目录按 Node 的 `storage.sessionDbPath` / `storage.dir`（用户级 → 项目级 → 环境变量，
    // config.md §2.3）：先读配置再开库。修复线 F2：以前总在缺省位置，Node 设了这些值时两边各用一个库。
    let paths = setup::storage_paths(&startup, &config.storage.settings());
    // 远端机器的启动输入在这里取（之后 `startup.explicit` 会被移走，§20.3.2）。
    let surface = (matches!(mode, Mode::Tui), matches!(mode, Mode::Print));
    let mut machine_start = machines::Start::new(&paths, surface, (&startup, &egress));
    // 自动更新（docs/specs/rust-cli/update.md §3，F48，照 Codex）：只在 TUI、工作区信任之前；没有启动器标记时什么都
    // 不做。`updates` 留到返回：它持有后台刷新的任务作用域。选了 Update now 时不建会话，安装后以它的结果退出。
    let vars = tui::vars(&startup.env);
    let tui_mode = matches!(mode, Mode::Tui);
    let updates = setup::updates(tui_mode, &startup, &config, &paths, (&egress, &clock)).await;
    let os = startup.os;
    let ask =
        async |info| rust_cli_tui::update::ask(info, &vars, config.locale, config.theme, os).await;
    if let Some(code) = update::offer(updates.as_ref(), ask, &stop, &startup.env).await? {
        return Ok(code);
    }
    // 自动记忆目录的解析还要用 `paths`（options.rs）。
    // 时间与 ID 只有一份（README §4，F7）：内核、会话库的记录、模型、定时任务存储与 TUI 共用。
    let ids = Arc::new(NodeIds::new(clock.clone()));
    let store = Arc::new(NodeStore::new(paths.clone(), ids.clone()).with_version(VERSION));
    // `--session-id`（cli-args.md §7）：会话 ID 用 Node 的写法 `sess_<uuid>`；已有这个会话时报错退出。
    let session_id = match session_id {
        Some(uuid) => Some(setup::fresh_session_id(store.as_ref(), &uuid).await?),
        None => None,
    };
    // 设备身份（platform.md §10，工作线 F52）：每个请求的 `metadata.user_id`；第一次请求时才读文件。
    let device = setup::device_id(&paths, ids.clone());
    let models::Assembled {
        model,
        selection,
        limits,
    } = models::assemble(
        egress,
        platform,
        startup.explicit,
        (model_id, effort),
        (clock.clone(), device),
    )
    .await;
    // stream-json 输入的 MCP prewait 看能不能用工具搜索（headless.md §6.2）：乐观判断，`ENABLE_TOOL_SEARCH` 不是关（工作线
    // F89，tools-more.md §3.2.1）。修改：F84 时看 `tool_search_mode`（禁用表里的模型不算）。
    mcp_flags.tool_search = limits::tool_search(&startup.env).allows();
    // 模型请求的来源头与每请求头的 ID（照 Node，F64，model.md §2.4；`user-agent` 是来源头的一项，账号接口不发）。
    let var = |name: &str| startup.env.get(name).map(str::to_owned);
    let headers = request_headers::request_headers(&var, VERSION, ids.clone());
    // 流空闲看门狗的上限从环境变量快照算出，显式传给模型端口（stall-visibility.md §2.2）。
    let model = model
        .with_stream_idle(options::stream_idle(&startup.env))
        // `IS_SANDBOX`（非空即真）：连续过载时照常重试（cli-args.md §9）。
        .with_overload_retries_in_sandbox(options::is_sandbox(&startup.env))
        // `ZCODE_MAX_OUTPUT_TOKENS`：每个请求的 `max_tokens` 不超过它（cli-args.md §9.1，F42）。
        .with_max_output_tokens(startup.max_output_tokens)
        .with_request_headers(headers.map_err(anyhow::Error::msg)?);
    // 模型轨迹（工作线 F91，model-io.md §3）：目录跟随合并后的 `storage.dir`，全量保留读 App 的设置。
    let model = model_io.attach(
        model,
        model_io::Target {
            env: &startup.env,
            paths: &paths,
            storage_dir: config.storage.dir.as_deref(),
            base: &startup.cwd,
            settings: account.settings().clone(),
        },
    );
    let ports = Ports {
        model: Arc::new(model),
        environments,
        store: store.clone(),
        artifacts: store.clone(),
        clock: clock.clone(),
        ids: ids.clone(),
    };
    // 工作区信任（docs/specs/rust-cli/workspace-trust.md §1）：TUI 在启动扩展、建会话之前问，拒绝就退出；
    // `-p` 不问。
    if matches!(mode, Mode::Tui) {
        let inputs = workspace_trust::Inputs {
            info: local_machine.info().as_ref(),
            files: local_machine.files().as_ref(),
            store: store.as_ref(),
            workspace: &workspace,
            grants: config
                .project_allow
                .as_ref()
                .and_then(|(source, rules)| workspace_trust::grants(source, rules)),
        };
        let (locale, theme) = (config.locale, config.theme);
        let ask = async |prompt| rust_cli_tui::trust::ask(prompt, &vars, locale, theme, os).await;
        // 信任对话框期间收到退出信号：放弃对话框（它的终端守卫随之恢复终端），不建会话。
        let outcome = tokio::select! {
            outcome = workspace_trust::ensure(inputs, ask) => outcome?,
            () = stop.cancelled() => return Ok(0),
        };
        match outcome {
            workspace_trust::Outcome::Continue => {}
            workspace_trust::Outcome::ContinueWithoutGrants => config.without_project_grants(),
            workspace_trust::Outcome::Exit(code) => return Ok(code),
        }
    }
    // 命令行模式只来自 `--permission-mode`（工作线 F63 删掉了排在它前面的 `--dangerously-skip-permissions`，
    // cli-args.md §5.1）。
    let permission_mode =
        config::initial_mode(permission_mode, config.mode, store.as_ref(), &workspace).await;
    flags::refuse_root_bypass(
        permission_mode == PermissionMode::BypassPermissions,
        startup.uid,
        &startup.env,
    )?;
    let defaults = Defaults {
        selection: selection.clone(),
        mode: permission_mode,
        max_turns,
        search_tools: names_search_tools(&allowed),
        task_tools: options::task_tools(&startup.env),
        limits: limits::limits(&startup.env, startup.max_output_tokens),
    };
    let surface = match mode {
        Mode::Print => Surface::Print,
        Mode::Tui | Mode::Version | Mode::Help => Surface::Interactive,
    };
    // 自动记忆目录先建好，提示词的记忆段才能说“目录已存在”（memory.md §2）。
    let mut context = ContextConfig::new(surface);
    context.custom_system_prompt = resolved.system_prompt;
    context.append_system_prompt = resolved.append_system_prompt;
    context.memory_dir = options::memory_dir(
        local_machine.files().as_ref(),
        &paths,
        &workspace,
        &startup.env,
    )
    .await;
    // 压缩的上限两种模式都接（显式覆盖按内置规则，修复线 F61：以前显式覆盖时是“没有目录”，按 200,000）。
    let compact = options::compact(&startup.env, startup.max_output_tokens, limits);
    let mut extensions = setup::extensions(
        local_machine.files().as_ref(),
        &workspace,
        (&startup.env, &startup.cwd, &startup.home),
        (&mcp_flags, &allowed),
        (&plugin_dirs, &paths.mcp_machines()),
        &config,
    )
    .await;
    extensions.monitor = monitor;
    // 绑定到远端机器的 MCP 服务器（R5，remote-exec.md §22.5、§22.8）：连上机器时的期望集合，TUI 的标记。
    machine_start = machine_start.with_mcp(&extensions.config);
    let mcp_machines = machine_start.mcp_marks.clone();
    // 本机跨会话通信（cross-session.md §2）：进程启动时绑定收件箱、写登记；退出时删除。
    let peers = peers::start(
        peers::Startup {
            paths: &paths,
            temp: &startup.temp,
            cwd: &startup.cwd,
            env: &startup.env,
            version: VERSION,
            print: matches!(mode, Mode::Print),
        },
        clock.clone(),
        ids.clone(),
    )
    .await;
    extensions.peers = peers.deps.clone();
    // 持久定时任务：启动工作区（项目根）下的 `.zcode/scheduled_tasks.json`（tools-more.md §16）。
    extensions.cron_store = Some(Arc::new(LocalCronStore::new(
        &workspace.root,
        clock.clone(),
    )));
    // 任务清单（Task v2）放在本地存储：文件是事实源，写后投影到 Node 的 todo 表（agents.md §7）。
    let kernel = LocalKernel::with_extensions(ports, defaults, context, compact, store, extensions);
    // 会话标题只在 TUI 生成（titles.md §2），模型经小模型槽位从会话当前的主模型得出。
    let titles = TitleModel {
        model: kernel.ports().model.clone(),
        directory: directory.clone(),
    };
    let kernel = Arc::new(match mode {
        Mode::Tui => kernel.with_titles(titles),
        Mode::Print | Mode::Version | Mode::Help => kernel,
    });
    kernel.replace_hooks(config.hooks.clone());
    let local = config::local_rules(kernel.ports().store.as_ref(), &workspace).await;
    kernel.replace_permission_rules(
        config.permission_rules(local, cli_rules(&allowed, &disallowed, additional)),
    );
    // 自动连上次连着的机器（§20.3.2）：TUI 在后台；`-p` 只在测试变量下、同步连完（§20.10）。R5（remote-exec.md
    // §22.6）：先于 MCP 启动，绑定到远端机器的服务器看得到“正在连”、等它连完。没有清单里的机器时两者都不做事。
    machines.start(machine_start).await?;
    // 开始连接 MCP 服务器、加载插件。`-p` 只短等（alwaysLoad 的服务器与第一条命令前的 prewait，ext.md §3.3.1），
    // 没连上的在后台继续连；TUI 不等。
    kernel.start_extensions().await;
    if matches!(mode, Mode::Print) {
        // 等 MCP 连接期间收到退出信号：不再等（headless 看到令牌已取消就不开回合）。
        tokio::select! {
            () = kernel.wait_for_extensions() => {}
            () = stop.cancelled() => {}
        }
    }
    match mode {
        Mode::Print => {
            let output = rust_cli_headless::Output {
                format: format.print_format(),
                stamps: Some(rust_cli_headless::Stamps {
                    ids: ids.clone(),
                    clock: clock.clone(),
                }),
                version: VERSION.to_owned(),
                // system/init 的 `apiKeySource`（headless.md §3.3）：key 来自哪个变量（本工程读 `ZCODE_API_KEY`，
                // config.md §2.5）。
                api_key_source: if explicit { "ZCODE_API_KEY" } else { "none" }.to_owned(),
            };
            let options = PrintOptions {
                prompt: prompt.unwrap_or_default(),
                session_id,
                workspace,
                selection,
                mode: permission_mode,
                target,
                fork: fork_session,
                stop,
                cancel,
                output,
            };
            let stdout = &mut tokio::io::stdout();
            let printed = match format.input {
                rust_cli_headless::InputFormat::Text => {
                    rust_cli_headless::run(kernel, options, stdout).await
                }
                // stdin 每行一条 user 消息（headless.md §4）。
                rust_cli_headless::InputFormat::StreamJson => {
                    let input = tokio::io::BufReader::new(tokio::io::stdin());
                    rust_cli_headless::run_stream(kernel, options, input, stdout).await
                }
            };
            peers.shutdown().await;
            printed?;
            // 未信任的项目 hooks 被跳过：按 Node 写一段提示到 stderr（config.md §4.4）。
            if let Some(pending) = &config.pending {
                eprint_raw(&pending.notice());
            }
            Ok(0)
        }
        Mode::Tui => {
            // 输入历史：启动时读本工作区的，写者任务按发送顺序追加（tui.md「输入历史」）。
            let loaded = history.load(workspace.root.as_str()).await;
            let (recorder, writer) = history.writer(workspace.root.as_str().to_owned());
            let scope = TaskScope::root("tui-history", |_| {});
            let writer = scope.spawner().spawn("history-writer", writer);
            let inputs = tui::Inputs {
                workspace: workspace.clone(),
                cwd: &startup.cwd,
                home: &startup.home,
                env: &startup.env,
                temp: &startup.temp,
                selection,
                mode: permission_mode,
                models: directory,
                history: loaded,
                recorder,
                startup: tui::startup(&target, resume_pick),
                fork: fork_session,
                locale: config.locale,
                theme: config.theme,
                paths: paths.clone(),
                account,
                mcp: tui_mcp::McpConfigFiles {
                    origins: config.mcp_origins.clone(),
                    files: local_machine.files().clone(),
                    ids: kernel.ports().ids.clone(),
                    workspace: workspace.clone(),
                },
                stop,
                clock,
                ids,
                update: updates.as_ref().and_then(update::Updates::notice),
                trust: Arc::new(workspace_trust::InAppTrust {
                    info: local_machine.info().clone(),
                    files: local_machine.files().clone(),
                    store: kernel.ports().store.clone(),
                    accepted: Arc::default(),
                }),
                os,
                question_timeout: config.question_timeout,
                machines: (machines.service(), mcp_machines),
            };
            let exit = rust_cli_tui::run(kernel, tui::options(inputs, VERSION)).await;
            // TUI 退出后入口全部释放：等写者写完剩下的；跨会话的登记与 socket 删掉。
            let _ = writer.await;
            peers.shutdown().await;
            let exit = exit?;
            // 退出的顺序（session-lifecycle.md §3.5、§6.2）：终端已在 `run` 返回前恢复，先打印
            // 恢复提示，再跑 SessionEnd，hook 的提示写 stderr。缺陷修复（F14）：以前 SessionEnd 在恢复终端之前跑，
            // 恢复提示排在 hook 的提示之后。
            // 终端关掉时（SIGHUP）stdout / stderr 已经写不进去：写失败不能 panic，否则 SessionEnd 跑不到（恢复提示
            // 与 hook 提示的写入都吞掉错误）。
            if let Some(session) = &exit.session {
                write_lossy(
                    std::io::stdout(),
                    &format!("\nResume this session with:\nzcode --resume {session}\n"),
                );
            }
            for notice in exit.ending.run().await {
                write_lossy(std::io::stderr(), &notice);
            }
            Ok(0)
        }
        Mode::Version | Mode::Help => {
            peers.shutdown().await;
            Ok(0)
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程参数的位置（README §4 的环境类入口）"
)]
fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

fn main() -> ExitCode {
    // 内嵌的 rg：以工具的名字启动时直接跑它，不做 zcode 的任何初始化（embedded-search.md §3，F53）。
    if let Some(code) = embedded::dispatch() {
        return code;
    }
    let args = args();
    if let Some(code) = app_server::dispatch(&args) {
        return code;
    }
    // 登录与退出（Node：用法错误也是 `Error: …`、退出码 1）。
    let job = match login::parse(&args) {
        Some(Ok(command)) => Job::Account(command),
        Some(Err(usage)) => {
            eprint_line(&format!("Error: {usage}"));
            return ExitCode::FAILURE;
        }
        None => match hooks_trust::parse(&args) {
            Some(Ok(command)) => Job::Trust(command),
            Some(Err(text)) => {
                eprint_raw(&text);
                return ExitCode::FAILURE;
            }
            // `zcode update` / `upgrade`（update.md §9，F48）：commander 的报错原样写 stderr，退出码 1。
            None => match update::parse(&args) {
                Some(Ok(command)) => Job::Update(command),
                Some(Err(text)) => {
                    eprint_line(&text);
                    return ExitCode::FAILURE;
                }
                None => match parse(&args) {
                    Ok(Invocation {
                        mode: Mode::Version,
                        ..
                    }) => {
                        print_line(&version_line(VERSION));
                        return ExitCode::SUCCESS;
                    }
                    // 帮助写 stdout、退出码 0（commander `outputHelp`，cli-args.md §3）。
                    Ok(Invocation {
                        mode: Mode::Help, ..
                    }) => {
                        print_raw(&help::render(help_width()));
                        return ExitCode::SUCCESS;
                    }
                    // 解析时的警告（`--effort` 的值不认识，cli-args.md §1.4）先写 stderr，照常运行。
                    Ok(invocation) => {
                        for warning in &invocation.warnings {
                            eprint_line(warning);
                        }
                        Job::Session(Box::new(invocation))
                    }
                    // 缺陷修复（F40）：以前是自拟的 `zcode: unsupported arguments: …`、退出码 2；正确的是
                    // commander 的原文（`error: unknown option '…'` 等）或 `Error: …`，退出码 1（cli-args.md §2.2）。
                    Err(message) => {
                        eprint_line(&message);
                        return ExitCode::FAILURE;
                    }
                },
            },
        },
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprint_line(&format!("zcode: cannot start the async runtime: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let outcome = match job {
        Job::Account(command) => runtime.block_on(setup::account(command)),
        Job::Trust(command) => runtime.block_on(setup::trust(command)),
        Job::Update(command) => {
            let env = EnvSnapshot::from(setup::process_env());
            runtime.block_on(update::command(command, &env, &mut std::io::stdout()))
        }
        Job::Session(invocation) => runtime.block_on(run(*invocation)),
    };
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            // 标准代理变量格式不对：原样写这几行、不带前缀，退出码 1（config.md §2.4）；参数错误同样原样
            // （`Error: …`，cli-args.md §2.4）。
            if let Some(invalid) = error.downcast_ref::<network::InvalidProxy>() {
                eprint_line(&invalid.to_string());
            } else if let Some(fatal) = error.downcast_ref::<flags::Fatal>() {
                eprint_line(&fatal.to_string());
            // json / stream-json 的最后一个 result 是错误：输出已写，只按退出码 1 结束（headless.md §5）。
            } else if error.is::<rust_cli_headless::ResultIsError>() {
                // stream-json 输入的行错误：固定的文字，退出码 1（headless.md §4）。
            } else if let Some(input) = error.downcast_ref::<rust_cli_headless::InputError>() {
                eprint_line(&input.0);
            } else {
                eprint_line(&format!("zcode: {error:#}"));
            }
            ExitCode::FAILURE
        }
    }
}

/// 这次启动要做的事。
enum Job {
    Account(login::Command),
    /// `zcode hooks trust …`（config.md §4.3）。
    Trust(hooks_trust::TrustCommand),
    /// `zcode update`（update.md §9，F48）。
    Update(update::Command),
    Session(Box<Invocation>),
}
