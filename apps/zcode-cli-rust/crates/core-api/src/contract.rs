//! Public runtime ports. The application owns state; adapters own external IO.
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
pub use zcode_cli_domain::model::{ModelFailure, RetryState};
use zcode_cli_domain::session::Session;

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

pub use crate::contract_events::{
    Event, EventSink, Guide, ModelOutput, PermissionAnswer, PermissionRequest, RequestAuth,
    RequestKind, RequestOrigin, RunEvent, SessionHooks, WorkspaceHooks,
};
pub use crate::contract_tools::{
    HookProcess, ToolError, ToolOutput, ToolPermission, ToolPort, TrustLoad, TrustStorePort,
    render_failure,
};
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// The store keeps Node session records (spec rust-m11-node-storage §5.1):
    /// the engine then queues them on `Session.node` for `commit`.
    fn node_journal(&self) -> bool {
        false
    }
    /// Startup reads only the lightweight persisted index, never every transcript or ACK.
    async fn load_index(&self, workspace: &str) -> Result<BTreeMap<String, Value>>;
    /// Read one durable receipt, disposing a cold queued input without hydrating its session.
    async fn lookup_ack(&self, workspace: &str, key: &str) -> Result<Option<Value>>;
    /// `lookup_ack` knowing whether the key's session has a live runtime here;
    /// a cold lookup may settle its admitted inputs (Node `discardAdmittedOnLoad`).
    async fn lookup_ack_live(
        &self,
        workspace: &str,
        key: &str,
        _live: bool,
    ) -> Result<Option<Value>> {
        self.lookup_ack(workspace, key).await
    }
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
    /// Workspace-scoped settings (Node `local_setting` scope `project`), e.g.
    /// `permission/ruleset` and `permission/mode`.
    async fn project_settings(
        &self,
        _workspace: &str,
    ) -> Result<BTreeMap<(String, String), Value>> {
        Ok(BTreeMap::new())
    }
    async fn save_project_setting(
        &self,
        _workspace: &str,
        _namespace: &str,
        _key: &str,
        _value: &Value,
    ) -> Result<()> {
        anyhow::bail!("Project settings unavailable")
    }
    /// Reclaim only a draft with no history, atomically with the close receipt.
    async fn discard_draft(
        &self,
        _workspace: &str,
        _id: &str,
        _ack: Option<(String, Value)>,
    ) -> Result<()> {
        anyhow::bail!("Draft reclamation unavailable")
    }
    /// Stores prompt attachment bytes of `session` in an artifact named after
    /// `call` (spec rust-m11-node-storage §5.3); returns its
    /// `zcode-artifact://` reference and the stored bytes.
    async fn put_attachment(
        &self,
        _session: &str,
        _call: &str,
        _chunks: &[Vec<u8>],
        _mime: &str,
    ) -> Result<(String, zcode_cli_domain::session::StoredAttachment)> {
        anyhow::bail!("Attachment storage unavailable")
    }
    /// Resolves the local path attachment at `index` the way Node does:
    /// `(original, path)` is the submitted reference and its resolved file.
    /// Returns the reference the input keeps and the stored resolution.
    async fn local_attachment(
        &self,
        _session: &str,
        _index: usize,
        _file: (&str, &str),
        _mime: &str,
    ) -> Result<(String, zcode_cli_domain::session::StoredAttachment)> {
        anyhow::bail!("Attachment snapshot unavailable")
    }
    /// The stored bytes behind a reference the session does not hold in
    /// memory (a resumed or Node-written session).
    async fn attachment_of(
        &self,
        _reference: &str,
    ) -> Result<Option<zcode_cli_domain::session::StoredAttachment>> {
        Ok(None)
    }
    async fn read_attachment(
        &self,
        _asset: &zcode_cli_domain::session::StoredAttachment,
        _offset: u64,
        _limit: usize,
    ) -> Result<Vec<u8>> {
        anyhow::bail!("Attachment read unavailable")
    }
    /// Appends one usage fact through the storage writer without waiting for
    /// it; failures are logged by the store and never reach the caller.
    async fn record_usage(&self, _fact: zcode_cli_domain::usage::Fact) {}
    /// Node `queryAppUsage` over the whole database, after every usage fact
    /// sent before the call was written.
    async fn app_usage(
        &self,
        _since: i64,
        _until: i64,
        _tz_offset_ms: i64,
    ) -> Result<zcode_cli_domain::usage::AppRows> {
        Ok(Default::default())
    }
    /// One session's model requests in `started_at, id` order (same barrier).
    async fn task_usage(&self, _session_id: &str) -> Result<Vec<zcode_cli_domain::usage::TaskRow>> {
        Ok(vec![])
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
    /// Node `auxiliaryModelOptions` with `maxOutputTokens ≤ 4096`: the lowest
    /// reasoning level of the current model; `None` keeps this model.
    fn auxiliary(&self) -> Option<Arc<dyn ModelPort>> {
        None
    }
    /// Model property `supportsNativeWebSearch` (WebSearch is offered).
    fn supports_native_web_search(&self) -> bool {
        false
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
    /// Visible models as Node `toModelOption` (legacy session snapshots).
    fn legacy_models(&self) -> Vec<Value> {
        vec![]
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

pub use zcode_cli_domain::config::ConfigSnapshot;

/// Layered zcode configuration for one workspace. Implementations reload files on
/// every call (Node reloads at each entry point); consumers only read snapshots.
#[async_trait]
pub trait ConfigSource: Send + Sync {
    async fn load(&self) -> Result<Arc<ConfigSnapshot>>;
    /// Switches one project hook in `<cwd>/.zcode/config.json` if its
    /// declaration still has the reviewed digest. `Err` is a reason code.
    async fn set_workspace_hook_enabled(
        &self,
        _toggle: HookToggle,
    ) -> std::result::Result<(), &'static str> {
        Err("workspace_hooks_config_write_failed")
    }
}

/// A reviewed project hook declaration to switch (Node
/// `writeWorkspaceHookConfiguredToggle`).
pub struct HookToggle {
    pub path: String,
    pub event: zcode_cli_domain::hooks::HookEvent,
    pub relative_path: String,
    pub discovery_order: usize,
    pub matcher_index: usize,
    pub hook_index: usize,
    pub digest: String,
    pub resolved_timeout_ms: u64,
    pub resolved_max_output_bytes: u64,
    pub enabled: bool,
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
