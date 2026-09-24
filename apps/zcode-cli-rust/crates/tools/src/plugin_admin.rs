//! Marketplace and install methods over the plugins crate (spec
//! rust-m10-4-plugin-sources). They run under the storage root's lock, like
//! Node's `withPluginStorageLock`.
use super::tools::WorkspaceTools;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;
use tokio_util::sync::CancellationToken;
use zcode_cli_plugins as plugins;
use zcode_cli_plugins::install_admin::{self, Install};
use zcode_cli_plugins::market_admin::{self, Context};

pub(super) const METHODS: [&str; 7] = [
    "plugins/marketplace/add",
    "plugins/marketplace/remove",
    "plugins/marketplace/update",
    "plugins/install",
    "plugins/update",
    "plugins/validate",
    "plugins/describe",
];

/// Restores a suppressed built-in requested through `plugins/install` and
/// reports it from a fresh discovery (Node `restoreBuiltinPluginCore` path).
async fn restore(
    tools: &WorkspaceTools,
    id: &str,
    user_path: &Path,
    cancel: &CancellationToken,
) -> Result<Value> {
    let env = super::plugin_requests::env_lookup(tools);
    let lookup = |name: &str| env.get(name).map(|v| (*v).to_owned());
    let paths = plugins::mutations::Paths {
        user: user_path,
        cwd: &tools.cwd,
        project: &[],
    };
    let cua = plugins::overview::cua_enabled(&lookup);
    plugins::mutations::restore_builtin(&paths, id, cua).await?;
    // 恢复后重读配置，确保抑制集合不再包含刚恢复的 id。
    let snapshot = tools.config.load().await?;
    let storage =
        plugins::records::storage_root(&snapshot.config, &super::extension_config::home());
    let request = plugins::Request {
        config: &snapshot.config,
        storage: &storage,
        cwd: &tools.cwd,
        env: &lookup,
        cancel,
    };
    let outcome = plugins::discover(&request).await?;
    Ok(install_admin::restored(id, &outcome))
}

async fn install(
    tools: &WorkspaceTools,
    context: &Context<'_>,
    (marketplace, name, dry): (&str, &str, bool),
) -> Result<Value> {
    match install_admin::install(context, marketplace, name, dry).await? {
        Install::Done(result) => Ok(result),
        Install::Restore(id) => restore(tools, &id, context.user_path, context.ports.cancel).await,
    }
}

pub(super) async fn handle(
    tools: &WorkspaceTools,
    method: &str,
    params: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let snapshot = tools.config.load().await?;
    let storage =
        plugins::records::storage_root(&snapshot.config, &super::extension_config::home());
    let http = super::plugin_io::PluginHttp {
        egress: tools.egress.clone(),
    };
    let git = super::plugin_io::git(&tools.egress);
    let home = super::plugin_io::home(&tools.egress);
    let context = Context {
        ports: plugins::ports::Ports {
            storage: &storage,
            cancel,
            http: &http,
            git: &git,
        },
        config: &snapshot.config,
        user_path: Path::new(&snapshot.user_path),
        home: &home,
    };
    // 与 Node 一致：市场与安装方法在同一 storage root 的进程内锁里串行化。
    let _guard = plugins::atomic_write::lock(&storage).await;
    let text = |key: &str| params[key].as_str().unwrap_or_default();
    match method {
        "plugins/marketplace/add" => market_admin::add(&context, params).await,
        "plugins/marketplace/remove" => market_admin::remove(&context, params).await,
        "plugins/marketplace/update" => market_admin::update(&context, params).await,
        "plugins/install" => {
            let target = (
                text("marketplace"),
                text("pluginName"),
                params["dryRun"] == true,
            );
            install(tools, &context, target).await
        }
        "plugins/update" => {
            let mut results = vec![];
            for record in install_admin::update_targets(&context, params).await? {
                let target = (record.marketplace.as_str(), record.name.as_str(), false);
                results.push(install(tools, &context, target).await?);
            }
            Ok(install_admin::merge(&results))
        }
        "plugins/validate" => install_admin::validate(&context, params).await,
        "plugins/describe" => install_admin::describe(&context, params).await,
        other => anyhow::bail!("Unsupported plugin method: {other}"),
    }
}
