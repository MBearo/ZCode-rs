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

/// Workspace tools wired to the process environment's layered configuration,
/// as the binary does. Used by tests, examples and embedders.
pub fn workspace_tools(
    cwd: std::path::PathBuf,
    artifacts: std::path::PathBuf,
) -> zcode_cli_tools::WorkspaceTools {
    let home = std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let config =
        zcode_cli_host::WorkspaceConfig::new(cwd.clone(), home, std::env::vars().collect());
    zcode_cli_tools::WorkspaceTools::new(cwd, artifacts, std::sync::Arc::new(config))
}
