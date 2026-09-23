//! `session/create` with `importedHistory.source = "claudeCode"`.
use super::Engine;
use crate::{
    contract::StorageCommitFailure,
    domain::{
        claude_import,
        session::Session,
        session_runtime::{Persistence, RuntimeOptions},
    },
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn import_claude(&mut self, p: &Value) -> Result<Value> {
        let workspace = self.legacy_workspace(p)?;
        let (selection, model_given) = self.legacy_model(p.get("model"))?;
        let id = p["sessionId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("sess_{}", self.clock.id()));
        if !self.sessions.contains_key(&id)
            && self
                .store
                .load_session(&self.workspace, &id)
                .await?
                .is_some()
        {
            self.ensure_session(&id).await?;
        }
        // 与 Node 一致允许覆盖活跃会话（Host 的历史修复依赖这一点），但不在运行中改写历史。
        ensure!(
            self.sessions.get(&id).is_none_or(|s| !s.running()),
            "Cannot import history while a prompt is running"
        );
        let now = self.clock.now();
        let history = &p["importedHistory"];
        let previous = self.sessions.get(&id).cloned();
        let mut session = previous.clone().unwrap_or_else(|| {
            let mut s = Session::new(
                id.clone(),
                self.workspace.clone(),
                String::new(),
                String::new(),
                String::new(),
                self.clock.id(),
                claude_import::created_at(history, now),
            );
            s.trace_id = Some(self.clock.id());
            s
        });
        session.workspace_path = Some(self.workspace_path.clone());
        session.workspace_directory = Some(self.workspace_path.clone());
        session.epoch = self.clock.id();
        // Node 以 app.getMode() 记录：plan 记为 build，plan 开关不随导入保存。
        let state = self.initial_execution_state(&json!({"mode": p["mode"]}));
        session.mode = state.mode;
        session.plan_enabled = false;
        // 档位按导入前绑定的模型判定（Node 在恢复解绑模型之前应用 thoughtLevel）。
        let thought_applied = p["thoughtLevel"]
            .as_str()
            .is_some_and(|level| self.model_levels(&selection).iter().any(|l| l == level));
        claude_import::apply(&mut session, history, now);
        session.updated_at = now;
        session.runtime = RuntimeOptions {
            persistence: Some(Persistence::Immediate),
            tools: super::legacy_session::tool_filter(p),
            title_generation_disabled: p["titleGenerationEnabled"] == false,
            state_revision: u64::from(model_given) + u64::from(thought_applied) + 1,
            workspace: Some(workspace),
            fresh: false,
        };
        if let Some(servers) = p.get("mcpServers") {
            self.tools.configure_mcp(&id, servers).await?;
        }
        self.store
            .commit_receipt(&self.workspace, Some(&mut session), None)
            .await
            .context(StorageCommitFailure)?;
        let summary = session.summary();
        self.sessions.insert(id.clone(), session);
        self.touch_session(&id);
        self.closed.remove(&id);
        self.publish(&id, vec![])?;
        self.publish_index(&id, Some(summary))?;
        self.legacy_snapshot(&id)
    }
}
