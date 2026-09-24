use super::*;
use crate::domain::usage::{Tokens, ToolFact, TurnFact};

fn tool(status: &'static str, started_at: u64, duration: Option<u64>) -> Fact {
    Fact::Tool(Box::new(ToolFact {
        session_id: "s".into(),
        tool_call_id: "c".into(),
        tool_name: "Bash".into(),
        approval_status: "none",
        status,
        started_at,
        duration_ms: duration,
        ..ToolFact::default()
    }))
}

#[test]
fn upserts_merge_like_node_and_pruning_drops_old_rows() {
    let conn = Connection::open_in_memory().unwrap();
    prepare(&conn).unwrap();
    let mut writer = Writer::default();
    let now = 40 * 86_400_000;
    writer
        .record(&conn, &tool("running", now - 10, None), now)
        .unwrap();
    writer
        .record(&conn, &tool("completed", now - 5, Some(7)), now)
        .unwrap();
    // 迟到的 running 不能覆盖终态，开始时间取最早值。
    writer
        .record(&conn, &tool("running", now - 1, None), now)
        .unwrap();
    let row: (String, i64, Option<i64>) = conn
        .query_row(
            "SELECT status,started_at,duration_ms FROM rust_tool_usage",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("completed".into(), (now - 10) as i64, Some(7)));
    let turn = |started_at: u64, status: &'static str| {
        Fact::Turn(Box::new(TurnFact {
            session_id: "s".into(),
            turn_id: "t".into(),
            status,
            started_at,
            completed_at: started_at + 100,
            tokens: Tokens {
                input: 3,
                ..Tokens::default()
            },
            ..TurnFact::default()
        }))
    };
    writer.record(&conn, &turn(now - 50, "error"), now).unwrap();
    writer
        .record(&conn, &turn(now - 20, "completed"), now)
        .unwrap();
    let merged: (String, i64, i64) = conn
        .query_row(
            "SELECT status,started_at,input_tokens FROM rust_turn_usage",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(merged, ("completed".into(), (now - 50) as i64, 3));
    let Fact::Tool(mut old) = tool("completed", 1, None) else {
        unreachable!()
    };
    old.tool_call_id = "old".into();
    writer.record(&conn, &Fact::Tool(old), now).unwrap();
    prune(&conn, "rust_", now - RETENTION_MS).unwrap();
    let left: i64 = conn
        .query_row("SELECT count(*) FROM rust_tool_usage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 1, "the old row is gone, the recent one stays");
}
