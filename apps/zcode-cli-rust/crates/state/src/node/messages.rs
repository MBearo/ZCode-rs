//! `message` and `part` rows (Node `repositories/messages.ts`). Values are
//! Node `MessageInfo` / `MessagePart` objects including their id members.
//! Spec rust-m11-node-storage §2.3, §5.
use super::codecs::{decode_message, decode_part, legacy_user_model, part_created_at};
use super::json::stringify;
use super::sessions::touch;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};
use std::sync::LazyLock;

const MESSAGE_LEGACY: [&str; 4] = ["model", "providerID", "modelID", "variant"];
const PART_LEGACY: [&str; 3] = ["fromModel", "toModel", "model"];

/// Node `preserveLegacyMembers`: re-inject old top-level members only.
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

/// The source row of a fork copy (Node `copyFrom`).
#[derive(Clone, Copy, Debug)]
pub struct CopyFrom<'a> {
    pub session_id: &'a str,
    pub id: &'a str,
}

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

/// Node `copyLegacyMembers`: legacy keys of the source row win.
fn copy_legacy(
    conn: &Connection,
    table: &str,
    mut data: Map<String, Value>,
    source: Option<CopyFrom>,
) -> Result<Map<String, Value>> {
    let Some(source) = source else {
        return Ok(data);
    };
    let stored: Option<String> = conn
        .prepare_cached(&format!(
            "SELECT data FROM {table} WHERE id=? AND session_id=?"
        ))?
        .query_row(params![source.id, source.session_id], |r| r.get(0))
        .optional()?;
    let stored =
        stored.with_context(|| format!("Storage copy source missing: {table}/{}", source.id))?;
    let original: Map<String, Value> = serde_json::from_str(&stored)?;
    let keys: &[&str] = if table == "message" {
        &MESSAGE_LEGACY
    } else {
        &PART_LEGACY
    };
    for key in keys {
        if let Some(value) = original.get(*key) {
            data.insert((*key).into(), value.clone());
        }
    }
    Ok(data)
}

/// Node `saveMessage`. `now` stands for `Date.now()` at save time.
pub fn save_message(
    conn: &Connection,
    info: &Value,
    copy_from: Option<CopyFrom>,
    now: i64,
) -> Result<()> {
    let id = text(info, "id")?;
    let session = text(info, "sessionID")?;
    let mut data = members(info, &["id", "sessionID"])?;
    let user = info["role"] == "user";
    if user {
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
    let data = copy_legacy(conn, "message", data, copy_from)?;
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

/// Node `removeMessage` (parts go with it through the foreign key).
pub fn remove_message(conn: &Connection, session: &str, id: &str) -> Result<()> {
    conn.prepare_cached("delete from message where id = ? and session_id = ?")?
        .execute(params![id, session])?;
    Ok(())
}

/// Node `savePart`'s stored shape of `model_change` timelines and subtasks.
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

/// Node `savePart`. `now` stands for `Date.now()` at save time.
pub fn save_part(
    conn: &Connection,
    part: &Value,
    copy_from: Option<CopyFrom>,
    now: i64,
) -> Result<()> {
    let id = text(part, "id")?;
    let message = text(part, "messageID")?;
    let session = text(part, "sessionID")?;
    let data = stored_part(part, members(part, &["id", "sessionID", "messageID"])?);
    let data = copy_legacy(conn, "part", data, copy_from)?;
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

/// Node `removePart`.
pub fn remove_part(conn: &Connection, session: &str, message: &str, id: &str) -> Result<()> {
    conn.prepare_cached("delete from part where id = ? and message_id = ? and session_id = ?")?
        .execute(params![id, message, session])?;
    Ok(())
}

/// Node `MessageWithParts`.
#[derive(Clone, Debug, PartialEq)]
pub struct WithParts {
    pub info: Value,
    pub parts: Vec<Value>,
}

/// Node `messages`: `sequence` order with NULLs last; parts grouped per message.
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

/// Node `messageWithParts`.
pub fn message_with_parts(conn: &Connection, session: &str, id: &str) -> Result<Option<WithParts>> {
    let stored: Option<String> = conn
        .prepare_cached("select data from message where id = ? and session_id = ?")?
        .query_row(params![id, session], |r| r.get(0))
        .optional()?;
    let Some(data) = stored else { return Ok(None) };
    let info = decode_message(&data, id, session)?;
    let mut query = conn.prepare_cached(
        "select id, data from part where message_id = ? and session_id = ?
      order by sequence is null, sequence, time_created, id",
    )?;
    let parts = query
        .query_map(params![id, session], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (part, data) = row?;
            Ok(decode_part(&data, &part, session, id)?)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(WithParts { info, parts }))
}
