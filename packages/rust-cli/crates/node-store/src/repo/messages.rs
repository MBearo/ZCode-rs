// 复制自 apps/zcode-cli-rust/crates/state/src/node/messages.rs（feat/rs 79b6691c55）。
//! `message` 与 `part` 行（Node `repositories/messages.ts`，docs/specs/rust-m11-node-storage.md
//! §2.3、§5）。值是带 id 成员的 Node `MessageInfo` / `MessagePart` 对象。
//!
//! 与复制来源的差异：去掉 fork 复制（`copyFrom`）与删除操作，本 crate 还不使用。
use super::codecs::{decode_message, decode_part, legacy_user_model, part_created_at};
use super::sessions::touch;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use rust_cli_base::js::json::stringify;
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

/// Node `MessageWithParts`。
#[derive(Clone, Debug, PartialEq)]
pub struct WithParts {
    pub info: Value,
    pub parts: Vec<Value>,
}

impl WithParts {
    pub fn id(&self) -> &str {
        self.info["id"].as_str().unwrap_or("")
    }
}

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
