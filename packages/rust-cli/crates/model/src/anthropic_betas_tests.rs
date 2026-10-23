//! `anthropic-beta` 的生成条件（model.md §3.1）：每个 beta 只在请求用到对应特性时出现。

use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ApiContent, ApiMessage, CacheControl, ContextEdit, SystemBlock, ThinkingConfig, ToolFlags,
    TurnOutputConfig,
};

use rust_cli_kernel_api::model::{ApiRole, ModelRequest};

use super::*;
use crate::anthropic_request::tests::{profile, request};

fn names(request: &ModelRequest, first_party: bool) -> Vec<String> {
    let mut request = request.clone();
    let explicit = std::mem::take(&mut request.betas);
    crate::anthropic_request::encode(request, profile(first_party), &[], &explicit)
        .unwrap()
        .betas
}

fn tool(name: &str) -> rust_cli_kernel_api::model::ToolDefinition {
    rust_cli_kernel_api::model::ToolDefinition {
        name: name.to_owned(),
        description: String::new(),
        input_schema: serde_json::json!({"type": "object"}),
        cache_control: None,
    }
}

fn system_message() -> ApiMessage {
    ApiMessage {
        role: ApiRole::System,
        content: vec![ApiContent {
            block: ContentBlock::Text {
                text: "# Environment".to_owned(),
            },
            cache_control: None,
        }],
        output_config: None,
    }
}

#[test]
fn a_plain_request_needs_no_beta() {
    assert!(names(&request(Vec::new()), false).is_empty());
    // 官方端点的主循环请求带缓存诊断（写 diagnostics）。
    assert_eq!(names(&request(Vec::new()), true), [CACHE_DIAGNOSIS]);
}

/// 一个用例：名字、对请求的改动、期望的 beta。
type Case = (
    &'static str,
    Box<dyn Fn(&mut ModelRequest)>,
    Vec<&'static str>,
);

#[test]
fn each_feature_brings_its_beta() {
    let cases: Vec<Case> = vec![
        (
            "enabled thinking",
            Box::new(|r| {
                r.thinking = Some(ThinkingConfig::Enabled {
                    budget_tokens: 1024,
                })
            }),
            vec![INTERLEAVED_THINKING, THINKING_TOKEN_COUNT],
        ),
        (
            "adaptive thinking with display",
            Box::new(|r| {
                r.thinking = Some(ThinkingConfig::Adaptive {
                    display: Some("updates".to_owned()),
                });
            }),
            vec![
                INTERLEAVED_THINKING,
                THINKING_TOKEN_COUNT,
                THINKING_DISPLAY_UPDATES,
            ],
        ),
        (
            "disabled thinking",
            Box::new(|r| r.thinking = Some(ThinkingConfig::Disabled)),
            vec![],
        ),
        (
            "context edits",
            Box::new(|r| {
                r.options.context_edits = vec![ContextEdit::ClearThinking {
                    keep: "all".to_owned(),
                }];
            }),
            vec![CONTEXT_MANAGEMENT],
        ),
        (
            "scoped cache",
            Box::new(|r| {
                r.system = vec![SystemBlock {
                    text: "static".to_owned(),
                    cache_control: Some(CacheControl {
                        kind: "ephemeral".to_owned(),
                        ttl: None,
                        scope: Some("global".to_owned()),
                    }),
                }];
            }),
            vec![PROMPT_CACHING_SCOPE],
        ),
        (
            "unscoped cache",
            Box::new(|r| {
                r.system = vec![SystemBlock {
                    text: "static".to_owned(),
                    cache_control: Some(CacheControl {
                        kind: "ephemeral".to_owned(),
                        ttl: Some("1h".to_owned()),
                        scope: None,
                    }),
                }];
            }),
            vec![],
        ),
        (
            "system message",
            Box::new(|r| r.messages.push(system_message())),
            vec![MID_CONVERSATION_SYSTEM],
        ),
        (
            "effort",
            Box::new(|r| r.options.effort = Some("medium".to_owned())),
            vec![EFFORT],
        ),
        (
            "per-message effort",
            Box::new(|r| {
                r.messages.push(ApiMessage {
                    output_config: Some(TurnOutputConfig {
                        effort: Some("low".to_owned()),
                    }),
                    ..system_message()
                });
            }),
            vec![MID_CONVERSATION_SYSTEM, PER_TURN_CONTROL, EFFORT],
        ),
        (
            "deferred tool",
            Box::new(|r| {
                let flags = ToolFlags {
                    defer_loading: true,
                    ..ToolFlags::default()
                };
                r.tools.push(tool("Placeholder"));
                r.options.tool_flags.insert("Placeholder".to_owned(), flags);
                // 标记只作用于请求里有的工具。
                r.options.tool_flags.insert("Missing".to_owned(), flags);
            }),
            vec![ADVANCED_TOOL_USE],
        ),
        (
            "strict tool only",
            Box::new(|r| {
                let flags = ToolFlags {
                    strict: true,
                    ..ToolFlags::default()
                };
                r.options.tool_flags.insert("Read".to_owned(), flags);
            }),
            vec![],
        ),
    ];
    for (name, apply, expected) in cases {
        let mut request = request(Vec::new());
        apply(&mut request);
        assert_eq!(names(&request, false), expected, "{name}");
    }
}

#[test]
fn cache_diagnosis_follows_the_endpoint_and_the_source() {
    for (source, expected) in [
        (QuerySource::MainThread, true),
        (QuerySource::Subagent("Explore".to_owned()), true),
        (QuerySource::Compact, true),
        (QuerySource::Side("hook_agent".to_owned()), true),
        (QuerySource::Side("title".to_owned()), false),
    ] {
        let mut request = request(Vec::new());
        request.source = source.clone();
        assert_eq!(
            names(&request, true).contains(&CACHE_DIAGNOSIS.to_owned()),
            expected,
            "{source:?}"
        );
        assert!(names(&request, false).is_empty(), "{source:?}");
    }
}

#[test]
fn explicit_betas_follow_without_duplicates() {
    let mut request = request(Vec::new());
    request.options.effort = Some("high".to_owned());
    request.betas = vec![
        EFFORT.to_owned(),
        "context-1m-2025-08-07".to_owned(),
        "context-1m-2025-08-07".to_owned(),
    ];
    assert_eq!(names(&request, false), [EFFORT, "context-1m-2025-08-07"]);
}

#[test]
fn provider_patches_bring_their_betas() {
    // GLM 的 option map 给 thinking 与 effort：补丁合并后的请求体决定 beta。
    let request = request(Vec::new());
    let patches =
        [serde_json::json!({"thinking": {"type": "enabled"}, "output_config": {"effort": "high"}})];
    let encoded = crate::anthropic_request::encode(request, profile(false), &patches, &[]).unwrap();
    assert_eq!(
        encoded.betas,
        [INTERLEAVED_THINKING, THINKING_TOKEN_COUNT, EFFORT]
    );
}
