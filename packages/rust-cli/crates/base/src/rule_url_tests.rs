use super::{is_opencode_go_endpoint, is_openrouter_endpoint, is_zhipu_endpoint, normalize};

/// 智谱端点（F58）：两个根域名与子域名；写法（大小写、端口、空白）不影响；长得像的域名与路径里的域名不算。
#[test]
fn zhipu_endpoints_are_the_two_domains_and_their_subdomains() {
    for base_url in [
        "https://api.z.ai/api/anthropic",
        "https://open.bigmodel.cn/api/anthropic",
        "https://open.bigmodel.cn/api/anthropic",
        "https://zcode.z.ai/api/v1/off-peak/anthropic",
        " https://API.Z.AI:443/api/anthropic// ",
        "https://bigmodel.cn",
        "https://z.ai",
    ] {
        assert!(is_zhipu_endpoint(base_url), "{base_url}");
    }
    for base_url in [
        "https://notz.ai/api/anthropic",
        "https://z.ai.example.com",
        "https://example.com/api.z.ai/anthropic",
        "https://api.example.site/api/anthropic",
        "https://api.anthropic.com/v1",
        "not a url",
        "",
    ] {
        assert!(!is_zhipu_endpoint(base_url), "{base_url}");
    }
}

/// F64：OpenRouter 只认 https 与它的域名；OpenCode Go 认域名与路径（末尾的 `/` 与大小写不算）。
#[test]
fn openrouter_and_opencode_go_endpoints() {
    for base_url in [
        "https://openrouter.ai/api/v1",
        " https://OpenRouter.ai/api/v1 ",
        "https://eu.openrouter.ai/api/v1",
    ] {
        assert!(is_openrouter_endpoint(base_url), "{base_url}");
    }
    for base_url in [
        "http://openrouter.ai/api/v1",
        "https://notopenrouter.ai/api/v1",
        "https://openrouter.ai.example.com",
        "not a url",
    ] {
        assert!(!is_openrouter_endpoint(base_url), "{base_url}");
    }
    for base_url in [
        "https://opencode.ai/zen/go/v1",
        "https://opencode.ai/zen/go/v1/",
        "http://api.opencode.ai/ZEN/Go/v1//",
    ] {
        assert!(is_opencode_go_endpoint(base_url), "{base_url}");
    }
    for base_url in [
        "https://opencode.ai/zen/v1",
        "https://opencode.ai/zen/go/v1/chat",
        "https://myopencode.ai/zen/go/v1",
        "",
    ] {
        assert!(!is_opencode_go_endpoint(base_url), "{base_url}");
    }
}

#[test]
fn hosts_ports_and_slashes_do_not_change_the_match() {
    for (input, expected) in [
        (
            "https://api.z.ai/api/anthropic",
            "https://api.z.ai/api/anthropic",
        ),
        (
            "https://API.Z.AI/api/anthropic/",
            "https://api.z.ai/api/anthropic",
        ),
        (
            "https://api.z.ai:443/api/anthropic//",
            "https://api.z.ai/api/anthropic",
        ),
        ("http://127.0.0.1:80/", "http://127.0.0.1"),
        ("https://api.anthropic.com", "https://api.anthropic.com"),
        // path 的大小写保留（Node 的 URL parser 只改 scheme 与 host）。
        ("https://gw.example.com/V1/", "https://gw.example.com/V1"),
        (
            "https://gw.example.com/v1/?a=1#x",
            "https://gw.example.com/v1?a=1#x",
        ),
        ("  https://gw.example.com/v1 ", "https://gw.example.com/v1"),
    ] {
        assert_eq!(normalize(input).as_deref(), Some(expected), "{input}");
    }
    assert_eq!(normalize("not a url"), None);
    assert_eq!(normalize(""), None);
}
