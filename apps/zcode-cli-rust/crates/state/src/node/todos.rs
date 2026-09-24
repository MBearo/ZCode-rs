//! `todo` rows (Node `repositories/todos.ts`): a replace-all list. The caller
//! owns the transaction. Spec rust-m11-node-storage §4.
use super::sessions::touch;
use anyhow::Result;
use rusqlite::{Connection, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    pub content: String,
    pub status: String,
    pub priority: String,
}

/// Node `readTodos` (`order by position asc`).
pub fn read(conn: &Connection, session: &str) -> Result<Vec<Todo>> {
    Ok(conn
        .prepare_cached(
            "select content, status, priority from todo where session_id = ? order by position asc",
        )?
        .query_map([session], |r| {
            Ok(Todo {
                content: r.get(0)?,
                status: r.get(1)?,
                priority: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Node `updateTodos`: delete, insert with positions 0..n, touch the session.
pub fn update(conn: &Connection, session: &str, todos: &[Todo], now: i64) -> Result<()> {
    conn.prepare_cached("delete from todo where session_id = ?")?
        .execute([session])?;
    let mut insert = conn.prepare_cached(
        "insert into todo (session_id, content, status, priority, position, time_created, time_updated)
      values (?, ?, ?, ?, ?, ?, ?)",
    )?;
    for (position, todo) in todos.iter().enumerate() {
        insert.execute(params![
            session,
            todo.content,
            todo.status,
            todo.priority,
            position as i64,
            now,
            now
        ])?;
    }
    touch(conn, session, now)?;
    Ok(())
}
