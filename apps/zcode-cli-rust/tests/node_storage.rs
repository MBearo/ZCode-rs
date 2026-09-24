//! The engine over the Node session database (spec rust-m11-node-storage
//! §5.2 acceptance): turns are written as Node records, and Node's cold
//! readers give back the live model context and rows, across a restart.
#[path = "node_storage/harness.rs"]
mod harness;

use serde_json::{Value, json};
use tokio::sync::oneshot;
use zcode_cli_state::node::{acks, cold, inputs, resume, sessions};

#[tokio::test]
async fn a_tool_turn_is_stored_as_node_records_and_reads_back_as_the_live_context() {
    let mut h = harness::start(false, None).await;
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
    let mut h = harness::start(true, Some((started_tx, release_rx))).await;
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

    if std::env::var_os("ZCODE_CLI_RUST_NODE_DUMP").is_some() {
        let active = cold::active(&conn, &session).unwrap();
        let history: Vec<Value> = zcode_cli_rust::domain::node_history::hydrate(&active, &|_| None)
            .entries
            .iter()
            .map(|e| e.to_node())
            .collect();
        let read = json!({"sessionId": session, "history": history,
            "rows": resumed.conversation.rows, "state": resumed.conversation.state});
        std::fs::write(next.root.join("rust.json"), read.to_string()).unwrap();
    }
}
