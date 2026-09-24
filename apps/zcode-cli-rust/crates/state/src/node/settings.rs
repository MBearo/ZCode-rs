//! `local_setting` and the legacy `permission` table (Node
//! `repositories/local-settings.ts`). Spec rust-m11-node-storage §4.
use super::json::stringify;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const PROJECT: &str = "project";
const PERMISSION: &str = "permission";

/// One `local_setting` value.
pub fn read(
    conn: &Connection,
    scope: &str,
    scope_id: &str,
    namespace: &str,
    key: &str,
) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "select value from local_setting where scope = ? and scope_id = ? and namespace = ? and key = ?",
        )?
        .query_row(params![scope, scope_id, namespace, key], |r| r.get(0))
        .optional()?)
}

/// Node `writeLocalSetting`: an upsert that keeps `time_created`.
pub fn write(
    conn: &Connection,
    scope: &str,
    scope_id: &str,
    namespace: &str,
    key: &str,
    value: &str,
    now: i64,
) -> Result<()> {
    conn.prepare_cached(
        "insert into local_setting (
        scope, scope_id, namespace, key, value, schema_version, time_created, time_updated
      ) values (?, ?, ?, ?, ?, ?, ?, ?)
      on conflict(scope, scope_id, namespace, key) do update set
        value = excluded.value,
        schema_version = excluded.schema_version,
        time_updated = excluded.time_updated",
    )?
    .execute(params![scope, scope_id, namespace, key, value, 1, now, now])?;
    Ok(())
}

/// Node `getProjectPermission`: `local_setting`, else the legacy table.
pub fn project_permission(conn: &Connection, project: &str) -> Result<Option<Value>> {
    if let Some(value) = read(conn, PROJECT, project, PERMISSION, "ruleset")? {
        return Ok(Some(serde_json::from_str(&value)?).filter(|v: &Value| !v.is_null()));
    }
    let legacy: Option<String> = conn
        .prepare_cached("select data from permission where project_id = ?")?
        .query_row([project], |r| r.get(0))
        .optional()?;
    Ok(legacy
        .map(|data| serde_json::from_str::<Value>(&data))
        .transpose()?
        .filter(|v| !v.is_null()))
}

/// Node `saveProjectPermission`.
pub fn save_project_permission(
    conn: &Connection,
    project: &str,
    ruleset: &Value,
    now: i64,
) -> Result<()> {
    write(
        conn,
        PROJECT,
        project,
        PERMISSION,
        "ruleset",
        &stringify(ruleset),
        now,
    )
}

/// Node `getProjectPermissionMode`: an invalid value reads as none.
pub fn project_permission_mode(conn: &Connection, project: &str) -> Result<Option<String>> {
    let Some(value) = read(conn, PROJECT, project, PERMISSION, "mode")? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(&value)?;
    Ok(value["mode"]
        .as_str()
        .filter(|m| matches!(*m, "plan" | "build" | "edit" | "yolo" | "auto"))
        .map(str::to_owned))
}

/// Node `saveProjectPermissionMode`.
pub fn save_project_permission_mode(
    conn: &Connection,
    project: &str,
    mode: &str,
    now: i64,
) -> Result<()> {
    write(
        conn,
        PROJECT,
        project,
        PERMISSION,
        "mode",
        &stringify(&json!({"mode": mode})),
        now,
    )
}
