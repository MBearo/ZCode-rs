//! Session lists over the Node `session` table (spec rust-m11-node-storage
//! §6.2): the sessions-index seed of sessions not loaded yet (Node
//! `loadStoredSessionSummaries`) and the persisted part of `session/list`
//! (Node `listSessions` with `mapSessionInfo`).
use super::sessions::{self, List, SessionRow};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use zcode_cli_domain::session_listing::ListParams;
use zcode_cli_domain::workspace_identity::parse_remote;

/// Node `TASK_LIST_SESSION_TYPES`: the sessions the task list shows.
const TASK_LIST_SESSION_TYPES: [&str; 3] = ["interactive", "fork", "workflow_parent"];

fn task_list_types() -> Vec<String> {
    TASK_LIST_SESSION_TYPES.map(str::to_owned).to_vec()
}

/// Node `normalizeStoredTitleSource`.
fn summary_title_source(source: &str) -> &'static str {
    match source {
        "custom" => "custom",
        "default" => "default",
        _ => "generated",
    }
}

/// The sessions-index summaries of a workspace's stored sessions. A remote
/// identity matches its own sessions; a local path only sessions without one.
pub fn stored_summaries(conn: &Connection, workspace_id: &str) -> Result<Vec<Value>> {
    let remote = parse_remote(workspace_id);
    let rows = sessions::list(
        conn,
        &List {
            directory: Some(remote.map_or(workspace_id, |r| r.path).to_owned()),
            workspace_id: Some(remote.map(|_| workspace_id.to_owned())),
            task_types: task_list_types(),
            include_archived: false,
            limit: Some(200),
            ..List::default()
        },
    )?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut summary = json!({"sessionId": row.id, "workspaceId": workspace_id});
            if let Some(parent) = &row.parent_id {
                summary["parentSessionId"] = parent.clone().into();
            }
            summary["title"] = row.title.clone().into();
            summary["titleSource"] = summary_title_source(&row.title_source).into();
            summary["phase"] = "completedSuccess".into();
            summary["sessionEnded"] = true.into();
            summary["hasBackgroundWork"] = false.into();
            summary["lastActivityAt"] = row.time_updated.into();
            summary["createdAt"] = row.time_created.into();
            summary
        })
        .collect())
}

/// Node `mapSessionInfo` for a stored session without a live runtime.
fn session_info(row: &SessionRow, workspace: Value) -> Value {
    let mut info = json!({});
    if let Some(archived) = row.time_archived {
        info["archivedAt"] = archived.into();
    }
    info["createdAt"] = row.time_created.into();
    info["mode"] = "build".into();
    if let Some(parent) = &row.parent_id {
        info["parentSessionId"] = parent.clone().into();
    }
    if let Some(trace) = &row.trace_id {
        info["traceId"] = trace.clone().into();
    }
    info["sessionId"] = row.id.clone().into();
    info["sessionKind"] = row.task_type.clone().into();
    info["status"] = "idle".into();
    info["title"] = row.title.clone().into();
    info["titleSource"] = row.title_source.clone().into();
    info["updatedAt"] = row.time_updated.into();
    info["workspace"] = workspace;
    info
}

/// The workspace key a stored session belongs to: its identity, else its path
/// (a non-empty one), else its directory.
fn stored_workspace_key(row: &SessionRow) -> &str {
    let identity = row.workspace_id.as_deref().map(str::trim).unwrap_or("");
    if !identity.is_empty() {
        return identity;
    }
    match row.path.as_deref() {
        Some(path) if !path.is_empty() => path,
        _ => &row.directory,
    }
}

/// The stored sessions `session/list` returns, in Node's order.
pub fn list(conn: &Connection, params: &ListParams) -> Result<Vec<Value>> {
    let stored = match &params.session_ids {
        Some(ids) => ids
            .iter()
            .map(|id| sessions::get(conn, id))
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect(),
        None => sessions::list(
            conn,
            &List {
                directory: params.workspace.as_ref().map(|w| w.workspace_path.clone()),
                include_archived: params.include_archived,
                limit: Some(params.limit.unwrap_or(50) as i64),
                task_types: task_list_types(),
                ..List::default()
            },
        )?,
    };
    Ok(stored
        .iter()
        .filter(|row| params.include_archived || row.time_archived.is_none())
        .filter(|row| {
            params
                .workspace
                .as_ref()
                .is_none_or(|w| stored_workspace_key(row) == w.identity())
        })
        .map(|row| {
            let workspace = match &params.workspace {
                Some(workspace) => json!(workspace),
                None => {
                    // Node `buildWorkspaceRef({ workspacePath: session.path ?? session.directory })`.
                    let path = row.path.clone().unwrap_or_else(|| row.directory.clone());
                    json!({"workspaceKey": path, "workspacePath": path})
                }
            };
            session_info(row, workspace)
        })
        .collect())
}
