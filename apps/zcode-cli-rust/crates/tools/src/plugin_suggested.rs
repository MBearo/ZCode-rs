//! `plugins/resolveSuggestedReference` (Node `resolveSuggestedPluginReference`):
//! a local catalog hit answers at once; otherwise the Host is told the
//! operation is refreshing and the official marketplace refreshes within
//! 10 s. Spec rust-m10-4-plugin-sources §9.
use super::tools::WorkspaceTools;
use crate::contract::{Event, EventSink};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zcode_cli_plugins as plugins;
use zcode_cli_plugins::official::MARKETPLACE;
use zcode_cli_plugins::suggested;

/// The workspace catalog entry of `id` with its listing icon, from a fresh
/// discovery (Node reads synchronously, so it is not cancellable).
async fn read_state(tools: &WorkspaceTools, id: &str) -> Result<(Option<Value>, Value)> {
    let snapshot = tools.config.load().await?;
    let storage =
        plugins::records::storage_root(&snapshot.config, &super::extension_config::home());
    let env = super::plugin_requests::env_lookup(tools);
    let lookup = |name: &str| env.get(name).map(|v| (*v).to_owned());
    let request = plugins::Request {
        config: &snapshot.config,
        storage: &storage,
        cwd: &tools.cwd,
        env: &lookup,
        cancel: &CancellationToken::new(),
    };
    let outcome = plugins::discover(&request).await?;
    let entry = plugins::catalog::build(&outcome.plugins)
        .into_iter()
        .find(|e| e["pluginId"] == id);
    let input = plugins::overview::Input {
        config: &snapshot.config,
        storage: &storage,
        user_path: Path::new(&snapshot.user_path),
        env: &lookup,
    };
    let overview = plugins::overview::overview(&input, &outcome).await?;
    Ok((entry, overview))
}

fn icon(overview: &Value, id: &str) -> Option<String> {
    let display = plugins::catalog::display(overview);
    display
        .get(id)
        .and_then(|d| d["icon"].as_str())
        .map(str::to_owned)
}

/// Refreshes the official marketplace; `Err` carries the unavailable result.
async fn refresh(
    tools: &WorkspaceTools,
    id: &str,
    cancel: &CancellationToken,
) -> Result<(), Value> {
    let snapshot = tools
        .config
        .load()
        .await
        .map_err(|e| suggested::refresh_failed(id, &e.to_string()))?;
    let storage =
        plugins::records::storage_root(&snapshot.config, &super::extension_config::home());
    let http = super::plugin_io::PluginHttp {
        egress: tools.egress.clone(),
    };
    let git = super::plugin_io::git(&tools.egress);
    let home = super::plugin_io::home(&tools.egress);
    // 刷新超时必须中止底层网络与进程，否则旧操作会继续改写目录快照。
    let refresh_cancel = cancel.child_token();
    let context = plugins::market_admin::Context {
        ports: plugins::ports::Ports {
            storage: &storage,
            cancel: &refresh_cancel,
            http: &http,
            git: &git,
        },
        config: &snapshot.config,
        user_path: Path::new(&snapshot.user_path),
        home: &home,
    };
    let params = json!({ "marketplace": MARKETPLACE });
    let refreshing = plugins::market_admin::update(&context, &params);
    tokio::pin!(refreshing);
    let timeout = Duration::from_millis(suggested::REFRESH_TIMEOUT_MS);
    let outcome = tokio::select! {
        outcome = &mut refreshing => Some(outcome),
        _ = tokio::time::sleep(timeout) => None,
    };
    let Some(outcome) = outcome else {
        refresh_cancel.cancel();
        let _ = refreshing.await;
        return Err(suggested::refresh_failed(id, &suggested::timeout_message()));
    };
    match outcome {
        _ if cancel.is_cancelled() => Err(suggested::cancelled(id)),
        Err(error) => Err(suggested::refresh_failed(id, &error.to_string())),
        Ok(result) => {
            let failure = result["diagnostics"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|d| d["pluginId"] == MARKETPLACE);
            match failure {
                Some(failure) => Err(suggested::refresh_failed(
                    id,
                    failure["message"].as_str().unwrap_or_default(),
                )),
                None => Ok(()),
            }
        }
    }
}

pub(super) async fn resolve(
    tools: &WorkspaceTools,
    params: &Value,
    cancel: &CancellationToken,
    sink: &EventSink,
) -> Result<Value> {
    let id = zcode_cli_domain::js_string::trim(params["stableId"].as_str().unwrap_or_default());
    let name = match suggested::parse(id) {
        Ok(name) => name,
        Err(result) => return Ok(result),
    };
    let (entry, overview) = read_state(tools, id).await?;
    if let Some(entry) = entry {
        return Ok(suggested::from_entry(
            id,
            &name,
            &entry,
            icon(&overview, id).as_deref(),
        ));
    }
    // 本地缺失时先通知同一 operation 进入 loading，网络等待期间 UI 才有反馈。
    let progress = json!({"operationId":params["operationId"],"state":"refreshing"});
    let progress = Event::HostCall {
        method: "plugins/operationProgress",
        params: progress,
        reply: None,
    };
    let _ = sink.send(progress).await;
    if let Err(result) = refresh(tools, id, cancel).await {
        return Ok(result);
    }
    let (entry, overview) = read_state(tools, id).await?;
    Ok(match entry {
        Some(entry) => suggested::from_entry(id, &name, &entry, icon(&overview, id).as_deref()),
        None => suggested::from_overview(id, &name, &overview),
    })
}
