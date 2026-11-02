//! 两种 OpenAI 协议共用的发送（model.md §10）。

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{ApiRole, ServerTool};
use rust_cli_kernel_api::provider::{ModelCapabilities, WireProtocol};

use super::*;
use crate::endpoint::{chat_completions_url, responses_url};
use crate::openai::lower::tests::{message, text};
use crate::wire::tests::request;

fn endpoint(protocol: WireProtocol) -> ModelEndpoint {
    ModelEndpoint {
        protocol,
        base_url: "https://api.example.com/v1".to_owned(),
        model: "m".to_owned(),
        credential: Credential::ApiKey(RedactedString::new("sk-test")),
        headers: Vec::new(),
        body_patches: Vec::new(),
        capabilities: ModelCapabilities::default(),
        renewable: false,
    }
}

#[test]
fn server_tools_are_unsupported_on_openai() {
    let mut request = request(vec![message(ApiRole::User, vec![text("hi")])]);
    request.options.server_tools = vec![ServerTool::WebSearch {
        allowed_domains: None,
        blocked_domains: None,
        max_uses: 8,
    }];
    assert!(matches!(
        encode(
            &endpoint(WireProtocol::OpenAiChatCompletions),
            request.clone(),
            chat_completions_url,
            chat::body,
        ),
        Err(ModelError::Unsupported { .. })
    ));
    assert!(matches!(
        encode(
            &endpoint(WireProtocol::OpenAiResponses),
            request,
            responses_url,
            responses::body,
        ),
        Err(ModelError::Unsupported { .. })
    ));
}

#[test]
fn both_protocols_post_json_with_the_header_timeout() {
    let request = request(vec![message(ApiRole::User, vec![text("hi")])]);
    for (url, body) in [
        (
            chat_completions_url as fn(&str) -> String,
            chat::body as Body,
        ),
        (responses_url, responses::body),
    ] {
        let http = encode(
            &endpoint(WireProtocol::OpenAiChatCompletions),
            request.clone(),
            url,
            body,
        )
        .unwrap();
        assert_eq!(http.url, url("https://api.example.com/v1"));
        assert_eq!(http.timeout, Some(HEADER_TIMEOUT));
    }
}
