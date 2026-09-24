//! `input_history` rows (Node `repositories/input-history.ts`): deduplicated
//! against the project's latest entry and capped at 100 rows over the whole
//! table. The caller owns the transaction. Spec rust-m11-node-storage §4.
use super::json::stringify;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const LIMIT: i64 = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub text: String,
    pub attachments: Option<Value>,
    pub kind: String,
    pub time_created: i64,
}

fn trimmed(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(|s| s.trim_matches(super::migrations::js_whitespace))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node `normalizedInputHistoryAttachments`.
fn normalize(attachments: Option<&Value>) -> Option<Value> {
    let normalized: Vec<Value> = attachments
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let kind = a["type"]
                .as_str()
                .filter(|t| matches!(*t, "file" | "image" | "pdf" | "url"))?;
            let path = trimmed(&a["path"]);
            let content = trimmed(&a["content"]).filter(|c| !c.starts_with("data:"));
            if path.is_none() && content.is_none() {
                return None;
            }
            let mut out = json!({"type": kind});
            if let Some(path) = path {
                out["path"] = path.into();
            }
            if let Some(content) = content {
                out["content"] = content.into();
            }
            Some(out)
        })
        .collect();
    (!normalized.is_empty()).then_some(Value::Array(normalized))
}

fn stable(attachments: Option<&Value>) -> String {
    stringify(&normalize(attachments).unwrap_or_else(|| json!([])))
}

/// Node `recordInputHistory`; `None` for blank text or a repeat of the latest entry.
#[allow(clippy::too_many_arguments)]
pub fn record(
    conn: &Connection,
    id: &str,
    project: &str,
    session: Option<&str>,
    text: &str,
    attachments: Option<&Value>,
    kind: &str,
    created: i64,
) -> Result<Option<Entry>> {
    let text = text.trim_matches(super::migrations::js_whitespace);
    if text.is_empty() {
        return Ok(None);
    }
    let attachments = normalize(attachments);
    if let Some(latest) = recall(conn, project, 0)?
        && latest.text == text
        && stable(latest.attachments.as_ref()) == stable(attachments.as_ref())
    {
        return Ok(None);
    }
    conn.prepare_cached(
        "insert into input_history (id, project_id, session_id, text, attachments, kind, time_created)
        values (?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        project,
        session,
        text,
        attachments.as_ref().map(stringify),
        kind,
        created
    ])?;
    conn.prepare_cached(
        "delete from input_history where id not in (
          select id from input_history order by time_created desc, id desc limit ?
        )",
    )?
    .execute([LIMIT])?;
    Ok(Some(Entry {
        id: id.into(),
        project_id: project.into(),
        session_id: session.map(str::to_owned),
        text: text.into(),
        attachments,
        kind: kind.into(),
        time_created: created,
    }))
}

/// Node `recallPreviousInputHistory`.
pub fn recall(conn: &Connection, project: &str, skip: i64) -> Result<Option<Entry>> {
    let decode = |r: &rusqlite::Row| -> rusqlite::Result<Entry> {
        let attachments: Option<String> = r.get(3)?;
        Ok(Entry {
            id: r.get(0)?,
            project_id: project.into(),
            session_id: r.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty()),
            text: r.get(2)?,
            // 存储值在读取后再按 Node 规则归一化；解析失败在下方报错。
            attachments: attachments.map(Value::String),
            kind: r.get(4)?,
            time_created: r.get(5)?,
        })
    };
    let entry = conn
        .prepare_cached(
            "select id, session_id, text, attachments, kind, time_created from input_history
      where project_id = ? order by time_created desc, id desc limit 1 offset ?",
        )?
        .query_row(params![project, skip], decode)
        .optional()?;
    let Some(mut entry) = entry else {
        return Ok(None);
    };
    let stored = match entry.attachments.take() {
        Some(Value::String(text)) => Some(serde_json::from_str::<Value>(&text)?),
        _ => None,
    };
    entry.attachments = normalize(stored.as_ref());
    Ok(Some(entry))
}
