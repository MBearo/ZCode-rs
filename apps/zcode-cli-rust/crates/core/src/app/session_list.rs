use super::Engine;
use crate::domain::{
    execution::Phase,
    session::Session,
    session_listing::{ListParams, MAX_LIST_BYTES},
    session_runtime::Persistence,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Node `TASK_LIST_SESSION_TYPES`.
const TASK_LIST_TYPES: [&str; 3] = ["interactive", "fork", "workflow_parent"];

impl Engine {
    pub(super) async fn list_sessions(&self, p: &Value) -> Result<Value> {
        let params = ListParams::parse(p)?;
        let records = self
            .store
            .list_sessions(&params, (&self.workspace, &self.workspace_path))
            .await?;
        let mut sessions = records
            .iter()
            .map(|record| record.projection(params.workspace.as_ref()))
            .collect::<Vec<_>>();
        if params.session_ids.is_none() {
            let stored: BTreeSet<&str> = records.iter().map(|r| r.id.as_str()).collect();
            let key = params.workspace.as_ref().map(|w| w.workspace_key.as_str());
            let now = self.clock.now();
            for s in self.sessions.values() {
                if !stored.contains(s.id.as_str()) {
                    sessions.extend(self.resident_listing(s, key, now));
                }
            }
        }
        let result = json!({"sessions":sessions});
        ensure!(
            serde_json::to_vec(&result)?.len() <= MAX_LIST_BYTES,
            "Session list exceeds frame budget; use a smaller limit or sessionIds batch"
        );
        Ok(result)
    }

    /// Node appends every resident record outside the stored page through
    /// `mapSessionInfo({app, workspace, taskType, parentSessionId})`: listing
    /// time as both timestamps, no title, idle.
    fn resident_listing(&self, s: &Session, key: Option<&str>, now: u64) -> Option<Value> {
        let deferred =
            s.phase == Phase::Draft && s.runtime.persistence != Some(Persistence::Immediate);
        if deferred || !TASK_LIST_TYPES.contains(&s.task_type.as_str()) {
            return None;
        }
        let workspace = s
            .runtime
            .workspace
            .clone()
            .unwrap_or_else(|| self.rebuilt_workspace(&s.id));
        if key.is_some_and(|key| workspace["workspaceKey"] != key) {
            return None;
        }
        let mut value = json!({"sessionId": s.id, "workspace": workspace, "sessionKind": s.task_type,
            "title": "", "mode": s.mode, "status": "idle", "createdAt": now, "updatedAt": now});
        if !s.provider.is_empty() && !s.model.is_empty() {
            value["model"] = json!({"providerId": s.provider, "modelId": s.model});
        }
        for (key, field) in [("parentSessionId", &s.parent_id), ("traceId", &s.trace_id)] {
            if let Some(field) = field {
                value[key] = field.clone().into();
            }
        }
        Some(value)
    }
}
