//! TUI frontend contract. Rendering stays outside the core runtime so the
//! terminal UI cannot create a second session owner.
use std::sync::Arc;
use tokio::sync::mpsc;
use zcode_cli_core_api::{RuntimeEvent, SessionRuntime};

pub struct TuiFrontend<R> {
    runtime: Arc<R>,
}

impl<R> TuiFrontend<R>
where
    R: SessionRuntime + 'static,
{
    pub fn new(runtime: Arc<R>) -> Self {
        Self { runtime }
    }

    pub async fn send(
        &self,
        command: zcode_cli_protocol::Command,
    ) -> anyhow::Result<zcode_cli_protocol::CommandAck> {
        self.runtime.dispatch(command).await
    }

    pub async fn query(&self, method: &str, params: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.runtime.query(method, params).await
    }

    pub async fn subscribe(&self, session_id: &str) -> anyhow::Result<mpsc::Receiver<RuntimeEvent>> {
        self.runtime.subscribe(session_id).await
    }
}
