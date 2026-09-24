//! `plugins/*` requests over the plugins crate (spec rust-m10-plugins §3.6–3.7).
//! Configuration is reloaded on every request, as Node creates it per call.
use super::extension_config as config;
use super::tools::WorkspaceTools;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::sync::CancellationToken;
use zcode_cli_plugins as plugins;

pub(super) async fn handle(
    tools: &WorkspaceTools,
    method: &str,
    params: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let snapshot = tools.config.load().await?;
    // Settings 的 User 视图不加载项目配置（Node createPluginConfigView）。
    let user_scope =
        params["configScope"] == "user" && matches!(method, "plugins/list" | "plugins/overview");
    let view = if user_scope {
        &snapshot.user_view
    } else {
        &snapshot.config
    };
    let storage = plugins::records::storage_root(view, &config::home());
    let env: HashMap<&str, &str> = tools
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let lookup = |name: &str| env.get(name).map(|v| (*v).to_owned());
    let request = plugins::Request {
        config: view,
        storage: &storage,
        cwd: &tools.cwd,
        env: &lookup,
        cancel,
    };
    let outcome = tokio::select! {biased;
        _ = cancel.cancelled() => bail!("Plugin operation cancelled"),
        outcome = plugins::discover(&request) => outcome?,
    };
    let project_paths = snapshot.project_paths.clone();
    let paths = plugins::mutations::Paths {
        user: Path::new(&snapshot.user_path),
        cwd: &tools.cwd,
        project: &project_paths,
    };
    let input = plugins::overview::Input {
        config: view,
        storage: &storage,
        user_path: Path::new(&snapshot.user_path),
        env: &lookup,
    };
    match method {
        "plugins/list" => {
            let workspace = if user_scope {
                &Value::Null
            } else {
                &snapshot.project_plugins
            };
            let sources = plugins::list::Sources {
                user: &snapshot.user_plugins,
                workspace,
                cwd: &tools.cwd,
            };
            Ok(plugins::list::list(&outcome, view, &sources))
        }
        "plugins/overview" => Ok(plugins::overview::overview(&input, &outcome).await?),
        "plugins/referenceCatalog" | "plugins/referenceCatalogWithCategory" => {
            // 带 sessionId 的请求由 engine 附上该会话冻结的目录；权威不回退到 workspace。
            let frozen = params["frozenCatalog"].as_array();
            let overview = plugins::overview::overview(&input, &outcome).await?;
            let display = plugins::catalog::display(&overview);
            let category = method == "plugins/referenceCatalogWithCategory";
            let current;
            let (authority, catalog) = match frozen {
                Some(entries) => ("session", entries),
                None => {
                    current = plugins::catalog::build(&outcome.plugins);
                    ("workspace", &current)
                }
            };
            let entries: Vec<Value> = catalog
                .iter()
                .map(|entry| plugins::catalog::project(entry, &display, category))
                .collect();
            Ok(json!({"authority":authority,"plugins":entries}))
        }
        "plugins/setEnabled" => {
            let id = params["pluginId"].as_str().unwrap_or_default();
            let enabled = params["enabled"].as_bool().unwrap_or_default();
            let scope = params["scope"].as_str();
            plugins::mutations::set_enabled(&outcome, &paths, (id, enabled, scope)).await
        }
        "plugins/configure" => plugins::mutations::configure(&outcome, &paths, params).await,
        "plugins/resetConfig" => plugins::mutations::reset(&paths, params).await,
        "plugins/restoreBuiltin" => {
            let _guard = plugins::atomic_write::lock(&storage).await;
            let id = params["pluginId"].as_str().unwrap_or_default();
            let cua = plugins::overview::cua_enabled(&lookup);
            plugins::mutations::restore_builtin(&paths, id, cua).await
        }
        "plugins/uninstall" => {
            // 与 Node 一致：卸载在同一 storage root 的进程内锁里串行化。
            let _guard = plugins::atomic_write::lock(&storage).await;
            plugins::mutations::uninstall(&outcome, (&storage, &paths), params).await
        }
        other => bail!("Unsupported plugin method: {other}"),
    }
}

/// Hook matchers of the enabled plugins under the current configuration.
pub(super) async fn hooks(
    tools: &WorkspaceTools,
    cancel: &CancellationToken,
) -> Result<Vec<(zcode_cli_domain::hooks::HookEvent, Value)>> {
    let snapshot = tools.config.load().await?;
    Ok(config::plugins(&tools.cwd, &snapshot.config, cancel)
        .await?
        .hooks)
}

/// The workspace reference catalog with provenance roots (frozen per session by the engine).
pub(super) async fn catalog(
    tools: &WorkspaceTools,
    cancel: &CancellationToken,
) -> Result<Vec<Value>> {
    let snapshot = tools.config.load().await?;
    let outcome = config::plugins(&tools.cwd, &snapshot.config, cancel).await?;
    Ok(plugins::catalog::build(&outcome.plugins))
}
