//! Session-level journal hooks: the session row and its runtime entries
//! (Node `ensureSessionPersisted`, `persistRuntimeModelSelection`,
//! `persistExecutionState`, `updateTodos`, `updateSession`).
use super::Op;
use super::records::{execution_state_entry, model_selection_entry, title_from_input};
use crate::session::Session;
use serde_json::{Value, json};

impl Session {
    /// Node `getSessionModelSelection`: `None` until a model is chosen.
    pub fn node_selection(&self) -> Option<Value> {
        if self.provider.is_empty() || self.model.is_empty() {
            return None;
        }
        let mut selection = json!({"providerId": self.provider, "modelId": self.model});
        if !self.reasoning_level.is_empty() {
            selection["options"] = json!({"reasoningLevel": self.reasoning_level});
        }
        Some(selection)
    }

    /// Node `config.agentName`: `zcode-<type>` for a subagent child.
    pub fn node_agent(&self) -> String {
        match &self.agent_profile {
            Some(profile) if self.task_type == "subagent_child" => {
                format!("zcode-{}", profile.name)
            }
            _ => super::records::AGENT.into(),
        }
    }

    /// The directory Node stores (the protocol workspace path).
    pub fn node_directory(&self) -> &str {
        self.workspace_path
            .as_deref()
            .or(self.workspace_directory.as_deref())
            .unwrap_or(&self.workspace)
    }

    /// Node `ensureSessionPersisted`: the session row, its model selection and
    /// execution state, once, before the first input is recorded.
    pub fn node_ensure_created(&mut self, now: u64, first_input: &str, version: &str) {
        if self.node.created {
            return;
        }
        let directory = self.node_directory().to_owned();
        let mut create = json!({
            "id": self.id,
            "projectID": crate::node_ids::project_id(&directory),
        });
        if self.workspace != directory {
            create["workspaceID"] = self.workspace.clone().into();
        }
        if let Some(parent) = &self.parent_id {
            create["parentID"] = parent.clone().into();
        }
        if let Some(trace) = &self.trace_id {
            create["traceID"] = trace.clone().into();
        }
        if self.task_type != "interactive" {
            create["taskType"] = self.task_type.clone().into();
        }
        create["slug"] = crate::node_ids::slugify(&self.id).into();
        create["directory"] = directory.clone().into();
        create["path"] = directory.into();
        create["title"] = title_from_input(first_input).into();
        create["titleSource"] = "first_input".into();
        create["version"] = version.into();
        create["permission"] = json!({"mode": self.mode.as_str()});
        self.node.push(now, Op::CreateSession(create));
        self.node.created = true;
        self.node_model_selection(now);
        let shell = super::records::shell_selection_entry(&self.id, now);
        self.node.push(now, Op::Entry(shell));
        self.node_execution_state(now);
    }

    /// Node `persistRuntimeModelSelection` (only once the session is stored).
    pub fn node_model_selection(&mut self, now: u64) {
        if !self.node.created {
            return;
        }
        if let Some(selection) = self.node_selection() {
            let entry = model_selection_entry(&self.id, now, selection);
            self.node.push(now, Op::Entry(entry));
        }
    }

    /// Node `persistExecutionState`.
    pub fn node_execution_state(&mut self, now: u64) {
        if !self.node.created {
            return;
        }
        let entry = execution_state_entry(&self.id, now, self.mode.as_str(), self.plan_enabled);
        self.node.push(now, Op::Entry(entry));
    }

    /// Node `persistGeneratedSessionTitle`: `updateSession` guarded by the
    /// title sources a generated title may replace.
    pub fn node_generated_title(&mut self, now: u64, message: Option<&str>) {
        if !self.node.created {
            return;
        }
        let mut update = json!({"expectedTitleSources": ["default", "first_input", "generated"],
            "id": self.id, "title": self.title});
        if let Some(message) = message {
            update["titleMessageID"] = message.into();
        }
        update["titleSource"] = "generated".into();
        self.node.push(now, Op::UpdateSession(update));
    }

    /// Node `updateSession({title, titleSource})`.
    pub fn node_title(&mut self, now: u64) {
        if !self.node.created {
            return;
        }
        let update = json!({"id": self.id, "title": self.title, "titleSource": self.title_source});
        self.node.push(now, Op::UpdateSession(update));
    }

    /// Node `grantPermissionFullAccess` after the session switched to yolo:
    /// `previous` is the mode before, `ids` the receipt event's id and trace.
    pub fn node_full_access(
        &mut self,
        now: u64,
        (interaction, queue): (&str, Vec<String>),
        previous: &str,
        (event, trace): (String, String),
    ) {
        if !self.node.created {
            return;
        }
        let payload = json!({"mode": "yolo", "planEnabled": self.plan_enabled,
            "previousMode": previous, "previousPlanEnabled": self.plan_enabled, "source": "command",
            "permissionGrant": {"interactionId": interaction, "queueItemIds": queue}});
        let event = json!({"id": event, "sessionId": self.id, "type": "session_mode_changed",
            "timestamp": crate::hooks::input::iso_timestamp(now), "traceId": trace,
            "sequenceNumber": 0, "payload": payload});
        let receipt = json!({"id": format!("{}:permission-full-access:{interaction}", self.id),
            "sessionID": self.id, "type": "runtime/permission_full_access", "touchSession": false,
            "time": {"created": now, "updated": now},
            "data": {"interactionId": interaction, "event": event}});
        let execution = execution_state_entry(&self.id, now, "yolo", self.plan_enabled);
        self.node.push(
            now,
            Op::FullAccess {
                queue,
                execution,
                receipt,
            },
        );
    }

    /// Node `persistDurableSessionEvent` of `UserInputAutoResolutionUpdated`:
    /// the pending question's phase, one entry per interaction overwritten in
    /// place. `ids` are the event id and trace.
    pub fn node_auto_resolution(
        &mut self,
        now: u64,
        pending: &Value,
        (event, trace): (String, String),
    ) {
        if !self.node.created {
            return;
        }
        let interaction = pending["interactionId"].as_str().unwrap_or("");
        let phase = &pending["autoResolution"];
        let mut data = json!({"interactionId": interaction,
            "toolCallId": pending["payload"]["toolCallId"], "autoResolution": phase,
            "eventId": event, "sequenceNumber": self.revision, "traceId": trace});
        if let Some(turn) = &self.node.turn {
            data["turnId"] = turn.runtime.clone().into();
        }
        let entry = json!({"id": format!("user-input-auto-resolution:{interaction}"),
            "sessionID": self.id, "type": "runtime/user_input_auto_resolution",
            "time": {"created": phase["startedAt"], "updated": now}, "data": data});
        self.node.push(now, Op::Entry(entry));
    }

    /// Node `updateTodos` (a replace-all list).
    pub fn node_todos(&mut self, now: u64) {
        if !self.node.created {
            return;
        }
        let todos = self
            .todos
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
            .collect();
        self.node.push(now, Op::Todos(todos));
    }
}
