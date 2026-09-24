//! `session_entry` rows (Node `repositories/session-entries.ts`).
//! Spec rust-m11-node-storage §4.
use super::codecs::{MODEL_SELECTION_ENTRY, decode_entry_data};
use super::json::stringify;
use super::sessions::touch;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// Node `SessionEntryInfo`.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub time_created: i64,
    pub time_updated: i64,
    /// The logical data; model selections are wrapped on write and unwrapped on read.
    pub data: Value,
    /// Node `touchSession`; the default touches the session.
    pub touch_session: bool,
}

/// Node `saveSessionEntry`: an upsert that keeps `time_created`; a model
/// selection update only replaces `$.modelSelection`, keeping legacy fields.
pub fn save(conn: &Connection, entry: &Entry) -> Result<()> {
    let selection = entry.kind == MODEL_SELECTION_ENTRY;
    let data = if selection {
        json!({"modelSelection": entry.data})
    } else {
        entry.data.clone()
    };
    anyhow::ensure!(
        selection || !data.is_null(),
        "Session entry data must be JSON-serializable"
    );
    conn.prepare_cached(
        "insert into session_entry (id, session_id, type, time_created, time_updated, data)
      values (?, ?, ?, ?, ?, ?)
      on conflict(id) do update set
        session_id = excluded.session_id,
        type = excluded.type,
        time_updated = excluded.time_updated,
        data = case
          when ? and session_entry.type = excluded.type
            and session_entry.session_id = excluded.session_id
            and json_type(session_entry.data) = 'object'
          then json_set(session_entry.data, '$.modelSelection', json_extract(excluded.data, '$.modelSelection'))
          else excluded.data
        end",
    )?
    .execute(params![
        entry.id,
        entry.session_id,
        entry.kind,
        entry.time_created,
        entry.time_updated,
        stringify(&data),
        i64::from(selection)
    ])?;
    if entry.touch_session {
        touch(conn, &entry.session_id, entry.time_updated)?;
    }
    Ok(())
}

/// Node `sessionEntries` (`order by time_created, rowid`); `kind` filters by type.
pub fn list(conn: &Connection, session: &str, kind: Option<&str>) -> Result<Vec<Entry>> {
    let sql = if kind.is_some() {
        "select id, session_id, type, time_created, time_updated, data from session_entry
      where session_id = ? and type = ? order by time_created, rowid"
    } else {
        "select id, session_id, type, time_created, time_updated, data from session_entry
      where session_id = ? order by time_created, rowid"
    };
    let mut query = conn.prepare_cached(sql)?;
    let decode = |r: &rusqlite::Row| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, String>(5)?,
        ))
    };
    let rows = match kind {
        Some(kind) => query
            .query_map(params![session, kind], decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        None => query
            .query_map(params![session], decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    rows.into_iter()
        .map(|(id, session_id, kind, created, updated, data)| {
            Ok(Entry {
                data: decode_entry_data(&kind, &data)?,
                id,
                session_id,
                kind,
                time_created: created,
                time_updated: updated,
                touch_session: true,
            })
        })
        .collect()
}

/// The owning session of a global entry id (Node reads it before idempotent writes).
pub fn owner(conn: &Connection, id: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .prepare_cached("select session_id from session_entry where id = ?")?
        .query_row([id], |r| r.get(0))
        .optional()?)
}
