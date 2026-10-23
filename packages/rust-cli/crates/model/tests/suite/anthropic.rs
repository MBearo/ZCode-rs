//! 正常路径：线上的请求形状、SSE 的任意切分、tool 输入拼接、thinking 与 signature。

use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, CacheControl, ContextEdit, ModelEvent, QuerySource,
    RequestOptions, SystemBlock, ThinkingConfig,
};
use serde_json::json;

use super::common::{
    API_KEY, assemble, block_start, block_stop, collect, delta, fast_retry, message_end,
    message_start, model_at, request, text_response,
};
use super::fixture::{Fixture, Reply};

fn sse_reply() -> Reply {
    Reply::new(200).header("content-type", "text/event-stream")
}

#[tokio::test]
async fn the_wire_request_has_the_first_party_shape() {
    let server = Fixture::start(vec![sse_reply().send(text_response(&["Hi"]))]).await;
    // 基址带路径前缀与结尾的 `/`：与 SDK 一样只去掉一个 `/`。本地端点按 Anthropic 官方方案处理（只给测试用的
    // 声明，model.md §6），这里看的是第一方的线上形状；其他 Anthropic 兼容见 `profiles_tests.rs`。
    let model = model_at(format!("{}/gateway/", server.url()), fast_retry(0))
        .await
        .with_assumed_first_party();
    let mut request = request("hello", QuerySource::MainThread);
    request.system = vec![SystemBlock {
        text: "You are ZCode.".to_owned(),
        cache_control: Some(CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: None,
        }),
    }];
    // 对话中间的 system 消息（环境段）。
    request.messages.push(ApiMessage {
        role: ApiRole::System,
        content: vec![ApiContent {
            block: ContentBlock::Text {
                text: "# Environment".to_owned(),
            },
            cache_control: None,
        }],
        output_config: None,
    });
    request.thinking = Some(ThinkingConfig::Adaptive {
        display: Some("updates".to_owned()),
    });
    // 调用方显式给的 beta 接在按特性生成的列表之后；已生成的不重复。
    request.betas = vec![
        "example-beta-2025-02-19".to_owned(),
        "context-management-2025-06-27".to_owned(),
    ];
    request.options = RequestOptions {
        effort: Some("medium".to_owned()),
        context_edits: vec![ContextEdit::ClearThinking {
            keep: "all".to_owned(),
        }],
        user_id: Some(r#"{"session_id":"s"}"#.to_owned()),
        previous_message_id: Some("msg_00".to_owned()),
        ..RequestOptions::default()
    };
    let events = collect(&model, request).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let captured = &server.requests()[0];
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.target, "/gateway/v1/messages?beta=true");
    assert_eq!(captured.header("x-api-key"), Some(API_KEY));
    assert_eq!(captured.header("anthropic-version"), Some("2023-06-01"));
    // 官方方案写 diagnostics，cache-diagnosis 随之加上。
    assert_eq!(
        captured.header("anthropic-beta"),
        Some(concat!(
            "interleaved-thinking-2025-05-14,context-management-2025-06-27,",
            "mid-conversation-system-2026-04-07,thinking-token-count-2026-05-13,",
            "effort-2025-11-24,thinking-display-updates-2026-08-18,cache-diagnosis-2026-04-07,",
            "example-beta-2025-02-19"
        ))
    );
    assert_eq!(captured.header("content-type"), Some("application/json"));
    assert_eq!(captured.header("accept"), Some("application/json"));
    assert_eq!(
        captured.body,
        concat!(
            r#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]},"#,
            r##"{"role":"system","content":[{"type":"text","text":"# Environment"}]}],"##,
            r#""system":[{"type":"text","text":"You are ZCode.","cache_control":{"type":"ephemeral"}}],"#,
            r#""tools":[],"metadata":{"user_id":"{\"session_id\":\"s\"}"},"max_tokens":1024,"#,
            r#""thinking":{"type":"adaptive","display":"updates"},"#,
            r#""context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]},"#,
            r#""output_config":{"effort":"medium"},"diagnostics":{"previous_message_id":"msg_00"},"stream":true}"#,
        )
    );
}

/// 带 `scope: global` 的缓存断点（system 静态段、工具、消息块各一个）与一个工具。
fn scoped_request() -> rust_cli_kernel_api::model::ModelRequest {
    let global = || {
        Some(CacheControl {
            kind: "ephemeral".to_owned(),
            ttl: None,
            scope: Some("global".to_owned()),
        })
    };
    let mut request = request("hello", QuerySource::MainThread);
    request.system = vec![SystemBlock {
        text: "static".to_owned(),
        cache_control: global(),
    }];
    request.tools = vec![rust_cli_kernel_api::model::ToolDefinition {
        name: "Read".to_owned(),
        description: "Read a file".to_owned(),
        input_schema: json!({"type": "object"}),
        cache_control: global(),
    }];
    request.messages[0].content[0].cache_control = global();
    request
}

/// 修复线 F23 救急（model.md §3.2）：非官方端点（这里是 127.0.0.1 的 fixture）不写 `cache_control.scope`，也不发
/// `prompt-caching-scope` beta。以前照写，经网关转到 Bedrock 时整个请求被拒（`Extra inputs are not permitted`）。
#[tokio::test]
async fn a_gateway_gets_no_cache_scope() {
    let server = Fixture::start(vec![sse_reply().send(text_response(&["Hi"]))]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let events = collect(&model, scoped_request()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let captured = &server.requests()[0];
    assert!(!captured.body.contains("scope"), "{}", captured.body);
    assert!(
        captured
            .body
            .contains(r#""cache_control":{"type":"ephemeral"}"#),
        "the cache breakpoints stay: {}",
        captured.body
    );
    // 没有 `prompt-caching-scope`；兼容端点照 Node 发 beta（F64），但经网关不发 structured-outputs（工作线 F71：
    // 转到 Bedrock 等后端的网关可能拒绝它），这个请求就没有 beta。
    assert_eq!(captured.header("anthropic-beta"), None);
}

/// 入口声明按官方端点处理（只给测试用，model.md §6）：同一个本地端点也发第一方形状——`scope` 照写、带
/// `prompt-caching-scope` beta，主循环请求写 `diagnostics`。
#[tokio::test]
async fn an_assumed_first_party_endpoint_keeps_the_scope() {
    let server = Fixture::start(vec![sse_reply().send(text_response(&["Hi"]))]).await;
    let model = model_at(server.url(), fast_retry(0))
        .await
        .with_assumed_first_party();
    let events = collect(&model, scoped_request()).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let captured = &server.requests()[0];
    assert_eq!(captured.body.matches(r#""scope":"global""#).count(), 3);
    assert!(
        captured
            .body
            .contains(r#""diagnostics":{"previous_message_id":null}"#),
        "{}",
        captured.body
    );
    assert_eq!(
        captured.header("anthropic-beta"),
        Some("prompt-caching-scope-2026-01-05,cache-diagnosis-2026-04-07")
    );
}

#[tokio::test]
async fn no_beta_header_without_betas() {
    let server = Fixture::start(vec![sse_reply().send(text_response(&["x"]))]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    collect(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(server.requests()[0].header("anthropic-beta"), None);
}

#[tokio::test]
async fn a_text_response_maps_to_events() {
    let server = Fixture::start(vec![sse_reply().send(text_response(&["Hello", ", world"]))]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    let events: Vec<ModelEvent> = events.into_iter().map(Result::unwrap).collect();
    assert_eq!(
        events.first(),
        Some(&ModelEvent::MessageStart {
            id: "msg_01".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            usage: Usage {
                input_tokens: 25,
                output_tokens: 1,
                ..Usage::default()
            },
        })
    );
    assert_eq!(
        events[events.len() - 2..],
        [
            ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: Usage {
                    input_tokens: 25,
                    output_tokens: 9,
                    ..Usage::default()
                },
            },
            ModelEvent::MessageStop,
        ]
    );
    let blocks = assemble(&events.into_iter().map(Ok).collect::<Vec<_>>());
    assert_eq!(
        blocks,
        vec![ContentBlock::Text {
            text: "Hello, world".to_owned()
        }]
    );
}

/// 分片（一个事件跨多次写，切在多字节字符中间）与粘包（多个事件一次写完）得到同样的结果。
#[tokio::test]
async fn sse_split_anywhere_or_glued_together_decodes_the_same() {
    let body = text_response(&["你好", "，世界", "！"]);
    let mut split = sse_reply();
    for piece in body.as_bytes().chunks(5) {
        split = split.send(piece.to_vec()).pause(1);
    }
    let glued = sse_reply().send(format!("{body}{body}"));
    let server = Fixture::start(vec![split, glued]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let first = collect(&model, request("hi", QuerySource::MainThread)).await;
    let second = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(first.iter().all(Result::is_ok), "{first:?}");
    // 粘在一起的第二段在 message_stop 之后，不再读取。
    assert_eq!(first, second);
    assert_eq!(
        assemble(&first),
        vec![ContentBlock::Text {
            text: "你好，世界！".to_owned()
        }]
    );
}

#[tokio::test]
async fn tool_input_fragments_join_into_the_input() {
    let input = json!({"command": "ls -la", "description": "列出文件", "timeout": 1.5});
    let text = input.to_string();
    let (head, tail) = text.split_at(text.find("列").unwrap());
    let mut stream = message_start();
    stream.push_str(&block_start(0, json!({"type": "text", "text": ""})));
    stream.push_str(&delta(0, json!({"type": "text_delta", "text": "Running."})));
    stream.push_str(&block_stop(0));
    stream.push_str(&block_start(
        1,
        json!({"type": "tool_use", "id": "toolu_01", "name": "Bash", "input": {}}),
    ));
    for fragment in [&head[..7], &head[7..], tail] {
        stream.push_str(&delta(
            1,
            json!({"type": "input_json_delta", "partial_json": fragment}),
        ));
    }
    stream.push_str(&block_stop(1));
    stream.push_str(&block_start(
        2,
        json!({"type": "tool_use", "id": "toolu_02", "name": "TaskList", "input": {}}),
    ));
    stream.push_str(&delta(
        2,
        json!({"type": "input_json_delta", "partial_json": ""}),
    ));
    stream.push_str(&block_stop(2));
    stream.push_str(&message_end("tool_use", 30));
    let mut reply = sse_reply();
    for piece in stream.as_bytes().chunks(64) {
        reply = reply.send(piece.to_vec());
    }
    let server = Fixture::start(vec![reply]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let events = collect(&model, request("ls", QuerySource::MainThread)).await;
    let blocks = assemble(&events);
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[1],
        ContentBlock::ToolUse {
            id: rust_cli_kernel_api::ids::ToolUseId::new("toolu_01"),
            name: "Bash".to_owned(),
            input,
        }
    );
    assert!(matches!(&blocks[2], ContentBlock::ToolUse { input, .. } if *input == json!({})));
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            ..
        })
    )));
}

#[tokio::test]
async fn thinking_keeps_its_signature() {
    let mut stream = message_start();
    stream.push_str(&block_start(
        0,
        json!({"type": "thinking", "thinking": "", "signature": ""}),
    ));
    for part in ["The user ", "wants a greeting."] {
        stream.push_str(&delta(
            0,
            json!({"type": "thinking_delta", "thinking": part}),
        ));
    }
    stream.push_str(&delta(
        0,
        json!({"type": "signature_delta", "signature": "EqQBCkYIBxgCKkB"}),
    ));
    stream.push_str(&block_stop(0));
    stream.push_str(&block_start(
        1,
        json!({"type": "redacted_thinking", "data": "EmwKAhgB"}),
    ));
    stream.push_str(&block_stop(1));
    stream.push_str(&block_start(2, json!({"type": "text", "text": ""})));
    stream.push_str(&delta(2, json!({"type": "text_delta", "text": "Hello!"})));
    stream.push_str(&block_stop(2));
    stream.push_str(&message_end("end_turn", 12));
    let server = Fixture::start(vec![sse_reply().send(stream)]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(
        assemble(&events),
        vec![
            ContentBlock::Thinking {
                thinking: "The user wants a greeting.".to_owned(),
                signature: Some("EqQBCkYIBxgCKkB".to_owned()),
            },
            ContentBlock::RedactedThinking {
                data: "EmwKAhgB".to_owned()
            },
            ContentBlock::Text {
                text: "Hello!".to_owned()
            },
        ]
    );
}
