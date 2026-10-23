//! 请求编码的字节对照（修复线 F6，审查 R5-6）：消息改成从类型化消息直接写出之后，请求体与 beta 必须与旧写法
//! （先建 `serde_json::Value` 再 `JSON.stringify`）逐字节相同。`reference` 是旧写法的原样副本，只用作对照。

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, ToolResultContent};
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, CacheControl, ContextEdit, ModelRequest, ThinkingConfig,
    TurnOutputConfig,
};
use serde_json::{Value, json};

use super::tests::request;

/// 旧写法（修复线 F6 之前的 `anthropic_request::body_value` 与 `anthropic_betas::betas`）。
#[allow(clippy::needless_pass_by_value)]
mod reference {
    use rust_cli_kernel_api::message::{ContentBlock, ImageSource, ToolResultContent};
    use rust_cli_kernel_api::model::{
        ApiContent, ApiMessage, ApiRole, CacheControl, ModelError, ModelRequest,
    };
    use serde_json::{Map, Value};

    use super::super::{
        context_edit, number, server_tool, system_block, thinking, tool, tool_choice,
    };
    use crate::anthropic_betas::{
        ADVANCED_TOOL_USE, CACHE_DIAGNOSIS, CONTEXT_MANAGEMENT, EFFORT, INTERLEAVED_THINKING,
        MID_CONVERSATION_SYSTEM, PER_TURN_CONTROL, PROMPT_CACHING_SCOPE, STRUCTURED_OUTPUTS,
        THINKING_DISPLAY_UPDATES, THINKING_TOKEN_COUNT, writes_diagnostics,
    };
    use crate::patch;

    pub(super) fn body_value(
        request: ModelRequest,
        first_party: bool,
        patches: &[Value],
    ) -> Result<Value, ModelError> {
        let ModelRequest {
            selection,
            system,
            messages,
            tools,
            tool_choice,
            max_tokens,
            thinking,
            temperature,
            betas: _,
            source,
            options,
        } = request;
        let mut body = Map::new();
        body.insert("model".into(), Value::String(selection.model));
        let messages = messages
            .into_iter()
            .map(message)
            .collect::<Result<Vec<_>, _>>()?;
        body.insert("messages".into(), Value::Array(messages));
        body.insert(
            "system".into(),
            Value::Array(system.into_iter().map(system_block).collect()),
        );
        let mut flags = options.tool_flags;
        let mut tools: Vec<Value> = tools
            .into_iter()
            .map(|item| {
                let flags = flags.remove(&item.name).unwrap_or_default();
                tool(item, flags)
            })
            .collect();
        let version = crate::anthropic_request::tests::profile(first_party).web_search;
        tools.extend(
            options
                .server_tools
                .into_iter()
                .map(|item| server_tool(item, version)),
        );
        body.insert("tools".into(), Value::Array(tools));
        if let Some(choice) = tool_choice {
            body.insert("tool_choice".into(), self::tool_choice(choice));
        }
        // 每个请求都应写 metadata（`{user_id}`）；这里在上层给出 user_id 时才写。
        if let Some(user_id) = options.user_id {
            let metadata = object([("user_id", Value::String(user_id))]);
            body.insert("metadata".into(), Value::Object(metadata));
        }
        body.insert("max_tokens".into(), Value::from(max_tokens));
        if let Some(thinking) = thinking {
            body.insert("thinking".into(), self::thinking(thinking));
        }
        if let Some(temperature) = temperature {
            body.insert("temperature".into(), number(temperature)?);
        }
        // 有上下文编辑就写，beta context-management 跟着加（anthropic_betas）。
        if !options.context_edits.is_empty() {
            let edits = options
                .context_edits
                .into_iter()
                .map(context_edit)
                .collect();
            let management = object([("edits", Value::Array(edits))]);
            body.insert("context_management".into(), Value::Object(management));
        }
        if let Some(config) = crate::anthropic_output::config(options.effort, options.output_format)
        {
            body.insert("output_config".into(), config);
        }
        // 会话第一次请求没有上一条响应，显式写 `previous_message_id: null`。
        if writes_diagnostics(first_party, &source) {
            let previous = options
                .previous_message_id
                .map_or(Value::Null, Value::String);
            let diagnostics = object([("previous_message_id", previous)]);
            body.insert("diagnostics".into(), Value::Object(diagnostics));
        }
        // 发送时在请求参数之后追加 `stream: true`（`{...params, stream: true}`）。
        body.insert("stream".into(), Value::Bool(true));
        let mut body = Value::Object(body);
        patch::apply(&mut body, patches);
        Ok(body)
    }

    fn object<const N: usize>(fields: [(&str, Value); N]) -> Map<String, Value> {
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }

    fn text(value: &str) -> Value {
        Value::String(value.to_owned())
    }

    pub(super) fn message(message: ApiMessage) -> Result<Value, ModelError> {
        let role = match message.role {
            ApiRole::User => "user",
            ApiRole::Assistant => "assistant",
            // 对话中间的 system 消息（beta `mid-conversation-system-2026-04-07`）原样发出。
            ApiRole::System => "system",
        };
        let content = message
            .content
            .into_iter()
            .map(content)
            .collect::<Result<Vec<_>, _>>()?;
        let mut encoded = object([("role", text(role)), ("content", Value::Array(content))]);
        // 每轮 effort（beta `per-turn-control-2026-07-01`）跟在 content 之后。
        if let Some(config) = message.output_config
            && let Some(effort) = config.effort
        {
            encoded.insert(
                "output_config".into(),
                Value::Object(object([("effort", Value::String(effort))])),
            );
        }
        Ok(Value::Object(encoded))
    }

    fn content(content: ApiContent) -> Result<Value, ModelError> {
        let mut block = block(content.block)?;
        if let Some(cache) = content.cache_control {
            block.insert("cache_control".into(), cache_control(cache));
        }
        Ok(Value::Object(block))
    }

    fn block(block: ContentBlock) -> Result<Map<String, Value>, ModelError> {
        Ok(match block {
            ContentBlock::Text { text: body } => {
                object([("type", text("text")), ("text", Value::String(body))])
            }
            ContentBlock::Image { source } => {
                object([("type", text("image")), ("source", image_source(source)?)])
            }
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                let mut block = object([
                    ("type", text("thinking")),
                    ("thinking", Value::String(thinking)),
                ]);
                if let Some(signature) = signature {
                    block.insert("signature".into(), Value::String(signature));
                }
                block
            }
            ContentBlock::RedactedThinking { data } => object([
                ("type", text("redacted_thinking")),
                ("data", Value::String(data)),
            ]),
            ContentBlock::ToolUse { id, name, input } => object([
                ("type", text("tool_use")),
                ("id", Value::String(id.as_str().to_owned())),
                ("name", Value::String(name)),
                ("input", input),
            ]),
            // 服务端工具的块原样回放（docs/specs/rust-cli/tools-more.md §5）。
            ContentBlock::ServerToolUse { id, name, input } => object([
                ("type", text("server_tool_use")),
                ("id", Value::String(id)),
                ("name", Value::String(name)),
                ("input", input),
            ]),
            ContentBlock::WebSearchToolResult {
                tool_use_id,
                content,
            } => object([
                ("type", text("web_search_tool_result")),
                ("tool_use_id", Value::String(tool_use_id)),
                ("content", content),
            ]),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let content = content
                    .into_iter()
                    .map(tool_result_content)
                    .collect::<Result<Vec<_>, _>>()?;
                // 多数内置工具的结果块以 tool_use_id 开头；is_error 只在失败时写。
                let mut block = object([
                    (
                        "tool_use_id",
                        Value::String(tool_use_id.as_str().to_owned()),
                    ),
                    ("type", text("tool_result")),
                    ("content", Value::Array(content)),
                ]);
                if is_error {
                    block.insert("is_error".into(), Value::Bool(true));
                }
                block
            }
        })
    }

    fn tool_result_content(content: ToolResultContent) -> Result<Value, ModelError> {
        Ok(Value::Object(match content {
            ToolResultContent::Text { text: body } => {
                object([("type", text("text")), ("text", Value::String(body))])
            }
            ToolResultContent::Image { source } => {
                object([("type", text("image")), ("source", image_source(source)?)])
            }
            ToolResultContent::Document { source } => object([
                ("type", text("document")),
                ("source", image_source(source)?),
            ]),
            // ToolSearch 的结果（docs/specs/rust-cli/tools-more.md §3.6）。
            ToolResultContent::ToolReference { tool_name } => object([
                ("type", text("tool_reference")),
                ("tool_name", Value::String(tool_name)),
            ]),
        }))
    }

    fn image_source(source: ImageSource) -> Result<Value, ModelError> {
        match source {
            ImageSource::Base64 { media_type, data } => Ok(Value::Object(object([
                ("type", text("base64")),
                ("media_type", Value::String(media_type)),
                ("data", Value::String(data)),
            ]))),
            // 大图在发给模型之前由使用方物化成 base64（README §6 规则 10）；适配器拿不到 Artifact 存储。
            ImageSource::Artifact { .. } => Err(ModelError::InvalidRequest {
                message: "image artifacts must be materialized before the model request".to_owned(),
            }),
        }
    }

    fn cache_control(cache: CacheControl) -> Value {
        let mut value = object([("type", Value::String(cache.kind))]);
        if let Some(ttl) = cache.ttl {
            value.insert("ttl".into(), Value::String(ttl));
        }
        if let Some(scope) = cache.scope {
            value.insert("scope".into(), Value::String(scope));
        }
        Value::Object(value)
    }

    pub(super) fn betas(body: &Value, explicit: &[String]) -> Vec<String> {
        let thinking = matches!(
            body["thinking"]["type"].as_str(),
            Some("enabled" | "adaptive")
        );
        let messages = body["messages"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let per_turn = messages.iter().any(|m| m.get("output_config").is_some());
        let rules: [(&str, bool); 11] = [
            // 模型表（jN）：交错思考与思考 token 计数都跟着 thinking 走。
            (INTERLEAVED_THINKING, thinking),
            (CONTEXT_MANAGEMENT, body.get("context_management").is_some()),
            (PROMPT_CACHING_SCOPE, uses_cache_scope(body)),
            (
                MID_CONVERSATION_SYSTEM,
                messages.iter().any(|m| m["role"] == "system"),
            ),
            (THINKING_TOKEN_COUNT, thinking),
            // 每轮表（zN）：消息级的 output_config。
            (PER_TURN_CONTROL, per_turn),
            // 请求期追加：effort、工具搜索、思考展示、缓存诊断。
            (
                EFFORT,
                body["output_config"].get("effort").is_some() || per_turn,
            ),
            (
                ADVANCED_TOOL_USE,
                items(&body["tools"]).any(|tool| tool["defer_loading"] == true),
            ),
            (
                THINKING_DISPLAY_UPDATES,
                body["thinking"]["type"] == "adaptive" && body["thinking"].get("display").is_some(),
            ),
            (CACHE_DIAGNOSIS, body.get("diagnostics").is_some()),
            // structured output（请求带输出格式时追加，titles.md §8.4）。
            (
                STRUCTURED_OUTPUTS,
                body["output_config"].get("format").is_some(),
            ),
        ];
        let mut betas: Vec<String> = rules
            .into_iter()
            .filter(|(_, used)| *used)
            .map(|(name, _)| name.to_owned())
            .collect();
        for extra in explicit {
            if !betas.contains(extra) {
                betas.push(extra.clone());
            }
        }
        betas
    }

    fn items(value: &Value) -> impl Iterator<Item = &Value> {
        value.as_array().into_iter().flatten()
    }

    fn uses_cache_scope(body: &Value) -> bool {
        let scoped = |item: &Value| item["cache_control"].get("scope").is_some();
        items(&body["system"]).any(scoped)
            || items(&body["tools"]).any(scoped)
            || items(&body["messages"])
                .flat_map(|message| items(&message["content"]))
                .any(scoped)
    }
}

fn cache(ttl: Option<&str>, scope: Option<&str>) -> Option<CacheControl> {
    Some(CacheControl {
        kind: "ephemeral".to_owned(),
        ttl: ttl.map(str::to_owned),
        scope: scope.map(str::to_owned),
    })
}

fn item(block: ContentBlock, cache_control: Option<CacheControl>) -> ApiContent {
    ApiContent {
        block,
        cache_control,
    }
}

fn message(role: ApiRole, content: Vec<ApiContent>, effort: Option<&str>) -> ApiMessage {
    ApiMessage {
        role,
        content,
        output_config: effort.map(|effort| TurnOutputConfig {
            effort: Some(effort.to_owned()),
        }),
    }
}

/// 字符串与数字的边角：转义、多字节、数组下标形状的键（JS 先排）、浮点与大整数。
fn tricky() -> Value {
    serde_json::from_str(
        r#"{"file_path":"/w/a \"b\"\\c\n\t\u0001\u001f\u007f é中😀\u2028","2":"two","1":[1.0,-0.0,1e21,
        0.1,9007199254740993,18446744073709551615,-9223372036854775808],"nested":{"10":{},"0":[null,true,false]},
        "":"","limit":120000.0}"#,
    )
    .unwrap()
}

fn image() -> ImageSource {
    ImageSource::Base64 {
        media_type: "image/png".to_owned(),
        data: "iVBORw0KGgo=".to_owned(),
    }
}

/// 每种块、每种缓存断点、对话中间的 system 与消息级 effort。
fn shapes() -> Vec<ApiMessage> {
    use ApiRole::{Assistant, System, User};
    vec![
        message(User, Vec::new(), None),
        message(
            User,
            vec![
                item(
                    ContentBlock::Text {
                        text: "hi \"there\"\n".into(),
                    },
                    None,
                ),
                item(ContentBlock::Image { source: image() }, cache(None, None)),
            ],
            None,
        ),
        message(
            Assistant,
            vec![
                item(
                    ContentBlock::Thinking {
                        thinking: "hmm\u{1}".into(),
                        signature: Some("sig==".into()),
                    },
                    None,
                ),
                item(
                    ContentBlock::Thinking {
                        thinking: String::new(),
                        signature: None,
                    },
                    None,
                ),
                item(ContentBlock::RedactedThinking { data: "x".into() }, None),
                item(ContentBlock::Text { text: "a".into() }, None),
                item(
                    ContentBlock::ToolUse {
                        id: ToolUseId::new("toolu_1"),
                        name: "Read".into(),
                        input: tricky(),
                    },
                    cache(Some("1h"), None),
                ),
                item(
                    ContentBlock::ServerToolUse {
                        id: "srvtoolu_1".into(),
                        name: "web_search".into(),
                        input: json!({"query": "rust"}),
                    },
                    None,
                ),
                item(
                    ContentBlock::WebSearchToolResult {
                        tool_use_id: "srvtoolu_1".into(),
                        content: json!([{"type": "web_search_result", "url": "https://example.com", "2": 2, "1": 1}]),
                    },
                    None,
                ),
            ],
            None,
        ),
        message(
            User,
            vec![
                item(
                    ContentBlock::ToolResult {
                        tool_use_id: ToolUseId::new("toolu_1"),
                        content: vec![
                            ToolResultContent::Text {
                                text: "line\r\n\u{0}".into(),
                            },
                            ToolResultContent::Image { source: image() },
                            ToolResultContent::ToolReference {
                                tool_name: "WebFetch".into(),
                            },
                        ],
                        is_error: false,
                    },
                    None,
                ),
                item(
                    ContentBlock::ToolResult {
                        tool_use_id: ToolUseId::new("toolu_2"),
                        content: Vec::new(),
                        is_error: true,
                    },
                    cache(Some("5m"), Some("global")),
                ),
            ],
            None,
        ),
        message(
            System,
            vec![item(
                ContentBlock::Text {
                    text: "# Env".into(),
                },
                None,
            )],
            Some("high"),
        ),
        message(
            User,
            vec![item(ContentBlock::Text { text: "x".into() }, None)],
            None,
        ),
    ]
}

/// 请求的几种形状：只有消息；带思考、温度、上下文编辑、effort、诊断与显式 beta。
fn requests() -> Vec<(ModelRequest, bool)> {
    let mut plain = vec![(request(shapes()), false)];
    let mut full = request(shapes());
    full.thinking = Some(ThinkingConfig::Adaptive {
        display: Some("summarized".into()),
    });
    full.temperature = Some(0.7);
    full.options.context_edits = vec![ContextEdit::ClearThinking { keep: "all".into() }];
    full.options.effort = Some("medium".into());
    full.options.user_id = Some("user_1".into());
    full.betas = vec!["context-1m-2025-08-07".into()];
    plain.push((full, true));
    // 每种消息单独一个请求（beta 的判断按消息）。
    for shape in shapes() {
        plain.push((request(vec![shape]), true));
    }
    plain
}

/// Provider 补丁：没有、只改顶层、带数组下标形状的键、显式改 `messages`、删掉 `messages`、整个换掉请求体。
fn patches() -> Vec<Vec<Value>> {
    vec![
        Vec::new(),
        vec![json!({"thinking": {"type": "enabled", "display": null}, "max_tokens": 128000})],
        vec![json!({"1": true, "output_config": {"effort": "high"}})],
        vec![json!({"messages": [{"role": "user", "content": "patched"}]})],
        vec![json!({"messages": {"0": {"role": "system"}}})],
        vec![json!({"messages": null, "stream": null})],
        vec![json!({"tools": []}), json!("not an object")],
    ]
}

#[test]
fn the_body_and_betas_match_the_value_encoder_byte_for_byte() {
    let mut compared = 0;
    for (request, first_party) in requests() {
        for patches in patches() {
            let mut old = request.clone();
            let explicit = std::mem::take(&mut old.betas);
            let value = reference::body_value(old, first_party, &patches).unwrap();
            let expected_betas = reference::betas(&value, &explicit);
            let expected = rust_cli_base::js::json::stringify(&value);
            let encoded = super::encode(
                request.clone(),
                super::tests::profile(first_party),
                &patches,
                &explicit,
            )
            .unwrap();
            assert_eq!(encoded.body, expected, "patches {patches:?}");
            assert_eq!(encoded.betas, expected_betas, "patches {patches:?}");
            compared += 1;
        }
    }
    // 8 个请求（两种整体形状加 6 种单条消息）× 7 组补丁。
    assert_eq!(compared, 8 * 7);
}

/// count_tokens 的消息值与旧写法的值相同（数字按 JS 写出再读回，字节相同）。
#[test]
fn count_tokens_messages_match_the_value_encoder() {
    for shape in shapes() {
        let old = reference::message(shape.clone()).unwrap();
        let new = super::message(shape).unwrap();
        assert_eq!(
            rust_cli_base::js::json::stringify(&new),
            rust_cli_base::js::json::stringify(&old)
        );
    }
}

/// Artifact 图片在两种写法里都报同一个错。
#[test]
fn unmaterialized_images_fail_the_same_way() {
    let artifact = ImageSource::Artifact {
        media_type: "image/png".to_owned(),
        artifact: rust_cli_kernel_api::store::ArtifactRef {
            session: rust_cli_kernel_api::ids::SessionId::new("sess_1"),
            id: rust_cli_kernel_api::ids::ArtifactId::new("a.png"),
            purpose: rust_cli_kernel_api::store::ArtifactPurpose::Media,
            media_type: "image/png".to_owned(),
            size: 1,
        },
    };
    let request = request(vec![message(
        ApiRole::User,
        vec![item(ContentBlock::Image { source: artifact }, None)],
        None,
    )]);
    let old = reference::body_value(request.clone(), false, &[]).unwrap_err();
    let new = super::encode(request, super::tests::profile(false), &[], &[])
        .err()
        .unwrap();
    assert_eq!(new.to_string(), old.to_string());
}
