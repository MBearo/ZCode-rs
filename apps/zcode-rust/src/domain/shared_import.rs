use super::{
    session_listing::{ListParams, WorkspaceRef},
    shared_context::{Provenance, nonempty},
};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct History {
    pub source: String,
    pub title: String,
    pub created_at: Option<u64>,
    pub markdown: String,
    pub provenance: Provenance,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Import {
    pub session_id: Option<String>,
    pub workspace: WorkspaceRef,
    pub parent_session_id: Option<String>,
    pub mode: Option<String>,
    pub model: Option<Value>,
    pub persistence: Option<String>,
    pub thought_level: Option<String>,
    pub title_generation_enabled: Option<bool>,
    pub mcp_servers: Option<Vec<Value>>,
    pub tool_allowlist: Option<Vec<String>>,
    pub tool_denylist: Option<Vec<String>>,
    pub imported_history: History,
    pub off_peak_tool_enabled: Option<bool>,
    pub dynamic_workflow_enabled: Option<bool>,
}
impl Import {
    pub fn parse(value: &Value) -> Result<Self> {
        for object in [
            value,
            &value["importedHistory"],
            &value["importedHistory"]["provenance"],
        ] {
            ensure!(
                object
                    .as_object()
                    .is_some_and(|m| m.values().all(|v| !v.is_null())),
                "Null shared import field"
            );
        }
        let mut input: Self = serde_json::from_value(value.clone())?;
        input.workspace = ListParams::parse(&json!({"workspace":value["workspace"]}))?
            .workspace
            .unwrap();
        for s in input
            .session_id
            .iter_mut()
            .chain(input.parent_session_id.iter_mut())
            .chain(input.thought_level.iter_mut())
        {
            nonempty(s)?;
        }
        ensure!(
            input
                .mode
                .as_deref()
                .is_none_or(|m| matches!(m, "build" | "yolo" | "plan" | "auto")),
            "Invalid imported mode"
        );
        ensure!(
            input
                .persistence
                .as_deref()
                .is_none_or(|p| matches!(p, "immediate" | "deferred")),
            "Invalid persistence"
        );
        ensure!(
            input.tool_allowlist.as_ref().is_none_or(Vec::is_empty)
                && input.tool_denylist.as_ref().is_none_or(Vec::is_empty)
                && input.off_peak_tool_enabled != Some(true)
                && input.dynamic_workflow_enabled != Some(true),
            "Unsupported imported tool profile"
        );
        // 标题来源由 importedHistory 拥有，不启动异步标题生成。
        let _ = input.title_generation_enabled;
        ensure!(
            input.imported_history.source == "sharedContext",
            "Unsupported imported history source"
        );
        nonempty(&mut input.imported_history.title)?;
        Ok(input)
    }
}
