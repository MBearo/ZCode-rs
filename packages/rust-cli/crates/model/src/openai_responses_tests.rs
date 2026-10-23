//! OpenAI Responses 请求体（model.md §10.2）。

use rust_cli_kernel_api::model::{ApiRole, SystemBlock};
use serde_json::json;

use super::*;
use crate::anthropic_request::tests::request;
use crate::openai_lower::tests::{conversation, message, text};

#[test]
fn a_conversation_encodes_in_the_node_order() {
    let mut request = request(conversation());
    request.system = vec![SystemBlock {
        text: "You are ZCode.".to_owned(),
        cache_control: None,
    }];
    request.tools = vec![ToolDefinition {
        name: "Read".to_owned(),
        description: "Read a file".to_owned(),
        input_schema: json!({"type": "object"}),
        cache_control: None,
    }];
    let body = body("qwen3.8-max", request, &[]).unwrap();
    assert_eq!(
        body,
        concat!(
            r#"{"model":"qwen3.8-max","stream":true,"store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":32000,"input":["#,
            r#"{"role":"system","content":"You are ZCode."},"#,
            r#"{"role":"user","content":[{"type":"input_text","text":"<system-reminder>ctx</system-reminder>"},{"type":"input_text","text":"read a.png"},{"type":"input_text","text":"<system-reminder>\n# Environment\n</system-reminder>"}]},"#,
            r#"{"type":"function_call","call_id":"toolu_1","name":"Read","arguments":"{\"file_path\":\"a.png\"}"},"#,
            r#"{"type":"function_call_output","call_id":"toolu_1","output":"line 1\nline 2"},"#,
            r#"{"role":"user","content":[{"type":"input_text","text":"<system-reminder>\nlate note\n</system-reminder>"},{"type":"input_image","image_url":"data:image/png;base64,AAAA"},{"type":"input_text","text":"thanks"}]},"#,
            r#"{"role":"assistant","content":[{"type":"output_text","text":"It is a cat."}]}],"#,
            r#""tools":[{"type":"function","name":"Read","description":"Read a file","parameters":{"type":"object"},"strict":false}],"tool_choice":"auto"}"#,
        )
    );
}

#[test]
fn reasoning_models_get_the_developer_role_and_patches_apply() {
    let mut request = request(vec![message(ApiRole::User, vec![text("hi")])]);
    request.system = vec![SystemBlock {
        text: "sys".to_owned(),
        cache_control: None,
    }];
    let patches = [json!({"reasoning": {"effort": "high"}, "max_output_tokens": 64000})];
    let body = body("gpt-5.4", request.clone(), &patches).unwrap();
    assert_eq!(
        body,
        r#"{"model":"gpt-5.4","stream":true,"store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":64000,"input":[{"role":"developer","content":"sys"},{"role":"user","content":[{"type":"input_text","text":"hi"}]}],"reasoning":{"effort":"high"}}"#
    );
    assert!(
        super::body("gpt-5-chat-latest", request, &[])
            .unwrap()
            .contains(r#"{"role":"system","content":"sys"}"#)
    );
}

#[test]
fn documents_are_input_files_with_the_ai_sdk_name() {
    let request = request(crate::openai_lower::tests::pdf_result());
    let body = body("gpt-5.6", request, &[]).unwrap();
    assert!(
        body.contains(r#"{"role":"user","content":[{"type":"input_file","filename":"document.pdf","file_data":"data:application/pdf;base64,JVBERi0="}]}"#),
        "{body}"
    );
}
