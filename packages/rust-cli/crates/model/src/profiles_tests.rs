//! 每个请求方案的请求体字节快照（model.md §3.2 的表，修复线 F23）：同一个主循环请求发到各方案的端点。
//!
//! 卡片模型（claude-opus-5-5）带 query 按卡片写的卡片字段；没有卡片的模型（GLM、deepseek）不带（query 修 bug 之后，没有卡片
//! 不写 thinking / effort / context_management）。补丁是内置配置的 option map 在这个档位上的结果。

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, ApiRole, CacheControl, ContextEdit, ModelRequest, ModelSelection,
    QuerySource, RequestOptions, SystemBlock, ThinkingConfig, ToolDefinition, ToolFlags,
    TurnOutputConfig,
};
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_net::HeaderValue;
use serde_json::{Value, json};

use crate::protocol::for_wire;

fn cache(scope: Option<&str>) -> Option<CacheControl> {
    Some(CacheControl {
        kind: "ephemeral".to_owned(),
        ttl: None,
        scope: scope.map(str::to_owned),
    })
}

fn text(text: &str, cache_control: Option<CacheControl>) -> ApiContent {
    ApiContent {
        block: ContentBlock::Text {
            text: text.to_owned(),
        },
        cache_control,
    }
}

/// 主循环的请求：第一方 system（context 只产生这一种）、一条人类输入、对话中间的环境段、一个普通工具与一个延迟工具。
/// `carded` 为真时带 query 按卡片写的卡片字段（adaptive thinking、effort、context_management、每轮 effort）。
fn main_request(model: &str, carded: bool) -> ModelRequest {
    let turn = carded.then(|| TurnOutputConfig {
        effort: Some("medium".to_owned()),
    });
    let mut flags = std::collections::BTreeMap::new();
    flags.insert(
        "WebFetch".to_owned(),
        ToolFlags {
            defer_loading: true,
            ..ToolFlags::default()
        },
    );
    let tool = |name: &str| ToolDefinition {
        name: name.to_owned(),
        description: "d".to_owned(),
        input_schema: json!({"type": "object"}),
        cache_control: None,
    };
    ModelRequest {
        selection: ModelSelection {
            provider: "p".to_owned(),
            model: model.to_owned(),
            reasoning: None,
            connection: None,
        },
        system: vec![
            SystemBlock {
                text: "I".to_owned(),
                cache_control: None,
            },
            SystemBlock {
                text: "S".to_owned(),
                cache_control: cache(Some("global")),
            },
            SystemBlock {
                text: "D".to_owned(),
                cache_control: cache(None),
            },
        ],
        messages: vec![
            ApiMessage {
                role: ApiRole::User,
                content: vec![text("hi", None)],
                output_config: None,
            },
            ApiMessage {
                role: ApiRole::System,
                content: vec![text("# Env", cache(None))],
                output_config: turn,
            },
        ],
        tools: vec![tool("Read"), tool("WebFetch")],
        tool_choice: None,
        max_tokens: if carded { 128_000 } else { 32_000 },
        thinking: carded.then(|| ThinkingConfig::Adaptive {
            display: Some("updates".to_owned()),
        }),
        temperature: None,
        betas: Vec::new(),
        source: QuerySource::MainThread,
        options: RequestOptions {
            effort: carded.then(|| "medium".to_owned()),
            context_edits: if carded {
                vec![ContextEdit::ClearThinking {
                    keep: "all".to_owned(),
                }]
            } else {
                Vec::new()
            },
            tool_flags: flags,
            ..RequestOptions::default()
        },
    }
}

fn endpoint(
    protocol: WireProtocol,
    base_url: &str,
    model: &str,
    patches: Vec<Value>,
) -> ModelEndpoint {
    ModelEndpoint {
        protocol,
        base_url: base_url.to_owned(),
        model: model.to_owned(),
        credential: Credential::ApiKey(RedactedString::new("sk-test")),
        headers: Vec::new(),
        body_patches: patches,
        mid_conversation_system: true,
        supports_image: true,
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        renewable: false,
    }
}

/// （方案名，请求体，`anthropic-beta`）。
fn encode(
    endpoint: &ModelEndpoint,
    request: ModelRequest,
) -> (&'static str, String, Option<String>) {
    let profile = crate::profiles::select(endpoint.protocol, &endpoint.base_url, &endpoint.model);
    let http = for_wire(endpoint.protocol)
        .encode(endpoint, request, profile)
        .unwrap();
    let beta = http.headers.iter().find_map(|(name, value)| match value {
        HeaderValue::Plain(value) if name == "anthropic-beta" => Some(value.clone()),
        _ => None,
    });
    (
        profile.name,
        String::from_utf8(http.body.to_vec()).unwrap(),
        beta,
    )
}

/// GLM-5.1 在 `enabled` 档位上的 option map（内置配置：`thinking` 随档位，没有 effort）与输出上限。
fn glm_patches() -> Vec<Value> {
    vec![
        json!({"thinking": {"type": "enabled"}}),
        json!({"max_tokens": 64000}),
    ]
}

fn check(
    endpoint: &ModelEndpoint,
    request: ModelRequest,
    profile: &str,
    body: &str,
    beta: Option<&str>,
) {
    let (name, ours, ours_beta) = encode(endpoint, request);
    assert_eq!(name, profile);
    assert_eq!(ours, body);
    assert_eq!(ours_beta.as_deref(), beta);
}

/// Anthropic 官方上的 claude-opus-5-5：原样（第一方 system、`diagnostics`），beta 按特性。
#[test]
fn anthropic_official_carded_model() {
    check(
        &endpoint(
            WireProtocol::AnthropicMessages,
            "https://api.anthropic.com",
            "claude-opus-5-5",
            Vec::new(),
        ),
        main_request("claude-opus-5-5", true),
        "anthropic-official",
        r##"{"model":"claude-opus-5-5","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},{"role":"system","content":[{"type":"text","text":"# Env","cache_control":{"type":"ephemeral"}}],"output_config":{"effort":"medium"}}],"system":[{"type":"text","text":"I"},{"type":"text","text":"S","cache_control":{"type":"ephemeral","scope":"global"}},{"type":"text","text":"D","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"Read","description":"d","input_schema":{"type":"object"}},{"name":"WebFetch","description":"d","input_schema":{"type":"object"},"defer_loading":true}],"max_tokens":128000,"thinking":{"type":"adaptive","display":"updates"},"context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]},"output_config":{"effort":"medium"},"diagnostics":{"previous_message_id":null},"stream":true}"##,
        Some(
            "interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,thinking-token-count-2026-05-13,per-turn-control-2026-07-01,effort-2025-11-24,advanced-tool-use-2025-11-20,thinking-display-updates-2026-08-18,cache-diagnosis-2026-04-07",
        ),
    );
}

/// z.ai 上的 GLM（没有卡片）：第一方 system 与 `diagnostics`（用户决定：一方都发）；没有 query 写的 effort（修 bug 之后），`thinking` 与 `max_tokens` 只来自 option map。
#[test]
fn zhipu_glm_on_z_ai() {
    check(
        &endpoint(
            WireProtocol::AnthropicMessages,
            "https://api.z.ai/api/anthropic",
            "glm-5.1",
            glm_patches(),
        ),
        main_request("glm-5.1", false),
        "zhipu-official",
        r##"{"model":"glm-5.1","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},{"role":"system","content":[{"type":"text","text":"# Env","cache_control":{"type":"ephemeral"}}]}],"system":[{"type":"text","text":"I"},{"type":"text","text":"S","cache_control":{"type":"ephemeral","scope":"global"}},{"type":"text","text":"D","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"Read","description":"d","input_schema":{"type":"object"}},{"name":"WebFetch","description":"d","input_schema":{"type":"object"},"defer_loading":true}],"max_tokens":64000,"diagnostics":{"previous_message_id":null},"stream":true,"thinking":{"type":"enabled"}}"##,
        Some(
            "interleaved-thinking-2025-05-14,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,thinking-token-count-2026-05-13,advanced-tool-use-2025-11-20,cache-diagnosis-2026-04-07",
        ),
    );
}

/// bigmodel 上的 GLM：同 z.ai。
#[test]
fn zhipu_glm_on_bigmodel() {
    check(
        &endpoint(
            WireProtocol::AnthropicMessages,
            "https://open.bigmodel.cn/api/anthropic",
            "glm-5.1",
            glm_patches(),
        ),
        main_request("glm-5.1", false),
        "zhipu-official",
        r##"{"model":"glm-5.1","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},{"role":"system","content":[{"type":"text","text":"# Env","cache_control":{"type":"ephemeral"}}]}],"system":[{"type":"text","text":"I"},{"type":"text","text":"S","cache_control":{"type":"ephemeral","scope":"global"}},{"type":"text","text":"D","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"Read","description":"d","input_schema":{"type":"object"}},{"name":"WebFetch","description":"d","input_schema":{"type":"object"},"defer_loading":true}],"max_tokens":64000,"diagnostics":{"previous_message_id":null},"stream":true,"thinking":{"type":"enabled"}}"##,
        Some(
            "interleaved-thinking-2025-05-14,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,thinking-token-count-2026-05-13,advanced-tool-use-2025-11-20,cache-diagnosis-2026-04-07",
        ),
    );
}

/// DeepSeek 官方：一方；effort 来自 option map（`max` 档位），beta 随之有 `effort`。
#[test]
fn deepseek_official() {
    check(
        &endpoint(
            WireProtocol::AnthropicMessages,
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-pro",
            vec![
                json!({"thinking": {"type": "enabled"}, "output_config": {"effort": "max"}}),
                json!({"max_tokens": 128000}),
            ],
        ),
        main_request("deepseek-v4-pro", false),
        "deepseek-official",
        r##"{"model":"deepseek-v4-pro","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},{"role":"system","content":[{"type":"text","text":"# Env","cache_control":{"type":"ephemeral"}}]}],"system":[{"type":"text","text":"I"},{"type":"text","text":"S","cache_control":{"type":"ephemeral","scope":"global"}},{"type":"text","text":"D","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"Read","description":"d","input_schema":{"type":"object"}},{"name":"WebFetch","description":"d","input_schema":{"type":"object"},"defer_loading":true}],"max_tokens":128000,"diagnostics":{"previous_message_id":null},"stream":true,"thinking":{"type":"enabled"},"output_config":{"effort":"max"}}"##,
        Some(
            "interleaved-thinking-2025-05-14,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,thinking-token-count-2026-05-13,effort-2025-11-24,advanced-tool-use-2025-11-20,cache-diagnosis-2026-04-07",
        ),
    );
}

/// 自建网关（经它转到 Bedrock）上的 claude-opus-5-5：其他 Anthropic 兼容。第三方 system、没有 `scope` 与 `diagnostics`，卡片的 thinking / effort / context_management / 每轮 effort 去掉，option map 照常；beta 照 Node（对话中间的 system 消息、延迟工具，F64）。
#[test]
fn a_self_hosted_gateway() {
    check(
        &endpoint(
            WireProtocol::AnthropicMessages,
            "https://gw.example.com",
            "claude-opus-5-5",
            vec![
                json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "max"}}),
                json!({"max_tokens": 128000}),
            ],
        ),
        main_request("claude-opus-5-5", true),
        "anthropic-compatible",
        r##"{"model":"claude-opus-5-5","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},{"role":"system","content":[{"type":"text","text":"# Env","cache_control":{"type":"ephemeral"}}]}],"system":[{"type":"text","text":"I","cache_control":{"type":"ephemeral"}},{"type":"text","text":"S\n\nD","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"Read","description":"d","input_schema":{"type":"object"}},{"name":"WebFetch","description":"d","input_schema":{"type":"object"},"defer_loading":true}],"max_tokens":128000,"stream":true,"thinking":{"type":"adaptive"},"output_config":{"effort":"max"}}"##,
        Some("mid-conversation-system-2026-04-07,advanced-tool-use-2025-11-20"),
    );
}

/// OpenAI Chat：system 合成一条，对话中间的 system 降成提醒（model.md §10）。
#[test]
fn openai_chat() {
    check(
        &endpoint(
            WireProtocol::OpenAiChatCompletions,
            "https://api.example.com/v1",
            "glm-5.1",
            vec![
                json!({"reasoning_effort": "high"}),
                json!({"max_completion_tokens": 64000}),
            ],
        ),
        main_request("glm-5.1", false),
        "openai-chat",
        r##"{"model":"glm-5.1","messages":[{"role":"system","content":"I\n\nS\n\nD"},{"role":"user","content":[{"type":"text","text":"hi"},{"type":"text","text":"<system-reminder>\n# Env\n</system-reminder>"}]}],"tools":[{"type":"function","function":{"name":"Read","description":"d","parameters":{"type":"object"}}},{"type":"function","function":{"name":"WebFetch","description":"d","parameters":{"type":"object"}}}],"tool_choice":"auto","stream":true,"stream_options":{"include_usage":true},"reasoning_effort":"high","max_completion_tokens":64000}"##,
        None,
    );
}

/// OpenAI Responses。
#[test]
fn openai_responses() {
    check(
        &endpoint(
            WireProtocol::OpenAiResponses,
            "https://api.example.com/v1",
            "gpt-5",
            vec![
                json!({"reasoning": {"effort": "high"}}),
                json!({"max_output_tokens": 64000}),
            ],
        ),
        main_request("gpt-5", false),
        "openai-responses",
        r##"{"model":"gpt-5","stream":true,"store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":64000,"input":[{"role":"developer","content":"I\n\nS\n\nD"},{"role":"user","content":[{"type":"input_text","text":"hi"},{"type":"input_text","text":"<system-reminder>\n# Env\n</system-reminder>"}]}],"tools":[{"type":"function","name":"Read","description":"d","parameters":{"type":"object"},"strict":false},{"type":"function","name":"WebFetch","description":"d","parameters":{"type":"object"},"strict":false}],"tool_choice":"auto","reasoning":{"effort":"high"}}"##,
        None,
    );
}

/// 发给非官方 Anthropic 端点（其他 Anthropic 兼容）的请求里没有哪些字段（修复线 F23，贴近 Node 在同一端点上的
/// 请求）：没有 option map 补丁时（显式覆盖那样），卡片字段一个不剩，beta 照 Node（F64）：对话中间的 system 消息与
/// 延迟工具各一项；调用方显式给的 beta 不发。
#[test]
fn a_gateway_request_has_none_of_the_card_fields() {
    for base_url in [
        "https://gw.example.com",
        "http://127.0.0.1:8080",
        "https://bedrock-gateway.example.com/anthropic",
    ] {
        let gateway = endpoint(
            WireProtocol::AnthropicMessages,
            base_url,
            "claude-opus-5-5",
            Vec::new(),
        );
        let (name, body, beta) = encode(&gateway, main_request("claude-opus-5-5", true));
        assert_eq!(name, "anthropic-compatible");
        for field in [
            r#""scope""#,
            r#""diagnostics""#,
            r#""context_management""#,
            r#""thinking""#,
            r#""output_config""#,
            r#""effort""#,
        ] {
            assert!(!body.contains(field), "{base_url}: {field} in {body}");
        }
        assert_eq!(
            beta.as_deref(),
            Some("mid-conversation-system-2026-04-07,advanced-tool-use-2025-11-20")
        );
        let mut plain = main_request("claude-opus-5-5", true);
        plain.options.tool_flags.clear();
        plain.messages.truncate(1);
        plain.betas = vec!["context-1m-2025-08-07".to_owned()];
        let (_, _, beta) = encode(&gateway, plain);
        assert_eq!(beta, None, "{base_url}");
    }
}

/// 修 bug（F64）：兼容端点照 Node 发 beta（AI SDK 3.0.81 的条件，model.md §3.1 的“兼容端点”）。以前只留
/// `advanced-tool-use` 与 `code-execution-web-tools` 两项：对话中间有 system 消息时少发 `mid-conversation-system`，
/// tool_result 里有 PDF 时少发 `pdfs`，SDK 能力表里支持 structured output 的模型（claude-opus-4-8 等）带工具时少发
/// `structured-outputs`（工作线 F71 起这一项只在 Anthropic 官方与智谱的端点上发，经网关不发）。
#[test]
fn a_gateway_sends_the_betas_node_sends() {
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::message::{ImageSource, ToolResultContent};
    use rust_cli_kernel_api::model::ServerTool;
    let gateway = |model: &str| {
        endpoint(
            WireProtocol::AnthropicMessages,
            "https://gw.example.com",
            model,
            Vec::new(),
        )
    };
    // 主循环的请求：对话中间的 system 消息、普通工具与延迟工具。智谱的端点上 SDK 能力表里的 claude-sonnet-4-5 带工具时照 Node 发
    // structured-outputs；经网关不发（工作线 F71）。
    let zhipu = endpoint(
        WireProtocol::AnthropicMessages,
        "https://open.bigmodel.cn/api/anthropic",
        "claude-sonnet-4-5-20250929",
        Vec::new(),
    );
    let (_, _, beta) = encode(&zhipu, main_request("claude-sonnet-4-5-20250929", true));
    assert_eq!(
        beta.as_deref(),
        Some(
            "mid-conversation-system-2026-04-07,structured-outputs-2025-11-13,advanced-tool-use-2025-11-20"
        )
    );
    let (_, _, beta) = encode(
        &gateway("claude-sonnet-4-5-20250929"),
        main_request("claude-sonnet-4-5-20250929", true),
    );
    assert_eq!(
        beta.as_deref(),
        Some("mid-conversation-system-2026-04-07,advanced-tool-use-2025-11-20")
    );
    // SDK 的能力表里没有的模型（claude-opus-5-5、GLM）不发 structured-outputs；`claude-sonnet-4-` 的其他版本也不发。
    for model in ["claude-opus-5-5", "glm-5.1", "claude-sonnet-4-0"] {
        let (_, _, beta) = encode(&gateway(model), main_request(model, true));
        assert_eq!(
            beta.as_deref(),
            Some("mid-conversation-system-2026-04-07,advanced-tool-use-2025-11-20"),
            "{model}"
        );
    }
    // tool_result 里的 PDF、Node 的 WebSearch 工具；没有普通工具时不发 structured-outputs。
    let mut request = main_request("claude-opus-4-1", false);
    request.messages.truncate(1);
    request.tools.clear();
    request.options.tool_flags.clear();
    request.messages.push(ApiMessage {
        role: ApiRole::User,
        content: vec![ApiContent {
            block: ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: vec![ToolResultContent::Document {
                    source: ImageSource::Base64 {
                        media_type: "application/pdf".to_owned(),
                        data: "JVBERi0=".to_owned(),
                    },
                }],
                is_error: false,
            },
            cache_control: None,
        }],
        output_config: None,
    });
    request.options.server_tools = vec![ServerTool::WebSearch {
        allowed_domains: None,
        blocked_domains: None,
        max_uses: 8,
    }];
    let mut pdf = gateway("claude-opus-4-1");
    pdf.supports_pdf = true;
    let (_, _, beta) = encode(&pdf, request);
    assert_eq!(
        beta.as_deref(),
        Some("pdfs-2024-09-25,code-execution-web-tools-2026-02-09")
    );
}
