//! Tool execution port and result shape.
use crate::contract::{EventSink, RewindTransaction};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

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
    /// Validated hook matchers of the enabled plugins, each hook carrying its
    /// plugin context (spec rust-m10-plugins §3.5).
    async fn plugin_hooks(
        &self,
        _cancel: &CancellationToken,
    ) -> Result<Vec<(zcode_cli_domain::hooks::HookEvent, Value)>> {
        Ok(vec![])
    }
    /// The workspace plugin reference catalog with provenance roots, frozen by
    /// the engine per session (spec rust-m10-plugins §3.9).
    async fn plugin_catalog(&self, _cancel: &CancellationToken) -> Result<Vec<Value>> {
        Ok(vec![])
    }
    /// `(server, bound tool names)` of the session's connected MCP servers.
    fn mcp_inventory(&self, _session: &str) -> Vec<(String, Vec<String>)> {
        vec![]
    }
    /// The engine's event channel, for adapters that ask the Host (official
    /// MCP identity headers, spec rust-m10-plugins §3.11).
    fn attach_events(
        &self,
        _events: tokio::sync::mpsc::Sender<crate::contract::RunEvent>,
        _workspace_path: &str,
    ) {
    }
    /// A `plugins/*` management request of the workspace (spec rust-m10-plugins);
    /// progress notifications travel through `sink`.
    async fn plugins(
        &self,
        _method: &str,
        _params: &Value,
        _cancel: &CancellationToken,
        _sink: &crate::contract::EventSink,
    ) -> Result<Value> {
        anyhow::bail!("Plugins unavailable")
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
    /// Permission capability of one call (Node `resolveRuntimePermissionCapability`):
    /// the tool's static declaration unless the port knows better (MCP annotations,
    /// read-only Bash commands).
    fn capability(
        &self,
        _session: &str,
        name: &str,
        _args: &Value,
    ) -> zcode_cli_domain::permission::ToolCapability {
        zcode_cli_domain::permission::tool_capability(name)
            .cloned()
            .unwrap_or_default()
    }
    /// Everything the policy needs for one call: the capability, the tool's own
    /// rule matching (Node `resolvePermissionRulePolicy`, Bash only) and the
    /// "always allow" suggestions (`suggestedPermissionUpdates`).
    async fn permission(&self, session: &str, name: &str, args: &Value) -> ToolPermission {
        let capability = self.capability(session, name, args);
        let suggestions = zcode_cli_domain::permission::default_updates(
            name,
            args,
            capability.permission_capability_group.as_deref(),
        );
        ToolPermission {
            capability,
            rules: None,
            suggestions,
        }
    }
    /// Writes the approved plan to `<workspace>/.zcode/plans/plan-<id>.md`
    /// atomically; failures are swallowed by the caller, as in Node.
    async fn write_plan_file(&self, _session: &str, _plan: &str) -> Result<()> {
        Ok(())
    }
    /// The approved plan file `(path, content)` if present and not blank.
    async fn read_plan_file(&self, _session: &str) -> Result<Option<(String, String)>> {
        Ok(None)
    }
    /// Runs one hook process to its end (Node `ExecutionPort.run` for hooks):
    /// the timeout and `cancel` stop the whole process tree.
    async fn run_hook(
        &self,
        _request: HookProcess,
        _cancel: &CancellationToken,
    ) -> Result<zcode_cli_domain::hooks::output::Exec> {
        anyhow::bail!("Hooks unavailable")
    }
    /// WebFetch's network side behind the process cache (Node
    /// `fetchAndExtractContent`); URL checks run first.
    async fn web_fetch(
        &self,
        _request: &zcode_cli_domain::web::FetchRequest,
        _cancel: &CancellationToken,
    ) -> Result<zcode_cli_domain::web::Fetched> {
        anyhow::bail!("HttpClientPort is not configured for WebFetch tool")
    }
    /// The session's text Read views, newest first, then its read state is
    /// cleared (Node `readFileState` after compaction).
    async fn take_reads(&self, _session: &str) -> Vec<zcode_cli_domain::compact_ptl::ReadView> {
        vec![]
    }
    /// Adjusts the run's definitions to the model's input formats (Node:
    /// Read's PDF variant).
    fn model_definitions(&self, _definitions: &mut [Value], _input_format: &Value) {}
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
/// What a trust store load found (Node `WorkspaceHookTrustStoreLoadResult`).
#[derive(Clone, Debug, PartialEq)]
pub enum TrustLoad {
    /// A missing file loads as no records.
    Records(Vec<zcode_cli_domain::hooks::trust::Record>),
    /// Invalid content, moved aside; every project hook stays blocked.
    Corrupt,
}

/// The workspace hook trust store shared with the Desktop (Node
/// `FileWorkspaceHookTrustStore`). Calls are serialized in process and locked
/// across processes; mutations return the records now on disk.
#[async_trait]
pub trait TrustStorePort: Send + Sync {
    async fn load(&self) -> Result<TrustLoad>;
    /// Upsert by `(workspaceIdentity, digest)`, keeping existing positions.
    async fn grant(
        &self,
        records: Vec<zcode_cli_domain::hooks::trust::Record>,
    ) -> Result<Vec<zcode_cli_domain::hooks::trust::Record>>;
    /// `None` removes every record of the workspace; `Some` must not be empty.
    async fn revoke(
        &self,
        identity: &str,
        digests: Option<Vec<String>>,
    ) -> Result<Vec<zcode_cli_domain::hooks::trust::Record>>;
}

/// One hook process. Variables are already expanded.
pub struct HookProcess {
    pub program: zcode_cli_domain::hooks::Program,
    pub cwd: String,
    /// Session and plugin variables added over the tool environment.
    pub env: Vec<(String, String)>,
    /// The hook input; the port writes the transcript and stdin from it.
    pub input: Value,
    pub timeout: std::time::Duration,
    /// Kept per stream; the rest is read and dropped.
    pub max_output_bytes: usize,
}

/// Permission inputs of one tool call, resolved by the tool port.
pub struct ToolPermission {
    pub capability: zcode_cli_domain::permission::ToolCapability,
    /// Tool-specific rule matching; `None` matches rules on the generic subjects.
    pub rules: Option<Box<dyn zcode_cli_domain::permission::RulePolicy + Send + Sync>>,
    pub suggestions: Vec<zcode_cli_domain::permission::Update>,
}

pub struct ToolOutput {
    pub failed: bool,
    /// Denied by the permission policy or the user; the tool never ran.
    pub denied: bool,
    /// Node `turnControl.stopTurnAfterResult`: later tools are cancelled and the
    /// turn ends once this result is committed.
    pub stop_turn: bool,
    /// Text form: rows, hooks, legacy events and the empty-result check.
    pub content: String,
    /// Content blocks for the model when they are not just `content` (media
    /// as `_zcode_attachment` parts); `None` sends `content`.
    pub model_content: Option<Value>,
    pub data: Value,
    pub display: Option<Value>,
}
impl ToolOutput {
    pub fn text(content: String) -> Self {
        Self {
            failed: false,
            denied: false,
            stop_turn: false,
            content,
            model_content: None,
            data: Value::Null,
            display: None,
        }
    }
    pub fn new(content: String, data: Value) -> Self {
        Self {
            failed: false,
            denied: false,
            stop_turn: false,
            content,
            model_content: None,
            data,
            display: None,
        }
    }
}

/// A tool failure as the model reads it (Node `createErrorResult`). Other
/// errors a tool returns are thrown errors: their message, sanitized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// Node `ToolHandlerFailure`: `<tool_use_error>{message}</tool_use_error>`, message kept raw.
    Handler { code: u32, message: String },
    /// Model content used as given (e.g. an input validation envelope).
    Rendered(String),
}
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handler { message, .. } => f.write_str(message),
            Self::Rendered(text) => f.write_str(text),
        }
    }
}
impl std::error::Error for ToolError {}
impl ToolError {
    pub fn handler(code: u32, message: impl Into<String>) -> anyhow::Error {
        Self::Handler {
            code,
            message: message.into(),
        }
        .into()
    }
}
/// Node's fallback when a thrown error has no readable message.
const FALLBACK_FAILURE: &str = "Turn execution failed";
/// The model-visible text of a failed tool call.
pub fn render_failure(error: &anyhow::Error) -> String {
    match error.downcast_ref::<ToolError>() {
        Some(ToolError::Handler { message, .. }) => {
            format!("<tool_use_error>{message}</tool_use_error>")
        }
        Some(ToolError::Rendered(text)) => text.clone(),
        None => zcode_cli_domain::js_string::sanitize_message(&error.to_string())
            .unwrap_or_else(|| FALLBACK_FAILURE.into()),
    }
}
