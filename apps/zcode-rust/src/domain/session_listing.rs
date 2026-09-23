use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_LIST_BYTES: usize = 900 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRef {
    pub workspace_path: String,
    pub workspace_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_session_id: Option<String>,
}
impl WorkspaceRef {
    pub fn identity(&self) -> &str {
        self.workspace_identity
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.workspace_path)
    }
}
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListParams {
    pub workspace: Option<WorkspaceRef>,
    pub session_ids: Option<Vec<String>>,
    #[serde(default)]
    pub include_archived: bool,
    pub limit: Option<u64>,
}
impl ListParams {
    pub fn parse(value: &Value) -> Result<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        for key in ["workspace", "sessionIds", "limit"] {
            ensure!(
                value.get(key).is_none_or(|v| !v.is_null()),
                "Invalid session/list optional field: {key}"
            );
        }
        if let Some(workspace) = value.get("workspace") {
            for key in ["workspaceIdentity", "remoteSessionId"] {
                ensure!(
                    workspace
                        .get(key)
                        .is_none_or(|v| v.as_str().is_some_and(|s| !s.is_empty())),
                    "Invalid workspace optional field: {key}"
                );
            }
        }
        let mut normalized = value.clone();
        if let Some(limit) = value.get("limit").and_then(Value::as_f64) {
            ensure!(
                limit > 0.0 && limit <= 9_007_199_254_740_991.0 && limit.fract() == 0.0,
                "Invalid list limit"
            );
            normalized["limit"] = (limit as u64).into();
        }
        let mut params: Self = serde_json::from_value(normalized)?;
        if let Some(workspace) = &mut params.workspace {
            for value in [&mut workspace.workspace_path, &mut workspace.workspace_key]
                .into_iter()
                .chain(workspace.workspace_identity.iter_mut())
                .chain(workspace.remote_session_id.iter_mut())
            {
                *value = value.trim().to_owned();
                ensure!(!value.is_empty(), "Invalid workspace string");
            }
        }
        if let Some(ids) = &mut params.session_ids {
            for id in ids {
                *id = id.trim().to_owned();
            }
        }
        ensure!(
            params
                .limit
                .is_none_or(|n| n > 0 && n <= 9_007_199_254_740_991),
            "Invalid list limit"
        );
        ensure!(
            params.session_ids.as_ref().is_none_or(|ids| !ids.is_empty()
                && ids.len() <= 64
                && ids.iter().all(|id| !id.is_empty())),
            "Invalid session IDs"
        );
        ensure!(
            params
                .workspace
                .as_ref()
                .is_none_or(|w| !w.workspace_path.is_empty() && !w.workspace_key.is_empty()),
            "Invalid workspace"
        );
        Ok(params)
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListing {
    pub id: String,
    pub workspace: String,
    pub workspace_path: Option<String>,
    pub workspace_directory: Option<String>,
    pub prompt_path: Option<String>,
    pub trace_id: Option<String>,
    pub task_type: String,
    pub title: String,
    pub title_source: String,
    pub parent_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub archived_at: Option<u64>,
}
impl SessionListing {
    pub fn projection(&self, requested: Option<&WorkspaceRef>) -> Value {
        let workspace = requested.map(|w| json!(w)).unwrap_or_else(|| {
            let path = self.workspace_path.as_deref().unwrap_or(&self.workspace);
            let mut w = json!({"workspacePath":path,"workspaceKey":self.workspace});
            if self.workspace != path {
                w["workspaceIdentity"] = self.workspace.clone().into();
            }
            w
        });
        // 旧 session/list 返回 stored mapSessionInfo 的身份视图；实时状态由 V4/session/read 提供。
        let mut value = json!({"sessionId":self.id,"workspace":workspace,"sessionKind":self.task_type,"title":self.title,"titleSource":self.title_source,"mode":"build","status":"idle","createdAt":self.created_at,"updatedAt":self.updated_at});
        for (key, field) in [
            ("parentSessionId", &self.parent_id),
            ("traceId", &self.trace_id),
        ] {
            if let Some(s) = field {
                value[key] = s.clone().into();
            }
        }
        if let Some(at) = self.archived_at {
            value["archivedAt"] = at.into();
        }
        value
    }
}
