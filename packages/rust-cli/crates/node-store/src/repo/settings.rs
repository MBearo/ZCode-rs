//! `local_setting` 表（Node `writeLocalSetting` / `readLocalSetting`）：按（scope, scope_id, namespace, key）存一个 JSON 文本值。
//! 以前同一条 upsert 写在 project_rules 与 workspace_trust 两处、同一条读写在 project_rules 与 project_mode 两处（F128，审查 X5）。

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

/// 一个设置的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingKey<'a> {
    pub scope: &'a str,
    pub scope_id: &'a str,
    pub namespace: &'a str,
    pub key: &'a str,
}

/// 这个设置的值（JSON 文本）；没有这一行时 None。
pub fn get(conn: &Connection, at: SettingKey<'_>) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "select value from local_setting where scope = ? and scope_id = ? and namespace = ? and key = ?",
    )?
    .query_row(params![at.scope, at.scope_id, at.namespace, at.key], |row| {
        row.get(0)
    })
    .optional()
}

/// Node `writeLocalSetting` 的 upsert：`schema_version` 1，已有行只改 value、schema_version 与 time_updated。
pub fn put(conn: &Connection, at: SettingKey<'_>, value: &str, now: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "insert into local_setting (scope, scope_id, namespace, key, value, schema_version, time_created, time_updated)
         values (?, ?, ?, ?, ?, ?, ?, ?)
         on conflict(scope, scope_id, namespace, key) do update set
           value = excluded.value,
           schema_version = excluded.schema_version,
           time_updated = excluded.time_updated",
    )?
    .execute(params![
        at.scope,
        at.scope_id,
        at.namespace,
        at.key,
        value,
        1,
        now,
        now
    ])?;
    Ok(())
}

/// 同一个（scope, namespace, key）下这些 scope_id 里有值的：（scope_id, 值），一条查询。
pub fn get_many(
    conn: &Connection,
    (scope, namespace, key): (&str, &str, &str),
    scope_ids: &[&str],
) -> rusqlite::Result<Vec<(String, String)>> {
    if scope_ids.is_empty() {
        return Ok(Vec::new());
    }
    let holes = vec!["?"; scope_ids.len()].join(", ");
    let sql = format!(
        "select scope_id, value from local_setting
         where scope = ? and namespace = ? and key = ? and scope_id in ({holes})"
    );
    let bound = [scope, namespace, key]
        .into_iter()
        .chain(scope_ids.iter().copied());
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(bound), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect()
}

/// 旧版 Node 的项目权限规则（`permission` 表，`local_setting` 没有 `ruleset` 时读它）。
pub fn legacy_permission(conn: &Connection, project: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached("select data from permission where project_id = ?")?
        .query_row(params![project], |row| row.get(0))
        .optional()
}
