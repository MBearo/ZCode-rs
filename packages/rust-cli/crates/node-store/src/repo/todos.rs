//! `todo` 表（Node 的 todo 投影，App 的清单读它）：按会话整组替换（F128，审查 X5：以前 SQL 在 task_lists 里）。

use rusqlite::{Connection, params};

use super::sessions::touch;

/// 这个会话的 todo，按 `position` 升序：（内容，状态，优先级）。
pub fn list(conn: &Connection, session: &str) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut statement = conn.prepare(
        "select content, status, priority from todo where session_id = ? order by position asc",
    )?;
    let rows = statement.query_map(params![session], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    rows.collect()
}

/// 整组替换这个会话的 todo，推进会话的 `time_updated`（调用方在一个事务里调）。
pub fn replace(
    conn: &Connection,
    session: &str,
    rows: &[(String, String, String)],
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute("delete from todo where session_id = ?", params![session])?;
    let mut insert = conn.prepare(
        "insert into todo (session_id, content, status, priority, position, time_created, time_updated) values (?, ?, ?, ?, ?, ?, ?)",
    )?;
    for (position, (content, status, priority)) in rows.iter().enumerate() {
        let position = i64::try_from(position).unwrap_or(i64::MAX);
        insert.execute(params![
            session, content, status, priority, position, now, now
        ])?;
    }
    touch(conn, session, now)
}
