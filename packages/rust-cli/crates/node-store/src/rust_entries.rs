//! 本工程独有的 entry 的读回（docs/specs/rust-cli/node-store.md §4.7，README §6 规则 9）：按种类的记录。
//!
//! 以前这里还读两种不再写的 entry，都没有读者，库里的旧记录不读、不删：操作意图（`rust-cli/operation_intent` /
//! `operation_result`，工作线 F119 删掉）；待回答的交互（`rust-cli/pending_interaction` / `interaction_resolved` →
//! `LoadedSession.pending_interactions`，工作线 F130 删掉：进程重启后询问已无法回答，续写时未结的调用按中断结算）。
use rusqlite::Connection;
use rust_cli_kernel_api::store::StoreError;
use serde_json::Value;

use crate::OrStorage;
use crate::keys::{self, EntryKey};
use crate::repo::entries;

/// 一种 `rust-cli/<kind>` entry 的全部记录（`SessionStore::entries`，docs/specs/rust-cli/cross-session.md §2.2）：
/// `(id, data)`，id 去掉 `<会话>:rust-cli/<kind>:` 前缀，按写入顺序。
pub fn kind_entries(
    conn: &Connection,
    session: &str,
    kind: &str,
) -> Result<Vec<(String, Value)>, StoreError> {
    Ok(entries::list(conn, session, Some(&keys::named(kind)))
        .storage()?
        .into_iter()
        .filter_map(|entry| {
            let id = EntryKey::parse(&entry.id, session, kind)?.id?.to_owned();
            Some((id, entry.data))
        })
        .collect())
}

#[cfg(test)]
#[path = "rust_entries_tests.rs"]
mod tests;
