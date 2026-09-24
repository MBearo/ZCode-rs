//! Legacy `session/setModel`, `session/setThoughtLevel` and `session/setMode`
//! (Node `server-operations.ts` setters and `afterStateMutation`).
use super::Engine;
use super::legacy_session::{model_error, params_error, validate_selection};
use crate::{
    contract::{Method, ModelIdentity, RuntimeError, ServerMsg},
    domain::legacy_params,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn legacy_setter(&mut self, method: Method, raw: &Value) -> Result<Value> {
        let (parsed, reason) = match method {
            Method::SessionSetModel => (legacy_params::set_model(raw), "model_changed"),
            Method::SessionSetThoughtLevel => (
                legacy_params::set_thought_level(raw),
                "thought_level_changed",
            ),
            _ => (legacy_params::set_mode(raw), "mode_changed"),
        };
        let p = parsed.map_err(params_error)?;
        if method == Method::SessionSetThoughtLevel && p.get("thoughtLevel").is_none() {
            return Err(RuntimeError::Coded {
                code: -32602,
                message: "thoughtLevel is required".into(),
            }
            .into());
        }
        let id = self.legacy_resident(&p)?;
        self.legacy_expect(&id, &p)?;
        self.touch_session(&id);
        let before = self.session_selection(&id)?;
        let changed = match method {
            Method::SessionSetModel => {
                let selection = self.legacy_selection(&p["model"])?;
                self.apply_selection(&id, selection)?;
                self.session_selection(&id)? != before
            }
            Method::SessionSetThoughtLevel => {
                let level = p["thoughtLevel"].as_str().unwrap_or_default();
                self.legacy_level(&id, level)?;
                self.session_selection(&id)? != before
            }
            _ => {
                let revision = self.sessions[&id].revision;
                self.set_mode(&id, p["mode"].as_str().unwrap_or_default())
                    .await?;
                // apply_execution_state 在状态变化时已推进 V4 revision。
                return self
                    .after_state_mutation(&id, reason, self.sessions[&id].revision != revision)
                    .await;
            }
        };
        if changed {
            self.sessions.get_mut(&id).unwrap().revision += 1;
        }
        self.after_state_mutation(&id, reason, changed).await
    }

    /// Node `app.setModel(selection)` validation; the requested level is kept.
    fn legacy_selection(&self, model: &Value) -> Result<ModelIdentity> {
        let provider = model["providerId"].as_str().unwrap_or_default();
        let id = model["modelId"].as_str().unwrap_or_default();
        match &self.registry {
            // Node 把对象插入错误文本模板，得到 "[object Object]"。
            Some(registry) => {
                validate_selection(&registry.model_options(), true, model, "[object Object]")?
            }
            // 无 Registry 的开发配置：只有配置文件模型本身。
            None => validate_selection(&self.catalog(), false, model, "")?,
        }
        Ok(ModelIdentity {
            provider_id: provider.into(),
            model_id: id.into(),
            reasoning_level: model["options"]["reasoningLevel"]
                .as_str()
                .unwrap_or_default()
                .into(),
        })
    }

    /// Node `app.setThoughtLevel(level)`: only the level of the current selection changes.
    fn legacy_level(&mut self, id: &str, level: &str) -> Result<()> {
        let s = &self.sessions[id];
        let (provider, model) = (s.provider.clone(), s.model.clone());
        let options = match &self.registry {
            Some(registry) => registry.model_options(),
            None => self.catalog(),
        };
        if provider.is_empty() || !options.iter().any(|o| o["modelProviderId"] == provider) {
            bail!("当前 Session Model 不属于 Provider Registry");
        }
        let entry = options
            .iter()
            .find(|o| o["modelProviderId"] == provider && o["value"] == model)
            .ok_or_else(|| {
                model_error(
                    "model_not_found",
                    format!("Provider Registry 中不存在 Model: {provider}/{model}"),
                )
            })?;
        let supported = entry["modelThoughtLevels"]
            .as_array()
            .is_some_and(|levels| levels.iter().any(|l| l == level));
        if !supported {
            bail!("Unsupported reasoning effort: {level}");
        }
        self.sessions.get_mut(id).unwrap().reasoning_level = level.into();
        Ok(())
    }

    /// Node `afterStateMutation` for configuration-only reasons: the legacy
    /// revision advances (without touching `updatedAt`), the Host is told with
    /// `state.updated` and the snapshot lists only the current model.
    async fn after_state_mutation(
        &mut self,
        id: &str,
        reason: &str,
        changed: bool,
    ) -> Result<Value> {
        if changed {
            self.publish(id, vec![])?;
            self.persist(id, None).await?;
            self.notify_selection(id)?;
        }
        let s = self.sessions.get_mut(id).unwrap();
        s.runtime.state_revision += 1;
        let revision = s.runtime.state_revision;
        let snapshot = self.legacy_snapshot_with(id, false)?;
        self.outbox.push(ServerMsg::HostNotification {
            method: "state.updated",
            params: json!({"patch": snapshot["settings"], "reason": reason, "revision": revision,
                "scope": "session", "sessionId": id, "type": "state.updated",
                "workspace": snapshot["session"]["workspace"]}),
        });
        Ok(snapshot)
    }
}
