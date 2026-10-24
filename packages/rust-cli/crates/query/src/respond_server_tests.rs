//! 工作线 F90（docs/specs/rust-cli/query.md §1.9、tools-more.md §5.1）：主请求不带服务端工具，端点仍在主回复里返回
//! `server_tool_use` 与非标准的 `tool_result`（模型适配器解成 `ToolResult`），另有被适配器跳过的块（序号空着）。
//! 回合不失败：回复去掉服务端块后提交（文字与 tool_use），工具照常执行，下一次请求里也没有服务端块。
use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, Message, StopReason, ToolResultContent, Usage};
use rust_cli_kernel_api::metering::{CallStatus, Metered};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent};
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;

use crate::run::tests::{parts, request, scope};

fn whole(index: usize, block: ContentBlock) -> Vec<Result<ModelEvent, ModelError>> {
    vec![
        Ok(ModelEvent::BlockStart { index, block }),
        Ok(ModelEvent::BlockStop { index }),
    ]
}

/// 文字、服务端调用与它的结果、跳过的 index 3、乱序的 index 5 先于 index 4、最后一个 tool_use。
fn reply_with_server_blocks() -> Vec<Result<ModelEvent, ModelError>> {
    let mut events = vec![Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })];
    events.extend([
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Text("Looking.".to_owned()),
        }),
        Ok(ModelEvent::BlockStop { index: 0 }),
    ]);
    events.extend(whole(
        1,
        ContentBlock::ServerToolUse {
            id: "srvtoolu_g".to_owned(),
            name: "tool_search".to_owned(),
            input: json!({}),
        },
    ));
    events.extend(whole(
        2,
        ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("srvtoolu_g"),
            content: vec![ToolResultContent::Text {
                text: "server side".to_owned(),
            }],
            is_error: false,
        },
    ));
    events.extend([
        Ok(ModelEvent::BlockStart {
            index: 5,
            block: ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_read"),
                name: "Read".to_owned(),
                input: json!({}),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 5,
            delta: BlockDelta::InputJson("{}".to_owned()),
        }),
        Ok(ModelEvent::BlockStop { index: 5 }),
    ]);
    events.extend(whole(
        4,
        ContentBlock::Text {
            text: "Then read.".to_owned(),
        },
    ));
    events.extend([
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]);
    events
}

fn is_server_block(block: &ContentBlock) -> bool {
    matches!(
        block,
        ContentBlock::ServerToolUse { .. }
            | ContentBlock::WebSearchToolResult { .. }
            | ContentBlock::ToolResult { .. }
    )
}

#[tokio::test]
async fn server_blocks_in_a_main_reply_do_not_fail_the_turn() {
    let model = Arc::new(ScriptedModel::new(vec![
        reply_with_server_blocks(),
        text_response(&["done"]),
    ]));
    let read: Arc<dyn Tool> = Arc::new(FakeTool::new("Read", true, "read ran"));
    let request = request("go");
    let (scope, sink) = scope(parts(model.clone(), vec![read]), &request, Abort::new());
    // 修 bug 前：跳过的 index 3 是空槽，流结束时报 `stream ended with an unfinished block`，半截回复连同服务端块提交
    // （node-store 拒收，回合失败）。
    crate::run::run(scope, request).await.unwrap();

    let messages = sink.messages();
    let first = messages
        .iter()
        .find_map(|message| match message {
            Message::Assistant(assistant) if assistant.stop_reason == Some(StopReason::ToolUse) => {
                Some(assistant)
            }
            _ => None,
        })
        .expect("the reply with the tool_use is committed");
    assert_eq!(
        first.content,
        [
            ContentBlock::Text {
                text: "Looking.".to_owned()
            },
            ContentBlock::Text {
                text: "Then read.".to_owned()
            },
            ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_read"),
                name: "Read".to_owned(),
                input: json!({}),
            },
        ]
    );
    // 下一次请求：assistant 里没有服务端块，Read 的结果照常在。
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let second = &requests[1];
    for message in &second.messages {
        if message.role == rust_cli_kernel_api::model::ApiRole::Assistant {
            assert!(
                !message
                    .content
                    .iter()
                    .any(|item| is_server_block(&item.block)),
                "{message:?}"
            );
        }
    }
    let text = serde_json::to_string(&second.messages).unwrap();
    assert!(
        text.contains("read ran") && !text.contains("srvtoolu_g"),
        "{text}"
    );
    assert!(matches!(
        messages.last(),
        Some(Message::Assistant(last)) if last.content == vec![ContentBlock::Text { text: "done".to_owned() }]
    ));
    // 计量：第一次模型调用是完成的（不是协议错误），一个 tool_use。
    let models: Vec<_> = sink
        .metered()
        .iter()
        .filter_map(|(_, metered)| match metered {
            Metered::Model(call) => Some((call.status, call.tool_calls)),
            Metered::Tool(_) => None,
        })
        .collect();
    assert_eq!(
        models,
        vec![(CallStatus::Completed, 1), (CallStatus::Completed, 0)]
    );
}

/// 回复只有服务端块（去掉后一个块都不剩）：按没有内容的回复处理，不提交 assistant（同 Node 删掉空的 assistant），
/// 计量单独提交；之后照没有文字的回复的规则提示一次再请求（query.md §3 ③）。
#[tokio::test]
async fn a_reply_with_only_server_blocks_commits_no_assistant() {
    let mut only = vec![Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })];
    only.extend(whole(
        0,
        ContentBlock::ServerToolUse {
            id: "srvtoolu_g".to_owned(),
            name: "tool_search".to_owned(),
            input: json!({}),
        },
    ));
    only.extend([
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]);
    let model = Arc::new(ScriptedModel::new(vec![only, text_response(&["done"])]));
    let request = request("go");
    let (scope, sink) = scope(parts(model.clone(), Vec::new()), &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let assistants: Vec<Vec<ContentBlock>> = sink
        .messages()
        .into_iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant.content),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistants,
        [vec![ContentBlock::Text {
            text: "done".to_owned()
        }]]
    );
    assert!(
        sink.metered()
            .iter()
            .any(|(message, metered)| message.is_none() && matches!(metered, Metered::Model(_)))
    );
}
