//! Compaction records over a real Node database: the preserved tail is chosen
//! by assistant rounds in the stored transcript, and an interrupted timeline
//! closes on resume.
use super::compact::recover;
use super::resume::resume;
use super::{cold, open};
use crate::domain::node_journal::{CompactStart, Outcome, Prompt};
use crate::domain::session::Session;
use serde_json::{Value, json};

fn database() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = open::open(
        &dir.path().join("db.sqlite"),
        open::BUSY_TIMEOUT,
        &mut |_| {},
    )
    .unwrap();
    (dir, conn)
}

fn commit(conn: &rusqlite::Connection, s: &mut Session) {
    let tx = conn.unchecked_transaction().unwrap();
    super::apply::apply(&tx, &s.id.clone(), &s.node.take()).unwrap();
    tx.commit().unwrap();
}

fn ids() -> impl FnMut() -> String {
    let mut n = 0;
    move || {
        n += 1;
        format!("00000000-0000-4000-8000-{n:012}")
    }
}

/// Two plain turns: `one` → `first`, `two` → `second`.
fn two_turns(conn: &rusqlite::Connection) -> Session {
    let mut s = Session::new(
        "sess_c".into(),
        "/w".into(),
        "p".into(),
        "m".into(),
        "".into(),
        "e".into(),
        1,
    );
    s.workspace_path = Some("/w".into());
    let mut next = ids();
    s.node_ensure_created(10, "one", "0.0.0");
    for (turn, (text, answer)) in [("one", "first"), ("two", "second")]
        .into_iter()
        .enumerate()
    {
        let at = 100 * (turn as u64 + 1);
        s.node_user_prompt(
            at,
            Prompt {
                message: format!("msg_u{turn}"),
                part: format!("part_u{turn}"),
                turn: &format!("t{turn}"),
                text,
                command: None,
                queue_id: None,
                metadata: None,
                tools: &[],
                files: vec![],
            },
        );
        s.node_step_started(
            at + 1,
            (format!("msg_a{turn}"), format!("part_s{turn}")),
            "p",
            "m",
        );
        s.node_model_status(&json!({"type": "model_request_completed", "finishReason": "stop"}));
        let message = json!({"role": "assistant", "content": answer, "_zcode_origin": {"provider": "p", "model": "m"}});
        s.node_model_done(at + 2, Some(&message), &mut next);
        s.node_finished(at + 3, Outcome::Success, &mut next);
    }
    commit(conn, &mut s);
    s
}

fn start(s: &mut Session, now: u64, trigger: &str) {
    s.node_compact_started(
        now,
        CompactStart {
            ids: ("cmp_1".into(), "msg_host".into(), "part_cmp".into()),
            trigger,
            source_command: None,
            pre_tokens: 90,
            custom_instructions: false,
        },
    );
}

#[test]
fn auto_compaction_keeps_the_last_rounds_after_the_summary() {
    let (_dir, conn) = database();
    let mut s = two_turns(&conn);
    start(&mut s, 500, "auto");
    s.node_compact_done(
        510,
        json!({"summaryMessageId": "msg_sum", "textPartId": "part_st", "compactionPartId": "part_sc",
            "boundaryId": "compact_1", "content": "This session is being continued. Summary: one",
            "body": "Summary: one", "selection": {"providerId": "p", "modelId": "m"}, "tools": {},
            "turnId": "turn_t1", "traceId": "trace", "summarizedMessageCount": 2, "groupsPreserved": 1,
            "postCompactTokenCount": 30, "truePostCompactTokenCount": 40,
            "reminders": [{"messageId": "msg_rem", "partId": "part_rem",
                "source": "resume_referenced_session_context", "content": "Note: a.ts"}]}),
    );
    commit(&conn, &mut s);
    let summary = super::messages::messages(&conn, "sess_c")
        .unwrap()
        .into_iter()
        .find(|m| m.info["id"] == "msg_sum")
        .unwrap();
    let boundary = &summary.parts[1]["compactBoundary"];
    // 保留最后一个助手轮：Node 按 assistant 开启的轮分组，最后一轮只有第二个回答。
    assert_eq!(
        boundary["preservedSegment"],
        json!({"anchorMessageId": "msg_sum", "headMessageId": "msg_a1", "tailMessageId": "msg_a1"})
    );
    assert_eq!(boundary["keptMessageCount"], 1, "{boundary:#}");
    assert_eq!(boundary["lastSummarizedMessageId"], "msg_a1");
    let history = cold::history(&conn, "sess_c", &|_| None).unwrap();
    assert!(
        history
            .summary
            .unwrap()
            .starts_with("This session is being continued")
    );
    let contents: Vec<&Value> = history.messages.iter().map(|m| &m["content"]).collect();
    assert_eq!(contents[0], &json!("second"));
    assert_eq!(
        history.messages[1]["_zcode_source"],
        "resume_referenced_session_context"
    );
    let host = super::messages::messages(&conn, "sess_c")
        .unwrap()
        .into_iter()
        .find(|m| m.info["id"] == "msg_host")
        .unwrap();
    assert_eq!(host.info["finish"], "completed");
    assert_eq!(host.parts[1]["timelineStatus"], "completed");
}

#[test]
fn a_compaction_left_running_closes_on_resume() {
    let (_dir, conn) = database();
    let mut s = two_turns(&conn);
    start(&mut s, 500, "manual");
    commit(&conn, &mut s);
    let host = crate::domain::node_journal::timeline::Host {
        session: "sess_c".into(),
        agent: "zcode-agent".into(),
        provider: "p".into(),
        model: "m".into(),
        mode: "build".into(),
        plan: false,
        cwd: "/w".into(),
    };
    assert_eq!(recover(&conn, &host, 900).unwrap(), 1);
    assert_eq!(recover(&conn, &host, 901).unwrap(), 0, "closed once");
    let timeline = super::messages::messages(&conn, "sess_c")
        .unwrap()
        .into_iter()
        .find(|m| m.info["id"] == "msg_host")
        .unwrap();
    assert_eq!(timeline.info["time"]["completed"], 900);
    assert_eq!(timeline.parts[1]["timelineStatus"], "interrupted");
    let resumed = resume(&conn, "sess_c", &|_| None, None).unwrap().unwrap();
    let marker = resumed
        .conversation
        .rows
        .iter()
        .find(|r| r["kind"] == "timelineMarker")
        .unwrap();
    assert_ne!(marker["marker"]["status"], "running");
}
