//! A goal over the Node database (spec rust-m11-node-storage §5.4): `/goal`
//! is a control-only turn, continuation turns open with Node's notice, the
//! verifier's lifecycle is stored, and the target row keeps Node's
//! accounting; a restarted runtime and Node read the same goal.
use super::harness;
use serde_json::{Value, json};
use zcode_cli_state::node::{entries, resume, targets};

fn user_messages(conn: &rusqlite::Connection, session: &str) -> Vec<Value> {
    conn.prepare(
        "select m.data, (select group_concat(json_extract(p.data, '$.text'), '') from part p
          where p.message_id = m.id) from message m where m.session_id = ? order by m.id",
    )
    .unwrap()
    .query_map([session], |r| {
        let mut info: Value = serde_json::from_str(&r.get::<_, String>(0)?).unwrap();
        info["_text"] = r.get::<_, Option<String>>(1)?.into();
        Ok(info)
    })
    .unwrap()
    .map(Result::unwrap)
    .filter(|m| m["role"] == "user")
    .collect()
}

#[tokio::test]
async fn a_goal_loop_is_stored_as_nodes_target_verifications_and_notices() {
    let mut h = harness::start(Some("goal"), None).await;
    let session = h.create("c1", "hi").await;
    h.settled(&session, 1).await;
    let ack = h
        .command(
            10,
            json!({"commandId": "g1", "clientId": "cli", "sessionId": session,
            "type": "sendGoalCommand", "issuedAt": 1, "payload": {"text": "ship it"}}),
        )
        .await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    // 两轮续跑（第一次验证未通过），每轮都有稳定边界。
    let conn = h.settled(&session, 3).await;
    let mut target = None;
    for _ in 0..250 {
        target = targets::read(&conn, &session)
            .unwrap()
            .filter(|t| t.status == "complete");
        if target.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let target = target.expect("goal completed");
    assert!(
        target.target_id.starts_with("target_"),
        "{}",
        target.target_id
    );
    assert_eq!(target.objective, "ship it");
    // 每轮两次模型调用（105 token）；验证请求不计入目标。
    assert_eq!(target.tokens_used, 420);
    assert_eq!(target.active_run_started_at, None);

    let users = user_messages(&conn, &session);
    let goal = users
        .iter()
        .find(|m| m["_text"] == "/goal ship it")
        .unwrap();
    assert_eq!(goal["metadata"]["executionKind"], "controlOnly");
    assert!(
        goal["anchor"].get("turnId").is_none(),
        "{:#}",
        goal["anchor"]
    );
    let notices: Vec<&Value> = users
        .iter()
        .filter(|m| m["source"] == "goal-continuation")
        .collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(
        notices[0]["metadata"]["targetId"],
        target.target_id.as_str()
    );
    assert!(
        notices[1]["_text"]
            .as_str()
            .unwrap()
            .contains("Next action: Write the tests.")
    );

    let lifecycle: Vec<String> =
        entries::list(&conn, &session, Some("target_completion_verification"))
            .unwrap()
            .iter()
            .map(|e| e.data["payload"]["status"].as_str().unwrap().to_owned())
            .collect();
    assert_eq!(lifecycle, ["started", "completed", "started", "completed"]);
    let timeline: String = conn
        .query_row(
            "select data from message where id = ?",
            [format!("msg_goal_verify_{}_2", target.target_id)],
            |r| r.get(0),
        )
        .unwrap();
    let timeline: Value = serde_json::from_str(&timeline).unwrap();
    assert_eq!(timeline["finish"], "completed");

    // 冷读取的模型上下文与最后一次续跑请求一致（/goal 原文与 target_continuation 提醒）。
    let mut last = vec![];
    for _ in 0..6 {
        last = h.requests.recv().await.unwrap();
    }
    let live: Vec<Value> = last
        .into_iter()
        .filter(|m| m["role"] != "system")
        .filter(|m| {
            !m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("<system-reminder>\nCurrent session goal state"))
                && !m["content"].as_str().is_some_and(|c| {
                    c.starts_with("<system-reminder>") && m.get("_zcode_source").is_none()
                })
        })
        .map(|mut m| {
            m.as_object_mut().unwrap().remove("_zcode_request_content");
            m
        })
        .collect();
    let cold = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap()
        .history
        .messages;
    assert_eq!(cold[..live.len()], live[..], "{cold:#?}");
    assert!(cold.iter().any(|m| m["content"] == "/goal ship it"));
    assert_eq!(
        cold.iter()
            .filter(|m| m["_zcode_source"] == "target_continuation")
            .count(),
        2
    );

    drop(conn);
    let h = harness::restart(&h).await;
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.conversation.state["goal"]["status"], "verified");
    harness::dump(&h, &conn, &session);
}

#[tokio::test]
async fn pausing_and_resuming_a_goal_writes_nodes_status_and_notices() {
    let mut h = harness::start(Some("goal-pause"), None).await;
    let session = h.create("c1", "hi").await;
    h.settled(&session, 1).await;
    let goal = |_: u64, command: &str, kind: &str, payload: Value| {
        json!({"commandId": command, "clientId": "cli", "sessionId": session,
            "type": kind, "issuedAt": 1, "payload": payload})
    };
    let ack = h
        .command(
            10,
            goal(10, "g1", "sendGoalCommand", json!({"text": "stall here"})),
        )
        .await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    // 验证未通过且没有下一步：循环停止，目标保持 active。
    let conn = h.settled(&session, 2).await;
    let mut entries_seen = 0;
    for _ in 0..250 {
        entries_seen = entries::list(&conn, &session, Some("target_completion_verification"))
            .unwrap()
            .len();
        if entries_seen == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(entries_seen, 2);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        targets::read(&conn, &session).unwrap().unwrap().status,
        "active"
    );
    let (_, revision, epoch) = h.rows(20, &session).await;
    let mut pause = goal(11, "g2", "pauseGoal", json!({}));
    pause["baseRevision"] = revision;
    pause["baseLogEpoch"] = epoch;
    let ack = h.command(11, pause).await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    assert_eq!(
        targets::read(&conn, &session).unwrap().unwrap().status,
        "paused"
    );
    let (_, revision, epoch) = h.rows(21, &session).await;
    let mut resume = goal(12, "g3", "resumeGoal", json!({}));
    resume["baseRevision"] = revision;
    resume["baseLogEpoch"] = epoch;
    let ack = h.command(12, resume).await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    let conn = h.settled(&session, 3).await;
    let mut status = String::new();
    for _ in 0..250 {
        status = targets::read(&conn, &session).unwrap().unwrap().status;
        if status == "complete" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(status, "complete");
    let changes: Vec<String> = user_messages(&conn, &session)
        .iter()
        .filter(|m| m["source"] == "goal_state_change")
        .map(|m| m["_text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        changes,
        [
            zcode_cli_rust::domain::node_journal::goal::state_text("paused"),
            zcode_cli_rust::domain::node_journal::goal::state_text("resumed"),
        ]
    );
    harness::dump(&h, &conn, &session);
}
