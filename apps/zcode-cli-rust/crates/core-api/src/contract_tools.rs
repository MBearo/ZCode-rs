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
    pub content: String,
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
            data,
            display: None,
        }
    }
}
