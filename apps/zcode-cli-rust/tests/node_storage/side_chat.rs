//! A selection side chat over the Node database (Node
//! `createSelectionSideConversation`): the fork bundle's child with the
//! parent's history as model-only records and the boundary reminder, then the
//! child's own first input.
use super::harness;
use serde_json::json;
use zcode_cli_state::node::{acks, resume, sessions};

#[tokio::test]
async fn a_selection_side_chat_is_a_node_child_with_hidden_parent_history() {
    let mut h = harness::start(Some("side-chat"), None).await;
    let session = h.create("c1", "Fix it").await;
    h.settled(&session, 1).await;
    let ack = h
        .command(
            2,
            json!({"commandId": "s1", "clientId": "cli", "sessionId": session,
                "type": "createSelectionSideSession", "issuedAt": 1,
                "payload": {"firstInput": {"text": "Why this way?"}}}),
        )
        .await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    let child = ack["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(ack["result"]["input"]["inputId"], "s1");
    // 复制的父会话边界、副屏边界提醒与副屏自己的一轮各带一个稳定分段锚点。
    let conn = h.settled(&child, 3).await;
    let row = sessions::get(&conn, &child).unwrap().unwrap();
    assert_eq!(
        (
            row.task_type.as_str(),
            row.title.as_str(),
            row.title_source.as_str()
        ),
        ("selection_side_chat", "Selection side chat", "generated")
    );
    assert_eq!(row.parent_id.as_deref(), Some(session.as_str()));
    let fact = acks::lookup(&conn, (&session, false), "s1", 0)
        .unwrap()
        .unwrap();
    assert_eq!(
        fact["result"],
        json!({"type": "createSelectionSideSession", "sessionId": child})
    );
    let resumed = resume::resume(&conn, &child, &|_| None, None)
        .unwrap()
        .unwrap();
    let history = &resumed.history.messages;
    assert_eq!(history[0]["content"], "Fix it");
    assert!(
        history
            .iter()
            .any(|m| m["_zcode_source"] == "selection_side_chat"),
        "{history:#?}"
    );
    // 继承的历史对界面隐藏：只剩副屏自己的输入与回答。
    let inputs: Vec<_> = resumed
        .conversation
        .rows
        .iter()
        .filter(|r| r["kind"] == "userInput")
        .map(|r| r["text"].clone())
        .collect();
    assert_eq!(inputs, [json!("Why this way?")]);
    harness::dump(&h, &conn, &child);
}
