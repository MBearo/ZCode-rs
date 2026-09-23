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
        Some(
            self.registry
                .resolve(&self.selection.borrow())
                .unwrap_or_else(|_| Arc::new(Unavailable)),
        )
    }
    fn context_policy(&self) -> crate::domain::context::ContextPolicy {
        self.bind().unwrap().context_policy()
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
struct Unavailable;
#[async_trait::async_trait]
impl ModelPort for Unavailable {
    async fn complete(
        &self,
        _: Vec<Value>,
        _: &[Value],
        _: &EventSink,
        _: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        Err(ModelFailure::new("model_not_found", false))
    }
}
impl Engine {
    pub(super) fn catalog(&self) -> Vec<Value> {
        self.registry.as_ref().map(|r| r.catalog()).unwrap_or_else(|| self.config.as_ref().map(|c| vec![json!({"value":c.model_id,"name":c.model_id,"modelProviderId":c.provider_id,"modelProviderName":c.provider_id,"modelThoughtLevels":[c.reasoning_level]})]).unwrap_or_default())
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
        let fallback = fallback
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
    pub(super) fn apply_selection(&mut self, id: &str, selection: ModelIdentity) -> Result<()> {
        let levels = self
            .registry
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
            .unwrap_or_default();
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        s.provider = selection.provider_id;
        s.model = selection.model_id;
        s.reasoning_level = selection.reasoning_level;
        s.thought_levels = levels;
        Ok(())
    }
    pub(super) fn notify_selection(&self, id: &str) -> Result<()> {
        if let Some(active) = self.active.get(id) {
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
        let topic = format!("workspace-config/{}", self.workspace);
        let ids = self
            .subscriptions
            .values()
            .filter(|s| s.topic == topic && !s.paused)
            .map(|s| s.id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            self.snapshot_frame(&id, "online")?;
        }
        Ok(())
    }
    pub(super) fn cancel_auth(&mut self, id: &str) {
        let keys = self
            .auth
            .iter()
            .filter(|(_, (session, _, _, _))| session == id)
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>();
        for key in keys {
            if let Some((session, _, workspace, _)) = self.auth.remove(&key) {
                self.outbox.push(json!({"method":"interaction/providerRuntimeHeadersCancelled","params":{"requestId":key,"sessionId":session,"workspace":workspace}}));
            }
        }
    }
}
