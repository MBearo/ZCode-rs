// 复制自 apps/zcode-cli-rust/crates/state/src/node/entries.rs（feat/rs 79b6691c55）。
//! `session_entry` 行（Node `repositories/session-entries.ts`，docs/specs/rust-m11-node-storage.md §4）。
use super::codecs::{MODEL_SELECTION_ENTRY, decode_entry_data};
use super::sessions::touch;
use anyhow::Result;
use rusqlite::{Connection, params};
use rust_cli_base::js::json::stringify;
use serde_json::{Value, json};

/// Node `SessionEntryInfo`。
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub time_created: i64,
    pub time_updated: i64,
    /// 逻辑数据；模型选择写入时包成 `{modelSelection}`，读取时拆开。
    pub data: Value,
    /// Node `touchSession`；默认推进会话的活动时间。
    pub touch_session: bool,
}

/// Node `saveSessionEntry`：保留 `time_created` 的 upsert；模型选择只替换 `$.modelSelection`，
/// 保留 legacy 字段。
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

/// Node `sessionEntries`（`order by time_created, rowid`）；`kind` 按 type 过滤。
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
