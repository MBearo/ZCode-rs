//! 会话当前状态的读取：模型选择、执行状态、当前轮（docs/specs/rust-cli/node-store.md §4）。
//!
//! 写入 assistant 记录要用的 provider、模式从库里读，库是唯一事实源：同一事务里先提交的事实（例如同一批的
//! `ModelSelected`）立刻可见，Node 续写过的会话也一样。父消息与 `anchor.turnId` 由批次的回合锚点给出
//! （`facts::Commit::current_user`，修复线 F6）；[`current_user`] 只在没有锚点时与冷读取时用。
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use crate::mode::{self, Execution};
use crate::records::{EXECUTION_STATE_ENTRY, MODEL_SELECTION_ENTRY};
use crate::repo::codecs::{decode_message, parse_model_selection};
use crate::repo::entries;
use crate::repo::sessions::SessionRow;

/// 本工程记录内核模式名的 entry（dontAsk 在 Node 里没有对应值）。
pub const PERMISSION_MODE_ENTRY: &str = "rust-cli/permission_mode";
/// 提交回执的序号 entry。
pub const COMMIT_ENTRY: &str = "rust-cli/commit";

/// 最后一条 entry 的 data。
pub fn last_entry(conn: &Connection, session: &str, kind: &str) -> anyhow::Result<Option<Value>> {
    Ok(entries::list(conn, session, Some(kind))?
        .pop()
        .map(|entry| entry.data))
}

/// 会话的模型选择（Node 形状）：最后一条 `runtime/model_selection`，严格结构优先，否则只取
/// `providerId` 与 `modelId`（docs/specs/rust-m11-node-storage.md §6.1）。
pub fn selection(conn: &Connection, session: &str) -> anyhow::Result<Option<Value>> {
    let Some(data) = last_entry(conn, session, MODEL_SELECTION_ENTRY)? else {
        return Ok(None);
    };
    if let Some(parsed) = parse_model_selection(&data) {
        return Ok(Some(parsed));
    }
    let text = |key: &str| {
        data[key]
            .as_str()
            .map(rust_cli_base::js::string::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Ok(text("providerId")
        .zip(text("modelId"))
        .map(|(provider, model)| json!({"providerId": provider, "modelId": model})))
}

/// 会话的执行状态（Node `resumeFromStore` 的持久部分）：以会话行 `permission.mode` 为底，最后一条
/// 合法的 `runtime/execution_state` 整体覆盖。两者都没有时为 `None`。
pub fn execution(conn: &Connection, row: &SessionRow) -> anyhow::Result<Option<Execution>> {
    let base = row
        .permission
        .as_ref()
        .and_then(|permission| permission["mode"].as_str())
        .map(|mode| mode::resolve(Some(mode), &Execution::default()));
    let saved = last_entry(conn, &row.id, EXECUTION_STATE_ENTRY)?
        .as_ref()
        .and_then(mode::parse);
    Ok(saved.or(base))
}

// 性能修复（L21，docs/specs/rust-cli/performance.md §6）：原来是一条 SQL，按
// `sequence is null desc, sequence desc, …` 排序。这个顺序用不上索引，SQLite 先把会话的全部消息
// 逐条解析 JSON 过滤，再整体排序取第一条；每一步落库要调用 2–3 次，耗时随整个历史线性增长。
// 拆成两条：先看没有 sequence 的旧行（原顺序里它们排最前），再沿 `message_session_sequence_idx`
// 倒序扫，遇到第一条真实用户输入就停，结果与原来相同。实测 24 个提示 × 20 次 Read（2.7 MB）的会话，
// 最后一个提示的每步客户端开销中位数从 11.8 ms 降到 9.0 ms。
const CURRENT_USER_UNSEQUENCED: &str = "select id, data from message
          where session_id = ?1 and sequence is null
          and (case when json_valid(data) then json_extract(data, '$.role') end) = 'user'
          and (case when json_valid(data) then json_extract(data, '$.semantics.origin') end) = 'real_user'
          order by time_created desc, rowid desc limit 1";
const CURRENT_USER_SEQUENCED: &str = "select id, data from message
          where session_id = ?1 and sequence is not null
          and (case when json_valid(data) then json_extract(data, '$.role') end) = 'user'
          and (case when json_valid(data) then json_extract(data, '$.semantics.origin') end) = 'real_user'
          order by sequence desc, time_created desc, rowid desc limit 1";

/// `current_user` 依次执行的查询：前一条有结果就不再执行后面的（测试检查它们的查询计划）。
pub(crate) const CURRENT_USER_QUERIES: [&str; 2] =
    [CURRENT_USER_UNSEQUENCED, CURRENT_USER_SEQUENCED];

/// 当前轮：最后一条真实用户输入（`semantics.origin = real_user`）的 id 与 `anchor.turnId`。
/// 它是 assistant 的 `parentID`（Node `currentUserMessageId`）。非法 JSON 视为不匹配。
pub fn current_user(
    conn: &Connection,
    session: &str,
) -> anyhow::Result<Option<(String, Option<String>)>> {
    let mut stored: Option<(String, String)> = None;
    for sql in CURRENT_USER_QUERIES {
        stored = conn
            .prepare_cached(sql)?
            .query_row([session], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        if stored.is_some() {
            break;
        }
    }
    let Some((id, data)) = stored else {
        return Ok(None);
    };
    let info = decode_message(&data, &id, session)?;
    let turn = info["anchor"]["turnId"].as_str().map(str::to_owned);
    Ok(Some((id, turn)))
}

/// 本会话里的一条消息（Node `MessageInfo`）；不存在时为 `None`。
pub fn message(conn: &Connection, session: &str, id: &str) -> anyhow::Result<Option<Value>> {
    let data: Option<String> = conn
        .prepare_cached("select data from message where id = ? and session_id = ?")?
        .query_row([id, session], |r| r.get(0))
        .optional()?;
    data.map(|data| Ok(decode_message(&data, id, session)?))
        .transpose()
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
