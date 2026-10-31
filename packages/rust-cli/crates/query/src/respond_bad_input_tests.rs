//! R5-1：同一响应里一个 tool_use 已经开工，另一个的输入不是合法 JSON（docs/specs/rust-cli/query.md §3
//! 「tool_use 输入不是 JSON」）。回合不失败：坏输入换成标记对象交给执行管线，已开工的兄弟调用的结果、
//! assistant 消息与两次调用的计量都提交，接着发下一次请求。
use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{
    ContentBlock, Message, StopReason, ToolResultContent, Usage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, Metered};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent};
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;
use tokio::sync::Notify;

use crate::run::tests::{parts, request, scope};

const RAW: &str = "{\"file_path\": \"/work/x.txt\"";

fn tool_block(
    index: usize,
    id: &str,
    name: &str,
    json: &str,
) -> Vec<Result<ModelEvent, ModelError>> {
    vec![
        Ok(ModelEvent::BlockStart {
            index,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new(id),
                name: name.to_owned(),
                input: json!({}),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index,
            delta: BlockDelta::InputJson(json.to_owned()),
        }),
        Ok(ModelEvent::BlockStop { index }),
    ]
}

fn two_tools() -> Vec<Result<ModelEvent, ModelError>> {
    let mut events = vec![Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })];
    events.extend(tool_block(0, "toolu_slow", "Slow", "{}"));
    events.extend(tool_block(1, "toolu_read", "Read", RAW));
    events.extend([
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]);
    events
}

fn results(message: &Message) -> Vec<(String, String)> {
    let Message::User(user) = message else {
        return Vec::new();
    };
    user.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                let text = content
                    .iter()
                    .filter_map(|part| match part {
                        ToolResultContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                Some((tool_use_id.as_str().to_owned(), text))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn bad_tool_input_with_a_running_sibling_commits_everything_and_continues() {
    let model = Arc::new(ScriptedModel::new(vec![
        two_tools(),
        text_response(&["after tools"]),
    ]));
    // 兄弟调用开工后一直等着：坏输入的块结束时它还在跑（Bash 写副作用的那种）。
    let gate = Arc::new(Notify::new());
    let slow = FakeTool::new("Slow", false, "slow done").gated(gate.clone());
    let started = slow.started.clone();
    let release = tokio::spawn(async move {
        started.notified().await;
        gate.notify_one();
    });
    let read: Arc<dyn Tool> = Arc::new(FakeTool::new("Read", true, "read ran"));
    let slow: Arc<dyn Tool> = Arc::new(slow);
    let request = request("go");
    let (scope, sink) = scope(
        parts(model.clone(), vec![slow, read]),
        &request,
        Abort::new(),
    );
    let stop = crate::run::run(scope, request).await;
    release.await.unwrap();
    // 修复前：`assembler.push(event)?` 让整个回合以 Protocol 错误结束，库里只有意图。
    stop.unwrap();

    let messages = sink.messages();
    let assistant = messages
        .iter()
        .find_map(|message| match message {
            Message::Assistant(assistant) if assistant.stop_reason == Some(StopReason::ToolUse) => {
                Some(assistant)
            }
            _ => None,
        })
        .expect("the assistant message with both tool_use blocks is committed");
    let inputs: Vec<_> = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { input, .. } => Some(input.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        inputs,
        vec![
            json!({}),
            json!({"__unparsedToolInput": {"raw": RAW, "len": RAW.len()}}),
        ]
    );
    // 两个调用都有结果（意图都结清），按加入顺序。
    let committed: Vec<_> = messages
        .iter()
        .filter(|message| {
            matches!(message, Message::User(user) if matches!(user.origin, UserOrigin::ToolResult { .. }))
        })
        .flat_map(results)
        .collect();
    assert_eq!(
        committed,
        vec![
            ("toolu_slow".to_owned(), "slow done".to_owned()),
            ("toolu_read".to_owned(), "read ran".to_owned()),
        ]
    );
    // 回合继续：第二次请求带着两个结果，最后的回复也提交了。
    assert_eq!(model.requests().len(), 2);
    assert!(matches!(
        messages.last(),
        Some(Message::Assistant(last)) if last.content == vec![ContentBlock::Text { text: "after tools".to_owned() }]
    ));
    // 计量：两次模型调用都完成，两次工具调用都有。
    let metered = sink.metered();
    let models: Vec<_> = metered
        .iter()
        .filter_map(|(_, metered)| match metered {
            Metered::Model(call) => Some((call.status, call.tool_calls)),
            Metered::Tool(_) => None,
        })
        .collect();
    assert_eq!(
        models,
        vec![(CallStatus::Completed, 2), (CallStatus::Completed, 0)]
    );
    let tools = metered
        .iter()
        .filter(|(_, metered)| matches!(metered, Metered::Tool(_)))
        .count();
    assert_eq!(tools, 2);
}

/// 流里出现与块不符的增量（协议错误）时，已开工的调用照样等完、提交（不再用 `?` 提前返回把它们丢下）。
#[tokio::test]
async fn a_protocol_error_after_a_tool_started_waits_for_it_and_commits() {
    let mut first = vec![Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })];
    first.extend(tool_block(0, "toolu_slow", "Slow", "{}"));
    first.push(Ok(ModelEvent::BlockDelta {
        index: 7,
        delta: BlockDelta::Text("stray".to_owned()),
    }));
    let model = Arc::new(ScriptedModel::new(vec![first, text_response(&["after"])]));
    let slow: Arc<dyn Tool> = Arc::new(FakeTool::new("Slow", false, "slow done"));
    let request = request("go");
    let (scope, sink) = scope(parts(model.clone(), vec![slow]), &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let committed: Vec<_> = sink.messages().iter().flat_map(results).collect();
    assert_eq!(
        committed,
        vec![("toolu_slow".to_owned(), "slow done".to_owned())]
    );
    let statuses: Vec<_> = sink
        .metered()
        .iter()
        .filter_map(|(_, metered)| match metered {
            Metered::Model(call) => Some(call.status),
            Metered::Tool(_) => None,
        })
        .collect();
    assert_eq!(statuses, vec![CallStatus::Error, CallStatus::Completed]);
}
