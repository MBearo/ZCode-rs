//! 本工程独有的 entry 的读回（docs/specs/rust-cli/node-store.md §4.7，README §6 规则 9）：待回答的交互、按种类的记录。
//!
//! 冷读取：没有 `interaction_resolved` 的 `pending_interaction`（会话 actor 经 `Fact::Entry` 写的两种 entry）→
//! `pending_interactions`。以前这里还记操作意图（`rust-cli/operation_intent` / `operation_result`），没有读者，工作线 F119
//! 删掉；库里的旧记录不读、不删。
use std::collections::HashSet;

use rusqlite::Connection;
use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::store::{PendingInteraction, StoreError};
use serde_json::Value;

use crate::OrStorage;
use crate::repo::entries;

/// 会话 actor 的交互 entry（session `turn.rs` 的 `PENDING_INTERACTION` / `INTERACTION_RESOLVED`，
/// 经 `Fact::Entry` 加上 `rust-cli/` 前缀）。
const PENDING_INTERACTION: &str = "rust-cli/pending_interaction";
const INTERACTION_RESOLVED: &str = "rust-cli/interaction_resolved";

/// `Fact::Entry` 的 entry id：`<会话>:<type>:<id>`。
fn entry_id(session: &str, kind: &str, id: &str) -> String {
    format!("{session}:{kind}:{id}")
}

/// 仍待回答的交互，按提问顺序。请求读不出时报错（是本工程自己写的记录）。
pub fn pending_interactions(
    conn: &Connection,
    session: &str,
) -> Result<Vec<PendingInteraction>, StoreError> {
    let resolved: HashSet<String> = entries::list(conn, session, Some(INTERACTION_RESOLVED))
        .storage()?
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let asked = format!("{session}:{PENDING_INTERACTION}:");
    let mut out = Vec::new();
    for entry in entries::list(conn, session, Some(PENDING_INTERACTION)).storage()? {
        let Some(interaction) = entry.id.strip_prefix(&asked) else {
            continue;
        };
        if resolved.contains(&entry_id(session, INTERACTION_RESOLVED, interaction)) {
            continue;
        }
        out.push(PendingInteraction {
            interaction: InteractionId::new(interaction),
            request: serde_json::from_value(entry.data).storage()?,
        });
    }
    Ok(out)
}

/// 一种 `rust-cli/<kind>` entry 的全部记录（`SessionStore::entries`，docs/specs/rust-cli/cross-session.md §2.2）：
/// `(id, data)`，id 去掉 `<会话>:rust-cli/<kind>:` 前缀，按写入顺序。
pub fn kind_entries(
    conn: &Connection,
    session: &str,
    kind: &str,
) -> Result<Vec<(String, Value)>, StoreError> {
    let kind = format!("rust-cli/{kind}");
    let prefix = format!("{session}:{kind}:");
    Ok(entries::list(conn, session, Some(&kind))
        .storage()?
        .into_iter()
        .filter_map(|entry| {
            let id = entry.id.strip_prefix(&prefix)?.to_owned();
            Some((id, entry.data))
        })
        .collect())
}

#[cfg(test)]
#[path = "rust_entries_tests.rs"]
mod tests;
