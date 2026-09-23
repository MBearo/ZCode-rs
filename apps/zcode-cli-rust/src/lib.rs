pub use zcode_cli_app_server as app_server;
pub use zcode_cli_core as app;
pub use zcode_cli_core_api as contract;
pub use zcode_cli_domain as domain;
pub mod adapters {
    pub use zcode_cli_host::{SystemClock, context_source};
    pub use zcode_cli_model::{config, model_protocol, provider, registry};
    pub use zcode_cli_state::storage;
    pub use zcode_cli_tools::tools;
}

/// Run an engine behind the in-process App Server, exchanging parsed wire
/// values instead of stdio. Used by tests and embedders.
pub async fn serve_values(
    engine: app::Engine,
    input: tokio::sync::mpsc::Receiver<contract::Input>,
    output: tokio::sync::mpsc::Sender<Vec<serde_json::Value>>,
    cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    app_server::serve(
        move |rx, tx| engine.serve(rx, tx, cancel),
        input,
        app_server::Sink::Values(output),
    )
    .await
}

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// The process environment as Node sees it after startup sanitization.
/// Non-Unicode entries are converted lossily, like Node's `process.env`.
pub fn runtime_env(home: &std::path::Path) -> zcode_cli_net::RuntimeEnv {
    let vars = std::env::vars_os().map(|(k, v)| {
        (
            k.to_string_lossy().into_owned(),
            v.to_string_lossy().into_owned(),
        )
    });
    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    zcode_cli_net::RuntimeEnv::capture(vars, home, &argv)
}

/// Egress built from the process environment without network configuration.
/// Used by tests, examples and embedders.
pub fn egress() -> std::sync::Arc<zcode_cli_net::Egress> {
    let home = home();
    let env = std::sync::Arc::new(runtime_env(&home));
    std::sync::Arc::new(
        zcode_cli_net::Egress::new(env, &Default::default(), &home, "electron")
            .expect("valid endpoint origin"),
    )
}

/// Workspace tools wired to the process environment's layered configuration,
/// as the binary does. Used by tests, examples and embedders.
pub fn workspace_tools(
    cwd: std::path::PathBuf,
    artifacts: std::path::PathBuf,
) -> zcode_cli_tools::WorkspaceTools {
    let egress = egress();
    let config = zcode_cli_host::WorkspaceConfig::new(
        cwd.clone(),
        home(),
        egress.runtime_env().vars().to_vec(),
    );
    zcode_cli_tools::WorkspaceTools::new(cwd, artifacts, std::sync::Arc::new(config), egress)
}
