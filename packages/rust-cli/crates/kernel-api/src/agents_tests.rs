//! 子代理与任务清单端口的 DTO：序列化往返、任务 JSON 的字段名。

use futures_util::{FutureExt, StreamExt};
use serde_json::json;

use super::{
    AgentNotice, AgentSignal, AgentSignals, NoAgentSignals, TaskList, TaskRecord, TaskStatus,
};
use crate::ids::{AgentId, SessionId};

fn task() -> TaskRecord {
    TaskRecord {
        id: "2".to_owned(),
        subject: "Fix parser".to_owned(),
        description: "Make the tests pass".to_owned(),
        active_form: Some("Fixing parser".to_owned()),
        owner: None,
        status: TaskStatus::InProgress,
        blocks: Vec::new(),
        blocked_by: vec!["1".to_owned()],
        metadata: None,
    }
}

#[test]
fn task_list_uses_the_task_json_field_names() {
    let list = TaskList {
        high_water_mark: 2,
        tasks: vec![task()],
    };
    let value = serde_json::to_value(&list).unwrap();
    assert_eq!(
        value,
        json!({
            "highWaterMark": 2,
            "tasks": [{
                "id": "2",
                "subject": "Fix parser",
                "description": "Make the tests pass",
                "activeForm": "Fixing parser",
                "status": "in_progress",
                "blocks": [],
                "blockedBy": ["1"]
            }]
        })
    );
    let back: TaskList = serde_json::from_value(value).unwrap();
    assert_eq!(back, list);
    // 旧文件或手写文件缺字段时按空值读。
    let empty: TaskList = serde_json::from_value(json!({})).unwrap();
    assert_eq!(empty, TaskList::default());
}

#[test]
fn signals_round_trip() {
    for signal in [
        AgentSignal::Notice(AgentNotice {
            seq: 3,
            agent: AgentId::new("agent_1"),
            status: "completed".to_owned(),
            text: "<system-reminder>…</system-reminder>".to_owned(),
        }),
        AgentSignal::TasksChanged {
            tasks: json!([{"id": "1"}]),
        },
    ] {
        let text = serde_json::to_string(&signal).unwrap();
        let back: AgentSignal = serde_json::from_str(&text).unwrap();
        assert_eq!(back, signal);
    }
}

#[test]
fn the_empty_implementation_has_no_signals_and_no_background_agents() {
    let signals = NoAgentSignals;
    let session = SessionId::new("sess_1");
    let background = signals.background(&session).now_or_never();
    assert_eq!(background, Some(Vec::new()));
    let mut stream = signals.signals(&session);
    assert_eq!(
        stream.next().now_or_never(),
        None,
        "the stream never yields"
    );
    assert_eq!(TaskStatus::Completed.as_str(), "completed");
}

#[test]
fn agent_output_with_a_child_links_the_tool_call() {
    use crate::ids::ToolUseId;
    use crate::store::Fact;
    use crate::tool::ToolOutput;
    let output = ToolOutput {
        content: Vec::new(),
        is_error: false,
        display: json!({"agent": {"status": "completed", "agentId": "agent_1",
            "childSessionId": "sess_subagent_agent_1", "agentType": "Explore"}}),
    };
    let tool_use = ToolUseId::new("toolu_1");
    assert_eq!(
        super::subagent_link(&tool_use, &output),
        Some(Fact::SubagentLinked {
            tool_use: tool_use.clone(),
            agent: AgentId::new("agent_1"),
            child: SessionId::new("sess_subagent_agent_1"),
            agent_type: "Explore".to_owned(),
        })
    );
    // 别的工具、没有子会话的输出不关联。
    let other = ToolOutput {
        display: json!({"agent": {"agentId": "agent_1"}}),
        ..output
    };
    assert_eq!(super::subagent_link(&tool_use, &other), None);
}

/// 任务提醒读清单的端口是对象安全的，可以装进 `Arc<dyn …>`。
#[test]
fn the_task_reader_is_object_safe() {
    struct Fixed;
    #[async_trait::async_trait]
    impl super::TaskReader for Fixed {
        async fn tasks(&self, _session: &SessionId) -> Vec<TaskRecord> {
            vec![task()]
        }
    }
    let reader: std::sync::Arc<dyn super::TaskReader> = std::sync::Arc::new(Fixed);
    let tasks = reader
        .tasks(&SessionId::new("s"))
        .now_or_never()
        .expect("ready");
    assert_eq!(tasks[0].subject, "Fix parser");
}
