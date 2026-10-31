//! `zcode app-server --stdio [--surface desktop]`（docs/specs/rust-cli/app-protocol.md §3.7、§10）：App 的 Host 经 stdio
//! 拉起的协议前端。参数照 Host 现在给 Node 的（`ZA:zcodeAgentProcessManager.ts` 的 `applyPresentationSurfaceToCommand`）。
//!
//! 组装与 `-p` 相同的内核（同一套提示词与工具，Surface 是交互形态；不问工作区信任、不生成标题、不连远端机器、
//! 不登记跨会话通信），区别只在注入的东西：平台层的账号凭据来源经 Host（G8），前端是 `protocol-zcode`。
//! 写者先起：Host 要求拉起后 30 s 内有 `startup/storageState`。

use std::hash::{Hash, Hasher};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rust_cli_base::TaskScope;
use rust_cli_env_local::{LocalCronStore, LocalEnvironment, LocalMcpTransport};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::store::{SessionFilter, SessionStore};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::StorageSettings;
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_zcode::{Context, Defaults, HostHeaders, Startup};
use rust_cli_runtime::{ContextConfig, Defaults as KernelDefaults, LocalKernel, Ports, Surface};

use crate::args::names_search_tools;
use crate::output::eprint_line;
use crate::{
    LOCAL_ENVIRONMENT, VERSION, config, limits, log, mcp, model_io, models, options, setup,
};

/// 退出时等进程级任务收尾的上限（存活的上限，不用来同步状态）。
const SCOPE_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// 子命令的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// `--surface`（Host 给 `desktop`）。P1 不按它改提示词（app-protocol.md §13 第 4 项）。
    pub(crate) surface: Option<String>,
}

/// `app-server` 子命令；不是它时为 None。只认 `--stdio` 与 `--surface`。
pub(crate) fn parse(args: &[String]) -> Option<Result<Parsed, String>> {
    let (first, rest) = args.split_first()?;
    if first != "app-server" {
        return None;
    }
    let mut stdio = false;
    let mut surface = None;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--stdio" => stdio = true,
            "--surface" => match rest.next() {
                Some(value) => surface = Some(value.clone()),
                None => {
                    return Some(Err(
                        "error: option '--surface <surface>' argument missing".into()
                    ));
                }
            },
            other => match other.strip_prefix("--surface=") {
                Some(value) => surface = Some(value.to_owned()),
                None => return Some(Err(format!("error: unknown option '{other}'"))),
            },
        }
    }
    if !stdio {
        return Some(Err("error: app-server needs --stdio".to_owned()));
    }
    Some(Ok(Parsed { surface }))
}

/// `executor` 与 `app-server` 两个子命令（不走常规命令行参数的解析）：是其中之一时返回退出码。
pub(crate) fn dispatch(args: &[String]) -> Option<ExitCode> {
    // `zcode executor`（远端执行 R1 / R2，remote-exec.md §12、§18.6、§19.3）：执行器模式，不读凭据、不开会话库。
    if let Some(parsed) = crate::executor::parse(args) {
        return Some(crate::executor::main(parsed));
    }
    let parsed = match parse(args)? {
        Ok(parsed) => parsed,
        Err(message) => {
            eprint_line(&message);
            return Some(ExitCode::FAILURE);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprint_line(&format!("zcode: cannot start the async runtime: {error}"));
            return Some(ExitCode::FAILURE);
        }
    };
    Some(match runtime.block_on(run(parsed)) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprint_line(&format!("zcode: {error:#}"));
            ExitCode::FAILURE
        }
    })
}

/// 会话库的不透明 ID：库路径的散列（固定的键，同一个库总是同一个 ID；不带路径）。
fn database_id(path: &std::path::Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("session-db-{:016x}", hasher.finish())
}

async fn run(parsed: Parsed) -> Result<u8> {
    let startup = crate::capture()?;
    let paths = setup::storage_paths(&startup, &StorageSettings::default());
    let clock = Arc::new(SystemClock::new(startup.zone.clone()));
    // 日志只写文件：stdout 是协议（logging.md §6）。
    let run_log = log::RunLog::start(paths.logs().as_path().to_path_buf(), clock.clone());
    tracing::info!(
        event = "process.start",
        version = VERSION,
        mode = "app-server",
        surface = parsed.surface.as_deref().unwrap_or("none"),
        pid = std::process::id(),
        "process started"
    );
    let scope = TaskScope::root("app-server", |panic| {
        tracing::error!(event = "task.panic", scope = %panic.scope, task = panic.task, message = %panic.message, "a task panicked");
    });
    let options = Options::default();
    let (peer, writer) = connection::start(tokio::io::stdout(), &scope.spawner(), options);
    let context = Arc::new(Context::new(peer, clock.clone()));
    let stop = crate::signals::Stop::listen();
    let model_io = model_io::ModelIo::default();
    let result = serve(
        startup,
        paths,
        clock,
        context,
        scope.spawner(),
        &model_io,
        &stop,
    )
    .await;
    model_io.flush().await;
    let code = match &result {
        Ok(()) => stop.code().unwrap_or(0),
        Err(_) => 1,
    };
    tracing::info!(event = "process.exit", code, "process exiting");
    // 写者写完队列里剩下的帧再退出。
    drop(writer);
    let _ = scope.close(SCOPE_CLOSE_GRACE).await;
    run_log.flush().await;
    result.map(|()| code)
}

async fn serve(
    startup: crate::Startup,
    paths: rust_cli_paths::StoragePaths,
    clock: Arc<SystemClock>,
    context: Arc<Context>,
    spawner: rust_cli_base::Spawner,
    model_io: &model_io::ModelIo,
    stop: &crate::signals::Stop,
) -> Result<()> {
    let state = Startup::new(context.clone(), database_id(paths.database().as_path()));
    state.checking().await;
    let egress = setup::egress(&startup).await?;
    // 缺口 G8：账号模型的凭据向 Host 要（与 Node 相同经反向请求）；API key 的 provider 不受影响。
    let platform = Arc::new(
        setup::platform(&startup, &paths, egress.clone(), clock.clone())?
            .with_header_source(Arc::new(HostHeaders::new(context.clone()))),
    );
    let account = platform.clone();
    let shell =
        crate::shell::config(startup.env.clone(), &startup.temp, !names_search_tools(&[])).await;
    let monitor = options::monitor(&startup.explicit.assume, shell.bash.is_some());
    let mcp_transport = Arc::new(LocalMcpTransport::new(
        shell.ledger.clone(),
        shell.env.clone(),
        Some(egress.clone()),
    ));
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
        root: EnvPath::new(startup.cwd.to_string_lossy()),
    };
    let env = Arc::new(
        LocalEnvironment::new(Some(startup.home.clone()))
            .with_shell(shell)
            .with_web(egress.clone()),
    );
    let environments = setup::environments(env, mcp_transport);
    let local_machine = environments.local_machine();
    let config = config::assemble(&config::ConfigInputs {
        files: local_machine.files().as_ref(),
        workspace: &workspace,
        paths: &paths,
        process_env: &startup.env,
        flag: None,
    })
    .await;
    let paths = setup::storage_paths(&startup, &config.storage.settings());
    let ids = Arc::new(NodeIds::new(clock.clone()));
    let store = Arc::new(NodeStore::new(paths.clone(), ids.clone()).with_version(VERSION));
    // 会话库：打开一次（node-store 在打开连接时校验迁移）；失败时报 failed，不再往下组装。
    if let Err(error) = store
        .list(SessionFilter {
            workspace: workspace.clone(),
            limit: 1,
        })
        .await
    {
        tracing::warn!(event = "app.storage.failed", error = %error, "opening the session database failed");
        state.failed("open_failed").await;
        anyhow::bail!("opening the session database failed: {error}");
    }
    let device = setup::device_id(&paths, ids.clone());
    let models::Assembled {
        model,
        selection,
        limits: model_limits,
    } = models::assemble(
        egress,
        platform,
        startup.explicit,
        (None, None),
        (clock.clone(), device),
    )
    .await;
    let var = |name: &str| startup.env.get(name).map(str::to_owned);
    let headers = crate::request_headers::request_headers(&var, VERSION, ids.clone());
    let model = model
        .with_stream_idle(options::stream_idle(&startup.env))
        .with_overload_retries_in_sandbox(options::is_sandbox(&startup.env))
        .with_max_output_tokens(startup.max_output_tokens)
        .with_request_headers(headers.map_err(anyhow::Error::msg)?);
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
    let mode = config::initial_mode(None, config.mode, store.as_ref(), &workspace).await;
    let kernel_defaults = KernelDefaults {
        selection: selection.clone(),
        mode,
        max_turns: None,
        search_tools: names_search_tools(&[]),
        task_tools: options::task_tools(&startup.env),
        limits: limits::limits(&startup.env, startup.max_output_tokens),
    };
    let mut context_config = ContextConfig::new(Surface::Interactive);
    context_config.memory_dir = options::memory_dir(
        local_machine.files().as_ref(),
        &paths,
        &workspace,
        &startup.env,
    )
    .await;
    let compact = options::compact(&startup.env, startup.max_output_tokens, model_limits);
    let mcp_flags = mcp::McpFlags {
        tool_search: limits::tool_search(&startup.env).allows(),
        ..mcp::McpFlags::default()
    };
    let mut extensions = setup::extensions(
        local_machine.files().as_ref(),
        &workspace,
        (&startup.env, &startup.cwd, &startup.home),
        (&mcp_flags, &[]),
        (&[], &paths.mcp_machines()),
        &config,
    )
    .await;
    extensions.monitor = monitor;
    extensions.cron_store = Some(Arc::new(LocalCronStore::new(
        &workspace.root,
        clock.clone(),
    )));
    let kernel = LocalKernel::with_extensions(
        ports,
        kernel_defaults,
        context_config,
        compact,
        store,
        extensions,
    );
    kernel.replace_hooks(config.hooks.clone());
    let local = config::local_rules(kernel.ports().store.as_ref(), &workspace).await;
    kernel.replace_permission_rules(
        config.permission_rules(local, crate::args::cli_rules(&[], &[], Vec::new())),
    );
    let kernel = Arc::new(kernel);
    kernel.start_extensions().await;
    state.ready().await;
    let defaults = Defaults {
        selection,
        mode,
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
    };
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    // 读到 Host 关掉输入或收到退出信号时结束（Host 回收整棵进程树）。
    tokio::select! {
        result = rust_cli_protocol_zcode::serve(input, context, kernel, defaults, spawner, Options::default()) => {
            result?;
        }
        () = stop.token.cancelled() => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "app_server_tests.rs"]
mod tests;
