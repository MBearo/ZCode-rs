//! Execution state and permission policy owner (Node `execution-state.ts`,
//! `PermissionService` inputs, `interaction-broker.ts` answers).
//!
//! The engine is the only writer of mode, plan switch, project and session rules.
//! Every active run holds a `watch` receiver of one immutable [`Snapshot`], so a
//! tool call reads mode, plan and rules once without a round trip.
use super::Engine;
use crate::{
    contract::{PermissionAnswer, PermissionRequest},
    domain::{
        execution::ExecutionState,
        permission::{self as policy, RulePolicy, Ruleset},
    },
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// Engine-owned permission inputs.
#[derive(Default)]
pub(super) struct Permissions {
    pub config: policy::Config,
    /// Config file `permission.mode`, the last fallback for new sessions.
    pub config_mode: Option<String>,
    pub project_rules: Arc<Ruleset>,
    /// Project preference written by `switchCollaborationMode`.
    pub project_mode: Option<String>,
    /// Session grants (Node `PermissionService.sessionRules`), per root session.
    pub session_rules: BTreeMap<String, Arc<Ruleset>>,
}

/// What one tool call is checked against.
pub(crate) struct Snapshot {
    pub state: ExecutionState,
    pub policy: policy::Policy,
    pub project_rules: Arc<Ruleset>,
    pub working_directory: String,
}

impl Snapshot {
    /// `rules` is the tool's own rule matching (Bash), `None` for generic subjects.
    pub fn check(
        &self,
        tool: &str,
        input: &Value,
        capability: &policy::ToolCapability,
        rules: Option<&dyn RulePolicy>,
    ) -> policy::Decision {
        let ctx = policy::Context {
            tool,
            input,
            mode: self.state.mode,
            plan_enabled: self.state.plan_enabled,
            working_directory: Some(&self.working_directory),
        };
        self.policy.check(
            &ctx,
            &policy::resolve(tool, capability),
            Some(&self.project_rules),
            rules,
        )
    }
}

/// Node `sanitizeText` for model-visible permission errors.
pub(super) fn summarize(message: &str) -> String {
    let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.encode_utf16().count() <= 500 {
        return compact;
    }
    let mut out = String::new();
    let mut units = 0;
    for c in compact.chars() {
        units += c.len_utf16();
        if units > 497 {
            break;
        }
        out.push(c);
    }
    out + "..."
}

impl Permissions {
    /// Project rules and mode preference from the workspace settings; the
    /// config file part comes from `Engine::with_permission_config`.
    pub fn from_settings(settings: &BTreeMap<(String, String), Value>) -> Self {
        let setting = |key: &str| settings.get(&("permission".to_owned(), key.to_owned()));
        Self {
            project_rules: Arc::new(
                setting("ruleset")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default(),
            ),
            project_mode: setting("mode")
                .and_then(|v| v["mode"].as_str())
                .map(str::to_owned),
            ..Self::default()
        }
    }
}

impl Engine {
    /// Node initial mode: create config → project preference → config file → build.
    pub(super) fn initial_execution_state(&self, config: &Value) -> ExecutionState {
        let mode = config["mode"]
            .as_str()
            .or(self.permissions.project_mode.as_deref())
            .or(self.permissions.config_mode.as_deref());
        ExecutionState::resolve(
            mode,
            config["planEnabled"].as_bool(),
            ExecutionState::default(),
        )
    }

    /// Node `resolveSubmittedExecutionState`: the input's mode and plan merged with
    /// the session's, `auto` submitted as `build`.
    pub(super) fn submitted_execution_state(&self, id: &str, p: &Value) -> Result<ExecutionState> {
        let s = self.sessions.get(id).context("Session unavailable")?;
        let current = ExecutionState {
            mode: s.mode,
            plan_enabled: s.plan_enabled,
        };
        let mut state =
            ExecutionState::resolve(p["mode"].as_str(), p["planEnabled"].as_bool(), current);
        if state.mode == crate::domain::execution::Mode::Auto {
            state.mode = crate::domain::execution::Mode::Build;
        }
        Ok(state)
    }

    pub(super) fn root_session(&self, id: &str) -> String {
        let mut root = id;
        while let Some(parent) = self.sessions.get(root).and_then(|s| s.parent_id.as_deref()) {
            root = parent;
        }
        root.into()
    }

    fn is_explore(&self, id: &str) -> bool {
        self.sessions
            .get(id)
            .and_then(|s| s.agent_profile.as_ref())
            .is_some_and(|p| p.name == "Explore" && p.source == "built-in")
    }

    pub(super) fn permission_snapshot(&self, id: &str) -> Arc<Snapshot> {
        let state = self
            .sessions
            .get(id)
            .map_or_else(ExecutionState::default, |s| ExecutionState {
                mode: s.mode,
                plan_enabled: s.plan_enabled,
            });
        // 与 Node 一致：内置 Explore 使用全新的默认策略；其余子代理共享根会话的授权与配置。
        let policy = if self.is_explore(id) {
            policy::Policy::default()
        } else {
            policy::Policy {
                config: self.permissions.config.clone(),
                session_rules: self
                    .permissions
                    .session_rules
                    .get(&self.root_session(id))
                    .map(|r| (**r).clone())
                    .unwrap_or_default(),
            }
        };
        Arc::new(Snapshot {
            state,
            policy,
            project_rules: self.permissions.project_rules.clone(),
            working_directory: self.workspace_path.clone(),
        })
    }

    /// Publishes new snapshots to every active run after an input changed.
    pub(super) fn refresh_permissions(&self) {
        for (id, active) in &self.active {
            active
                .permissions
                .send_replace(self.permission_snapshot(id));
        }
    }

    /// Node `applyRuntimeExecutionState`. Returns whether anything changed; the
    /// caller persists and publishes. `tool_call` marks a tool-driven transition.
    pub(super) fn apply_execution_state(
        &mut self,
        id: &str,
        mode: Option<&str>,
        plan: Option<bool>,
        tool_call: Option<&str>,
    ) -> Result<bool> {
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        let previous = ExecutionState {
            mode: s.mode,
            plan_enabled: s.plan_enabled,
        };
        let next = ExecutionState::resolve(mode, plan, previous);
        if next == previous {
            return Ok(false);
        }
        anyhow::ensure!(
            !(next.plan_enabled
                && !previous.plan_enabled
                && s.goal.as_ref().is_some_and(|g| g.active())),
            "Plan and Goal cannot be active at the same time."
        );
        s.mode = next.mode;
        s.plan_enabled = next.plan_enabled;
        if next.plan_enabled != previous.plan_enabled {
            s.needs_plan_exit_reminder = !next.plan_enabled;
        }
        if let Some(call) = tool_call {
            s.plan_transition = Some(json!({"toolCallId": call, "planEnabled": next.plan_enabled}));
        }
        s.revision += 1;
        self.refresh_permissions();
        Ok(true)
    }

    /// Node `app.setMode`: apply, then remember the permission mode for the project.
    pub(super) async fn set_mode(&mut self, id: &str, mode: &str) -> Result<()> {
        self.apply_execution_state(id, Some(mode), None, None)?;
        let applied = self.sessions[id].mode.as_str();
        if self.permissions.project_mode.as_deref() != Some(applied) {
            let value = json!({"mode": applied});
            match self
                .store
                .save_project_setting(&self.workspace, "permission", "mode", &value)
                .await
            {
                Ok(()) => self.permissions.project_mode = Some(applied.into()),
                Err(error) => tracing::warn!(
                    target: "zcode::permission",
                    event = "permission.mode_preference_failed",
                    error = %error,
                    "Project permission mode preference was not saved"
                ),
            }
        }
        Ok(())
    }

    /// Node `SubagentInteractionBroker`: a child's prompt is shown on the root
    /// session with an origin naming the child; the child keeps ownership.
    fn prompt_host(&self, id: &str) -> (String, Option<Value>, Option<Value>) {
        let Some(parent) = self.sessions.get(id).and_then(|s| s.parent_id.clone()) else {
            return (id.into(), None, None);
        };
        let task = self.sessions[&parent]
            .children
            .values()
            .find(|t| t.child_id == id);
        let origin = task.map(|task| {
            json!({"kind":"subagent","agentId":task.id,"agentType":task.agent_type,"childSessionId":id,
                "description":task.description,"parentSessionId":parent,"parentToolCallId":task.call_id})
        });
        let root = self.root_session(id);
        let mut top = id.to_owned();
        while let Some(p) = self.sessions.get(&top).and_then(|s| s.parent_id.clone()) {
            if p == root {
                break;
            }
            top = p;
        }
        let anchor = self.sessions[&root]
            .children
            .values()
            .find(|t| t.child_id == top)
            .and_then(|t| {
                self.sessions[&root]
                    .rows
                    .iter()
                    .find(|r| r["toolCallId"] == t.call_id.as_str())
            })
            .map(|r| r["rowId"].clone());
        (root, origin, anchor)
    }

    /// Projects a permission prompt (Node `permission_requested`) and returns the
    /// session that shows it.
    pub(super) fn register_permission(
        &mut self,
        id: &str,
        call: &Value,
        request: PermissionRequest,
        reply: tokio::sync::oneshot::Sender<PermissionAnswer>,
    ) -> Result<String> {
        let interaction = format!("perm_{}", self.clock.id());
        let now = self.clock.now();
        let tool = call["function"]["name"].as_str().unwrap_or("");
        let (host, origin, anchor) = self.prompt_host(id);
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        let row = s
            .rows
            .iter_mut()
            .find(|r| r["toolCallId"] == call["id"])
            .context("Tool row missing")?;
        row["status"] = "pendingApproval".into();
        row["approvalInteractionId"] = interaction.clone().into();
        let anchor = anchor.unwrap_or_else(|| row["rowId"].clone());
        let row = row.clone();
        s.revision += 1;
        let policy = request.options_policy.as_deref();
        let mut payload = json!({"kind":"permission","toolCallId":call["id"],"toolName":tool,
            "summary":request.reason,"detail":request.input,"freeText":true,
            "options":policy::v4_options(tool, &request.input, &request.suggestions, policy)});
        if origin.is_none() && policy.is_none() {
            payload["fullAccessOption"] = json!({"optionId":"fullAccess","label":"Full access","kind":"custom",
                "response":{"decision":"deny","reason":"Full access requires V4 approval"}});
        }
        if let Some(origin) = origin {
            payload["origin"] = origin;
        }
        let entry = json!({"interactionId":interaction,"kind":"permission","anchorRowId":anchor,"createdAt":now,"payload":payload});
        let host_session = self
            .sessions
            .get_mut(&host)
            .context("Session unavailable")?;
        host_session.pending.push(entry);
        // 修复：待决交互必须先入列再发布，否则同帧的 state.updated 带出空的 pendingInteractions，
        // 客户端要等下一次无关变更才看到权限请求。
        if host != id {
            host_session.revision += 1;
            self.publish(&host, vec![])?;
        }
        self.publish(id, vec![json!({"op":"row.upserted","row":row})])?;
        self.waiters.add_permission(
            interaction,
            super::waiters::PermissionWait {
                owner: id.into(),
                host: host.clone(),
                tool: tool.into(),
                call: call["id"].as_str().unwrap_or("").into(),
                suggestions: request.suggestions,
                reply,
            },
        );
        Ok(host)
    }
}
