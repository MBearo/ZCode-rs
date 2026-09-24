//! Legacy `session/create` and `session/resume` (Node `server-operations.ts`).
use super::Engine;
use crate::{
    contract::{ModelIdentity, RuntimeError},
    domain::{
        execution::ExecutionState,
        legacy_params::{self, ParamsError},
        session::Session,
        session_runtime::{Persistence, RuntimeOptions, ToolFilter},
    },
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

pub(super) fn params_error(error: ParamsError) -> anyhow::Error {
    RuntimeError::Params {
        message: error.message,
        data: error.data,
    }
    .into()
}

pub(super) fn model_error(code: &str, message: String) -> anyhow::Error {
    RuntimeError::Named {
        name: "ModelProtocolError",
        message,
        code: Some(code.into()),
    }
    .into()
}

/// Node `registry.validateSelection` with its protocol errors, over the model
/// options of the Registry (`registry`) or of the config file. `unknown` is how
/// Node prints the requested model when the provider is not in the Registry.
pub(super) fn validate_selection(
    options: &[Value],
    registry: bool,
    model: &Value,
    unknown: &str,
) -> Result<()> {
    let provider = model["providerId"].as_str().unwrap_or("");
    let id = model["modelId"].as_str().unwrap_or("");
    if registry && !options.iter().any(|o| o["modelProviderId"] == provider) {
        bail!("Provider Registry 中不存在 Model: {unknown}");
    }
    let entry = options
        .iter()
        .find(|o| o["modelProviderId"] == provider && o["value"] == id)
        .ok_or_else(|| {
            model_error(
                "model_not_found",
                format!("Provider Registry 中不存在 Model: {provider}/{id}"),
            )
        })?;
    let levels: Vec<&str> = entry["modelThoughtLevels"]
        .as_array()
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match model["options"]["reasoningLevel"].as_str() {
        _ if levels.is_empty() => Ok(()),
        None => Err(model_error(
            "invalid_model_request",
            format!("Reasoning level is required for {provider}/{id}"),
        )),
        Some(level) if !levels.contains(&level) => Err(model_error(
            "invalid_model_request",
            format!("Reasoning effort \"{level}\" is not supported by {provider}/{id}"),
        )),
        Some(_) => Ok(()),
    }
}

pub(super) fn tool_filter(p: &Value) -> ToolFilter {
    let list = |key: &str| {
        p[key].as_array().map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
    };
    ToolFilter::new(
        list("toolAllowlist"),
        list("toolDenylist").unwrap_or_default(),
    )
}

impl Engine {
    pub(super) async fn legacy_create(&mut self, raw: &Value) -> Result<Value> {
        let p = legacy_params::create(raw).map_err(params_error)?;
        if p.get("sessionId").is_some() && p.get("importedHistory").is_none() {
            return Err(RuntimeError::Coded {
                code: -32602,
                message: "sessionId is only supported for imported history creates".into(),
            }
            .into());
        }
        match p["importedHistory"]["source"].as_str() {
            Some("sharedContext") => self.import_shared_context(raw).await,
            Some("claudeCode") => self.import_claude(&p).await,
            _ => self.create_legacy_session(&p).await,
        }
    }

    /// The single workspace this runtime serves; the params ref is echoed as given.
    pub(super) fn legacy_workspace(&self, p: &Value) -> Result<Value> {
        let workspace = &p["workspace"];
        let path = workspace["workspacePath"].as_str().unwrap_or("");
        let key = workspace["workspaceIdentity"].as_str().unwrap_or(path);
        ensure!(
            key == self.workspace && path == self.workspace_path,
            "Workspace identity mismatch"
        );
        Ok(workspace.clone())
    }

    /// Node create-time model validation, then `setModel("provider/model")`, which
    /// drops the reasoning level. Returns the selection and whether one was given.
    pub(super) fn legacy_model(&self, model: Option<&Value>) -> Result<(ModelIdentity, bool)> {
        let Some(model) = model else {
            return Ok((self.select(&json!({}), None)?, false));
        };
        let provider = model["providerId"].as_str().unwrap_or("");
        let id = model["modelId"].as_str().unwrap_or("");
        let Some(registry) = &self.registry else {
            // 无 Registry 时只有配置文件中的模型（Rust 开发与测试配置，Node 无对应），保留其档位。
            let config = self
                .config
                .clone()
                .context("Model configuration required")?;
            if config.provider_id != provider || config.model_id != id {
                return Err(model_error(
                    "model_not_found",
                    format!("Provider Registry 中不存在 Model: {provider}/{id}"),
                ));
            }
            return Ok((config, true));
        };
        validate_selection(
            &registry.model_options(),
            true,
            model,
            &format!("{provider}/{id}"),
        )?;
        let selection = ModelIdentity {
            provider_id: provider.into(),
            model_id: id.into(),
            reasoning_level: String::new(),
        };
        Ok((selection, true))
    }

    /// Applies `thoughtLevel` when the current model offers it (Node skips others).
    pub(super) fn legacy_thought_level(&mut self, id: &str, level: Option<&str>) {
        let Some(level) = level else { return };
        let s = self.sessions.get_mut(id).unwrap();
        if s.thought_levels.iter().any(|l| l == level) {
            s.reasoning_level = level.into();
            s.runtime.state_revision += 1;
        } else {
            tracing::warn!(
                target: "zcode::protocol",
                event = "zcode_protocol.session_create.thought_level_skipped",
                session_id = id,
                "Thought level is not offered by the session model"
            );
        }
    }

    async fn create_legacy_session(&mut self, p: &Value) -> Result<Value> {
        let workspace = self.legacy_workspace(p)?;
        let (selection, model_given) = self.legacy_model(p.get("model"))?;
        let id = format!("sess_{}", self.clock.id());
        let mut session = Session::new(
            id.clone(),
            self.workspace.clone(),
            selection.provider_id.clone(),
            selection.model_id.clone(),
            selection.reasoning_level.clone(),
            self.clock.id(),
            self.clock.now(),
        );
        session.workspace_path = Some(self.workspace_path.clone());
        session.workspace_directory = Some(self.workspace_path.clone());
        session.trace_id = Some(self.clock.id());
        session.parent_id = p["parentSessionId"].as_str().map(str::to_owned);
        // Node 初始模式：创建参数 → 项目偏好 → 配置文件 → build；不写项目偏好。
        let state = self.initial_execution_state(&json!({"mode": p["mode"]}));
        session.mode = state.mode;
        session.plan_enabled = state.plan_enabled;
        session.runtime = RuntimeOptions {
            persistence: Some(if p["persistence"] == "deferred" {
                Persistence::Deferred
            } else {
                Persistence::Immediate
            }),
            tools: tool_filter(p),
            title_generation_disabled: p["titleGenerationEnabled"] == false,
            state_revision: u64::from(model_given),
            workspace: Some(workspace),
            fresh: true,
            ..Default::default()
        };
        if let Some(servers) = p.get("mcpServers") {
            self.tools.configure_mcp(&id, servers).await?;
        }
        self.sessions.insert(id.clone(), session);
        self.apply_selection(&id, selection)?;
        self.legacy_thought_level(&id, p["thoughtLevel"].as_str());
        self.legacy_snapshot(&id)
    }

    pub(super) async fn legacy_resume(&mut self, raw: &Value) -> Result<Value> {
        let p = legacy_params::resume(raw).map_err(params_error)?;
        let id = p["sessionId"].as_str().unwrap().to_owned();
        // 活跃会话原样返回，参数全部忽略：Host 每次切换配置后都会以 resume 重新附着。
        if self.sessions.contains_key(&id) {
            return self.legacy_snapshot(&id);
        }
        let missing = || RuntimeError::Coded {
            code: -32004,
            message: format!("Session not found: {id}"),
        };
        let stored = self.store.load_session(&self.workspace, &id).await?;
        let stored = stored.ok_or_else(missing)?;
        if stored.archived {
            return Err(RuntimeError::Fault {
                message: format!("Session not found: {id}"),
                code: Some("SESSION_NOT_FOUND".into()),
            }
            .into());
        }
        self.ensure_session(&id).await?;
        if let Some(servers) = p.get("mcpServers") {
            self.tools.configure_mcp(&id, servers).await?;
        }
        let rebuilt = self.rebuilt_workspace(&id);
        let s = self.sessions.get_mut(&id).unwrap();
        s.runtime = RuntimeOptions {
            persistence: Some(Persistence::Immediate),
            tools: tool_filter(&p),
            workspace: Some(p.get("workspace").cloned().unwrap_or(rebuilt)),
            ..Default::default()
        };
        // Node 以最后一条 assistant 消息记录的模式恢复，并跳过已保存的执行状态。
        if let Some(mode) = s.last_assistant_mode.clone() {
            let current = ExecutionState {
                mode: s.mode,
                plan_enabled: s.plan_enabled,
            };
            let valid = matches!(mode.as_str(), "plan" | "build" | "edit" | "yolo" | "auto");
            let next =
                ExecutionState::resolve(Some(mode.as_str()).filter(|_| valid), None, current);
            if valid && next != current {
                s.mode = next.mode;
                s.plan_enabled = next.plan_enabled;
                s.revision += 1;
                self.refresh_permissions();
                self.publish(&id, vec![])?;
            }
        }
        self.legacy_snapshot(&id)
    }

    pub(super) fn rebuilt_workspace(&self, id: &str) -> Value {
        let s = &self.sessions[id];
        let path = s
            .workspace_path
            .clone()
            .unwrap_or_else(|| self.workspace_path.clone());
        let mut workspace = json!({"workspacePath": path, "workspaceKey": s.workspace});
        if s.workspace != path {
            workspace["workspaceIdentity"] = s.workspace.clone().into();
        }
        workspace
    }
}
