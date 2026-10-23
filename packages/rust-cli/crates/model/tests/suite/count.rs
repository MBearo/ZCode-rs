//! count_tokens（docs/specs/rust-cli/context-usage.md §3）：请求形状，错误给调用方估算。

use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, CountTokensRequest, ModelError, ModelPort, ModelSelection,
    ToolDefinition,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{API_KEY, fast_retry, model_at};
use super::fixture::{Fixture, Reply};

fn selection() -> ModelSelection {
    ModelSelection {
        provider: "anthropic".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        reasoning: None,
        connection: None,
    }
}

#[tokio::test]
async fn an_empty_count_sends_foo_with_the_tools() {
    let server = Fixture::start(vec![
        Reply::new(200).send(r#"{"input_tokens":4242}"#),
        Reply::new(200).send(r#"{"input_tokens":7}"#),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let tool = ToolDefinition {
        name: "Read".to_owned(),
        description: "Reads a file.".to_owned(),
        input_schema: json!({"type": "object"}),
        cache_control: None,
    };
    let request = CountTokensRequest {
        selection: selection(),
        messages: Vec::new(),
        tools: vec![tool],
    };
    let counted = model.count_tokens(request, CancellationToken::new()).await;
    assert_eq!(counted.unwrap(), 4242);
    let section = CountTokensRequest {
        selection: selection(),
        messages: vec![ApiMessage {
            role: ApiRole::User,
            content: vec![ApiContent {
                block: ContentBlock::Text {
                    text: "# Memory".to_owned(),
                },
                cache_control: None,
            }],
            output_config: None,
        }],
        tools: Vec::new(),
    };
    assert_eq!(
        model
            .count_tokens(section, CancellationToken::new())
            .await
            .unwrap(),
        7
    );
    let requests = server.requests();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].target, "/v1/messages/count_tokens?beta=true");
    assert_eq!(
        requests[0].body,
        r#"{"model":"claude-opus-5-5","messages":[{"role":"user","content":"foo"}],"tools":[{"name":"Read","description":"Reads a file.","input_schema":{"type":"object"}}]}"#
    );
    assert_eq!(
        requests[0].header("anthropic-beta"),
        Some("token-counting-2024-11-01")
    );
    assert_eq!(requests[0].header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(requests[0].header("x-api-key"), Some(API_KEY));
    assert_eq!(
        requests[1].body,
        r##"{"model":"claude-opus-5-5","messages":[{"role":"user","content":[{"type":"text","text":"# Memory"}]}],"tools":[]}"##
    );
}

#[tokio::test]
async fn errors_and_odd_bodies_are_reported_not_retried() {
    let server = Fixture::start(vec![
        Reply::new(404)
            .send(r#"{"type":"error","error":{"type":"not_found_error","message":"no"}}"#),
        Reply::new(200).send(r#"{"tokens":1}"#),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let request = || CountTokensRequest {
        selection: selection(),
        messages: Vec::new(),
        tools: Vec::new(),
    };
    let error = model
        .count_tokens(request(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!matches!(error, ModelError::Cancelled), "{error:?}");
    let error = model
        .count_tokens(request(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, ModelError::Protocol { .. }), "{error:?}");
    assert_eq!(server.requests().len(), 2);
}
