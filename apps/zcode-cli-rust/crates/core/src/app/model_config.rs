use super::Engine;
use crate::contract::{
    EventSink, ModelFailure, ModelIdentity, ModelOutput, ModelPort, ModelRegistry,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) struct LiveModel {
    pub registry: Arc<dyn ModelRegistry>,
    pub selection: tokio::sync::watch::Receiver<ModelIdentity>,
}
#[async_trait::async_trait]
impl ModelPort for LiveModel {
    fn bind(&self) -> Option<Arc<dyn ModelPort>> {
        let selection = self.selection.borrow();
        // 与 Node 模型工厂一致：缺少推理档位的选择在构建模型时失败（invalid_model_request）。
        let reason = if selection.reasoning_level.is_empty() {
            "invalid_request"
        } else {
            "model_not_found"
        };
        Some(
            self.registry
                .resolve(&selection)
                .unwrap_or_else(|_| Arc::new(Unavailable(reason))),
        )
    }
    fn context_policy(&self) -> crate::domain::context::ContextPolicy {
        self.bind().unwrap().context_policy()
    }
    fn supports_native_web_search(&self) -> bool {
        self.bind().is_some_and(|m| m.supports_native_web_search())
    }
    fn account_auth(&self) -> bool {
        self.bind().is_some_and(|m| m.account_auth())
    }
    fn format_properties(&self) -> Value {
        self.bind().map_or(Value::Null, |m| m.format_properties())
    }
    fn auxiliary(&self) -> Option<Arc<dyn ModelPort>> {
        let mut selection = self.selection.borrow().clone();
        let lowest = self
            .registry
            .model_options()
            .into_iter()
            .find(|o| {
                o["modelProviderId"] == selection.provider_id.as_str()
                    && o["value"] == selection.model_id.as_str()
            })
            .and_then(|o| o["modelThoughtLevels"][0].as_str().map(str::to_owned));
        if let Some(level) = lowest {
            selection.reasoning_level = level;
        }
        let model = self.registry.resolve(&selection).ok()?;
        Some(
            model
                .with_max_output_tokens(4096)
                .ok()
                .flatten()
                .unwrap_or(model),
        )
    }
    async fn complete(
        &self,
        messages: Vec<Value>,
        tools: &[Value],
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        self.bind()
            .unwrap()
            .complete(messages, tools, sink, cancel)
            .await
    }
}
struct Unavailable(&'static str);
#[async_trait::async_trait]
impl ModelPort for Unavailable {
    async fn complete(
        &self,
        _: Vec<Value>,
        _: &[Value],
        _: &EventSink,
        _: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        Err(ModelFailure::new(self.0, false))
    }
}
impl Engine {
    pub(super) fn catalog(&self) -> Vec<Value> {
        self.registry.as_ref().map(|r| r.catalog()).unwrap_or_else(|| self.config.as_ref().map(|c| vec![json!({"value":c.model_id,"name":c.model_id,"modelProviderId":c.provider_id,"modelProviderName":c.provider_id,"modelThoughtLevels":[c.reasoning_level]})]).unwrap_or_default())
    }
    /// Node's cold usage seed (`sessionUsageSeedFromRuntimeContextUsage`): a
    /// resumed session's stored context use against its model's window.
    pub(super) fn seed_context_window(&self, s: &mut crate::domain::session::Session) {
        let Some(used) = s.cold_context_used.take().filter(|used| *used > 0) else {
            return;
        };
        if s.usage["contextWindow"].is_object() {
            return;
        }
        let window = self.model_window(s);
        s.usage["contextWindow"] = json!({"usedTokens": used, "maxTokens": window,
            "autoCompactThresholdTokens": null});
    }
    pub(super) fn session_selection(&self, id: &str) -> Result<ModelIdentity> {
        let s = self.sessions.get(id).context("Session unavailable")?;
        Ok(ModelIdentity {
            provider_id: s.provider.clone(),
            model_id: s.model.clone(),
            reasoning_level: s.reasoning_level.clone(),
        })
    }
    pub(super) fn select(
        &self,
        p: &Value,
        fallback: Option<ModelIdentity>,
    ) -> Result<ModelIdentity> {
        // 修复：没有存储模型选择的会话（Node 的分享导入、Claude 导入）冷加载后 provider/model
        // 为空，原先把空选择当回退而报 "Selected model is unavailable"；Node 缺省选择时用默认模型。
        let fallback = fallback
            .filter(|f| !f.provider_id.is_empty() && !f.model_id.is_empty())
            .or_else(|| self.registry.as_ref().and_then(|r| r.default_selection()))
            .or_else(|| self.config.clone())
            .unwrap_or(ModelIdentity {
                provider_id: String::new(),
                model_id: String::new(),
                reasoning_level: String::new(),
            });
        let selection = p.get("modelSelection").filter(|v| !v.is_null());
        let provider = selection
            .and_then(|s| s["providerId"].as_str())
            .or_else(|| p["provider"].as_str())
            .unwrap_or(&fallback.provider_id);
        let model = selection
            .and_then(|s| s["modelId"].as_str())
            .or_else(|| p["model"].as_str())
            .unwrap_or(&fallback.model_id);
        if let Some(s) = selection {
            let _: crate::domain::protocol::ModelSelection = serde_json::from_value(s.clone())?;
            ensure!(
                s.get("options").is_none_or(|v| v
                    .as_object()
                    .is_some_and(|o| o.keys().all(|k| k == "reasoningLevel"))),
                "Unsupported model options"
            );
        }
        let catalog = self
            .registry
            .as_ref()
            .map(|r| r.model_options())
            .unwrap_or_else(|| self.catalog());
        let option = catalog
            .iter()
            .find(|o| o["value"] == model && o["modelProviderId"] == provider)
            .context("Selected model is unavailable")?;
        let levels = option["modelThoughtLevels"]
            .as_array()
            .context("Missing reasoning levels")?;
        let explicit = selection
            .and_then(|s| s["options"]["reasoningLevel"].as_str())
            .or_else(|| p["thought"].as_str())
            .filter(|s| !s.is_empty());
        let level = explicit
            .or_else(|| {
                (fallback.provider_id == provider
                    && fallback.model_id == model
                    && levels.iter().any(|l| l == &fallback.reasoning_level))
                .then_some(fallback.reasoning_level.as_str())
            })
            .or_else(|| levels.last().and_then(Value::as_str))
            .context("Missing reasoning level")?;
        ensure!(
            levels.iter().any(|l| l == level),
            "Unsupported reasoning level"
        );
        Ok(ModelIdentity {
            provider_id: provider.into(),
            model_id: model.into(),
            reasoning_level: level.into(),
        })
    }
    pub(super) fn selection_marker(
        &mut self,
        id: &str,
        to: &ModelIdentity,
        command: &str,
    ) -> Option<Value> {
        let s = self.sessions.get_mut(id)?;
        let turn = s.rows.last()?["turnId"].as_str()?.to_owned();
        let mut row = s.row("timelineMarker", &turn, &self.clock.id(), self.clock.now());
        row["lane"] = "lightBoundary".into();
        row["sourceCommandId"] = command.into();
        row["marker"] = json!({"type":"modelChange","fromProvider":s.provider,"fromModel":s.model,"toProvider":to.provider_id,"toModel":to.model_id,"toThought":to.reasoning_level});
        s.rows.push(row.clone());
        Some(row)
    }
    /// Reasoning levels the selected model offers.
    pub(super) fn model_levels(&self, selection: &ModelIdentity) -> Vec<String> {
        self.registry
            .as_ref()
            .map(|r| r.model_options())
            .unwrap_or_else(|| self.catalog())
            .into_iter()
            .find(|o| {
                o["value"] == selection.model_id && o["modelProviderId"] == selection.provider_id
            })
            .map(|o| {
                o["modelThoughtLevels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(super) fn apply_selection(&mut self, id: &str, selection: ModelIdentity) -> Result<()> {
        let levels = self.model_levels(&selection);
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        let changed = (&s.provider, &s.model, &s.reasoning_level)
            != (
                &selection.provider_id,
                &selection.model_id,
                &selection.reasoning_level,
            );
        s.provider = selection.provider_id;
        s.model = selection.model_id;
        s.reasoning_level = selection.reasoning_level;
        s.thought_levels = levels;
        if changed {
            self.node(id, |s, now| s.node_model_selection(now));
            self.window_selected(id);
        }
        Ok(())
    }
    pub(super) fn notify_selection(&self, id: &str) -> Result<()> {
        // 执行级选型固定在本轮，会话选择的变化不影响它。
        if let Some(active) = self.active.get(id).filter(|a| a.execution.is_none()) {
            active.selection.send_replace(self.session_selection(id)?);
        }
        Ok(())
    }
    pub(super) async fn update_account(&mut self, p: &Value) -> Result<Value> {
        let registry = self
            .registry
            .as_ref()
            .context("Account Provider Registry is not configured")?;
        let received = registry.received_account().await.as_ref() != Some(p);
        let changed = registry.refresh(Some(p.clone())).await?;
        if changed {
            self.refresh_catalog()?;
        }
        Ok(
            json!({"receivedRevision":p["revision"],"providerCount":p["providers"].as_object().map_or(0,|o|o.len()),"status":if received{"received"}else{"unchanged"}}),
        )
    }
    pub(super) fn refresh_catalog(&mut self) -> Result<()> {
        self.config_seq += 1;
        self.config = self.registry.as_ref().and_then(|r| r.default_selection());
        self.publish_config();
        Ok(())
    }
    /// Cancel `id`'s pending host credential requests and announce each cancellation.
    pub(super) fn cancel_auth(&mut self, id: &str) {
        let released = self.waiters.release_host(id);
        self.announce_cancelled(released);
    }
    /// Release every waiter owned by `id` (terminal path of a run, job or session).
    pub(super) fn release_waiters(&mut self, id: &str) {
        // 子代理的权限提示挂在根会话上；释放时一并撤下，避免留下无法应答的交互。
        for (interaction, host) in self.waiters.hosted_elsewhere(id) {
            if let Some(s) = self.sessions.get_mut(&host) {
                s.pending
                    .retain(|p| p["interactionId"] != interaction.as_str());
                s.revision += 1;
                let _ = self.publish(&host, vec![]);
            }
        }
        let released = self.waiters.release(id);
        debug_assert!(!self.waiters.holds(id), "waiters of {id} survived release");
        self.announce_cancelled(released);
    }
    fn announce_cancelled(&mut self, released: Vec<(String, super::waiters::HostWait)>) {
        for (key, wait) in released {
            self.outbox.push(crate::contract::ServerMsg::HostNotification {
                method: "interaction/providerRuntimeHeadersCancelled",
                params: json!({"requestId":key,"sessionId":wait.owner,"workspace":wait.workspace}),
            });
        }
    }
}

impl Engine {
    /// Node `interaction/requestProviderRuntimeHeaders`: the Host supplies the
    /// credentials of one model request; the reply goes back to the run.
    pub(super) fn request_auth(
        &mut self,
        id: &str,
        turn: &str,
        provider: String,
        selection: Value,
        access: Value,
        reply: tokio::sync::oneshot::Sender<Value>,
    ) {
        if reply.is_closed() {
            return;
        }
        let request_id = format!("rust-auth-{}", self.clock.id());
        let workspace = json!({"workspaceKey":self.workspace,"workspacePath":self.workspace_path,"workspaceIdentity":self.workspace});
        let params = json!({"requestId":request_id,"sessionId":id,"turnId":turn,"workspace":workspace,"providerId":provider,"modelSelection":selection,"accountAccess":access,"reason":"model-request"});
        self.waiters.add_host(
            request_id.clone(),
            super::waiters::HostWait {
                owner: id.into(),
                workspace,
                reply,
            },
        );
        self.outbox.push(crate::contract::ServerMsg::HostRequest {
            id: request_id,
            method: "interaction/requestProviderRuntimeHeaders",
            params,
        });
    }
}
