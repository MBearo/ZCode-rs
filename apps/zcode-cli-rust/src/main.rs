mod args;
use anyhow::{Context, Result};
use args::Args;
use clap::Parser;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zcode_cli_app_server as stdio;
use zcode_cli_core::Engine;
use zcode_cli_core_api::{ModelIdentity, ModelPort, ModelRegistry, RuntimePorts};
use zcode_cli_host::{SystemClock, WorkspaceContext, legacy_paths};
use zcode_cli_model::{config::ModelConfig, provider::HttpModel, registry::Registry};
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
    let args = Args::parse();
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
    let (mut input, output, writer) = stdio::start(cancel.clone(), input_closed.clone());
    let attempt = zcode_cli_host::id();
    let database_id = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
    let progress = |phase: &str, sequence: u64| json!({"method":"startup/storageState","params":{"schemaVersion":1,"attemptId":attempt,"sequence":sequence,"databaseId":database_id,"databaseKind":"session","phase":phase,"elapsedMs":0}});
    if args.prepare_storage {
        stdio::storage_prepare(&path, &mut input, &output).await?;
    }
    let _owner = if args.prepare_storage {
        None
    } else {
        Some(Store::lock_workspace(data_dir.clone(), workspace.clone()).await?)
    };
    output.send(vec![progress("checking", 1)]).await?;
    let store = match Store::open(path).await {
        Ok(store) => store,
        Err(_) => {
            let mut frame = progress("failed", 2);
            frame["params"]["errorCode"] = "sql_failed".into();
            output.send(vec![frame]).await?;
            drop(output);
            let _ = stdio::finish(writer).await;
            anyhow::bail!("Session storage failed");
        }
    };
    if !args.prepare_storage {
        let import_cancel = cancel.child_token();
        let imported = async {
            if let Some(source) =
                legacy_paths::resolve(args.import_ts_db, &requested_cwd, args.config.is_none())
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
            output.send(vec![frame]).await?;
            drop(output);
            let _ = stdio::finish(writer).await;
            anyhow::bail!("TS history import failed; source remains unchanged: {error:#}");
        }
    }
    output.send(vec![progress("ready", 2)]).await?;
    if args.prepare_storage {
        drop(store);
        output
            .send(vec![
                json!({"method":"startup/storagePrepared","params":{}}),
            ])
            .await?;
    } else {
        let config = ModelConfig::load(args.config.as_ref()).await?;
        let registry = if config.is_none() {
            Registry::from_env()
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
            .map(HttpModel::new)
            .map(|m| Arc::new(m) as Arc<dyn ModelPort>);
        Engine::new(
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
                )),
                store: Arc::new(store),
                model,
                tools: Arc::new(WorkspaceTools::new(cwd, data_dir.join("tool-results"))),
                clock: Arc::new(SystemClock),
            },
        )
        .await?
        .with_question_timing(question_timing.0, question_timing.1)
        .with_registry(registry, requested_cwd.to_string_lossy().into_owned())
        .serve(input, output.clone(), cancel)
        .await?;
    }
    drop(output);
    stdio::finish(writer).await?;
    Ok(())
}
