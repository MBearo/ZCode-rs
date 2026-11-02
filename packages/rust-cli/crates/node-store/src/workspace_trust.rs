//! 工作区信任的记录（docs/specs/rust-cli/workspace-trust.md §2.3、node-store.md §4.9）。
//!
//! 一行 `local_setting(scope='directory', scope_id=<规范化后的路径>, namespace='rust-cli/workspace-trust',
//! key='trust_dialog')`，值 `{"hasTrustDialogAccepted":true}`。Node 只按 `(scope, scope_id, namespace, key)`
//! 精确读 `permission` 命名空间，读不到这些行；表结构不变。

use rusqlite::Connection;
use rust_cli_kernel_api::store::StoreError;
use rust_cli_kernel_api::workspace_trust::TrustKey;
use serde_json::Value;

use crate::OrStorage;
use crate::repo::settings::{self, SettingKey};

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
    let ids: Vec<&str> = candidates.iter().map(TrustKey::as_str).collect();
    let rows = settings::get_many(conn, (SCOPE, NAMESPACE, KEY), &ids).storage()?;
    let mut trusted = Vec::new();
    for (scope_id, value) in rows {
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
    let at = SettingKey {
        scope: SCOPE,
        scope_id: key.as_str(),
        namespace: NAMESPACE,
        key: KEY,
    };
    settings::put(conn, at, VALUE, now).storage()
}

#[cfg(test)]
#[path = "workspace_trust_tests.rs"]
mod tests;
