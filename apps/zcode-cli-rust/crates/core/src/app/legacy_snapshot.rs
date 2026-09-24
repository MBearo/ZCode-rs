//! `ZCodeSessionStateSnapshot` returned by legacy `session/*` methods (Node
//! `session-mapper.ts`): create and resume list every model, setters only the
//! current one.
use super::Engine;
use crate::domain::session::Session;
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
        snapshot["settings"] = self.legacy_settings(s, all_models);

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
        runtime["eventSeq"] = s.runtime.legacy.seq().into();
        runtime["goalVerifications"] = json!([]);
        runtime["goalVerificationTimeline"] = json!([]);
        if s.runtime.fresh {
            snapshot["projection"]["sessionId"] = "unknown".into();
            snapshot["projection"]["contextWindow"] = REDUCER_CONTEXT_WINDOW.into();
        }
        snapshot["todoGroups"] = json!([]);
        Ok(snapshot)
    }

    /// Node `mapSessionSettings` (also the `state.updated` patch). A selection
    /// without a reasoning level has no `options` (strict schema).
    pub(super) fn legacy_settings(&self, s: &Session, all_models: bool) -> Value {
        let bound = !s.provider.is_empty() && !s.model.is_empty();
        let levels: Vec<Value> = s
            .thought_levels
            .iter()
            .map(|l| json!({"label": l, "value": l}))
            .collect();
        let available = match &self.registry {
            Some(registry) => {
                let mut models = registry.legacy_models();
                if !all_models {
                    models.retain(|m| {
                        m["ref"]["providerId"] == s.provider && m["ref"]["modelId"] == s.model
                    });
                }
                models
            }
            // 无 Registry 的开发配置：只有配置文件模型。
            None => self
                .model
                .as_ref()
                .filter(|_| {
                    bound
                        && self
                            .config
                            .as_ref()
                            .is_some_and(|c| c.provider_id == s.provider && c.model_id == s.model)
                })
                .map(|model| {
                    let mut option = json!({"ref": {"providerId": s.provider, "modelId": s.model},
                        "label": s.model, "providerLabel": s.provider,
                        "contextWindow": model.context_policy().window,
                        "properties": model.format_properties(), "reasoning": {"levels": levels}});
                    if let Some(level) = s.thought_levels.last() {
                        option["reasoning"]["defaultLevel"] = level.clone().into();
                    }
                    option
                })
                .into_iter()
                .collect(),
        };
        let mut model = json!({"available": available});
        if bound {
            model["lastUsed"] = json!({"providerId": s.provider, "modelId": s.model});
            let mut current = json!({"providerId": s.provider, "modelId": s.model});
            if !s.reasoning_level.is_empty() {
                current["options"] = json!({"reasoningLevel": s.reasoning_level});
            }
            model["current"] = current;
        }
        let mut thought = json!({"available": levels, "enabled": !s.thought_levels.is_empty()});
        if s.thought_levels.contains(&s.reasoning_level) {
            thought["current"] = s.reasoning_level.clone().into();
        }
        // plan 在 legacy 设置中显示为其权限模式。
        json!({"mode": {"current": s.mode}, "model": model, "permission": {"mode": s.mode},
            "thoughtLevel": thought})
    }
}
