//! Public runtime ports. The application owns state; adapters own external IO.
pub use zcode_cli_domain::model::{ModelFailure, RetryState};
use zcode_cli_domain::session::Session;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelIdentity {
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_level: String,
}
pub type Output = mpsc::Sender<Vec<Value>>;
pub struct ChildHandle {
    pub task: zcode_cli_domain::subagent::Task,
    pub updates: tokio::sync::watch::Receiver<zcode_cli_domain::subagent::Task>,
    pub message_id: Option<String>,
    pub delivery: Option<String>,
}
#[derive(Debug)]
pub struct StorageCommitFailure;
impl std::fmt::Display for StorageCommitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fault.storage.commit")
    }
}
impl std::error::Error for StorageCommitFailure {}
#[derive(Debug)]
pub struct ProcessCleanupFailure;
impl std::fmt::Display for ProcessCleanupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fault.runtime.processCleanup")
    }
}
impl std::error::Error for ProcessCleanupFailure {}

/// Receipt returned after the storage transaction is durable. The core uses
/// this boundary before advancing the model/tool loop or publishing facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableCommitReceipt {
    pub receipt_id: String,
    pub workspace: String,
    pub session_id: Option<String>,
    pub sequence: u64,
}

pub enum Input {
    Request(zcode_cli_domain::protocol::Request),
    Response { id: String, result: Value },
    Invalid,
    TooLarge,
    Eof,
}

pub use crate::contract_events::{Event, EventSink, ModelOutput, RunEvent};
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Startup reads only the lightweight persisted index, never every transcript or ACK.
    async fn load_index(&self, workspace: &str) -> Result<BTreeMap<String, Value>>;
    /// Read one durable receipt, disposing a cold queued input without hydrating its session.
    async fn lookup_ack(&self, workspace: &str, key: &str) -> Result<Option<Value>>;
    /// Read persisted identities without loading history, resuming a runtime, or writing storage.
    async fn list_sessions(
        &self,
        _params: &zcode_cli_domain::session_listing::ListParams,
        _owner: (&str, &str),
    ) -> Result<Vec<zcode_cli_domain::session_listing::SessionListing>> {
        anyhow::bail!("Session listing unavailable")
    }
    /// Load one persisted conversation without activating a runtime or enumerating other history.
    async fn load_session(&self, _workspace: &str, _id: &str) -> Result<Option<Session>> {
        anyhow::bail!("Single session loading unavailable")
    }
    /// Reclaim only a draft with no history, atomically with the close receipt.
    async fn discard_draft(
        &self,
        _workspace: &str,
        _id: &str,
        _ack: (String, Value),
    ) -> Result<()> {
        anyhow::bail!("Draft reclamation unavailable")
    }
    async fn put_attachment(
        &self,
        _chunks: &[Vec<u8>],
        _mime: &str,
    ) -> Result<zcode_cli_domain::session::StoredAttachment> {
        anyhow::bail!("Attachment storage unavailable")
    }
    async fn snapshot_attachment(
        &self,
        _path: &str,
        _mime: &str,
    ) -> Result<zcode_cli_domain::session::StoredAttachment> {
        anyhow::bail!("Attachment snapshot unavailable")
    }
    async fn read_attachment(
        &self,
        _asset: &zcode_cli_domain::session::StoredAttachment,
        _offset: u64,
        _limit: usize,
    ) -> Result<Vec<u8>> {
        anyhow::bail!("Attachment read unavailable")
    }
    async fn load(&self, workspace: &str) -> Result<(Vec<Session>, BTreeMap<String, Value>)>;
    async fn commit(
        &self,
        workspace: &str,
        session: Option<&mut Session>,
        ack: Option<(String, Value)>,
    ) -> Result<()>;

    async fn commit_receipt(
        &self,
        workspace: &str,
        session: Option<&mut Session>,
        ack: Option<(String, Value)>,
    ) -> Result<DurableCommitReceipt> {
        let session_id = session.as_ref().map(|value| value.id.clone());
        let sequence = session.as_ref().map(|value| value.seq).unwrap_or_default();
        self.commit(workspace, session, ack).await?;
        let receipt_id = format!(
            "{}:{}:{}",
            workspace,
            session_id.as_deref().unwrap_or("workspace"),
            sequence
        );
        Ok(DurableCommitReceipt {
            receipt_id,
            workspace: workspace.to_owned(),
            session_id,
            sequence,
        })
    }
}
#[async_trait]
pub trait ModelPort: Send + Sync {
    fn identity(&self) -> Option<ModelIdentity> {
        None
    }
    fn format_properties(&self) -> Value {
        serde_json::json!({"inputFormat":{"supportsText":true,"supportsImage":false,"supportsVideo":false,"supportsAudio":false,"supportsPdf":false},"outputFormat":{"supportsText":true}})
    }
    fn with_max_output_tokens(&self, _max: usize) -> Result<Option<Arc<dyn ModelPort>>> {
        Ok(None)
    }
    fn bind(&self) -> Option<Arc<dyn ModelPort>> {
        None
    }
    fn context_policy(&self) -> zcode_cli_domain::context::ContextPolicy {
        Default::default()
    }
    async fn complete(
        &self,
        messages: Vec<Value>,
        tools: &[Value],
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure>;
}
/// Runtime configuration is resolved outside the actor; credentials are never session facts.
#[async_trait]
pub trait ModelRegistry: Send + Sync {
    async fn received_account(&self) -> Option<Value> {
        None
    }
    fn catalog(&self) -> Vec<Value>;
    fn model_options(&self) -> Vec<Value> {
        self.catalog()
    }
    fn default_selection(&self) -> Option<ModelIdentity>;
    fn resolve(&self, selection: &ModelIdentity) -> Result<Arc<dyn ModelPort>>;
    async fn refresh(&self, account: Option<Value>) -> Result<bool>;
}
#[async_trait]
pub trait RewindTransaction: Send {
    fn preview(&self) -> Value;
    fn checkpoint_ids(&self) -> Vec<String>;
    async fn finish(self: Box<Self>, commit: bool) -> Result<()>;
}
#[async_trait]
pub trait ToolPort: Send + Sync {
    async fn file_changes(
        &self,
        _changes: &[zcode_cli_domain::file_checkpoint::FileCheckpoint],
    ) -> Result<Value> {
        anyhow::bail!("File changes unavailable")
    }
    async fn pending_rewinds(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn recover_rewind(&self, _session: &str, _committed: Option<&str>) -> Result<()> {
        Ok(())
    }
    async fn rewind_preview(
        &self,
        _changes: &[zcode_cli_domain::file_checkpoint::FileCheckpoint],
    ) -> Result<Value> {
        anyhow::bail!("File rewind unavailable")
    }
    async fn begin_rewind(
        &self,
        _session: &str,
        _token: &str,
        _changes: &[zcode_cli_domain::file_checkpoint::FileCheckpoint],
    ) -> Result<Box<dyn RewindTransaction>> {
        anyhow::bail!("File rewind unavailable")
    }

    async fn agent_memory(
        &self,
        _profile: &zcode_cli_domain::subagent::Profile,
        _cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    async fn agent_profiles(
        &self,
        _cancel: &CancellationToken,
    ) -> Result<Vec<zcode_cli_domain::subagent::Profile>> {
        Ok(zcode_cli_domain::subagent::builtins())
    }
    async fn inherit_session(&self, _parent: &str, _child: &str) -> Result<()> {
        Ok(())
    }
    async fn agent_output(&self, _session: &str, _text: &str) -> Result<String> {
        anyhow::bail!("Agent artifact storage unavailable")
    }
    async fn configure_mcp(&self, _session: &str, servers: &Value) -> Result<()> {
        anyhow::ensure!(
            servers.as_array().is_some_and(Vec::is_empty),
            "MCP unavailable"
        );
        Ok(())
    }
    async fn mcp_list(&self, _params: &Value, _cancel: &CancellationToken) -> Result<Value> {
        anyhow::bail!("MCP unavailable")
    }
    async fn scoped_definitions(
        &self,
        _session: &str,
        _cancel: &CancellationToken,
    ) -> Result<Vec<Value>> {
        Ok(self.definitions())
    }
    fn concurrent_safe_scoped(&self, _session: &str, name: &str) -> bool {
        self.concurrent_safe(name)
    }
    async fn evict_session(&self, session: &str) -> Result<()> {
        self.close_session(session).await
    }
    async fn discover_skills(
        &self,
        _cancel: &CancellationToken,
    ) -> Result<zcode_cli_domain::skills::SkillCatalog> {
        Ok(Default::default())
    }
    async fn load_skill(
        &self,
        _skill: &zcode_cli_domain::skills::Skill,
        _name: &str,
        _cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        anyhow::bail!("Skill loading unavailable")
    }
    fn definitions(&self) -> Vec<Value>;
    fn requires_permission(&self, name: &str) -> bool;
    fn concurrent_safe(&self, _name: &str) -> bool {
        false
    }
    async fn execute(
        &self,
        name: &str,
        arguments: &Value,
        cancel: &CancellationToken,
    ) -> Result<String>;
    async fn execute_scoped(
        &self,
        name: &str,
        arguments: &Value,
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let _ = sink;
        Ok(ToolOutput::text(
            self.execute(name, arguments, cancel).await?,
        ))
    }
    async fn cancel_session(&self, _session: &str, _task: Option<&str>) -> Result<()> {
        Ok(())
    }
    async fn close_session(&self, session: &str) -> Result<()> {
        self.cancel_session(session, None).await
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
pub struct ToolOutput {
    pub failed: bool,
    pub content: String,
    pub data: Value,
    pub display: Option<Value>,
}
impl ToolOutput {
    pub fn text(content: String) -> Self {
        Self {
            failed: false,
            content,
            data: Value::Null,
            display: None,
        }
    }
    pub fn new(content: String, data: Value) -> Self {
        Self {
            failed: false,
            content,
            data,
            display: None,
        }
    }
}
pub trait RuntimeClock: Send + Sync {
    fn now(&self) -> u64;
    fn id(&self) -> String;
}
pub use RuntimeClock as Clock;

/// Request-scoped authentication. Implementations must never persist the
/// returned value in a session, queue, ACK or diagnostic record.
#[async_trait]
pub trait AuthPort: Send + Sync {
    async fn credentials(
        &self,
        session_id: &str,
        request_id: &str,
        workspace: &str,
    ) -> Result<Value>;
}

#[async_trait]
pub trait ContextPort: Send + Sync {
    fn desktop(&self) -> bool;
    async fn snapshot(
        &self,
        cancel: &CancellationToken,
    ) -> Result<zcode_cli_domain::prompt::PromptSnapshot>;
    async fn instructions(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<zcode_cli_domain::prompt::InstructionSource>>;
}
pub struct RuntimePorts {
    pub context: Arc<dyn ContextPort>,
    pub store: Arc<dyn SessionStore>,
    pub model: Option<Arc<dyn ModelPort>>,
    pub tools: Arc<dyn ToolPort>,
    pub clock: Arc<dyn RuntimeClock>,
}
