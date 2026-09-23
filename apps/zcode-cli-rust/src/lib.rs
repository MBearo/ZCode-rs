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
