//! A cold-loaded Node session as the Rust runtime's session (spec
//! rust-m11-node-storage §6.1): the persisted facts of `resume`, the model
//! context and the replayed conversation.
use super::resume::Resume;
use crate::domain::execution::Mode;
use crate::domain::goal::Goal;
use crate::domain::history_cold::ColdAnchors;
use crate::domain::session::Session;
use serde_json::{Value, json};

const LISTED_TASK_TYPES: [&str; 3] = ["interactive", "fork", "workflow_parent"];

/// Node `mapGoalStatus` for a goal without a replayed V4 state.
fn goal_status(status: &str) -> &'static str {
    match status {
        "active" => "active",
        "complete" => "verified",
        _ => "paused",
    }
}

fn goal(r: &Resume) -> Option<Goal> {
    let target = r.target.as_ref()?;
    let replayed = &r.conversation.state["goal"];
    let array = |key: &str| replayed[key].as_array().cloned().unwrap_or_default();
    Some(Goal {
        target_id: target.target_id.clone(),
        objective: target.objective.clone(),
        status: replayed["status"]
            .as_str()
            .unwrap_or_else(|| goal_status(&target.status))
            .into(),
        iteration: replayed["iteration"].as_u64().unwrap_or(0),
        verifications: array("verifications"),
        iterations: array("iterations"),
        tokens_used: target.tokens_used.max(0) as u64,
        token_budget: target.token_budget.map(|b| b.max(0) as u64),
        time_used_ms: target.time_used_seconds.max(0) as u64 * 1000,
        active_run_started_at_ms: target.active_run_started_at.map(|t| t as u64),
        last_seen: target.active_run_last_seen_at.map(|t| t as u64),
        created_at: target.time_created as u64,
        updated_at: target.time_updated as u64,
    })
}

/// The runtime session of `r` in `workspace` (the identity the engine owns).
pub fn session(workspace: &str, r: Resume, epoch: String) -> Session {
    let row = &r.session;
    let selection = r.model_selection.clone().unwrap_or(Value::Null);
    let text = |v: &Value| v.as_str().unwrap_or("").to_owned();
    let mut s = Session::new(
        row.id.clone(),
        workspace.into(),
        text(&selection["providerId"]),
        text(&selection["modelId"]),
        text(&selection["options"]["reasoningLevel"]),
        epoch,
        row.time_created as u64,
    );
    s.node.created = true;
    let directory = row.directory.clone();
    s.workspace_path = Some(
        row.path
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or(directory.clone()),
    );
    s.workspace_directory = Some(directory);
    s.trace_id = row.trace_id.clone();
    s.parent_id = row.parent_id.clone();
    s.task_type = row.task_type.clone();
    s.listed = LISTED_TASK_TYPES.contains(&row.task_type.as_str());
    s.title = row.title.clone();
    s.title_source = row.title_source.clone();
    s.updated_at = row.time_updated as u64;
    if let Some(execution) = &r.execution {
        s.mode = execution["mode"]
            .as_str()
            .and_then(Mode::parse)
            .unwrap_or(Mode::Build);
        s.plan_enabled = execution["planEnabled"] == true;
    }
    s.permission_grant = r.permission_grant.clone();
    s.todos = r
        .todos
        .iter()
        .filter_map(|t| {
            serde_json::from_value(
                json!({"content": t.content, "status": t.status, "priority": t.priority}),
            )
            .ok()
        })
        .collect();
    s.goal = goal(&r);
    let state = &r.conversation.state;
    s.phase = serde_json::from_value(state["control"]["phase"].clone())
        .unwrap_or(crate::domain::execution::Phase::CompletedSuccess);
    let last_error = &state["control"]["lastError"];
    s.last_error = last_error.is_object().then(|| last_error.clone());
    s.usage = state["usage"].clone();
    s.row_highwater = r
        .conversation
        .rows
        .iter()
        .filter_map(|row| row["rowId"].as_u64())
        .max()
        .unwrap_or(0);
    s.rows = r.conversation.rows;
    s.context.summary = r.history.summary;
    s.messages = r.history.messages;
    s.restore_boundaries(ColdAnchors {
        row_messages: &r.conversation.messages,
        edit_targets: &r.conversation.edit_targets,
        sources: &r.history.sources,
    });
    s.saved_rows = s.rows.len();
    s.saved_messages = s.messages.len();
    s
}
