use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use zcode_cli_protocol::CommandAck;

/// A frontend-facing event envelope. The core owns sequencing; frontends only
/// render or transport the resulting facts.
#[derive(Clone, Debug)]
pub struct RuntimeEvent {
    pub trace_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub sequence: u64,
    pub kind: String,
    pub payload: Value,
}

/// Stable command/query/event boundary shared by App Server and TUI.
#[async_trait]
pub trait SessionRuntime: Send + Sync {
    async fn dispatch(
        &self,
        command: zcode_cli_protocol::Command,
    ) -> anyhow::Result<CommandAck>;
    async fn query(&self, method: &str, params: &Value) -> anyhow::Result<Value>;
    async fn subscribe(
        &self,
        session_id: &str,
    ) -> anyhow::Result<mpsc::Receiver<RuntimeEvent>>;
    async fn shutdown(&self) -> anyhow::Result<()>;
}
