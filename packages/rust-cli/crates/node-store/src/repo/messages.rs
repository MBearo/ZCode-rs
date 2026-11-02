// 复制自 apps/zcode-cli-rust/crates/state/src/node/messages.rs（feat/rs 79b6691c55）。
//! `message` 与 `part` 行（Node `repositories/messages.ts`，docs/specs/rust-m11-node-storage.md
//! §2.3、§5）。值是带 id 成员的 Node `MessageInfo` / `MessagePart` 对象。
//!
//! 与复制来源的差异：去掉 fork 复制（`copyFrom`）与删除操作，本 crate 还不使用。
use super::sessions::touch;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use rust_cli_base::js::json::stringify;
use rust_cli_node_records::codecs::{
    decode_message, decode_part, legacy_user_model, part_created_at,
};
use serde_json::{Map, Value};
use std::sync::LazyLock;

const MESSAGE_LEGACY: [&str; 4] = ["model", "providerID", "modelID", "variant"];
const PART_LEGACY: [&str; 3] = ["fromModel", "toModel", "model"];

/// Node `preserveLegacyMembers`：只把旧的顶层成员注回新数据。
fn preserve_legacy_members(table: &str, keys: &[&str]) -> String {
    keys.iter().fold("excluded.data".to_owned(), |sql, key| {
        format!(
            "(select case when json_type({table}.data, '$.{key}') is not null
      then json_set(previous.data, '$.{key}', json_extract({table}.data, '$.{key}')) else previous.data end
      from (select {sql} as data) as previous)"
        )
    })
}

static SAVE_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "insert into message (id, session_id, time_created, time_updated, data, sequence)
      values (?, ?, ?, ?, ?, (select coalesce(max(sequence), -1) + 1 from message where session_id = ?))
      on conflict(id) do update set
        session_id = excluded.session_id,
        time_updated = excluded.time_updated,
        data = case when message.session_id = excluded.session_id then {} else excluded.data end,
        sequence = case
          when message.session_id = excluded.session_id then message.sequence
          else excluded.sequence
        end",
        preserve_legacy_members("message", &MESSAGE_LEGACY)
    )
});

static SAVE_PART: LazyLock<String> = LazyLock::new(|| {
    format!(
        "insert into part (id, message_id, session_id, time_created, time_updated, data, sequence)
      values (?, ?, ?, ?, ?, ?, (select coalesce(max(sequence), -1) + 1 from part where message_id = ?))
      on conflict(id) do update set
        message_id = excluded.message_id,
        session_id = excluded.session_id,
        time_updated = excluded.time_updated,
        data = case when part.message_id = excluded.message_id and part.session_id = excluded.session_id
          then {} else excluded.data end,
        sequence = case
          when part.message_id = excluded.message_id and part.session_id = excluded.session_id
            then part.sequence
          else excluded.sequence
        end",
        preserve_legacy_members("part", &PART_LEGACY)
    )
});

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Stored record needs a string {key}"))
}

fn members(value: &Value, strip: &[&str]) -> Result<Map<String, Value>> {
    let Value::Object(map) = value else {
        bail!("Stored record must be an object");
    };
    let mut data = map.clone();
    for key in strip {
        data.shift_remove(*key);
    }
    Ok(data)
}

/// Node `saveMessage`。`now` 是保存时的 `Date.now()`。
pub fn save_message(conn: &Connection, info: &Value, now: i64) -> Result<()> {
    let id = text(info, "id")?;
    let session = text(info, "sessionID")?;
    let mut data = members(info, &["id", "sessionID"])?;
    if info["role"] == "user" {
        // 冻结旧版 Reader 无条件读取 user.model，缺少整个对象会让消息无法打开。
        data.insert(
            "model".into(),
            legacy_user_model(info.get("modelSelection")),
        );
    }
    let created = info["time"]["created"]
        .as_i64()
        .context("Stored message needs time.created")?;
    let updated = if info["role"] == "assistant" {
        info["time"]["completed"].as_i64().unwrap_or(now)
    } else {
        created
    };
    conn.prepare_cached(&SAVE_MESSAGE)?.execute(params![
        id,
        session,
        created,
        updated,
        stringify(&Value::Object(data)),
        session
    ])?;
    touch(conn, session, updated)?;
    Ok(())
}

/// Node `savePart` 对 `model_change` 时间线与 subtask 的存储形态。
fn stored_part(part: &Value, mut data: Map<String, Value>) -> Map<String, Value> {
    if part["type"] == "timeline" && part["timelineType"] == "model_change" {
        let from = data.shift_remove("fromModel");
        let to = data.shift_remove("toModel");
        let legacy = match to.as_ref().filter(|t| t.is_object()) {
            Some(to) => {
                let mut legacy =
                    serde_json::json!({"providerID": to["providerId"], "modelID": to["modelId"]});
                if let Some(level) = to["options"]["reasoningLevel"]
                    .as_str()
                    .filter(|l| !l.is_empty())
                {
                    legacy["variant"] = level.into();
                }
                if let Some(label) = to.get("label") {
                    legacy["label"] = label.clone();
                }
                legacy
            }
            None => serde_json::json!({}),
        };
        data.insert("toModel".into(), legacy);
        // undefined 成员在 JSON.stringify 中省略。
        if let Some(from) = from {
            data.insert("fromModelSelection".into(), from);
        }
        if let Some(to) = to {
            data.insert("toModelSelection".into(), to);
        }
    } else if part["type"] == "subtask" {
        let model = data.shift_remove("model");
        if let Some(model) = model {
            data.insert("modelSelection".into(), model);
        }
    }
    data
}

/// Node `savePart`。`now` 是保存时的 `Date.now()`。
pub fn save_part(conn: &Connection, part: &Value, now: i64) -> Result<()> {
    let id = text(part, "id")?;
    let message = text(part, "messageID")?;
    let session = text(part, "sessionID")?;
    let data = stored_part(part, members(part, &["id", "sessionID", "messageID"])?);
    conn.prepare_cached(&SAVE_PART)?.execute(params![
        id,
        message,
        session,
        part_created_at(part, now),
        now,
        stringify(&Value::Object(data)),
        message
    ])?;
    touch(conn, session, now)?;
    Ok(())
}

pub use rust_cli_node_records::row::WithParts;

/// Node `messages`：按 `sequence` 排序（NULL 在后），part 按消息分组。
pub fn messages(conn: &Connection, session: &str) -> Result<Vec<WithParts>> {
    let mut parts: std::collections::HashMap<String, Vec<Value>> = Default::default();
    let mut query = conn.prepare_cached(
        "select id, message_id, session_id, data from part where session_id = ?
      order by message_id, sequence is null, sequence, time_created, id",
    )?;
    let mut rows = query.query([session])?;
    while let Some(row) = rows.next()? {
        let (id, message, owner, data): (String, String, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        let part = decode_part(&data, &id, &owner, &message)?;
        parts.entry(message).or_default().push(part);
    }
    let mut query = conn.prepare_cached(
        "select id, session_id, data from message where session_id = ?
      order by sequence is null, sequence, time_created, rowid",
    )?;
    let mut rows = query.query([session])?;
    let mut out = vec![];
    while let Some(row) = rows.next()? {
        let (id, owner, data): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let info = decode_message(&data, &id, &owner)?;
        out.push(WithParts {
            parts: parts.remove(&id).unwrap_or_default(),
            info,
        });
    }
    Ok(out)
}

/// 消息 `id` 是否已存在（任一会话；`message.id` 是全库主键）。
pub fn exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached("select 1 from message where id = ?")?
        .query_row([id], |_| Ok(()))
        .optional()?
        .is_some())
}

// ---- 按 Node 记录查一行（F128，审查 X5：以前散在 tool_parts、state、compaction、query 里）----

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
pub const CURRENT_USER_QUERIES: [&str; 2] = [CURRENT_USER_UNSEQUENCED, CURRENT_USER_SEQUENCED];

/// 当前轮的那条真实用户输入的行（id 与 data）：依次执行 [`CURRENT_USER_QUERIES`]，前一条有结果就停。
pub fn current_user(conn: &Connection, session: &str) -> Result<Option<(String, String)>> {
    for sql in CURRENT_USER_QUERIES {
        let stored = conn
            .prepare_cached(sql)?
            .query_row([session], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        if stored.is_some() {
            return Ok(stored);
        }
    }
    Ok(None)
}

/// 本会话里一条消息的 data 列；不存在时 None。
pub fn message_data(conn: &Connection, session: &str, id: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("select data from message where id = ? and session_id = ?")?
        .query_row([id, session], |r| r.get(0))
        .optional()?)
}

/// 会话里最后一条消息行的 id（冷读取的行序；`sequence` 在会话内唯一、递增）。
pub fn last_message_id(conn: &Connection, session: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "select id from message where session_id = ? order by sequence desc limit 1",
        )?
        .query_row([session], |r| r.get(0))
        .optional()?)
}

/// 一个 assistant 消息里某个调用最后写的 tool part 的（id, data）（Node 同一调用只恢复最后新建的记录）。
pub fn last_tool_part(
    conn: &Connection,
    (session, message): (&str, &str),
    call: &str,
) -> Result<Option<(String, String)>> {
    Ok(conn
        .prepare_cached(
            "select id, data from part where message_id = ? and session_id = ?
          and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
          and (case when json_valid(data) then json_extract(data, '$.callID') end) = ?
          order by sequence desc, rowid desc limit 1",
        )?
        .query_row([message, session, call], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

/// 本会话里某个调用最后写的 tool part 的（id, 消息 id, data）。
pub fn last_tool_part_in_session(
    conn: &Connection,
    session: &str,
    call: &str,
) -> Result<Option<(String, String, String)>> {
    Ok(conn
        .prepare_cached(
            "select id, message_id, data from part where session_id = ?
          and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
          and (case when json_valid(data) then json_extract(data, '$.callID') end) = ?
          order by sequence desc, rowid desc limit 1",
        )?
        .query_row([session, call], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?)
}

/// 一个 assistant 里已经有结果（completed / error）的 tool part 数。
pub fn settled_tool_parts(conn: &Connection, (session, message): (&str, &str)) -> Result<u64> {
    let count: i64 = conn
        .prepare_cached(
            "select count(*) from part where message_id = ? and session_id = ?
          and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
          and (case when json_valid(data) then json_extract(data, '$.state.status') end)
            in ('completed', 'error')",
        )?
        .query_row([message, session], |r| r.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// 一个结果消息 id（`metadata.rustCli.resultMessageId`）或 tool part id 所在的消息行。
pub fn message_of_result(conn: &Connection, session: &str, id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "select message_id from part where session_id = ?1
              and (json_extract(data, '$.metadata.rustCli.resultMessageId') = ?2
                or (id = ?2 and json_extract(data, '$.type') = 'tool'))
              limit 1",
            params![session, id],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// 首条真实用户输入的第一个文字 part：沿 `message_session_time_created_id_idx` 的顺序找第一条
/// `role = user`、`semantics.origin = real_user` 的消息，取它的第一个非合成 text part（按 part 的顺序）。
const FIRST_PROMPT: &str = "select p.data from message m join part p on p.message_id = m.id
      where m.session_id = ?1
        and (case when json_valid(m.data) then json_extract(m.data, '$.role') end) = 'user'
        and (case when json_valid(m.data) then json_extract(m.data, '$.semantics.origin') end) = 'real_user'
        and (case when json_valid(p.data) then json_extract(p.data, '$.type') end) = 'text'
        and coalesce(case when json_valid(p.data) then json_extract(p.data, '$.synthetic') end, 0) = 0
      order by m.time_created, m.id, p.sequence is null, p.sequence, p.time_created, p.id
      limit 1";

/// 首条真实用户输入的第一个文字 part 的 data 列（[`FIRST_PROMPT`]）。
pub fn first_prompt(conn: &Connection, session: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(FIRST_PROMPT)?
        .query_row([session], |row| row.get(0))
        .optional()?)
}
