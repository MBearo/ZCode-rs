use rust_cli_kernel_api::ids::{ArtifactId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, ToolResultContent};
use rust_cli_kernel_api::model::{
    ApiContent, ApiRole, ModelSelection, QuerySource, RequestOptions,
};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef};
use serde_json::json;

use super::*;

/// 编码层的方案：真是 Anthropic 官方，假是“其他 Anthropic 兼容”（这一层只差 `diagnostics` 与服务端工具的版本）。
pub(crate) fn profile(first_party: bool) -> &'static crate::profiles::Profile {
    let base_url = if first_party {
        "https://api.anthropic.com"
    } else {
        "http://127.0.0.1:9"
    };
    crate::profiles::select(
        rust_cli_kernel_api::provider::WireProtocol::AnthropicMessages,
        base_url,
        "claude-opus-5-5",
    )
}

/// 第三方端点、没有 Provider 补丁时的请求体。
fn body(request: ModelRequest) -> Result<String, ModelError> {
    super::body(request, false, &[])
}

fn cache(ttl: Option<&str>, scope: Option<&str>) -> Option<CacheControl> {
    Some(CacheControl {
        kind: "ephemeral".to_owned(),
        ttl: ttl.map(str::to_owned),
        scope: scope.map(str::to_owned),
    })
}

fn plain(block: ContentBlock) -> ApiContent {
    ApiContent {
        block,
        cache_control: None,
    }
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
    }
}

pub(crate) fn request(messages: Vec<ApiMessage>) -> ModelRequest {
    ModelRequest {
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            reasoning: None,
            connection: None,
        },
        system: Vec::new(),
        messages,
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: 32_000,
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        source: QuerySource::MainThread,
        options: RequestOptions::default(),
    }
}

fn user(content: Vec<ApiContent>) -> ApiMessage {
    ApiMessage {
        role: ApiRole::User,
        content,
        output_config: None,
    }
}

#[test]
fn a_full_request_has_the_fixed_field_order() {
    let mut request = request(vec![
        user(vec![ApiContent {
            block: text_block("hi"),
            cache_control: cache(Some("1h"), None),
        }]),
        ApiMessage {
            role: ApiRole::Assistant,
            content: vec![
                plain(ContentBlock::Thinking {
                    thinking: "plan".to_owned(),
                    signature: Some("sig".to_owned()),
                }),
                plain(ContentBlock::RedactedThinking {
                    data: "opaque".to_owned(),
                }),
                plain(text_block("ok")),
                plain(ContentBlock::ToolUse {
                    id: ToolUseId::new("toolu_1"),
                    name: "Read".to_owned(),
                    input: json!({"file_path": "/a", "limit": 2}),
                }),
            ],
            output_config: None,
        },
        user(vec![
            plain(ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: vec![
                    ToolResultContent::Text {
                        text: "no such file".to_owned(),
                    },
                    ToolResultContent::Image {
                        source: ImageSource::Base64 {
                            media_type: "image/png".to_owned(),
                            data: "iVBO".to_owned(),
                        },
                    },
                ],
                is_error: true,
            }),
            ApiContent {
                block: text_block("next"),
                cache_control: cache(None, None),
            },
        ]),
    ]);
    request.system = vec![
        SystemBlock {
            text: "billing header".to_owned(),
            cache_control: None,
        },
        SystemBlock {
            text: "You are ZCode.".to_owned(),
            cache_control: cache(Some("5m"), Some("global")),
        },
    ];
    request.tools = vec![
        ToolDefinition {
            name: "Read".to_owned(),
            description: "Reads a file.".to_owned(),
            input_schema: json!({"type": "object", "properties": {"file_path": {"type": "string"}}}),
            cache_control: None,
        },
        ToolDefinition {
            name: "Bash".to_owned(),
            description: "Runs a command.".to_owned(),
            input_schema: json!({"type": "object"}),
            cache_control: cache(None, None),
        },
    ];
    request.tool_choice = Some(ToolChoice::Auto);
    request.thinking = Some(ThinkingConfig::Enabled {
        budget_tokens: 31_999,
    });
    request.betas = vec!["example-beta-2025-02-19".to_owned()];
    let expected = concat!(
        r#"{"model":"claude-sonnet-4-5","messages":["#,
        r#"{"role":"user","content":[{"type":"text","text":"hi","cache_control":{"type":"ephemeral","ttl":"1h"}}]},"#,
        r#"{"role":"assistant","content":[{"type":"thinking","thinking":"plan","signature":"sig"},"#,
        r#"{"type":"redacted_thinking","data":"opaque"},{"type":"text","text":"ok"},"#,
        r#"{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/a","limit":2}}]},"#,
        r#"{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":["#,
        r#"{"type":"text","text":"no such file"},"#,
        r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBO"}}],"is_error":true},"#,
        r#"{"type":"text","text":"next","cache_control":{"type":"ephemeral"}}]}],"#,
        r#""system":[{"type":"text","text":"billing header"},"#,
        r#"{"type":"text","text":"You are ZCode.","cache_control":{"type":"ephemeral","ttl":"5m","scope":"global"}}],"#,
        r#""tools":[{"name":"Read","description":"Reads a file.","input_schema":{"type":"object","properties":{"file_path":{"type":"string"}}}},"#,
        r#"{"name":"Bash","description":"Runs a command.","input_schema":{"type":"object"},"cache_control":{"type":"ephemeral"}}],"#,
        r#""tool_choice":{"type":"auto"},"max_tokens":32000,"#,
        r#""thinking":{"budget_tokens":31999,"type":"enabled"},"stream":true}"#,
    );
    assert_eq!(body(request).unwrap(), expected);
}

#[test]
fn optional_fields_are_left_out_and_lists_stay() {
    let body = body(request(vec![user(vec![plain(text_block("hello"))])])).unwrap();
    assert_eq!(
        body,
        concat!(
            r#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"#,
            r#""system":[],"tools":[],"max_tokens":32000,"stream":true}"#,
        )
    );
}

#[test]
fn thinking_tool_choice_and_temperature_forms() {
    let encode = |thinking, choice, temperature| {
        let mut request = request(Vec::new());
        request.thinking = Some(thinking);
        request.tool_choice = Some(choice);
        request.temperature = Some(temperature);
        let value: serde_json::Value = serde_json::from_str(&body(request).unwrap()).unwrap();
        (
            value["thinking"].to_string(),
            value["tool_choice"].to_string(),
            stringify(&value["temperature"]),
        )
    };
    let tool = |name: &str| ToolChoice::Tool {
        name: name.to_owned(),
    };
    let adaptive = |display: Option<&str>| ThinkingConfig::Adaptive {
        display: display.map(str::to_owned),
    };
    assert_eq!(
        encode(adaptive(Some("updates")), ToolChoice::Any, 0.25).0,
        r#"{"type":"adaptive","display":"updates"}"#
    );
    assert_eq!(
        encode(adaptive(None), tool("Bash"), 1.0),
        (
            r#"{"type":"adaptive"}"#.to_owned(),
            r#"{"type":"tool","name":"Bash"}"#.to_owned(),
            "1".to_owned()
        )
    );
    assert_eq!(
        encode(ThinkingConfig::Disabled, ToolChoice::None, 0.7),
        (
            r#"{"type":"disabled"}"#.to_owned(),
            r#"{"type":"none"}"#.to_owned(),
            "0.7".to_owned()
        )
    );
    let mut nan = request(Vec::new());
    nan.temperature = Some(f32::NAN);
    assert!(matches!(body(nan), Err(ModelError::InvalidRequest { .. })));
}

/// 文本按 JavaScript `JSON.stringify` 转义：控制字符用 `\u00XX`，非 ASCII 与 `/` 原样输出。
#[test]
fn text_is_escaped_like_javascript() {
    let body = body(request(vec![user(vec![plain(text_block(
        "a\u{7}\"/\u{2028}你好\n",
    ))])]))
    .unwrap();
    assert!(
        body.contains(r#""text":"a\u0007\"/"#.to_owned().as_str()),
        "{body}"
    );
    assert!(body.contains("\u{2028}你好\\n\""), "{body}");
}

#[test]
fn unmaterialized_image_artifacts_are_rejected() {
    let artifact = ImageSource::Artifact {
        media_type: "image/png".to_owned(),
        artifact: ArtifactRef {
            session: SessionId::new("sess_1"),
            id: ArtifactId::new("art_1"),
            purpose: ArtifactPurpose::Media,
            media_type: "image/png".to_owned(),
            size: 3,
        },
    };
    let error = body(request(vec![user(vec![plain(ContentBlock::Image {
        source: artifact,
    })])]))
    .unwrap_err();
    assert!(
        matches!(error, ModelError::InvalidRequest { .. }),
        "{error:?}"
    );
}

fn keys(body: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body).unwrap();
    value.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn request_options_are_written_by_source_and_profile() {
    let options = RequestOptions {
        effort: Some("high".to_owned()),
        context_edits: vec![ContextEdit::ClearThinking {
            keep: "all".to_owned(),
        }],
        user_id: Some("u".to_owned()),
        previous_message_id: None,
        ..RequestOptions::default()
    };
    let encode = |source: QuerySource, first_party: bool| {
        let mut request = request(Vec::new());
        request.source = source;
        request.options = options.clone();
        super::body(request, first_party, &[]).unwrap()
    };
    let official = encode(QuerySource::MainThread, true);
    assert_eq!(
        keys(&official),
        [
            "model",
            "messages",
            "system",
            "tools",
            "metadata",
            "max_tokens",
            "context_management",
            "output_config",
            "diagnostics",
            "stream"
        ]
    );
    // 会话第一次请求：previous_message_id 显式为 null。
    assert!(
        official.contains(r#""diagnostics":{"previous_message_id":null}"#),
        "{official}"
    );
    // 第三方端点不写 diagnostics；context_management 跟着上下文编辑走，不看端点。
    assert_eq!(
        keys(&encode(QuerySource::MainThread, false)),
        [
            "model",
            "messages",
            "system",
            "tools",
            "metadata",
            "max_tokens",
            "context_management",
            "output_config",
            "stream"
        ]
    );
    // 旁路请求不写 diagnostics；子代理、hook agent 与 compact 写。
    let diagnosed = |source| keys(&encode(source, true)).contains(&"diagnostics".to_owned());
    assert!(!diagnosed(QuerySource::Side("title".to_owned())));
    assert!(diagnosed(QuerySource::Side("hook_agent".to_owned())));
    assert!(diagnosed(QuerySource::Subagent("Explore".to_owned())));
    assert!(diagnosed(QuerySource::Compact));
    // 默认选项：四个字段都不出现（官方端点的 diagnostics 除外）。
    let mut plain = request(Vec::new());
    plain.options.previous_message_id = Some("msg_1".to_owned());
    let plain = super::body(plain, true, &[]).unwrap();
    assert_eq!(
        keys(&plain),
        [
            "model",
            "messages",
            "system",
            "tools",
            "max_tokens",
            "diagnostics",
            "stream"
        ]
    );
    assert!(plain.contains(r#""diagnostics":{"previous_message_id":"msg_1"}"#));
}

#[test]
fn mid_conversation_system_messages_keep_their_role() {
    let message = ApiMessage {
        role: ApiRole::System,
        content: vec![ApiContent {
            block: text_block("# Environment"),
            cache_control: cache(None, None),
        }],
        output_config: None,
    };
    let body = body(request(vec![message])).unwrap();
    assert!(
        body.contains(r##"{"role":"system","content":[{"type":"text","text":"# Environment","cache_control":{"type":"ephemeral"}}]}"##),
        "{body}"
    );
}

#[test]
fn tool_references_encode_in_the_anthropic_shape() {
    let result = plain(ContentBlock::ToolResult {
        tool_use_id: ToolUseId::new("toolu_1"),
        content: vec![ToolResultContent::ToolReference {
            tool_name: "WebFetch".to_owned(),
        }],
        is_error: false,
    });
    let body = body(request(vec![user(vec![result])])).unwrap();
    assert!(
        body.contains(
            r#"{"tool_use_id":"toolu_1","type":"tool_result","content":[{"type":"tool_reference","tool_name":"WebFetch"}]}"#
        ),
        "{body}"
    );
}

#[test]
fn server_tools_follow_the_tool_list_and_their_blocks_replay() {
    let mut request = request(vec![ApiMessage {
        role: ApiRole::Assistant,
        content: vec![
            plain(ContentBlock::ServerToolUse {
                id: "srvtoolu_1".to_owned(),
                name: "web_search".to_owned(),
                input: json!({"query": "rust"}),
            }),
            plain(ContentBlock::WebSearchToolResult {
                tool_use_id: "srvtoolu_1".to_owned(),
                content: json!([{"type": "web_search_result", "title": "T", "url": "https://example.com"}]),
            }),
        ],
        output_config: None,
    }]);
    request.options.server_tools = vec![rust_cli_kernel_api::model::ServerTool::WebSearch {
        allowed_domains: None,
        blocked_domains: Some(vec!["example.org".to_owned()]),
        max_uses: 8,
    }];
    // Anthropic 官方是 `web_search_20250305`；其他 Anthropic 协议方案是 Node 发的版本（键序随 AI SDK，修复线 F23）。
    let node = super::body(request.clone(), false, &[]).unwrap();
    assert!(
        node.contains(r#""tools":[{"type":"web_search_20260209","name":"web_search","max_uses":8,"blocked_domains":["example.org"]}]"#),
        "{node}"
    );
    let body = super::body(request, true, &[]).unwrap();
    assert!(
        body.contains(r#""tools":[{"type":"web_search_20250305","name":"web_search","blocked_domains":["example.org"],"max_uses":8}]"#),
        "{body}"
    );
    assert!(body.contains(r#"{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{"query":"rust"}}"#));
    assert!(body.contains(r#"{"type":"web_search_tool_result","tool_use_id":"srvtoolu_1","content":[{"type":"web_search_result","title":"T","url":"https://example.com"}]}"#));
}
