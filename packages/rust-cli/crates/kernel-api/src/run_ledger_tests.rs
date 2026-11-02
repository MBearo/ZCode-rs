use serde_json::{Value, json};

use super::*;
use crate::ids::{AgentId, ArtifactId, SessionId};
use crate::message::{ToolResultContent, UserMessage};
use crate::metering::{CallStatus, Metered, ToolCall};
use crate::store::ArtifactPurpose;
use crate::testing::SequentialIds;

fn output(is_error: bool, display: Value) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text {
            text: "x".to_owned(),
        }],
        is_error,
        display,
    }
}

fn artifact() -> Value {
    serde_json::to_value(ArtifactRef {
        session: SessionId::new("s"),
        id: ArtifactId::new("checkpoint-tool-result-1.json"),
        purpose: ArtifactPurpose::Checkpoint,
        media_type: "application/json".to_owned(),
        size: 10,
    })
    .unwrap()
}

fn result(tool_uses: &[&str], is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("r1"),
        content: tool_uses
            .iter()
            .map(|tool_use| ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(*tool_use),
                content: Vec::new(),
                is_error,
            })
            .collect(),
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a1"),
        },
    })
}

fn human(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn kinds(facts: &[Fact]) -> Vec<&'static str> {
    facts
        .iter()
        .map(|fact| match fact {
            Fact::MessageAppended { .. } => "message",
            Fact::CheckpointCreated { .. } => "checkpoint",
            Fact::SubagentLinked { .. } => "link",
            Fact::ToolCalled { .. } => "usage",
            _ => "other",
        })
        .collect()
}

fn agent_display() -> Value {
    json!({"agent": {"agentId": "agent_2", "childSessionId": "sess_subagent_agent_2", "agentType": "Explore"}})
}

#[test]
fn a_noted_checkpoint_settles_with_its_tool_result_once() {
    let mut ledger = RunLedger::default();
    let ids = SequentialIds::default();
    let display = json!({"checkpoint": {"artifact": artifact(), "path": "/w/a.txt", "additions": 2, "deletions": 1}});
    ledger.tool_finished(&ToolUseId::new("t1"), &output(false, display));
    ledger.tool_finished(&ToolUseId::new("t2"), &output(false, Value::Null));
    let prompt = MessageId::new("u1");
    let none = ledger.settle(result(&["t2"], false), Some(&prompt), Vec::new(), &ids);
    assert!(none.checkpoints.is_empty());
    assert_eq!(kinds(&none.facts), ["message"]);
    let settled = ledger.settle(result(&["t1"], false), Some(&prompt), Vec::new(), &ids);
    assert_eq!(settled.checkpoints.len(), 1);
    let (checkpoint, change) = &settled.checkpoints[0];
    assert_eq!(checkpoint.prompt, MessageId::new("u1"));
    assert_eq!(checkpoint.assistant, MessageId::new("a1"));
    assert_eq!(checkpoint.tool_use, ToolUseId::new("t1"));
    assert!(checkpoint.id.starts_with("checkpoint_"));
    assert_eq!(
        change,
        &FileChange {
            path: "/w/a.txt".to_owned(),
            additions: 2,
            deletions: 1,
            machine: Default::default(),
        }
    );
    // 已结算的不再给出。
    let again = ledger.settle(result(&["t1"], false), Some(&prompt), Vec::new(), &ids);
    assert!(again.checkpoints.is_empty());
}

/// 结果被中断文本替换（is_error）时 checkpoint 仍然落库：写入已经发生（session-lifecycle.md §4）。
#[test]
fn a_replaced_error_result_still_settles_the_write() {
    let mut ledger = RunLedger::default();
    let ids = SequentialIds::default();
    let display = json!({"checkpoint": {"artifact": artifact(), "path": "/w/a.txt"}});
    ledger.tool_finished(&ToolUseId::new("t1"), &output(true, display));
    let settled = ledger.settle(
        result(&["t1"], true),
        Some(&MessageId::new("u1")),
        Vec::new(),
        &ids,
    );
    assert_eq!(settled.checkpoints.len(), 1);
}

/// 同一批的顺序：消息 → checkpoint → 子代理关联 → 计量（usage.md §2.5）。
#[test]
fn one_message_settles_its_checkpoint_link_and_usage_in_order() {
    let mut ledger = RunLedger::default();
    let ids = SequentialIds::default();
    let display = json!({"checkpoint": {"artifact": artifact(), "path": "/w/a.txt"}});
    ledger.tool_finished(&ToolUseId::new("t_write"), &output(false, display));
    ledger.tool_finished(&ToolUseId::new("t_agent"), &output(false, agent_display()));
    let usage = Fact::from(Metered::Tool(ToolCall {
        tool_use: ToolUseId::new("t_agent"),
        name: "Agent".to_owned(),
        started_at_ms: 1,
        completed_at_ms: 2,
        status: CallStatus::Completed,
        output_bytes: 2,
    }));
    let settled = ledger.settle(
        result(&["t_agent", "t_write"], false),
        Some(&MessageId::new("u1")),
        vec![usage],
        &ids,
    );
    assert_eq!(
        kinds(&settled.facts),
        ["message", "checkpoint", "link", "usage"]
    );
    assert_eq!(
        settled.facts[2],
        Fact::SubagentLinked {
            tool_use: ToolUseId::new("t_agent"),
            agent: AgentId::new("agent_2"),
            child: SessionId::new("sess_subagent_agent_2"),
            agent_type: "Explore".to_owned(),
        }
    );
}

/// 压缩的运行没有开头的输入：checkpoint 留着，等有输入的运行结算；子代理关联照常。
#[test]
fn without_a_prompt_the_checkpoint_waits_and_the_link_settles() {
    let mut ledger = RunLedger::default();
    let ids = SequentialIds::default();
    let display = json!({"checkpoint": {"artifact": artifact(), "path": "/w/a.txt"}, "agent": agent_display()["agent"]});
    ledger.tool_finished(&ToolUseId::new("t1"), &output(false, display));
    let settled = ledger.settle(result(&["t1"], false), None, Vec::new(), &ids);
    assert_eq!(kinds(&settled.facts), ["message", "link"]);
    let later = ledger.settle(
        result(&["t1"], false),
        Some(&MessageId::new("u1")),
        Vec::new(),
        &ids,
    );
    assert_eq!(kinds(&later.facts), ["message", "checkpoint"]);
}

/// 当前轮只在提交成功之后才换（node-store.md §4）：批里有新的人类输入时这一批锚在新轮上。
#[test]
fn the_turn_moves_only_when_the_batch_is_committed() {
    let mut ledger = RunLedger::new(None);
    let ids = SequentialIds::default();
    let batch = ledger.batch(
        Some(CommandId::new("c1")),
        vec![Fact::MessageAppended {
            message: human("m1"),
        }],
        &ids,
    );
    let turn = batch.turn.clone().unwrap();
    assert_eq!(turn.prompt, MessageId::new("m1"));
    assert_eq!(batch.command, Some(CommandId::new("c1")));
    assert_eq!(ledger.turn(), None, "not committed yet");
    ledger.committed(batch.turn);
    assert_eq!(ledger.turn(), Some(&turn));
    // 没有人类输入的一批沿用当前轮。
    let next = ledger.batch(
        None,
        vec![Fact::TitleChanged {
            title: "t".to_owned(),
        }],
        &ids,
    );
    assert_eq!(next.turn.as_ref(), Some(&turn));
}
