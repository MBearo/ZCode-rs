mod args;
use anyhow::{Context, Result};
use args::{AppServerArgs, Cli, Command};
use clap::Parser;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zcode_cli_app_server::{self as app_server, Sink, stdio};
use zcode_cli_core::Engine;
use zcode_cli_core_api::{ModelIdentity, ModelPort, ModelRegistry, RuntimePorts};
use zcode_cli_host::{SystemClock, WorkspaceContext, legacy_paths};
use zcode_cli_model::{config::ModelConfig, provider::HttpModel, registry::Registry};
use zcode_cli_net::{Egress, NetworkPolicy};
use zcode_cli_state::Store;
use zcode_cli_tools::WorkspaceTools;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // stderr 断管也不能递归进入错误处理；stdout 永远只用于协议。
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "zcode-cli-rust: {error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    match Cli::parse().command {
        Command::AppServer(args) => app_server(args).await,
        // TUI 入口先占位：保留子命令与退出码契约，避免未实现的前端伪装成可用。
        Command::Tui => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "zcode-cli-rust: TUI 尚未实现，请使用 app-server --stdio"
            );
            std::process::exit(zcode_cli_tui::UNAVAILABLE_EXIT_CODE);
        }
    }
}

async fn app_server(args: AppServerArgs) -> Result<()> {
    let home = std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let log_options = zcode_cli_host::logging::LogOptions::from_env(&home);
    let log_dir = log_options.directory.clone();
    let _logs = zcode_cli_host::logging::init(log_options);
    zcode_cli_host::log_retention::schedule(log_dir);
    tracing::info!(
        target: "zcode::runtime",
        event = "runtime.started",
        version = env!("CARGO_PKG_VERSION"),
        surface = args.surface.as_str(),
        prepare_storage = args.prepare_storage,
        "App server starting"
    );
    let question_timing = zcode_cli_host::question_timing()?;
    let requested_cwd = args.cwd.unwrap_or(std::env::current_dir()?);
    let cwd = tokio::fs::canonicalize(&requested_cwd)
        .await
        .context("Workspace unavailable")?;
    let requested_data = args.data_dir.unwrap_or_else(|| {
        std::env::var_os("ZCODE_CLI_RUST_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(
                    std::env::var_os("HOME")
                        .or_else(|| std::env::var_os("USERPROFILE"))
                        .unwrap_or_default(),
                )
                .join(".zcode/rust")
            })
    });
    let data_dir = if requested_data.is_absolute() {
        requested_data
    } else {
        std::env::current_dir()?.join(requested_data)
    };
    tokio::fs::create_dir_all(&data_dir).await?;
    let data_dir = tokio::fs::canonicalize(data_dir).await?;
    let path = data_dir.join("rust-sessions.sqlite");
    // 身份使用 Host 提交的路径，不把 macOS /var -> /private/var 的 realpath 改写成新工作区。
    let workspace = zcode_cli_host::workspace_identity(
        std::env::var("ZCODE_WORKSPACE_IDENTITY").ok().as_deref(),
        &requested_cwd,
    );
    // 与 Node 启动时净化 process.env 等价：只捕获一次，所有读取方共用这份视图，不改真实进程环境。
    let runtime_env = Arc::new(zcode_cli_rust::runtime_env(&home));
    // 配置按 Node 分层规则从 cwd 解析；各入口每次重新加载，与 Node 一致没有文件监听。
    let workspace_config = Arc::new(zcode_cli_host::WorkspaceConfig::new(
        std::path::absolute(&requested_cwd)?,
        home.clone(),
        runtime_env.vars().to_vec(),
    ));
    // 网络出口与权限配置与 Node 一样取启动时的配置快照，运行中修改不影响已建立的出口。
    let startup_config = workspace_config.snapshot().await;
    let egress = Arc::new(
        Egress::new(
            runtime_env.clone(),
            &NetworkPolicy::from_config(&startup_config.config["network"]),
            &home,
            "electron",
        )
        .map_err(anyhow::Error::msg)
        .context("Invalid network configuration")?,
    );
    let cancel = CancellationToken::new();
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            if let Ok(mut term) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        signal_cancel.cancel();
    });
    let input_closed = CancellationToken::new();
    let mut input = stdio::start(input_closed.clone());
    let (mut output, writer) = Sink::stdout(cancel.clone());
    let attempt = zcode_cli_host::id();
    let database_id = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
    let progress = |phase: &str, sequence: u64| json!({"method":"startup/storageState","params":{"schemaVersion":1,"attemptId":attempt,"sequence":sequence,"databaseId":database_id,"databaseKind":"session","phase":phase,"elapsedMs":0}});
    if args.prepare_storage {
        stdio::storage_prepare(&path, &mut input, &mut output).await?;
    }
    let _owner = if args.prepare_storage {
        None
    } else {
        Some(Store::lock_workspace(data_dir.clone(), workspace.clone()).await?)
    };
    output.send_values(vec![progress("checking", 1)]).await?;
    let store = match Store::open(path).await {
        Ok(store) => store,
        Err(_) => {
            let mut frame = progress("failed", 2);
            frame["params"]["errorCode"] = "sql_failed".into();
            output.send_values(vec![frame]).await?;
            drop(output);
            let _ = stdio::finish(writer).await;
            anyhow::bail!("Session storage failed");
        }
    };
    if !args.prepare_storage {
        let import_cancel = cancel.child_token();
        let imported = async {
            if let Some(source) = legacy_paths::resolve(
                args.import_ts_db,
                &requested_cwd,
                args.config.is_none(),
                &workspace_config.snapshot().await,
            )
            .await?
            {
                if tokio::fs::try_exists(&source.database).await? {
                    let operation = store.import_ts(
                        source.database,
                        workspace.clone(),
                        requested_cwd.to_string_lossy().into_owned(),
                        data_dir.clone(),
                        source.artifacts,
                        import_cancel.clone(),
                    );
                    tokio::pin!(operation);
                    // 只在实际导入期间处理 EOF；无导入时保留输入缓冲区交由 actor 排空。
                    let result = tokio::select! {
                        result = &mut operation => result,
                        _ = input_closed.cancelled() => {
                            import_cancel.cancel();
                            operation.await
                        }
                        _ = cancel.cancelled() => {
                            import_cancel.cancel();
                            operation.await
                        }
                    };
                    if import_cancel.is_cancelled() || input_closed.is_cancelled() {
                        return Ok(true);
                    }
                    result?;
                } else {
                    anyhow::ensure!(!source.required, "Explicit TS import source does not exist");
                }
            }
            Ok::<_, anyhow::Error>(false)
        }
        .await;
        if matches!(imported, Ok(true)) {
            drop(output);
            stdio::finish(writer).await?;
            return Ok(());
        }
        if let Err(error) = imported {
            let mut frame = progress("failed", 2);
            frame["params"]["errorCode"] = "sql_failed".into();
            output.send_values(vec![frame]).await?;
            drop(output);
            let _ = stdio::finish(writer).await;
            anyhow::bail!("TS history import failed; source remains unchanged: {error:#}");
        }
    }
    output.send_values(vec![progress("ready", 2)]).await?;
    if args.prepare_storage {
        drop(store);
        output
            .send_values(vec![
                json!({"method":"startup/storagePrepared","params":{}}),
            ])
            .await?;
    } else {
        let config = ModelConfig::load(args.config.as_ref()).await?;
        let registry = if config.is_none() {
            Registry::from_env(egress.clone())
                .await?
                .map(|r| r as Arc<dyn ModelRegistry>)
        } else {
            None
        };
        let identity = config.as_ref().map(|c| ModelIdentity {
            provider_id: c.provider_id.clone(),
            model_id: c.model_id.clone(),
            reasoning_level: c.reasoning_level.clone(),
        });
        let model = config
            .map(|c| HttpModel::new(c, egress.clone()))
            .map(|m| Arc::new(m) as Arc<dyn ModelPort>);
        // 与 Desktop 共用的项目 hook 信任存储：路径由用户配置文件的 storage.dir 决定。
        // 用户配置不可读时不启用信任（项目 hooks 不运行，保持 fail-closed），不影响启动。
        let user_config = std::path::Path::new(&startup_config.user_path);
        let trust_store =
            match zcode_cli_host::trust_store::trust_store_path(&home, user_config).await {
                Ok(path) => Some(Arc::new(zcode_cli_host::trust_store::FileTrustStore::new(
                    path,
                ))),
                Err(error) => {
                    tracing::warn!(
                        event = "workspace_hook.trust_store_unavailable",
                        error = %format!("{error:#}"),
                        "Workspace Hook Trust store is unavailable"
                    );
                    None
                }
            };
        let engine = Engine::new(
            workspace,
            identity,
            RuntimePorts {
                context: Arc::new(WorkspaceContext::new(
                    cwd.clone(),
                    std::env::var_os("HOME")
                        .filter(|s| !s.is_empty())
                        .or_else(|| std::env::var_os("USERPROFILE"))
                        .map(std::path::PathBuf::from)
                        .unwrap_or_default(),
                    args.surface == "desktop",
                    runtime_env.vars().into(),
                )),
                store: Arc::new(store),
                model,
                tools: Arc::new(WorkspaceTools::new(
                    cwd,
                    data_dir.join("tool-results"),
                    workspace_config.clone(),
                    egress.clone(),
                )),
                clock: Arc::new(SystemClock),
            },
        )
        .await?
        .with_question_timing(question_timing.0, question_timing.1)
        .with_permission_config(&startup_config.config["permission"])
        .with_hooks(&startup_config.config["hooks"], &startup_config.user_path)
        .with_registry(registry, requested_cwd.to_string_lossy().into_owned());
        let engine = match trust_store {
            Some(store) => engine.with_workspace_trust(
                store,
                workspace_config.clone(),
                Some(env!("CARGO_PKG_VERSION").into()),
            ),
            None => engine,
        };
        // App Server 独占 stdout；返回时已排空 runtime 输出并释放 sink。
        let served =
            app_server::serve(move |rx, tx| engine.serve(rx, tx, cancel), input, output).await;
        // 失败路径同样先等写线程落盘：存储失败的错误响应必须送达 Host 后进程才能退出。
        stdio::finish(writer).await?;
        return served;
    }
    drop(output);
    stdio::finish(writer).await?;
    Ok(())
}
