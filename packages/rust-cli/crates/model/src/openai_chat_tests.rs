//! OpenAI Chat Completions 请求体（model.md §10.1）。

use rust_cli_base::models;
use rust_cli_kernel_api::model::{ApiRole, SystemBlock, ThinkingConfig};
use serde_json::json;

use super::*;
use crate::anthropic_request::tests::request;
use crate::openai_lower::tests::{conversation, message, text};

fn read_tool() -> ToolDefinition {
    ToolDefinition {
        name: "Read".to_owned(),
        description: "Read a file".to_owned(),
        input_schema: json!({"type": "object", "properties": {"file_path": {"type": "string"}}}),
        cache_control: None,
    }
}

#[test]
fn a_conversation_encodes_in_the_node_order() {
    let mut request = request(conversation());
    request.system = vec![SystemBlock {
        text: "You are ZCode.".to_owned(),
        cache_control: None,
    }];
    request.tools = vec![read_tool()];
    request.temperature = Some(0.7);
    // Anthropic 专属的选项在 Chat 上不出现。
    request.thinking = Some(ThinkingConfig::Adaptive {
        display: Some("updates".to_owned()),
    });
    request.options.effort = Some("medium".to_owned());
    request.betas = vec!["effort-2025-11-24".to_owned()];
    let body = body(models::GLM_5, request, &[]).unwrap();
    assert_eq!(
        body,
        concat!(
            r#"{"model":"<model>","messages":["#,
            r#"{"role":"system","content":"You are ZCode."},"#,
            r#"{"role":"user","content":[{"type":"text","text":"<system-reminder>ctx</system-reminder>"},{"type":"text","text":"read a.png"},{"type":"text","text":"<system-reminder>\n# Environment\n</system-reminder>"}]},"#,
            r#"{"role":"assistant","content":null,"reasoning_content":"need the file","tool_calls":[{"id":"toolu_1","type":"function","function":{"name":"Read","arguments":"{\"file_path\":\"a.png\"}"}}]},"#,
            r#"{"role":"tool","tool_call_id":"toolu_1","content":"line 1\nline 2"},"#,
            r#"{"role":"user","content":[{"type":"text","text":"<system-reminder>\nlate note\n</system-reminder>"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}},{"type":"text","text":"thanks"}]},"#,
            r#"{"role":"assistant","content":"It is a cat."}],"#,
            r#""tools":[{"type":"function","function":{"name":"Read","description":"Read a file","parameters":{"type":"object","properties":{"file_path":{"type":"string"}}}}}],"#,
            r#""tool_choice":"auto","stream":true,"stream_options":{"include_usage":true},"max_tokens":32000,"temperature":0.7}"#,
        )
        .replace("<model>", models::GLM_5)
    );
}

#[test]
fn a_single_text_turn_is_a_string_and_tools_are_optional() {
    let request = request(vec![message(ApiRole::User, vec![text("hi")])]);
    assert_eq!(
        body("m", request, &[]).unwrap(),
        r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":true},"max_tokens":32000}"#
    );
}

#[test]
fn tool_choice_maps_to_the_chat_values() {
    for (choice, expected) in [
        (ToolChoice::Any, r#""tool_choice":"required""#),
        (ToolChoice::None, r#""tool_choice":"none""#),
        (
            ToolChoice::Tool {
                name: "Read".to_owned(),
            },
            r#""tool_choice":{"type":"function","function":{"name":"Read"}}"#,
        ),
    ] {
        let mut request = request(vec![message(ApiRole::User, vec![text("hi")])]);
        request.tools = vec![read_tool()];
        request.tool_choice = Some(choice);
        let body = body("m", request, &[]).unwrap();
        assert!(body.contains(expected), "{body}");
    }
}

#[test]
fn provider_patches_set_reasoning_and_the_completion_budget() {
    let request = request(vec![message(ApiRole::User, vec![text("hi")])]);
    let patches = [
        json!({"thinking": {"type": "enabled"}, "reasoning_effort": "high"}),
        json!({"max_completion_tokens": 128000}),
    ];
    // 补丁给出 max_completion_tokens 时去掉默认的 max_tokens。
    assert_eq!(
        body("m", request, &patches).unwrap(),
        r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":true},"thinking":{"type":"enabled"},"reasoning_effort":"high","max_completion_tokens":128000}"#
    );
}

#[test]
fn server_tools_are_unsupported_on_openai() {
    let mut request = request(vec![message(ApiRole::User, vec![text("hi")])]);
    request.options.server_tools = vec![rust_cli_kernel_api::model::ServerTool::WebSearch {
        allowed_domains: None,
        blocked_domains: None,
        max_uses: 8,
    }];
    assert!(matches!(
        body("m", request.clone(), &[]),
        Err(ModelError::Unsupported { .. })
    ));
    assert!(matches!(
        crate::openai_responses::body("m", request, &[]),
        Err(ModelError::Unsupported { .. })
    ));
}

#[test]
fn documents_are_file_parts_with_the_ai_sdk_name() {
    let request = request(crate::openai_lower::tests::pdf_result());
    let body = body(models::GPT_5_6, request, &[]).unwrap();
    assert!(
        body.contains(r#"{"role":"user","content":[{"type":"file","file":{"filename":"document.pdf","file_data":"data:application/pdf;base64,JVBERi0="}}]}"#),
        "{body}"
    );
}
