use super::{
    tool_files::{FileState, FileTools},
    tool_shell::ShellTasks,
};
use crate::contract::{EventSink, ToolOutput, ToolPort};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct WorkspaceTools {
    cwd: PathBuf,
    artifacts: PathBuf,
    reads: Mutex<HashMap<String, Arc<Mutex<FileState>>>>,
    // File writes from different sessions share one commit gate; reads remain concurrent.
    writes: Arc<Mutex<()>>,
    shell: ShellTasks,
    mcp: super::mcp_hub::Hub,
}
impl WorkspaceTools {
    pub fn new(cwd: PathBuf, artifacts: PathBuf) -> Self {
        Self {
            mcp: super::mcp_hub::Hub::new(cwd.clone()),
            cwd,
            artifacts,
            reads: Mutex::new(HashMap::new()),
            writes: Arc::new(Mutex::new(())),
            shell: ShellTasks::default(),
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
        let artifacts = self.artifacts.join(format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(session.as_bytes())
        ));
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
            _ => bail!("Unsupported tool: {name}"),
        }
    }
}
#[async_trait::async_trait]
impl ToolPort for WorkspaceTools {
    async fn file_changes(
        &self,
        changes: &[crate::domain::file_checkpoint::FileCheckpoint],
    ) -> Result<Value> {
        super::file_changes::details(&self.artifacts, changes).await
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
        super::agent_profiles::memory(&self.cwd, profile, cancel).await
    }
    async fn agent_profiles(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<crate::domain::subagent::Profile>> {
        super::agent_profiles::discover(&self.cwd, cancel).await
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
    async fn evict_session(&self, session: &str) -> Result<()> {
        self.shell.close_session(session).await?;
        self.reads.lock().await.remove(session);
        self.mcp.close_session(session, false).await
    }
    async fn discover_skills(
        &self,
        cancel: &CancellationToken,
    ) -> Result<crate::domain::skills::SkillCatalog> {
        super::tool_skills::discover(&self.cwd, cancel).await
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
        let schemas: Value = serde_json::from_str(include_str!("tool_schemas.json"))
            .expect("validated tool schemas");
        let mut definitions: Vec<Value> = [
            ("Read","Read a text file with 1-based numbered lines. Use offset and limit for large files."),
            ("Write","Create or overwrite a text file. Read existing files fully before overwriting."),
            ("Edit","Replace a unique exact string, or every occurrence with replace_all. Read the file first."),
            ("Glob","Find files by glob pattern, sorted by modification time. Returns at most 100 matches."),
            ("Grep","Search text with regex, glob/type filters, context, multiline and paginated output."),
            ("Bash","Execute a shell command in the workspace. run_in_background returns a task ID and output path. Use TaskOutput or Read for output; TaskStop stops the process tree."),
            ("TaskOutput","Retrieve a session-owned background shell task's output. block waits up to timeout milliseconds."),
            ("TaskStop","Stop a session-owned background shell task and wait for its process tree to exit."),
        ].into_iter().map(|(name,description)|json!({"type":"function","function":{"name":name,"description":description,"parameters":schemas[name]}})).collect();
        let description: String = serde_json::from_str(include_str!("skill_description.json"))
            .expect("validated Skill description");
        definitions.push(json!({"type":"function","function":{"name":"Skill","description":description,"parameters":schemas["Skill"]}}));
        let description: String = serde_json::from_str(include_str!("question_description.json"))
            .expect("validated question description");
        definitions.push(json!({"type":"function","function":{"name":"AskUserQuestion","description":description,"parameters":schemas["AskUserQuestion"]}}));
        let descriptions: Value = serde_json::from_str(include_str!("todo_descriptions.json"))
            .expect("validated todo descriptions");
        for name in ["TodoRead", "TodoWrite"] {
            definitions.push(json!({"type":"function","function":{"name":name,"description":descriptions[name],"parameters":schemas[name]}}));
        }
        let descriptions: Value = serde_json::from_str(include_str!("agent_descriptions.json"))
            .expect("agent descriptions");
        for name in ["Agent", "SendMessage"] {
            definitions.push(json!({"type":"function","function":{"name":name,"description":descriptions[name],"parameters":schemas[name]}}));
        }
        definitions
    }
    fn requires_permission(&self, _name: &str) -> bool {
        false
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
pub(super) fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .with_context(|| format!("{key} must be a string"))
}
pub(super) fn uint(args: &Value, key: &str, default: u64) -> Result<u64> {
    match args.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= 9_007_199_254_740_991)
            .with_context(|| format!("{key} must be a nonnegative integer")),
    }
}
pub(super) fn boolean(args: &Value, key: &str, default: bool) -> Result<bool> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Bool(v)) => Ok(*v),
        Some(v) => match v.as_str().map(|s| s.trim().to_lowercase()).as_deref() {
            Some("true" | "1" | "yes" | "y" | "on") => Ok(true),
            Some("false" | "0" | "no" | "n" | "off") => Ok(false),
            _ if v == 1 => Ok(true),
            _ if v == 0 => Ok(false),
            _ => bail!("{key} must be boolean"),
        },
    }
}
pub(super) fn keys(args: &Value, allowed: &[&str]) -> Result<()> {
    let object = args.as_object().context("Tool arguments must be object")?;
    if let Some(key) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        bail!("Unsupported argument: {key}");
    }
    Ok(())
}
pub(super) fn resolve(cwd: &Path, input: &str) -> Result<PathBuf> {
    if input.trim().is_empty() || input.contains('\0') {
        bail!("Tool path must not be empty or contain NUL");
    }
    let p = Path::new(input);
    Ok(if p.is_absolute() {
        p.to_owned()
    } else {
        cwd.join(p)
    })
}
pub(super) fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        bail!("Cancelled")
    }
    Ok(())
}
pub fn truncate_utf8(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1
        }
        text.truncate(end);
    }
}
