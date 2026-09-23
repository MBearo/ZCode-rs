use super::Engine;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) fn read_session(&self, p: &Value) -> Result<Value> {
        let id = p["sessionId"].as_str().context("Session id required")?;
        let s = self.sessions.get(id).context("Session unavailable")?;
        self.read_session_snapshot(s, p)
    }
    pub(super) fn read_session_snapshot(
        &self,
        s: &crate::domain::session::Session,
        p: &Value,
    ) -> Result<Value> {
        let id = s.id.as_str();
        // task-index 的观察不能触发 resume 或改变 continuous/replayable 订阅归属。
        let delivery = p["deliveryKind"].as_str();
        ensure!(
            delivery.is_none_or(|d| matches!(d, "desktop-continuous" | "web-remote-replayable")),
            "Invalid delivery kind"
        );
        let limit = p
            .get("messageLimit")
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n > 0)
                    .context("Invalid message limit")
            })
            .transpose()?;
        let status = if s.running() {
            "running"
        } else if s.last_error.is_some() {
            "error"
        } else {
            "idle"
        };
        let selection = crate::contract::ModelIdentity {
            provider_id: s.provider.clone(),
            model_id: s.model.clone(),
            reasoning_level: s.reasoning_level.clone(),
        };
        let selected = (!s.provider.is_empty() && !s.model.is_empty()).then(|| json!({"providerId":s.provider,"modelId":s.model,"options":{"reasoningLevel":s.reasoning_level}}));
        let model = self
            .registry
            .as_ref()
            .and_then(|r| r.resolve(&selection).ok())
            .or_else(|| {
                self.config
                    .as_ref()
                    .filter(|c| **c == selection)
                    .and(self.model.clone())
            });
        let levels = s
            .thought_levels
            .iter()
            .map(|l| json!({"value":l,"label":l}))
            .collect::<Vec<_>>();
        let mut available = vec![];
        if let (Some(model), Some(selected)) = (model, &selected) {
            available.push(json!({"ref":selected,"label":s.model,"providerLabel":s.provider,"contextWindow":model.context_policy().window,"properties":model.format_properties(),"reasoning":{"levels":levels}}));
        }
        let mut model_settings = json!({"available":available});
        if let Some(selected) = &selected {
            model_settings["current"] = selected.clone();
        }
        let mut thought = json!({"enabled":!levels.is_empty(),"available":levels});
        if !s.reasoning_level.is_empty() {
            thought["current"] = s.reasoning_level.clone().into();
        }
        let mut info = json!({"sessionId":s.id,"workspace":{"workspacePath":self.workspace_path,"workspaceKey":s.workspace},"sessionKind":s.task_type,"title":s.title,"titleSource":s.title_source,"mode":s.mode,"status":status,"createdAt":s.created_at,"updatedAt":s.updated_at});
        if s.workspace != self.workspace_path {
            // workspace key 的本地路径 fallback 不是远端 identity，不能把本地任务投影成远端。
            info["workspace"]["workspaceIdentity"] = s.workspace.clone().into();
        }
        if let Some(selected) = &selected {
            info["model"] = selected.clone();
        }
        if let Some(parent) = &s.parent_id {
            info["parentSessionId"] = parent.clone().into();
        }
        if let Some(archived) = s.archived_at {
            info["archivedAt"] = archived.into();
        }
        let mut projection = json!({"sessionId":s.id,"status":status,"mode":s.mode,"turnCount":s.rows.iter().filter(|r|r["kind"]=="turnHeader").count(),"totalTokenCount":s.usage["cumulative"]["inputTokens"].as_u64().unwrap_or(0).saturating_add(s.usage["cumulative"]["outputTokens"].as_u64().unwrap_or(0)),"contextUsed":s.usage["contextWindow"]["usedTokens"].as_u64().unwrap_or(0),"contextWindow":s.usage["contextWindow"]["maxTokens"].as_u64().unwrap_or(0),"pendingPermissions":[],"activeToolCalls":s.rows.iter().filter(|r|r["kind"]=="toolCall" && r["status"]=="running").map(|r|json!({"toolCallId":r["toolCallId"],"toolName":r["toolName"],"status":"running","startedAt":r["startedAt"]})).collect::<Vec<_>>(),"backgroundJobs":s.background.values().filter(|t|t.status=="running").map(|t|t.projection()).collect::<Vec<_>>()});
        if let Some(error) = &s.last_error {
            let mut error = error.clone();
            error
                .as_object_mut()
                .unwrap()
                .retain(|k, _| matches!(k.as_str(), "code" | "message" | "detail" | "attribution"));
            error["type"] = "runtime".into();
            projection["lastError"] = error;
        }
        let mut runtime = json!({"eventSeq":s.seq,"stateRevision":s.revision,"pendingRequestIds":self.auth.iter().filter(|(_, (session,_,_,_))|session==id).map(|(id,_)|id).collect::<Vec<_>>()});
        if let Some(delivery) = delivery {
            runtime["deliveryKind"] = delivery.into();
        }
        if let Some(active) = self.active.get(id) {
            projection["currentTurnId"] = active.turn_id.clone().into();
            runtime["activeTurnId"] = active.turn_id.clone().into();
            runtime["activeTurnKind"] = if s.compact_instructions.is_some() {
                "compact"
            } else {
                "regular"
            }
            .into();
        }
        if let Some(retry) = &s.api_retry {
            runtime["apiRetry"] = json!({"kind":"api_retry","attempt":retry.attempt,"maxRetries":retry.max_attempts.saturating_sub(1),"retryDelayMs":retry.next_retry_at.saturating_sub(self.clock.now()),"errorStatus":null,"error":retry.reason_code});
        }
        let mut messages = crate::domain::legacy_snapshot::messages(s, &self.workspace_path);
        if let Some(limit) = limit {
            let start = messages.len().saturating_sub(limit as usize);
            messages.drain(..start);
        }
        let snapshot = json!({"protocol":{"name":"ZCode Protocol","version":1},"session":info,"settings":{"model":model_settings,"thoughtLevel":thought,"mode":{"current":s.mode},"permission":{"mode":s.mode}},"projection":projection,"runtime":runtime,"messages":messages,"todos":s.todos,"slashCommands":self.workspace_config()["slashCommands"]});
        ensure!(
            serde_json::to_vec(&snapshot)?.len() <= 900 * 1024,
            "Session snapshot exceeds frame budget; use messageLimit and V4 history queries"
        );
        Ok(snapshot)
    }
}
