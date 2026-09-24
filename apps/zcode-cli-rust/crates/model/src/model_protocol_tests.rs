use super::*;
#[test]
fn migrated_media_and_foreign_reasoning_use_protocol_projection() {
    let mut config: ModelConfig = serde_json::from_value(json!({"providerId":"next","modelId":"same-id","reasoningLevel":"none","baseUrl":"https://example.invalid"})).unwrap();
    let history = vec![
        json!({"role":"user","content":[{"type":"text","text":"files"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aW1n"}},{"type":"file","file":{"filename":"a.pdf","file_data":"data:application/pdf;base64,cGRm"}}]}),
        json!({"role":"assistant","content":"preserved","reasoning_content":"private","_zcode_origin":{"provider":"previous","model":"same-id"},"_zcode_anthropic_thinking":[{"type":"thinking","signature":"private","thinking":"private"}],"_zcode_responses_reasoning":[{"type":"reasoning","id":"rs","encrypted_content":"private","summary":[]}],"tool_calls":[{"id":"call","function":{"name":"Read","arguments":"{}"}}]}),
        json!({"role":"tool","tool_call_id":"call","content":"result"}),
    ];
    for api in [ApiType::Chat, ApiType::Responses, ApiType::Anthropic] {
        config.api_type = api;
        let body = body(&config, history.clone(), &[], None).unwrap();
        assert!(!body.to_string().contains("private"));
        assert!(body.to_string().contains("preserved"));
        assert!(body.to_string().contains("call"));
        if api == ApiType::Responses {
            assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
            assert_eq!(body["input"][0]["content"][2]["type"], "input_file");
        }
        if api == ApiType::Anthropic {
            assert_eq!(body["messages"][0]["content"][1]["source"]["data"], "aW1n");
            assert_eq!(body["messages"][0]["content"][2]["type"], "document");
        }
    }
    assert_eq!(history[1]["reasoning_content"], "private");
}
#[test]
fn anthropic_does_not_replay_reasoning_only_assistants() {
    let config: ModelConfig = serde_json::from_value(json!({"apiType":"anthropic-messages","providerId":"fixture","modelId":"fixture","reasoningLevel":"none","baseUrl":"https://example.invalid"})).unwrap();
    let messages = vec![
        json!({"role":"user","content":"task"}),
        json!({"role":"assistant","content":"", "reasoning_content":"thought", "_zcode_anthropic_thinking":[{"type":"thinking","thinking":"thought","signature":"signature"}]}),
        json!({"role":"user","content":"continue"}),
    ];
    let body = body(&config, messages.clone(), &[], None).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 2);
    assert_eq!(
        messages[1]["_zcode_anthropic_thinking"][0]["signature"],
        "signature"
    );
}
#[test]
fn anthropic_gateway_url_matches_ts_adapter() {
    for (base, expected) in [
        (
            "https://example.invalid",
            "https://example.invalid/v1/messages",
        ),
        (
            "https://example.invalid/gateway/",
            "https://example.invalid/gateway/v1/messages",
        ),
        (
            "https://example.invalid/V1//",
            "https://example.invalid/V1/messages",
        ),
    ] {
        assert_eq!(ApiType::Anthropic.url(base), expected);
    }
    assert_eq!(
        ApiType::Responses.url("https://example.invalid/api/"),
        "https://example.invalid/api/responses"
    );
    assert_eq!(
        ApiType::Chat.url("https://example.invalid/v1"),
        "https://example.invalid/v1/chat/completions"
    );
}
#[test]
fn serializers_preserve_tool_associations_and_private_reasoning() {
    let mut config: ModelConfig = serde_json::from_value(json!({"providerId":"fixture","modelId":"fixture","reasoningLevel":"none","baseUrl":"https://example.invalid"})).unwrap();
    let messages = vec![
        json!({"role":"system","content":"system"}),
        json!({"role":"assistant","content":"answer","reasoning_content":"thought","tool_calls":[{"id":"call","function":{"name":"Read","arguments":"{}"}}],"_zcode_responses_reasoning":[{"type":"reasoning","id":"rs","summary":[],"encrypted_content":"opaque"}],"_zcode_anthropic_thinking":[{"type":"thinking","thinking":"thought","signature":"signature"},{"type":"redacted_thinking","data":"redacted"}]}),
        json!({"role":"tool","tool_call_id":"call","content":"failed","_zcode_tool_failed":true}),
        json!({"role":"user","content":"follow up"}),
    ];
    let chat = body(&config, messages.clone(), &[], None).unwrap();
    assert_eq!(chat["messages"][1]["reasoning_content"], "thought");
    assert!(!chat.to_string().contains("_zcode_"));
    assert!(!chat.to_string().contains("opaque"));
    config.api_type = ApiType::Responses;
    let response = body(&config, messages.clone(), &[], None).unwrap();
    assert_eq!(response["input"][1]["encrypted_content"], "opaque");
    assert_eq!(response["input"][3]["call_id"], "call");
    assert_eq!(response["input"][4]["call_id"], "call");
    assert!(!response.to_string().contains("signature"));
    config.api_type = ApiType::Anthropic;
    let anthropic = body(&config, messages.clone(), &[], None).unwrap();
    assert_eq!(anthropic["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        anthropic["messages"][0]["content"][0]["signature"],
        "signature"
    );
    assert_eq!(anthropic["messages"][0]["content"][1]["data"], "redacted");
    assert_eq!(
        anthropic["messages"][1]["content"][0]["tool_use_id"],
        "call"
    );
    assert_eq!(anthropic["messages"][1]["content"][0]["is_error"], true);
    assert_eq!(anthropic["messages"][1]["content"][1]["text"], "follow up");
    assert!(anthropic.get("tools").is_none());
    assert!(!anthropic.to_string().contains("opaque"));
}
