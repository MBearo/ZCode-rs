use super::*;
use base64::Engine as _;

fn record(info: Value, parts: Vec<Value>) -> Record {
    Record { info, parts }
}

fn agent_part(call: &str, output: &str, input: Value) -> Value {
    json!({"id": format!("prt_{call}"), "type": "tool", "callID": call, "tool": "Agent",
        "state": {"status": "completed", "input": input, "output": output,
            "time": {"start": 10, "end": 20}}})
}

fn child(updated: u64, text: &str) -> StoredChild {
    StoredChild {
        task_type: "subagent_child".into(),
        updated,
        messages: vec![record(
            json!({"id": "a", "role": "assistant", "time": {"created": 1, "completed": 15}}),
            vec![json!({"type": "text", "text": text})],
        )],
    }
}

#[test]
fn stored_agent_calls_list_like_node() {
    let messages = vec![record(
        json!({"id": "m1", "role": "assistant"}),
        vec![
            agent_part(
                "call-a",
                "child-one result\nagentId: one (use SendMessage with to: 'one' to continue this agent)",
                json!({"description": "read one", "subagent_type": "Explore"}),
            ),
            agent_part(
                "call-b",
                "{\"agentId\":\"two\",\"status\":\"cancelled\"}",
                json!({"prompt": "p"}),
            ),
        ],
    )];
    let live = Live::default();
    assert_eq!(
        child_session_ids(&messages, None, &live),
        ["sess_subagent_one", "sess_subagent_two"]
    );
    let children = vec![
        ("sess_subagent_one".to_owned(), child(30, "done one")),
        ("sess_subagent_two".to_owned(), child(40, "done two")),
    ];
    let (running, ended) = project(&messages, None, &children, &live);
    assert!(running.is_empty());
    assert_eq!(
        ended[0],
        json!({"childSessionId": "sess_subagent_two", "agentId": "two", "toolCallId": "call-b",
            "subagentType": "subagent", "title": "p", "startedAt": 10, "status": "cancelled",
            "summary": "done two", "endedAt": 20})
    );
    assert_eq!(ended[1]["title"], "read one");
    assert_eq!(ended[1]["subagentType"], "Explore");
    assert_eq!(ended[1]["status"], "success");
    // 运行中的父会话：未结束的调用按 running 列出。
    let mut open = messages.clone();
    open[0].parts[0]["state"]["status"] = "running".into();
    open[0].parts[0]["metadata"] = json!({"agentId": "one"});
    let live = Live {
        parent: true,
        ..Live::default()
    };
    let (running, ended) = project(&open, None, &children, &live);
    assert_eq!((running.len(), ended.len()), (1, 1));
    assert_eq!(running[0]["status"], "running");
}

#[test]
fn ended_pages_use_nodes_cursor() {
    let ended: Vec<Value> = (0..3)
        .map(|i| json!({"childSessionId": format!("c{i}"), "endedAt": 100 - i}))
        .collect();
    let (first, next) = paginate(&ended, None, 2);
    assert_eq!(first.len(), 2);
    let next = next.unwrap();
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&next)
            .unwrap(),
        br#"{"childSessionId":"c1","endedAt":99}"#
    );
    let (rest, after) = paginate(&ended, Some(&next), 2);
    assert_eq!(
        (rest.len(), rest[0]["childSessionId"].as_str(), after),
        (1, Some("c2"), None)
    );
    assert_eq!(paginate(&ended, Some("bad"), 2).0.len(), 2);
}
