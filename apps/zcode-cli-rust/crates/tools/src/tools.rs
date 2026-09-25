use super::{
    tool_files::{FileState, FileTools},
    tool_shell::ShellTasks,
};
use crate::contract::{EventSink, ToolOutput, ToolPort};
use anyhow::{Result, bail};
use serde_json::Value;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub(super) use super::tool_args::{
    boolean, check_cancel, keys, resolve, string, truncate_utf8, uint,
};
use zcode_cli_domain::hooks::HookEvent;

pub struct WorkspaceTools {
    pub(super) cwd: PathBuf,
    pub(super) config: Arc<dyn crate::contract::ConfigSource>,
    artifacts: PathBuf,
    reads: Mutex<HashMap<String, Arc<Mutex<FileState>>>>,
    // File writes from different sessions share one commit gate; reads remain concurrent.
    writes: Arc<Mutex<()>>,
    shell: ShellTasks,
    mcp: super::mcp_hub::Hub,
    /// Complete child environment shared by shell tools and hooks.
    pub(super) env: Arc<[(String, String)]>,
    web: super::web_fetch::WebFetcher,
    /// Network egress of plugin management (marketplace and archive downloads).
    pub(super) egress: Arc<zcode_cli_net::Egress>,
    /// MCP process notifications until the engine takes them.
    process_events:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<(&'static str, Value)>>>,
}
impl WorkspaceTools {
    fn session_artifacts(&self, session: &str) -> PathBuf {
        self.artifacts.join(format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(session.as_bytes())
        ))
    }
    pub fn new(
        cwd: PathBuf,
        artifacts: PathBuf,
        config: Arc<dyn crate::contract::ConfigSource>,
        egress: Arc<zcode_cli_net::Egress>,
    ) -> Self {
        let env = egress.tool_env();
        let (sink, events) = tokio::sync::mpsc::unbounded_channel();
        let mcp = super::mcp_hub::Hub::new(cwd.clone(), config.clone(), egress.clone(), Some(sink));
        // 资源采样依赖运行时；没有 tokio 运行时的构造（单元测试）不采样。
        if tokio::runtime::Handle::try_current().is_ok() {
            super::mcp_resources::start(mcp.telemetry.clone(), mcp.stopped());
        }
        Self {
            shell: ShellTasks::new(env.clone()),
            env,
            web: super::web_fetch::WebFetcher::new(egress.clone()),
            mcp,
            process_events: std::sync::Mutex::new(Some(events)),
            egress,
            config,
            cwd,
            artifacts,
            reads: Mutex::new(HashMap::new()),
            writes: Arc::new(Mutex::new(())),
        }
    }
    pub async fn call(
        &self,
        session: &str,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        self.call_inner(session, name, args, None, cancel).await
    }
    async fn call_inner(
        &self,
        session: &str,
        name: &str,
        args: &Value,
        sink: Option<&EventSink>,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        check_cancel(cancel)?;
        if !args.is_object() {
            bail!("Tool arguments must be an object");
        }
        let artifacts = self.session_artifacts(session);
        match name {
            name if name.starts_with("mcp__") => self.mcp.call(session, name, args, cancel).await,
            "Read" | "Write" | "Edit" => {
                let state = self
                    .reads
                    .lock()
                    .await
                    .entry(session.to_owned())
                    .or_default()
                    .clone();
                let files = FileTools {
                    sink,
                    checkpoint_root: &self.artifacts,
                    cwd: &self.cwd,
                    artifacts: &artifacts,
                    env: &self.env,
                    state: &state,
                    writes: &self.writes,
                };
                files.call(name, args, cancel).await
            }
            "Glob" | "Grep" => super::tool_search::search(&self.cwd, name, args, cancel).await,
            // Kept for existing native transcripts, but no longer advertised to the model.
            "List" => {
                let path = resolve(&self.cwd, string(args, "path")?)?;
                let mut dir = tokio::fs::read_dir(path).await?;
                let mut entries = vec![];
                while let Some(entry) = dir.next_entry().await? {
                    check_cancel(cancel)?;
                    entries.push(entry.file_name().to_string_lossy().into_owned());
                    if entries.len() >= 1000 {
                        break;
                    }
                }
                entries.sort();
                Ok(ToolOutput::text(entries.join("\n")))
            }
            "Bash" | "TaskOutput" | "TaskStop" => {
                self.shell
                    .call((&self.cwd, &artifacts), session, name, args, sink, cancel)
                    .await
            }
            // Node：未注册的工具以 `Tool not found: {name}` 失败。
            _ => bail!("Tool not found: {name}"),
        }
    }
}
#[async_trait::async_trait]
impl ToolPort for WorkspaceTools {
    fn process_events(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<(&'static str, Value)>> {
        self.process_events.lock().unwrap().take()
    }
    fn child_processes(&self) -> Vec<Value> {
        self.mcp.telemetry.processes()
    }
    async fn file_changes(
        &self,
        changes: &[crate::domain::file_checkpoint::FileCheckpoint],
    ) -> Result<Value> {
        super::file_changes::details(&self.artifacts, changes).await
    }
    async fn import_checkpoint(
        &self,
        before: Option<&[u8]>,
        after: &[u8],
    ) -> Result<(Option<String>, String)> {
        super::file_checkpoints::import(&self.artifacts, before, after).await
    }
    async fn pending_rewinds(&self) -> Result<Vec<String>> {
        super::file_rewind::pending(&self.artifacts).await
    }
    async fn recover_rewind(&self, session: &str, committed: Option<&str>) -> Result<()> {
        super::file_rewind::recover(&self.artifacts, session, committed, self.writes.clone()).await
    }
    async fn rewind_preview(
        &self,
        changes: &[crate::domain::file_checkpoint::FileCheckpoint],
    ) -> Result<Value> {
        super::file_rewind::preview(&self.artifacts, changes).await
    }
    async fn begin_rewind(
        &self,
        session: &str,
        token: &str,
        changes: &[crate::domain::file_checkpoint::FileCheckpoint],
    ) -> Result<Box<dyn crate::contract::RewindTransaction>> {
        super::file_rewind::begin(
            &self.artifacts,
            session,
            token,
            changes,
            self.writes.clone(),
        )
        .await
    }

    async fn agent_memory(
        &self,
        profile: &crate::domain::subagent::Profile,
        cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        let config = self.config.load().await?;
        super::agent_profiles::memory(&self.cwd, &config.config, profile, cancel).await
    }
    async fn agent_profiles(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<crate::domain::subagent::Profile>> {
        let config = self.config.load().await?;
        super::agent_profiles::discover(&self.cwd, &config.config, cancel).await
    }
    async fn inherit_session(&self, parent: &str, child: &str) -> Result<()> {
        self.mcp.inherit(parent, child);
        Ok(())
    }
    async fn agent_output(&self, session: &str, text: &str) -> Result<String> {
        let dir = self.artifacts.join(format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(session.as_bytes())
        ));
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join("agent.output");
        let temp = dir.join("agent.output.tmp");
        tokio::fs::write(&temp, text).await?;
        tokio::fs::rename(temp, &path).await?;
        Ok(path.to_string_lossy().into_owned())
    }
    async fn configure_mcp(&self, session: &str, servers: &Value) -> Result<()> {
        self.mcp.configure(session, servers)
    }
    async fn mcp_list(&self, params: &Value, cancel: &CancellationToken) -> Result<Value> {
        self.mcp.list(params, cancel).await
    }
    async fn plugin_hooks(&self, cancel: &CancellationToken) -> Result<Vec<(HookEvent, Value)>> {
        super::plugin_requests::hooks(self, cancel).await
    }
    async fn plugin_catalog(&self, cancel: &CancellationToken) -> Result<Vec<Value>> {
        super::plugin_requests::catalog(self, cancel).await
    }
    fn mcp_inventory(&self, session: &str) -> Vec<(String, Vec<String>)> {
        self.mcp.inventory(session)
    }
    async fn persist_result(&self, session: &str, call_id: &str, content: &str) -> Result<String> {
        super::result_file::persist(&self.session_artifacts(session), call_id, content).await
    }
    fn attach_events(
        &self,
        events: tokio::sync::mpsc::Sender<crate::contract::RunEvent>,
        workspace_path: &str,
    ) {
        self.mcp.official.attach(events, workspace_path);
    }
    async fn plugins(
        &self,
        method: &str,
        p: &Value,
        cancel: &CancellationToken,
        sink: &EventSink,
    ) -> Result<Value> {
        if method == "plugins/resolveSuggestedReference" {
            return super::plugin_suggested::resolve(self, p, cancel, sink).await;
        }
        super::plugin_requests::handle(self, method, p, cancel).await
    }
    async fn scoped_definitions(
        &self,
        session: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Value>> {
        let mut definitions = self.definitions();
        definitions.extend(self.mcp.definitions(session, cancel).await?);
        Ok(definitions)
    }
    fn concurrent_safe_scoped(&self, session: &str, name: &str) -> bool {
        self.concurrent_safe(name) || self.mcp.safe(session, name)
    }
    fn mcp_annotations(&self, session: &str, name: &str) -> Option<Value> {
        self.mcp.hints(session, name)
    }
    async fn evict_session(&self, session: &str) -> Result<()> {
        self.shell.close_session(session).await?;
        self.reads.lock().await.remove(session);
        self.mcp.close_session(session, false).await
    }
    async fn discover_skills(
        &self,
        cancel: &CancellationToken,
    ) -> Result<crate::domain::skills::SkillCatalog> {
        let config = self.config.load().await?;
        super::tool_skills::discover(&self.cwd, &config.config, cancel).await
    }
    async fn load_skill(
        &self,
        skill: &crate::domain::skills::Skill,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        super::tool_skills::load(skill, name, cancel).await
    }
    fn definitions(&self) -> Vec<Value> {
        super::tool_definitions::definitions()
    }
    fn capability(
        &self,
        session: &str,
        name: &str,
        _args: &Value,
    ) -> crate::domain::permission::ToolCapability {
        if let Some((read_only, destructive)) = self.mcp.annotations(session, name) {
            return crate::domain::permission::ToolCapability::mcp(read_only, destructive);
        }
        crate::domain::permission::tool_capability(name)
            .cloned()
            .unwrap_or_default()
    }
    async fn write_plan_file(&self, session: &str, plan: &str) -> Result<()> {
        super::plan_file::write(&self.cwd, session, plan).await
    }
    async fn read_plan_file(&self, session: &str) -> Result<Option<(String, String)>> {
        super::plan_file::read(&self.cwd, session).await
    }
    async fn run_hook(
        &self,
        request: crate::contract::HookProcess,
        cancel: &CancellationToken,
    ) -> Result<crate::domain::hooks::output::Exec> {
        super::hook_process::run(request, &self.env, cancel).await
    }
    async fn permission(
        &self,
        session: &str,
        name: &str,
        args: &Value,
    ) -> crate::contract::ToolPermission {
        let capability = self.capability(session, name, args);
        super::bash_permission::resolve(&self.cwd, name, args, capability).await
    }
    fn concurrent_safe(&self, name: &str) -> bool {
        matches!(
            name,
            "Read"
                | "List"
                | "Glob"
                | "Grep"
                | "AskUserQuestion"
                | "TodoRead"
                | "Skill"
                | "Agent"
                | "Task"
                | "WebFetch"
                | "WebSearch"
        )
    }
    async fn execute(
        &self,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<String> {
        Ok(self.call("default", name, args, cancel).await?.content)
    }
    async fn execute_scoped(
        &self,
        name: &str,
        args: &Value,
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        self.call_inner(&sink.session_id, name, args, Some(sink), cancel)
            .await
    }
    async fn take_reads(&self, session: &str) -> Vec<crate::domain::compact_ptl::ReadView> {
        let state = self.reads.lock().await.remove(session);
        match state {
            Some(state) => state.lock().await.views.take(),
            None => vec![],
        }
    }
    fn model_definitions(&self, definitions: &mut [Value], input_format: &Value) {
        super::tool_definitions::for_model(definitions, input_format);
    }
    async fn web_fetch(
        &self,
        request: &crate::domain::web::FetchRequest,
        cancel: &CancellationToken,
    ) -> Result<crate::domain::web::Fetched> {
        let artifacts = self.session_artifacts(&request.session);
        self.web.fetch(request, &artifacts, cancel).await
    }
    async fn cancel_session(&self, session: &str, task: Option<&str>) -> Result<()> {
        self.shell.cancel(session, task).await
    }
    async fn close_session(&self, session: &str) -> Result<()> {
        self.shell.close_session(session).await?;
        self.reads.lock().await.remove(session);
        self.mcp.close_session(session, true).await?;
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        self.shell.shutdown().await?;
        self.mcp.shutdown().await
    }
}
