//! 恢复分支在主循环里的效果：提交了什么、下一次请求里有什么（docs/specs/rust-cli/query.md）。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::message::{ContentBlock, Message, StopReason, UserOrigin};
use rust_cli_kernel_api::model::{
    ApiRole, BlockDelta, ModelError, ModelEvent, ModelRequest, QuerySource,
};
use rust_cli_kernel_api::orchestration::QueryStop;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use serde_json::json;

use super::tests::{parts, request, scope};

type Response = Vec<Result<ModelEvent, ModelError>>;

fn stopped(mut events: Response, stop: StopReason) -> Response {
    for event in &mut events {
        if let Ok(ModelEvent::MessageDelta { stop_reason, .. }) = event {
            *stop_reason = Some(stop);
        }
    }
    events
}

/// 只有 thinking 块的响应。
fn thinking_only() -> Response {
    let mut events = text_response(&[]);
    for event in &mut events {
        match event {
            Ok(ModelEvent::BlockStart { block, .. }) => {
                *block = ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: None,
                };
            }
            Ok(ModelEvent::BlockStop { index }) => {
                let index = *index;
                *event = Ok(ModelEvent::BlockDelta {
                    index,
                    delta: BlockDelta::Thinking("hmm".to_owned()),
                });
            }
            _ => {}
        }
    }
    let stop = events
        .iter()
        .position(|event| matches!(event, Ok(ModelEvent::MessageDelta { .. })))
        .unwrap();
    events.insert(stop, Ok(ModelEvent::BlockStop { index: 0 }));
    events
}

async fn run(
    responses: Vec<Response>,
    source: QuerySource,
) -> (QueryStop, Vec<Message>, Vec<ModelRequest>) {
    let model = Arc::new(ScriptedModel::new(responses));
    let mut request = request("go");
    request.source = source;
    let (scope, sink) = scope(parts(model.clone(), Vec::new()), &request, Abort::new());
    let stop = super::run(scope, request).await.unwrap();
    (stop, sink.messages(), model.requests())
}

fn last_user_text(request: &ModelRequest) -> String {
    let message = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ApiRole::User)
        .unwrap();
    match &message.content.last().unwrap().block {
        ContentBlock::Text { text } => text.clone(),
        other => format!("{other:?}"),
    }
}

fn meta_sources(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => match &user.origin {
                UserOrigin::Meta { source } => Some(source.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn roles(request: &ModelRequest) -> Vec<ApiRole> {
    request
        .messages
        .iter()
        .map(|message| message.role)
        .collect()
}

#[tokio::test]
async fn a_cut_reply_is_resumed_and_then_completes() {
    let (stop, messages, requests) = run(
        vec![
            stopped(text_response(&["half"]), StopReason::MaxTokens),
            text_response(&["rest"]),
        ],
        QuerySource::MainThread,
    )
    .await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests.len(), 2);
    assert_eq!(meta_sources(&messages), ["max_output_tokens_recovery"]);
    // 截断的响应留在下一次请求里，提示跟在它后面。
    assert_eq!(
        roles(&requests[1]),
        [ApiRole::User, ApiRole::Assistant, ApiRole::User]
    );
    assert!(last_user_text(&requests[1]).starts_with("Output token limit hit. Resume directly"));
}

#[tokio::test]
async fn the_fourth_cut_reply_ends_with_the_api_error() {
    let cut = || stopped(text_response(&["half"]), StopReason::MaxTokens);
    let (stop, messages, requests) =
        run(vec![cut(), cut(), cut(), cut()], QuerySource::MainThread).await;
    assert_eq!(requests.len(), 4);
    assert_eq!(meta_sources(&messages).len(), 3);
    // 第四次的响应照常提交。
    assert!(matches!(messages.last(), Some(Message::Assistant(_))));
    let QueryStop::ApiError { message } = stop else {
        panic!("expected an API error: {stop:?}");
    };
    assert!(
        message.contains("exceeded the 32000 output token maximum"),
        "{message}"
    );
}

#[tokio::test]
async fn a_tool_use_stop_without_a_tool_call_is_discarded_and_retried_once() {
    let leaked = || stopped(text_response(&["<invoke>"]), StopReason::ToolUse);
    let (stop, messages, requests) = run(vec![leaked(), leaked()], QuerySource::MainThread).await;
    assert_eq!(requests.len(), 2);
    // 第一次的响应不提交，也不进下一次请求。
    assert_eq!(roles(&requests[1]), [ApiRole::User]);
    assert_eq!(
        last_user_text(&requests[1]),
        "The previous response failed to produce a valid tool call. Please retry the tool call now."
    );
    let assistants = messages
        .iter()
        .filter(|message| matches!(message, Message::Assistant(_)))
        .count();
    assert_eq!(assistants, 1);
    assert_eq!(
        stop,
        QueryStop::ApiError {
            message: "The model's tool call could not be parsed (retry also failed).".to_owned()
        }
    );
}

#[tokio::test]
async fn a_reply_without_visible_text_is_kept_out_of_the_retry() {
    let (stop, messages, requests) = run(
        vec![thinking_only(), thinking_only()],
        QuerySource::MainThread,
    )
    .await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests.len(), 2);
    // 第一次的响应已提交，但下一次请求里没有它。
    let assistants = messages
        .iter()
        .filter(|message| matches!(message, Message::Assistant(_)))
        .count();
    assert_eq!(assistants, 2);
    assert_eq!(roles(&requests[1]), [ApiRole::User]);
    assert_eq!(
        last_user_text(&requests[1]),
        "[Your previous response had no visible output. Please continue and produce a user-visible response.]"
    );

    let (stop, _messages, requests) = run(vec![thinking_only()], QuerySource::Compact).await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn max_turns_records_the_attachment_and_zero_means_no_limit() {
    let model = Arc::new(ScriptedModel::new(vec![tool_use_response(
        "toolu_1",
        "Read",
        &json!({}),
    )]));
    let mut limited = request("go");
    limited.max_turns = Some(1);
    let (scope_1, sink) = scope(parts(model.clone(), Vec::new()), &limited, Abort::new());
    assert_eq!(
        super::run(scope_1, limited).await.unwrap(),
        QueryStop::MaxTurns
    );
    let messages = sink.messages();
    let Some(Message::Attachment(attachment)) = messages.last() else {
        panic!("expected the max_turns_reached attachment: {messages:?}");
    };
    assert_eq!(attachment.source, "max_turns_reached");
    assert_eq!(attachment.data, json!({"maxTurns": 1, "turnCount": 2}));
    assert!(attachment.text.is_empty());

    let mut unlimited = request("go");
    unlimited.max_turns = Some(0);
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({})),
        text_response(&["done"]),
    ]));
    let (scope_2, _sink) = scope(parts(model.clone(), Vec::new()), &unlimited, Abort::new());
    assert_eq!(
        super::run(scope_2, unlimited).await.unwrap(),
        QueryStop::Completed
    );
    assert_eq!(model.requests().len(), 2);
}

/// 一段结束了的文本之后流断开。
fn cut_mid_stream() -> Response {
    let mut events = text_response(&["half"]);
    let stop = events
        .iter()
        .position(|event| matches!(event, Ok(ModelEvent::MessageDelta { .. })))
        .unwrap();
    events.truncate(stop);
    events.push(Err(ModelError::Network {
        message: "connection reset".to_owned(),
    }));
    events
}

async fn run_with(
    responses: Vec<Response>,
    non_interactive: bool,
) -> (QueryStop, Vec<Message>, Vec<ModelRequest>) {
    let model = Arc::new(ScriptedModel::new(responses));
    let request = request("go");
    let mut parts = parts(model.clone(), Vec::new());
    parts.non_interactive = non_interactive;
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = super::run(scope, request).await.unwrap();
    (stop, sink.messages(), model.requests())
}

#[tokio::test]
async fn a_stream_cut_after_a_block_is_resumed_in_print_mode() {
    let (stop, messages, requests) =
        run_with(vec![cut_mid_stream(), text_response(&["rest"])], true).await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(meta_sources(&messages), ["truncated_response_recovery"]);
    // 结束了的块留在下一次请求里，提示跟在后面。
    assert_eq!(
        roles(&requests[1]),
        [ApiRole::User, ApiRole::Assistant, ApiRole::User]
    );
    assert!(
        last_user_text(&requests[1]).starts_with("Your response above was cut off mid-stream.")
    );
}

#[tokio::test]
async fn an_interactive_turn_ends_with_the_cut_off_error_and_keeps_the_blocks() {
    let (stop, messages, requests) = run_with(vec![cut_mid_stream()], false).await;
    assert_eq!(requests.len(), 1);
    let Some(Message::Assistant(assistant)) = messages.last() else {
        panic!("expected the partial reply: {messages:?}");
    };
    assert_eq!(assistant.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        stop,
        QueryStop::ApiError {
            message:
                "API Error: Connection lost mid-response. The response above may be incomplete."
                    .to_owned()
        }
    );
}

#[tokio::test]
async fn a_stream_cut_before_any_block_is_still_a_model_error() {
    let model = Arc::new(ScriptedModel::new(vec![vec![Err(ModelError::Network {
        message: "refused".to_owned(),
    })]]));
    let request = request("go");
    let (scope, sink) = scope(parts(model, Vec::new()), &request, Abort::new());
    assert!(super::run(scope, request).await.is_err());
    assert!(sink.messages().is_empty());
}
