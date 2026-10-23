//! 工作区信任的记录（docs/specs/rust-cli/workspace-trust.md §2.3、node-store.md §4.9）。
//!
//! 一行 `local_setting(scope='directory', scope_id=<规范化后的路径>, namespace='rust-cli/workspace-trust',
//! key='trust_dialog')`，值 `{"hasTrustDialogAccepted":true}`。Node 只按 `(scope, scope_id, namespace, key)`
//! 精确读 `permission` 命名空间，读不到这些行；表结构不变。

use rusqlite::{Connection, params, params_from_iter};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_kernel_api::workspace_trust::TrustKey;
use serde_json::Value;

use crate::OrStorage;

const SCOPE: &str = "directory";
const NAMESPACE: &str = "rust-cli/workspace-trust";
const KEY: &str = "trust_dialog";
const VALUE: &str = r#"{"hasTrustDialogAccepted":true}"#;

/// 值里的 `hasTrustDialogAccepted` 是 `true`（只认 true）；读不出的值当作没有。
fn accepted(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .is_some_and(|value| value["hasTrustDialogAccepted"] == true)
}

/// 一条查询取全部候选，再按候选顺序返回第一个有记录的键。
pub(crate) fn find(
    conn: &Connection,
    candidates: &[TrustKey],
) -> Result<Option<TrustKey>, StoreError> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let holes = vec!["?"; candidates.len()].join(", ");
    let sql = format!(
        "select scope_id, value from local_setting
         where scope = ? and namespace = ? and key = ? and scope_id in ({holes})"
    );
    let bound = [SCOPE, NAMESPACE, KEY]
        .into_iter()
        .chain(candidates.iter().map(TrustKey::as_str));
    let mut statement = conn.prepare(&sql).storage()?;
    let rows = statement
        .query_map(params_from_iter(bound), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .storage()?;
    let mut trusted = Vec::new();
    for row in rows {
        let (scope_id, value) = row.storage()?;
        if accepted(&value) {
            trusted.push(scope_id);
        }
    }
    Ok(candidates
        .iter()
        .find(|key| trusted.iter().any(|found| found == key.as_str()))
        .cloned())
}

/// Node `writeLocalSetting` 的 upsert：已有行只改 value、schema_version 与 time_updated。
pub(crate) fn record(conn: &Connection, key: &TrustKey, now_ms: u64) -> Result<(), StoreError> {
    let now = i64::try_from(now_ms).unwrap_or(i64::MAX);
    conn.prepare_cached(
        "insert into local_setting (scope, scope_id, namespace, key, value, schema_version, time_created, time_updated)
         values (?, ?, ?, ?, ?, ?, ?, ?)
         on conflict(scope, scope_id, namespace, key) do update set
           value = excluded.value,
           schema_version = excluded.schema_version,
           time_updated = excluded.time_updated",
    )
    .storage()?
    .execute(params![SCOPE, key.as_str(), NAMESPACE, KEY, VALUE, 1, now, now])
    .storage()?;
    Ok(())
}

#[cfg(test)]
#[path = "workspace_trust_tests.rs"]
mod tests;
