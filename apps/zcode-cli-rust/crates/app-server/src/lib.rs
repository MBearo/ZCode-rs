use zcode_cli_core_api as contract;
use zcode_cli_domain as domain;
use std::sync::Arc;
use tokio::sync::mpsc;
pub mod stdio;
pub use stdio::{start, finish, storage_prepare};

/// App Server projection over the shared core runtime. Framing stays in
/// `stdio`; this type owns no session, queue, model or tool facts.
pub struct AppServer<R> {
    runtime: Arc<R>,
}

impl<R> AppServer<R>
where
    R: contract::SessionRuntime + 'static,
{
    pub fn new(runtime: Arc<R>) -> Self {
        Self { runtime }
    }

    pub async fn dispatch(
        &self,
        command: zcode_cli_protocol::Command,
    ) -> anyhow::Result<zcode_cli_protocol::CommandAck> {
        self.runtime.dispatch(command).await
    }

    pub async fn query(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.runtime.query(method, params).await
    }

    pub async fn subscribe(
        &self,
        session_id: &str,
    ) -> anyhow::Result<mpsc::Receiver<contract::RuntimeEvent>> {
        self.runtime.subscribe(session_id).await
    }
}
