use std::sync::Arc;
use std::time::Duration;

use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::message::{ContentBlock, Message, StopReason, Usage};
use rust_cli_kernel_api::model::{ModelError, ModelEvent};
use rust_cli_kernel_api::orchestration::QueryStop;
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::Tool;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::run::tests::{parts, request, scope};

fn start() -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })
}

/// 一次响应里按顺序给出多个 tool_use。
pub(crate) fn tool_uses(calls: &[(&str, &str, Value)]) -> Vec<Result<ModelEvent, ModelError>> {
    let mut events = vec![start()];
    for (index, (id, name, input)) in calls.iter().enumerate() {
        events.extend([
            Ok(ModelEvent::BlockStart {
                index,
                block: ContentBlock::ToolUse {
                    id: rust_cli_kernel_api::ids::ToolUseId::new(*id),
                    name: (*name).to_owned(),
                    input: json!({}),
                },
            }),
            Ok(ModelEvent::BlockDelta {
                index,
                delta: rust_cli_kernel_api::model::BlockDelta::InputJson(input.to_string()),
            }),
            Ok(ModelEvent::BlockStop { index }),
        ]);
    }
    events.extend([
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]);
    events
}

/// 提交的工具结果，按提交顺序：(tool_use_id, 文本, is_error)。
fn results(messages: &[Message]) -> Vec<(String, String, bool)> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => user.content.iter().find_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some((
                    tool_use_id.as_str().to_owned(),
                    format!("{content:?}"),
                    *is_error,
                )),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn safe_tools_run_together_and_unsafe_tools_run_alone_in_order() {
    let gate = Arc::new(Notify::new());
    let read_a = Arc::new(FakeTool::new("ReadA", true, "a").gated(gate.clone()));
    let read_b = Arc::new(FakeTool::new("ReadB", true, "b"));
    let write = Arc::new(FakeTool::new("Write", false, "w"));
    let model = Arc::new(ScriptedModel::new(vec![
        tool_uses(&[
            ("t1", "ReadA", json!({})),
            ("t2", "ReadB", json!({})),
            ("t3", "Write", json!({})),
        ]),
        text_response(&["ok"]),
    ]));
    let tools: Vec<Arc<dyn Tool>> = vec![read_a.clone(), read_b.clone(), write.clone()];
    let request = request("go");
    let (scope, sink) = scope(parts(model, tools), &request, Abort::new());
    let running = tokio::spawn(crate::run::run(scope, request));
    // 两个并发安全的调用同时开始；不安全的 Write 要等它们都结束。
    tokio::time::timeout(Duration::from_secs(5), read_a.started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), read_b.started.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), write.started.notified())
            .await
            .is_err()
    );
    gate.notify_one();
    assert_eq!(running.await.unwrap().unwrap(), QueryStop::Completed);
    let results = results(&sink.messages());
    // 并发安全的调用按完成先后：b 先于 a；Write 最后。
    let order: Vec<_> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(order, vec!["t2", "t1", "t3"]);
}

/// Bash 出错不取消排在后面的调用（执行器的中止原因只有中断、权限停止与结束对话，没有 sibling error：
/// `exit 3` 之后的 `echo second` 照常执行）。缺陷修复（R4 F3）：
/// 原来按旧的执行器取消兄弟调用，结果换成 “Cancelled: parallel tool call … errored”。
#[tokio::test]
async fn a_failing_bash_leaves_the_later_calls_running() {
    let bash = Arc::new(FakeTool::new("Bash", false, "Exit code 3").failing());
    let echo = Arc::new(FakeTool::new("Grep", false, "second"));
    let model = Arc::new(ScriptedModel::new(vec![
        tool_uses(&[
            ("t1", "Bash", json!({"command": "exit 3"})),
            ("t2", "Grep", json!({"pattern": "second"})),
        ]),
        text_response(&["ok"]),
    ]));
    let request = request("go");
    let (scope, sink) = scope(parts(model, vec![bash, echo]), &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::Completed
    );
    let results = results(&sink.messages());
    let order: Vec<_> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(order, vec!["t1", "t2"]);
    let second = results.iter().find(|(id, _, _)| id == "t2").unwrap();
    assert!(second.1.contains("second"), "{second:?}");
}

#[tokio::test]
async fn a_user_interrupt_rejects_running_tools_and_appends_the_interruption() {
    let never = Arc::new(Notify::new());
    let slow = Arc::new(FakeTool::new("Bash", false, "out").gated(never));
    let model = Arc::new(ScriptedModel::new(vec![tool_uses(&[(
        "t1",
        "Bash",
        json!({"command": "sleep 100"}),
    )])]));
    let abort = Abort::new();
    let request = request("go");
    let (scope, sink) = scope(parts(model, vec![slow.clone()]), &request, abort.clone());
    let running = tokio::spawn(crate::run::run(scope, request));
    tokio::time::timeout(Duration::from_secs(5), slow.started.notified())
        .await
        .unwrap();
    abort.abort(AbortReason::User);
    assert_eq!(running.await.unwrap().unwrap(), QueryStop::Aborted);
    let messages = sink.messages();
    let results = results(&messages);
    assert!(
        results[0].1.contains("The user doesn't want to proceed"),
        "{results:?}"
    );
    let Some(Message::User(last)) = messages.last() else {
        panic!("{messages:?}");
    };
    assert_eq!(
        last.content,
        vec![ContentBlock::Text {
            text: "[Request interrupted by user for tool use]".to_owned()
        }]
    );
}
