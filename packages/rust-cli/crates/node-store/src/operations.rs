//! 操作意图与待回答的交互（docs/specs/rust-cli/node-store.md §4.7，README §6 规则 3、5、9）。
//!
//! - `OperationIntent` → `rust-cli/operation_intent` entry（`{operationId, fingerprint}`，fingerprint 存摘要）。同一个
//!   operationId 再次登记时，fingerprint 相同不变，不同则拒绝。
//! - 工具结果落库时，fingerprint 对得上的最早一个未结意图写一条 `rust-cli/operation_result` entry。
//! - 冷读取：没有结果的意图 → `open_operations`；没有 `interaction_resolved` 的
//!   `pending_interaction`（会话 actor 经 `Fact::Entry` 写的两种 entry）→ `pending_interactions`。
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};
use rust_cli_base::js::json::stringify;
use rust_cli_kernel_api::ids::{InteractionId, OperationId};
use rust_cli_kernel_api::store::{OpenOperation, PendingInteraction, StoreError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::facts::{Commit, save_entry};
use crate::repo::entries;
use crate::{OrStorage, records, storage};

const INTENT: &str = "rust-cli/operation_intent";
const RESULT: &str = "rust-cli/operation_result";
/// 会话 actor 的交互 entry（session `turn.rs` 的 `PENDING_INTERACTION` / `INTERACTION_RESOLVED`，
/// 经 `Fact::Entry` 加上 `rust-cli/` 前缀）。
const PENDING_INTERACTION: &str = "rust-cli/pending_interaction";
const INTERACTION_RESOLVED: &str = "rust-cli/interaction_resolved";

/// `Fact::Entry` 的 entry id：`<会话>:<type>:<id>`。
fn entry_id(session: &str, kind: &str, id: &str) -> String {
    format!("{session}:{kind}:{id}")
}

/// 存下的 fingerprint 是摘要（修复线 F6，审查 R5-12）：以前存执行器给的整份 `{"tool", "input"}` JSON，Write 的全文在
/// tool part 与意图里各存一份，落库与读回都要多处理一遍。Node 不读这两种 entry（`rust-cli/` 前缀）。
const DIGEST: &str = "sha256:";

/// `sha256:` + `JSON.stringify({tool, input: toRecordInput(input)})` 的摘要。tool part 的 `state.input` 读回后再
/// stringify 得到同一段文字（数字、键序都按 JS 规则），所以落库结果时能算出同一个摘要。
fn digest(tool: &Value, input: &Value) -> String {
    let text = stringify(&json!({"tool": tool, "input": crate::tool_parts::record_input(input)}));
    format!("{DIGEST}{:x}", Sha256::digest(text.as_bytes()))
}

/// 执行器的 fingerprint → 存下的摘要；不是 `{"tool", "input"}` 形状时取原文的摘要（同样对不上任何工具结果）。
fn stored(fingerprint: &str) -> String {
    match serde_json::from_str::<Value>(fingerprint) {
        Ok(parsed @ Value::Object(_)) => digest(&parsed["tool"], &parsed["input"]),
        _ => format!("{DIGEST}{:x}", Sha256::digest(fingerprint.as_bytes())),
    }
}

pub fn record_intent(
    ctx: &Commit<'_>,
    operation: &OperationId,
    fingerprint: &str,
) -> Result<(), StoreError> {
    let id = entry_id(ctx.session, INTENT, operation.as_str());
    let existing: Option<String> = ctx
        .conn
        .prepare_cached("select data from session_entry where id = ? and session_id = ?")
        .storage()?
        .query_row([id.as_str(), ctx.session], |r| r.get(0))
        .optional()
        .storage()?;
    let digest = stored(fingerprint);
    if let Some(existing) = existing {
        let existing: Value = serde_json::from_str(&existing).storage()?;
        // 旧记录存的是整份 JSON：照旧与新到的原文比。
        let same = match existing["fingerprint"].as_str() {
            Some(old) if old.starts_with(DIGEST) => old == digest,
            _ => existing["fingerprint"] == fingerprint,
        };
        if same {
            return Ok(());
        }
        return Err(storage(format!(
            "operation {operation} was already recorded with another fingerprint"
        )));
    }
    let data = json!({"operationId": operation.as_str(), "fingerprint": digest});
    save_entry(
        ctx,
        &records::entry(id, ctx.session, INTENT, ctx.now, data, false),
    )
}

/// 还没有结果的意图，按登记顺序。
///
/// 性能修复（修复线 F6，审查 R5-12）：以前 `not exists` 对本会话的每个意图都 `json_extract` 出 operationId 再拼结果
/// 的 id，每个工具结果落库都要把全部意图的 JSON 解析一遍。意图与结果的 entry id 只差中间的类型名（`entry_id`），
/// 现在直接按 id 配对，只解析未结意图的 data。
fn open_intents(conn: &Connection, session: &str) -> anyhow::Result<Vec<OpenOperation>> {
    let mut query = conn.prepare_cached(
        "select i.data from session_entry i
      where i.session_id = ?1 and i.type = 'rust-cli/operation_intent'
        and not exists (select 1 from session_entry r
          where r.id = ?1 || ':rust-cli/operation_result:'
            || substr(i.id, length(?1 || ':rust-cli/operation_intent:') + 1))
      order by i.time_created, i.rowid",
    )?;
    let rows = query
        .query_map([session], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .iter()
        .filter_map(|data| {
            let data: Value = serde_json::from_str(data).ok()?;
            Some(OpenOperation {
                operation: OperationId::new(data["operationId"].as_str()?),
                fingerprint: data["fingerprint"].as_str()?.to_owned(),
            })
        })
        .collect())
}

pub fn open_operations(conn: &Connection, session: &str) -> Result<Vec<OpenOperation>, StoreError> {
    open_intents(conn, session).storage()
}

/// 工具结果落库：fingerprint 对得上的最早一个未结意图记为已结束。存的是 `{tool, input}` 的摘要（[`digest`]），
/// 旧记录是 query 执行器写的整份 JSON（解析后比 `tool` 与归一后的 `input`）；对不上（格式变了、不是工具的操作）时
/// 意图保持未结，恢复逻辑按 Unknown 处理，不会重跑。
pub fn settle(
    ctx: &Commit<'_>,
    (tool, input): (&str, &Value),
    call: &str,
    status: &str,
) -> Result<(), StoreError> {
    let open = open_intents(ctx.conn, ctx.session).storage()?;
    let wanted = digest(&Value::from(tool), input);
    let matched = open.into_iter().find(|intent| {
        if intent.fingerprint.starts_with(DIGEST) {
            return intent.fingerprint == wanted;
        }
        serde_json::from_str::<Value>(&intent.fingerprint).is_ok_and(|fingerprint| {
            fingerprint["tool"] == tool
                && crate::tool_parts::record_input(&fingerprint["input"]) == *input
        })
    });
    let Some(intent) = matched else {
        return Ok(());
    };
    let operation = intent.operation.as_str();
    let data = json!({"operationId": operation, "toolUseId": call, "status": status});
    let id = entry_id(ctx.session, RESULT, operation);
    save_entry(
        ctx,
        &records::entry(id, ctx.session, RESULT, ctx.now, data, false),
    )
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
#[path = "operations_tests.rs"]
mod tests;
