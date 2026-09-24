mod args;
mod prompt;
mod runtime;
use anyhow::Result;
use args::{AppServerArgs, Cli, Command};
use clap::Parser;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zcode_cli_app_server::{self as app_server, Sink, stdio};
use zcode_cli_core_api::SessionStore;
use zcode_cli_host::legacy_paths;
use zcode_cli_state::Store;

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // 子命令走 clap；其余与 Node 一样由顶层参数决定（-p 无头运行或 TUI）。
    if !matches!(argv.first().map(String::as_str), Some("app-server" | "tui")) {
        std::process::exit(prompt::main(&argv).await);
    }
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
        Command::Tui => tui_unavailable(),
    }
}

/// TUI 入口先占位：保留子命令与退出码契约，避免未实现的前端伪装成可用。
pub(crate) fn tui_unavailable() -> ! {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "zcode-cli-rust: TUI 尚未实现，请使用 app-server --stdio 或 -p"
    );
    std::process::exit(zcode_cli_tui::UNAVAILABLE_EXIT_CODE);
}

async fn app_server(args: AppServerArgs) -> Result<()> {
    let home = runtime::home();
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
    let ctx = runtime::Context::prepare(args.cwd, args.data_dir).await?;
    let node_storage = runtime::node_storage(args.node_storage);
    let path = if node_storage {
        ctx.node_database().await?.database
    } else {
        ctx.data_dir.join("rust-sessions.sqlite")
    };
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
        Some(Store::lock_workspace(ctx.data_dir.clone(), ctx.workspace.clone()).await?)
    };
    output.send_values(vec![progress("checking", 1)]).await?;
    let opened = if node_storage {
        ctx.node_store()
            .await
            .map(|store| (Arc::new(store) as Arc<dyn SessionStore>, None))
    } else {
        Store::open(path).await.map(|store| {
            (
                Arc::new(store.clone()) as Arc<dyn SessionStore>,
                Some(store),
            )
        })
    };
    let (store, rust_store) = match opened {
        Ok(store) => store,
        Err(error) => {
            let mut frame = progress("failed", 2);
            // Node `DatabaseStartupErrorCode`：迁移与打开失败按原因上报，其余为 sql_failed。
            frame["params"]["errorCode"] = error
                .downcast_ref::<zcode_cli_state::node::open::StartupError>()
                .map_or("sql_failed", |e| e.code)
                .into();
            output.send_values(vec![frame]).await?;
            drop(output);
            let _ = stdio::finish(writer).await;
            anyhow::bail!("Session storage failed");
        }
    };
    if let (false, Some(rust_store)) = (args.prepare_storage, &rust_store) {
        let import_cancel = cancel.child_token();
        let imported = async {
            if let Some(source) = legacy_paths::resolve(
                args.import_ts_db,
                &ctx.requested_cwd,
                args.config.is_none(),
                &ctx.workspace_config.snapshot().await,
            )
            .await?
            {
                if tokio::fs::try_exists(&source.database).await? {
                    let operation = rust_store.import_ts(
                        source.database,
                        ctx.workspace.clone(),
                        ctx.requested_cwd.to_string_lossy().into_owned(),
                        ctx.data_dir.clone(),
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
        let engine = ctx
            .engine(store, args.config.as_ref(), args.surface == "desktop")
            .await?;
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
