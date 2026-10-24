//! 来源头的组装与各层的合并（工作线 F64，model.md §2.4 的第一张表与合并顺序）。

use std::sync::Arc;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::provider::{Credential, ModelEndpoint, WireProtocol};
use rust_cli_net::{HeaderValue, HttpRequest};

use super::*;
use crate::anthropic::Anthropic;
use crate::openai_chat::OpenAiChat;

fn facts() -> SourceFacts {
    SourceFacts {
        version: "0.1.52".to_owned(),
        endpoint_origin: "https://zcode.z.ai".to_owned(),
        release_channel: "production".to_owned(),
        locale: Some("zh-CN".to_owned()),
        time_zone: Some("Asia/Shanghai".to_owned()),
        platform: "linux".to_owned(),
        arch: "x64".to_owned(),
        os_release: Some("6.8.0".to_owned()),
    }
}

/// 键序与取值同 Node `buildCliZCodeSourceHeaders`。
#[test]
fn source_headers_follow_node() {
    let headers = source_headers(&facts());
    let expected = [
        ("http-referer", "https://zcode.z.ai"),
        ("user-agent", "ZCode/0.1.52"),
        ("x-zcode-app-version", "0.1.52"),
        ("x-title", "Z Code@cli"),
        ("x-release-channel", "production"),
        ("x-client-language", "zh-CN"),
        ("x-client-timezone", "Asia/Shanghai"),
        ("x-zcode-agent", "glm"),
        ("x-platform", "linux-x64"),
        ("x-os-category", "linux"),
        ("x-os-version", "6.8.0"),
    ];
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    assert_eq!(headers, expected);
}

/// 不可打印（去掉首尾空白后为空，或含 0x20–0x7e 以外的字符）：版本写 `unknown`、不发版本头；locale 与时区写
/// `unknown`；系统版本不发；值取去掉首尾空白后的文本。
#[test]
fn unprintable_values_follow_node() {
    let value = |headers: &[(String, String)], name: &str| {
        headers
            .iter()
            .find(|(existing, _)| existing == name)
            .map(|(_, value)| value.clone())
    };
    for version in ["", "  ", "0.1\u{7f}", "版本"] {
        let headers = source_headers(&SourceFacts {
            version: version.to_owned(),
            locale: None,
            time_zone: Some("\tAsia/Tokyo é".to_owned()),
            os_release: Some(" ".to_owned()),
            ..facts()
        });
        assert_eq!(
            value(&headers, "user-agent").as_deref(),
            Some("ZCode/unknown")
        );
        assert_eq!(value(&headers, "x-zcode-app-version"), None, "{version:?}");
        assert_eq!(
            value(&headers, "x-client-language").as_deref(),
            Some("unknown")
        );
        assert_eq!(
            value(&headers, "x-client-timezone").as_deref(),
            Some("unknown")
        );
        assert_eq!(value(&headers, "x-os-version"), None);
    }
    let headers = source_headers(&SourceFacts {
        version: " 1.2.3 ".to_owned(),
        os_release: Some(" 24.6.0\n".to_owned()),
        ..facts()
    });
    assert_eq!(
        value(&headers, "user-agent").as_deref(),
        Some("ZCode/1.2.3")
    );
    assert_eq!(value(&headers, "x-os-version").as_deref(), Some("24.6.0"));
    for (platform, category) in [
        ("darwin", "macos"),
        ("win32", "windows"),
        ("freebsd", "linux"),
    ] {
        let headers = source_headers(&SourceFacts {
            platform: platform.to_owned(),
            ..facts()
        });
        assert_eq!(value(&headers, "x-os-category").as_deref(), Some(category));
        assert_eq!(
            value(&headers, "x-platform"),
            Some(format!("{platform}-x64"))
        );
    }
    let headers = source_headers(&SourceFacts {
        arch: String::new(),
        ..facts()
    });
    assert_eq!(value(&headers, "x-platform"), None);
}

fn endpoint(protocol: WireProtocol, base_url: &str, headers: &[(&str, &str)]) -> ModelEndpoint {
    ModelEndpoint {
        protocol,
        base_url: base_url.to_owned(),
        model: "m".to_owned(),
        credential: Credential::ApiKey(RedactedString::new("sk-test")),
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), RedactedString::new(*value)))
            .collect(),
        body_patches: Vec::new(),
        mid_conversation_system: true,
        supports_image: true,
        supports_pdf: false,
        supports_video: false,
        supports_audio: false,
        renewable: false,
    }
}

fn ids() -> RequestHeaders {
    RequestHeaders {
        source: source_headers(&facts()),
        trace_id: "t".to_owned(),
        request_id: Arc::new(|| "q".to_owned()),
    }
}

fn encoded(betas: Option<&str>) -> HttpRequest {
    let mut http = HttpRequest::post("http://127.0.0.1:9/v1/messages", "{}")
        .header("accept", "application/json")
        .header("anthropic-version", "2023-06-01");
    if let Some(betas) = betas {
        http = http.header("anthropic-beta", betas);
    }
    http
}

/// 线上的头：名字小写，值不区分秘密与否。
fn wire(http: &HttpRequest) -> Vec<(String, String)> {
    http.headers
        .iter()
        .map(|(name, value)| {
            let value = match value {
                HeaderValue::Plain(value) => value.clone(),
                HeaderValue::Secret(value) => value.expose().to_owned(),
            };
            (name.to_lowercase(), value)
        })
        .collect()
}

fn find<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let found: Vec<_> = headers
        .iter()
        .filter(|(existing, _)| existing == name)
        .collect();
    assert!(found.len() <= 1, "{name}: {headers:?}");
    found.first().map(|(_, value)| value.as_str())
}

/// 合并顺序：Provider 配置覆盖凭据、来源头与适配器的头；每请求头压过配置；配置的用途头去掉。
#[test]
fn provider_headers_sit_between_the_source_and_the_request_headers() {
    let configured = endpoint(
        WireProtocol::AnthropicMessages,
        "http://127.0.0.1:9",
        &[
            ("X-Api-Key", "configured-key"),
            ("User-Agent", "ProbeUA/9"),
            ("ACCEPT", "*/*"),
            ("x-zcode-query-source", "static"),
            ("X-Session-Type", "kept"),
            ("X-ZCode-Session-Type", "static"),
            ("X-Request-Id", "static"),
        ],
    );
    let origin = crate::attribution::Origin::default();
    let http = outbound(
        &Anthropic,
        &encoded(None),
        &configured,
        Some(&ids()),
        &origin,
    );
    let headers = wire(&http);
    assert_eq!(find(&headers, "x-api-key"), Some("configured-key"));
    assert_eq!(find(&headers, "user-agent"), Some("ProbeUA/9"));
    assert_eq!(find(&headers, "accept"), Some("*/*"));
    assert_eq!(find(&headers, "x-zcode-query-source"), None);
    assert_eq!(find(&headers, "x-session-type"), Some("kept"));
    assert_eq!(find(&headers, "x-zcode-session-type"), Some("other"));
    assert_eq!(find(&headers, "x-request-id"), Some("q"));
    assert_eq!(find(&headers, "x-title"), Some("Z Code@cli"));
}

/// 没有入口给的来源头与 ID（测试）：只有适配器的头、凭据与 Provider 配置。
#[test]
fn without_request_headers_only_the_adapter_and_provider_layers_remain() {
    let plain = endpoint(
        WireProtocol::AnthropicMessages,
        "http://127.0.0.1:9",
        &[("X-Probe", "p")],
    );
    let http = outbound(
        &Anthropic,
        &encoded(None),
        &plain,
        None,
        &crate::attribution::Origin::default(),
    );
    let names: Vec<_> = wire(&http).into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        ["accept", "anthropic-version", "x-api-key", "x-probe"]
    );
}

/// OpenRouter 端点另带两个归因头（配置可以覆盖）；其他端点不带。
#[test]
fn openrouter_gets_the_attribution_headers() {
    for (base_url, expected) in [
        ("https://openrouter.ai/api/v1", Some("ZCode")),
        ("http://openrouter.ai/api/v1", None),
        ("https://api.example.com/v1", None),
    ] {
        let chat = endpoint(WireProtocol::OpenAiChatCompletions, base_url, &[]);
        let http = outbound(
            &OpenAiChat,
            &encoded(None),
            &chat,
            Some(&ids()),
            &crate::attribution::Origin::default(),
        );
        let headers = wire(&http);
        assert_eq!(find(&headers, "x-openrouter-title"), expected, "{base_url}");
        assert_eq!(
            find(&headers, "x-openrouter-categories").is_some(),
            expected.is_some()
        );
    }
    let configured = endpoint(
        WireProtocol::OpenAiChatCompletions,
        "https://openrouter.ai/api/v1",
        &[("X-OpenRouter-Title", "Mine")],
    );
    let http = outbound(
        &OpenAiChat,
        &encoded(None),
        &configured,
        Some(&ids()),
        &crate::attribution::Origin::default(),
    );
    assert_eq!(find(&wire(&http), "x-openrouter-title"), Some("Mine"));
}

/// 配置的 `anthropic-beta`：与算出的取并集（先算出的，配置的转小写、去空项与重复）；拆开后为空时原样发配置的值；
/// OpenAI 协议上它只是一个普通的 Provider 头。
#[test]
fn configured_betas_are_merged() {
    let cases = [
        (Some("a-1,b-2"), "B-2, C-3 ,,", Some("a-1,b-2,c-3")),
        (None, "C-3", Some("c-3")),
        (Some("a-1"), " , ", Some("a-1")),
        (None, " , ", Some(" , ")),
    ];
    for (computed, configured, expected) in cases {
        let beta = endpoint(
            WireProtocol::AnthropicMessages,
            "http://127.0.0.1:9",
            &[("Anthropic-Beta", configured)],
        );
        let http = outbound(
            &Anthropic,
            &encoded(computed),
            &beta,
            None,
            &crate::attribution::Origin::default(),
        );
        assert_eq!(
            find(&wire(&http), "anthropic-beta"),
            expected,
            "{computed:?} {configured:?}"
        );
    }
    let chat = endpoint(
        WireProtocol::OpenAiChatCompletions,
        "http://127.0.0.1:9",
        &[("Anthropic-Beta", "C-3")],
    );
    let http = outbound(
        &OpenAiChat,
        &encoded(None),
        &chat,
        None,
        &crate::attribution::Origin::default(),
    );
    assert_eq!(find(&wire(&http), "anthropic-beta"), Some("C-3"));
}

/// 同名头的合并与 Node `mergeModelRequestHeaders` 逐项相同（名字不分大小写，后来的替换旧的、保留后来的写法、排到末尾；
/// 夹具由 `scripts/rust-cli/headers-golden.mjs` 生成）。
#[test]
fn merging_matches_node() {
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/attribution-golden.json")).unwrap();
    let pairs = |value: &serde_json::Value| -> Vec<(String, String)> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().to_owned(),
                    pair[1].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    for case in golden["merge"].as_array().unwrap() {
        let mut headers = Vec::new();
        for source in case["sources"].as_array().unwrap() {
            for (name, value) in pairs(source) {
                put(&mut headers, &name, HeaderValue::Plain(value));
            }
        }
        let ours: Vec<(String, String)> = headers
            .into_iter()
            .map(|(name, value)| match value {
                HeaderValue::Plain(value) => (name, value),
                HeaderValue::Secret(value) => (name, value.expose().to_owned()),
            })
            .collect();
        assert_eq!(ours, pairs(&case["merged"]), "{case}");
    }
}
