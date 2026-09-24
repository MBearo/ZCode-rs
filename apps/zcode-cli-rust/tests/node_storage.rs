//! The engine over the Node session database (spec rust-m11-node-storage
//! §5.2 acceptance): turns are written as Node records, and Node's cold
//! readers give back the live model context and rows, across a restart.
#[path = "node_storage/attach.rs"]
mod attach;
#[path = "node_storage/grants.rs"]
mod grants;
#[path = "node_storage/harness.rs"]
mod harness;

use serde_json::{Value, json};
use tokio::sync::oneshot;
use zcode_cli_state::node::{acks, inputs, resume, sessions};

#[tokio::test]
async fn a_tool_turn_is_stored_as_node_records_and_reads_back_as_the_live_context() {
    let mut h = harness::start(None, None).await;
    let session = h.create("c1", "Fix it").await;
    assert!(session.starts_with("sess_"), "{session}");
    let conn = h.settled(&session, 1).await;
    let _first = h.requests.recv().await.unwrap();
    let second = h.requests.recv().await.unwrap();

    let row = sessions::get(&conn, &session).unwrap().unwrap();
    assert_eq!(row.title, "Fix it");
    assert_eq!(row.title_source, "first_input");
    assert_eq!(row.directory, h.workspace);
    let ledger = inputs::get(&conn, "queue_c1").unwrap().unwrap();
    assert_eq!(ledger.status, "promoted");
    assert_eq!(ledger.payload["sourceCommandType"], "createSession");

    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    // 冷读取的模型上下文等于第二次请求看到的对话，再加上最终回答；
    // 环境上下文提醒只在请求时注入，Node 同样不落库。
    let live: Vec<Value> = second
        .into_iter()
        .filter(|m| m["role"] != "system")
        .filter(|m| {
            !m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("<system-reminder>"))
        })
        .map(|mut m| {
            m.as_object_mut().unwrap().remove("_zcode_request_content");
            m
        })
        .collect();
    let cold = &resumed.history.messages;
    assert_eq!(cold[..live.len()], live[..], "{cold:#?}");
    assert_eq!(cold.len(), live.len() + 1);
    assert_eq!(cold.last().unwrap()["content"], "Done.");
    let kinds: Vec<&str> = resumed
        .conversation
        .rows
        .iter()
        .filter_map(|r| r["kind"].as_str())
        .collect();
    for kind in [
        "turnHeader",
        "userInput",
        "reasoning",
        "assistantText",
        "toolCall",
    ] {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    let tool = resumed
        .conversation
        .rows
        .iter()
        .find(|r| r["kind"] == "toolCall")
        .unwrap();
    assert_eq!(tool["status"], "success");
    assert_eq!(resumed.model_selection.as_ref().unwrap()["modelId"], "m");
    assert_eq!(resumed.execution.as_ref().unwrap()["mode"], "yolo");

    // 重复的 createSession 与 sendText 命令从耐久事实得到回执。
    let create = acks::lookup_create(&conn, "c1", 0).unwrap().unwrap();
    assert_eq!(create["result"]["sessionId"], session.as_str());
    let transcript = acks::lookup(&conn, (&session, false), "c1", 0)
        .unwrap()
        .unwrap();
    assert_eq!(transcript["status"], "accepted");
}

#[tokio::test]
async fn queued_input_and_a_restarted_runtime_continue_the_node_session() {
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let mut h = harness::start(Some("restart"), Some((started_tx, release_rx))).await;
    let session = h.create("c1", "Fix it").await;
    started.await.unwrap();
    // 运行中的输入先进入账本（admitted/queue），轮次结束后提升为下一轮。
    let ack = h.send_text(2, &session, "c2", "Then this").await;
    assert_eq!(ack["result"]["delivery"], "queue");
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    let queued = inputs::get(&conn, "queue_c2").unwrap().unwrap();
    assert_eq!(
        (queued.status.as_str(), queued.delivery.as_str()),
        ("admitted", "queue")
    );
    let intent = &queued.payload["conversationInputIntent"];
    assert_eq!(intent["dispatch"]["state"], "queued");
    assert_eq!(intent["order"]["queuePosition"], 0);
    assert_eq!(queued.payload["intent"]["admittedDelivery"], "queue");
    release.send(()).unwrap();
    h.settled(&session, 2).await;
    assert_eq!(
        inputs::get(&conn, "queue_c2").unwrap().unwrap().status,
        "promoted"
    );

    let mut next = harness::restart(&h).await;
    drop(h);
    let ack = next.send_text(3, &session, "c3", "Again").await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    let conn = next.settled(&session, 3).await;
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    let users: Vec<&Value> = resumed
        .history
        .messages
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| &m["content"])
        .collect();
    assert_eq!(
        users,
        [&json!("Fix it"), &json!("Then this"), &json!("Again")]
    );
    assert_eq!(resumed.history.messages.len(), 12);
    let headers = resumed
        .conversation
        .rows
        .iter()
        .filter(|r| r["kind"] == "turnHeader")
        .count();
    assert_eq!(headers, 3);
    let sources: Vec<&Value> = resumed
        .conversation
        .rows
        .iter()
        .filter(|r| r["kind"] == "userInput")
        .map(|r| &r["sourceCommandId"])
        .collect();
    assert_eq!(sources, [&json!("c1"), &json!("c2"), &json!("c3")]);
    harness::dump(&next, &conn, &session);
}

#[tokio::test]
async fn guided_and_removed_busy_inputs_are_recorded_like_node() {
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let mut h = harness::start(Some("guide"), Some((started_tx, release_rx))).await;
    let session = h.create("c1", "Fix it").await;
    started.await.unwrap();
    let guide = json!({"commandId": "c2", "clientId": "cli", "sessionId": session,
        "type": "sendText", "issuedAt": 1, "payload": {"text": "Also this", "requestedDelivery": "guide"}});
    assert_eq!(h.command(2, guide).await["status"], "accepted");
    let queued = h.send_text(3, &session, "c3", "Drop me").await;
    let delete = json!({"commandId": "d1", "clientId": "cli", "sessionId": session,
        "type": "deleteQueueItem", "issuedAt": 1, "baseRevision": queued["revisionAtDecision"],
        "payload": {"queueItemId": "queue_c3"}});
    assert_eq!(h.command(4, delete).await["status"], "accepted");
    release.send(()).unwrap();
    let conn = h.settled(&session, 1).await;
    let _first = h.requests.recv().await.unwrap();
    let second = h.requests.recv().await.unwrap();

    let guided = inputs::get(&conn, "queue_c2").unwrap().unwrap();
    assert_eq!(guided.status, "promoted");
    let removed = inputs::get(&conn, "queue_c3").unwrap().unwrap();
    assert_eq!(
        (removed.status.as_str(), removed.status_reason.as_deref()),
        ("cancelled", Some("user_removed"))
    );
    let ack = acks::lookup(&conn, (&session, false), "c3", 0)
        .unwrap()
        .unwrap();
    assert_eq!(ack["reasonCode"], "fault.command.inputCancelled");

    // 引导输入在同一轮内、以插话形态进入模型上下文，冷读取与运行时一致。
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    let live: Vec<Value> = second
        .into_iter()
        .filter(|m| m["role"] != "system")
        .filter(|m| {
            !m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("<system-reminder>"))
        })
        .collect();
    let cold = &resumed.history.messages;
    assert_eq!(cold[..live.len()], live[..], "{cold:#?}");
    assert!(
        live.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Also this")
    );
    let headers = resumed
        .conversation
        .rows
        .iter()
        .filter(|r| r["kind"] == "turnHeader")
        .count();
    assert_eq!(headers, 1, "a guided input stays in its turn");
    harness::dump(&h, &conn, &session);
}

/// `{rowId, entityId}` of the last row of `kind` (real user for inputs).
fn target(rows: &[Value], kind: &str) -> Value {
    let row = rows
        .iter()
        .rev()
        .find(|r| r["kind"] == kind && (kind != "userInput" || r["origin"] == "realUser"))
        .expect("target row");
    json!({"rowId": row["rowId"], "entityId": row["entityId"]})
}

fn user_texts(conn: &rusqlite::Connection, session: &str) -> Vec<Value> {
    let resumed = resume::resume(conn, session, &|_| None, None)
        .unwrap()
        .unwrap();
    resumed
        .history
        .messages
        .iter()
        .filter(|m| m["role"] == "user" && m.get("_zcode_source").is_none())
        .map(|m| m["content"].clone())
        .collect()
}

#[tokio::test]
async fn edit_and_retry_cut_the_node_branch_before_and_after_a_restart() {
    let mut h = harness::start(Some("rewind"), None).await;
    let session = h.create("c1", "Fix it").await;
    h.settled(&session, 1).await;
    let (rows, revision, epoch) = h.rows(2, &session).await;
    let edit = json!({"commandId": "e1", "clientId": "cli", "sessionId": session,
        "type": "editUserQuery", "issuedAt": 1, "baseRevision": revision, "baseLogEpoch": epoch,
        "payload": {"target": target(&rows, "userInput"), "newText": "Fix it better"}});
    let ack = h.command(3, edit).await;
    assert_eq!(ack["result"]["disposition"], "rewind", "{ack}");
    let conn = h.settled(&session, 2).await;
    let row = sessions::get(&conn, &session).unwrap().unwrap();
    let revert = row.revert.unwrap();
    assert_eq!(revert["kind"], "conversation_rewind");
    assert_eq!(revert["branchGeneration"], 1);
    assert_eq!(revert["keptMessageIDs"], json!([]));
    assert_eq!(user_texts(&conn, &session), [json!("Fix it better")]);
    let rerun = inputs::get(&conn, "queue_e1").unwrap().unwrap();
    assert_eq!(rerun.status, "promoted");
    assert_eq!(rerun.payload["sourceCommandType"], "editUserQuery");
    assert_eq!(
        rerun.payload["conversationInputIntent"]["provenance"]["sourceCommandId"],
        "c1"
    );

    let (rows, revision, epoch) = h.rows(4, &session).await;
    let retry = json!({"commandId": "r1", "clientId": "cli", "sessionId": session,
        "type": "retryTurn", "issuedAt": 1, "baseRevision": revision, "baseLogEpoch": epoch,
        "payload": {"target": target(&rows, "assistantText")}});
    assert_eq!(h.command(5, retry).await["status"], "accepted");
    let conn = h.settled(&session, 3).await;
    let revert = sessions::get(&conn, &session)
        .unwrap()
        .unwrap()
        .revert
        .unwrap();
    assert_eq!(revert["branchGeneration"], 2);
    // Node 重试以被重试的 assistant 为请求锚点（保留前缀为空时 messageID 同为该锚点）。
    let anchor_role: String = conn
        .query_row(
            "select json_extract(data, '$.role') from message where id = ?",
            [revert["targetMessageID"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(anchor_role, "assistant");
    assert_eq!(user_texts(&conn, &session), [json!("Fix it better")]);
    let retried = inputs::get(&conn, "queue_r1").unwrap().unwrap();
    // 重试沿用被重试输入的来源（该输入本身来自编辑，来源仍指向 c1）。
    assert_eq!(
        retried.payload["conversationInputIntent"]["provenance"]["sourceCommandId"],
        "c1"
    );

    // 重启后从 Node 记录重建编辑边界，最新输入仍可编辑。
    let mut next = harness::restart(&h).await;
    drop(h);
    let (rows, revision, epoch) = next.rows(6, &session).await;
    let edit = json!({"commandId": "e2", "clientId": "cli", "sessionId": session,
        "type": "editUserQuery", "issuedAt": 1, "baseRevision": revision, "baseLogEpoch": epoch,
        "payload": {"target": target(&rows, "userInput"), "newText": "Third try"}});
    let ack = next.command(7, edit).await;
    assert_eq!(ack["result"]["disposition"], "rewind", "{ack}");
    let conn = next.settled(&session, 4).await;
    let revert = sessions::get(&conn, &session)
        .unwrap()
        .unwrap()
        .revert
        .unwrap();
    assert_eq!(revert["branchGeneration"], 3);
    assert_eq!(user_texts(&conn, &session), [json!("Third try")]);
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.history.messages.len(), 4);
    let headers = resumed
        .conversation
        .rows
        .iter()
        .filter(|r| r["kind"] == "turnHeader")
        .count();
    assert_eq!(headers, 1, "only the active branch is projected");
    harness::dump(&next, &conn, &session);
}

#[tokio::test]
async fn fork_copies_the_stable_segment_into_a_node_child_session() {
    let mut h = harness::start(Some("fork"), None).await;
    let session = h.create("c1", "Fix it").await;
    h.settled(&session, 1).await;
    let (rows, revision, epoch) = h.rows(2, &session).await;
    let fork = json!({"commandId": "f1", "clientId": "cli", "sessionId": session,
        "type": "forkAssistant", "issuedAt": 1, "baseRevision": revision, "baseLogEpoch": epoch,
        "payload": {"target": target(&rows, "assistantText")}});
    let ack = h.command(3, fork).await;
    let child = ack["result"]["sessionId"]
        .as_str()
        .expect("child session")
        .to_owned();
    assert!(child.starts_with("sess_") && child != session, "{ack}");

    let conn = rusqlite::Connection::open(&h.db).unwrap();
    let row = sessions::get(&conn, &child).unwrap().unwrap();
    assert_eq!(row.task_type, "fork");
    assert_eq!(row.parent_id.as_deref(), Some(session.as_str()));
    assert_eq!(row.title, "Fork of Fix it");
    // 父会话记录 child 命令事实：重复的 forkAssistant 从耐久事实得到同一个子会话。
    let fact = acks::lookup(&conn, (&session, false), "f1", 0)
        .unwrap()
        .unwrap();
    assert_eq!(fact["result"]["sessionId"], child.as_str());
    let resumed = resume::resume(&conn, &child, &|_| None, None)
        .unwrap()
        .unwrap();
    let history = &resumed.history.messages;
    assert_eq!(history[0]["content"], "Fix it");
    assert_eq!(
        history.last().unwrap()["_zcode_source"],
        "conversation_fork"
    );
    assert!(
        resumed
            .conversation
            .rows
            .iter()
            .any(|r| r["kind"] == "timelineMarker" || r["kind"] == "sessionFork"),
        "{:#?}",
        resumed.conversation.rows
    );

    // 子会话按冷加载注册，可以继续对话。
    let ack = h.send_text(4, &child, "c2", "Continue here").await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    // 复制的边界、fork 提示的两条消息与新一轮各带一个稳定分段锚点。
    let conn = h.settled(&child, 4).await;
    assert_eq!(
        user_texts(&conn, &child),
        [json!("Fix it"), json!("Continue here")]
    );
    harness::dump(&h, &conn, &child);
}

#[tokio::test]
async fn manual_compaction_is_stored_as_a_node_summary_and_timeline() {
    let mut h = harness::start(Some("compact"), None).await;
    let session = h.create("c1", "Fix it").await;
    h.settled(&session, 1).await;
    let ack = h.send_text(2, &session, "k1", "/compact").await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    let summary_stored = || -> bool {
        conn.query_row(
            "select count(*) from message where session_id = ? and json_extract(data, '$.summary.title') = 'Compact summary'",
            [&session],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    };
    for _ in 0..250 {
        if summary_stored() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(summary_stored(), "compaction summary persisted");
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    // 压缩后的模型上下文：摘要成为 context summary，其后没有被保留的旧消息。
    let summary = resumed.history.summary.clone().expect("context summary");
    assert!(
        summary.starts_with("This session is being continued"),
        "{summary}"
    );
    assert!(summary.contains("Fixed the parser."));
    assert!(
        resumed
            .history
            .messages
            .iter()
            .all(|m| m["content"] != "Fix it"),
        "{:#?}",
        resumed.history.messages
    );
    let marker = resumed
        .conversation
        .rows
        .iter()
        .find(|r| r["kind"] == "timelineMarker" && r["marker"]["type"] == "compact")
        .expect("compact marker row");
    assert_eq!(marker["marker"]["origin"], "manual");
    // 压缩命令的回执从时间线 part 的 sourceCommandId 反查（Node timeline 事实）。
    let ack = acks::lookup(&conn, (&session, false), "k1", 0)
        .unwrap()
        .unwrap();
    assert_eq!(ack["status"], "accepted");

    // 压缩后继续对话，模型上下文从摘要开始。
    assert_eq!(
        h.send_text(3, &session, "c2", "Next").await["status"],
        "accepted"
    );
    let conn = h.settled(&session, 2).await;
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    assert!(resumed.history.summary.is_some());
    assert_eq!(user_texts(&conn, &session), [json!("Next")]);
    harness::dump(&h, &conn, &session);
}

#[tokio::test]
async fn a_subagent_child_is_stored_as_a_node_child_session() {
    let mut h = harness::start(Some("subagent"), None).await;
    let session = h.create("c1", "spawn a child").await;
    let conn = h.settled(&session, 1).await;
    let child: String = conn
        .query_row(
            "select id from session where parent_id = ?",
            [&session],
            |r| r.get(0),
        )
        .unwrap();
    assert!(child.starts_with("sess_subagent_agent_"), "{child}");
    let row = sessions::get(&conn, &child).unwrap().unwrap();
    assert_eq!(row.task_type, "subagent_child");
    assert_eq!(row.title, "Inspect a.ts");
    assert_eq!(row.title_source, "first_input");
    let messages = zcode_cli_state::node::messages::messages(&conn, &child).unwrap();
    // 子会话首轮前落模型切换分隔线，任务提示以 coordinator_input 呈现。
    assert_eq!(messages[0].parts[0]["timelineType"], "model_change");
    assert!(messages[0].parts[0].get("fromModel").is_none());
    let prompt = &messages[1].info;
    assert_eq!(prompt["metadata"]["inputPresentation"], "coordinator_input");
    assert_eq!(prompt["agent"], "zcode-general-purpose");
    let resumed = resume::resume(&conn, &child, &|_| None, None)
        .unwrap()
        .unwrap();
    let history = &resumed.history.messages;
    assert_eq!(history[0]["content"], "Inspect a.ts");
    assert_eq!(history.last().unwrap()["content"], "Done.");
    // 父会话的 Agent 工具结果带 agentId，冷投影据此还原子代理行。
    let parent = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    assert!(
        parent
            .conversation
            .rows
            .iter()
            .any(|r| r["kind"] == "subagent"
                || r["subagent"].is_object()
                || r["toolName"] == "Agent"),
        "{:#?}",
        parent.conversation.rows
    );
    harness::dump(&h, &conn, &session);
    harness::dump_as(&h, &conn, &child, "rust-child.json");
}

#[tokio::test]
async fn a_background_subagent_result_opens_a_node_notification_turn() {
    let mut h = harness::start(Some("notify"), None).await;
    let session = h.create("c1", "spawn a background child").await;
    // 父会话首轮与后台结果轮各有一个稳定边界。
    let conn = h.settled(&session, 2).await;
    let ledger: (String, String, String) = conn
        .query_row(
            "select id, status, payload from session_input where session_id = ? and kind = 'backgroundNotification'",
            [&session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(ledger.0.starts_with("runtime_command_"), "{ledger:?}");
    assert_eq!(ledger.1, "promoted");
    let payload: Value = serde_json::from_str(&ledger.2).unwrap();
    assert_eq!(payload["originMeta"]["backgroundSource"], "subagent");
    assert_eq!(payload["originMeta"]["title"], "Look around");
    let notice = zcode_cli_state::node::messages::messages(&conn, &session)
        .unwrap()
        .into_iter()
        .find(|m| m.info["source"] == "background_task")
        .expect("notification notice");
    assert_eq!(
        notice.info["metadata"]["inputPresentation"],
        "task_notification"
    );
    assert_eq!(notice.info["anchor"]["origin"], "backgroundResult");
    // 模型上下文里的后台结果按 Node 呈现（系统通知前缀 + incoming_message 包装）。
    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    let presented = resumed
        .history
        .messages
        .iter()
        .find(|m| m["_zcode_source"] == "legacy_synthetic")
        .expect("presented notification");
    let content = presented["content"].as_str().unwrap();
    assert!(
        content.starts_with("<system-reminder>\n[SYSTEM NOTIFICATION"),
        "{content}"
    );
    harness::dump(&h, &conn, &session);
}
