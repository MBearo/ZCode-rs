//! `ZCodeSessionStateSnapshot` returned by legacy `session/*` methods (Node
//! `session-mapper.ts`): create and resume list every model, setters only the
//! current one.
use super::Engine;
use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Node's event reducer starts every session at this window and mode.
const REDUCER_CONTEXT_WINDOW: u64 = 200_000;

impl Engine {
    pub(super) fn legacy_snapshot(&self, id: &str) -> Result<Value> {
        self.legacy_snapshot_with(id, true)
    }

    /// `all_models: false` is Node `modelAvailability: "current"`.
    pub(super) fn legacy_snapshot_with(&self, id: &str, all_models: bool) -> Result<Value> {
        let s = self.sessions.get(id).context("Session unavailable")?;
        let mut snapshot = self.read_session_snapshot(s, &json!({}))?;
        let bound = !s.provider.is_empty() && !s.model.is_empty();
        let levels: Vec<Value> = s
            .thought_levels
            .iter()
            .map(|l| json!({"label": l, "value": l}))
            .collect();
        let settings = &mut snapshot["settings"];
        // plan 在 legacy 设置中显示为其权限模式。
        settings["mode"] = json!({"current": s.mode});
        settings["permission"] = json!({"mode": s.mode});
        if let Some(registry) = &self.registry {
            let mut models = registry.legacy_models();
            if !all_models {
                models.retain(|m| {
                    m["ref"]["providerId"] == s.provider && m["ref"]["modelId"] == s.model
                });
            }
            settings["model"]["available"] = models.into();
        }
        if bound {
            settings["model"]["lastUsed"] = json!({"providerId": s.provider, "modelId": s.model});
            let mut current = json!({"providerId": s.provider, "modelId": s.model});
            if !s.reasoning_level.is_empty() {
                current["options"] = json!({"reasoningLevel": s.reasoning_level});
            }
            settings["model"]["current"] = current;
        }
        let mut thought = json!({"available": levels, "enabled": !s.thought_levels.is_empty()});
        if s.thought_levels.contains(&s.reasoning_level) {
            thought["current"] = s.reasoning_level.clone().into();
        }
        settings["thoughtLevel"] = thought;

        let info = &mut snapshot["session"];
        info["mode"] = "build".into();
        info["target"] = Value::Null;
        if let Some(trace) = &s.trace_id {
            info["traceId"] = trace.clone().into();
        }
        if let Some(workspace) = &s.runtime.workspace {
            info["workspace"] = workspace.clone();
        }
        if bound {
            info["model"] = json!({"providerId": s.provider, "modelId": s.model});
        }
        let object = info.as_object_mut().unwrap();
        if s.runtime.fresh {
            // Node 新建会话的快照不含标题来源与父会话（尚未持久化）。
            object.remove("titleSource");
            object.remove("parentSessionId");
        }

        let projection = &mut snapshot["projection"];
        projection["mode"] = "build".into();
        projection["target"] = Value::Null;
        let runtime = &mut snapshot["runtime"];
        runtime["stateRevision"] = s.runtime.state_revision.into();
        runtime["goalVerifications"] = json!([]);
        runtime["goalVerificationTimeline"] = json!([]);
        if s.runtime.fresh {
            snapshot["projection"]["sessionId"] = "unknown".into();
            snapshot["projection"]["contextWindow"] = REDUCER_CONTEXT_WINDOW.into();
            snapshot["runtime"]["eventSeq"] = 0.into();
        }
        snapshot["todoGroups"] = json!([]);
        Ok(snapshot)
    }
}
