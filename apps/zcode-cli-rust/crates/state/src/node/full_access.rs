//! Node `commitPermissionFullAccess` (`repositories/permission-full-access.ts`):
//! the admitted queued inputs switch to yolo with the execution state and the
//! grant receipt, inside the caller's transaction.
use super::entries;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

pub fn commit(
    conn: &Connection,
    session: &str,
    queue: &[String],
    (execution, receipt): (&entries::Entry, &entries::Entry),
    now: i64,
) -> Result<()> {
    ensure!(
        execution.session_id == session && receipt.session_id == session,
        "Permission commit session mismatch"
    );
    let existing: Option<String> = conn
        .query_row(
            "select session_id from session_entry where id = ?",
            [&receipt.id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(owner) = existing {
        ensure!(owner == session, "Permission receipt session mismatch");
        return Ok(());
    }
    for id in queue {
        let payload: Option<String> = conn
            .query_row(
                "select payload from session_input where id = ? and session_id = ? and status = 'admitted'",
                params![id, session],
                |r| r.get(0),
            )
            .optional()?;
        // Node 在此抛错整体回滚；Rust 的写入失败会反复重试，缺失的行跳过，不阻塞后续提交。
        let Some(payload) = payload else {
            continue;
        };
        let mut payload: Value = serde_json::from_str(&payload)?;
        for key in ["intent", "conversationInputIntent"] {
            if let Some(intent) = payload.get_mut(key).and_then(Value::as_object_mut) {
                intent.insert("mode".into(), "yolo".into());
            }
        }
        conn.execute(
            "update session_input set payload = ?, time_updated = ? where id = ? and session_id = ? and status = 'admitted'",
            params![super::json::stringify(&payload), now, id, session],
        )?;
    }
    entries::save(conn, execution)?;
    entries::save(conn, receipt)?;
    Ok(())
}
