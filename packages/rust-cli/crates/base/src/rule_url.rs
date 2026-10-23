//! 规则匹配用的 base URL 规范化（Node `normalizeBaseURLForRuleMatch`，`packages/provider/src/config/model-config.ts`
//! 548；docs/specs/rust-cli/platform.md §3.2 第 7 条）。平台层的 providerSiteRules 与模型适配器的请求方案表共用。
//!
//! 智谱端点的判定也在这里（[`is_zhipu_endpoint`]，修复线 F58）：平台层的 ToolSearch 与模型适配器的请求方案共用一个。

/// 智谱的两个根域名（Node / App `MODEL_PROVIDER_FAMILY_SPECS` 的 `rootDomain`）。
pub const ZHIPU_DOMAINS: [&str; 2] = ["z.ai", "bigmodel.cn"];

/// 这个 base URL 是不是 z.ai / bigmodel 的端点：去掉首尾空白后按 WHATWG URL 解析，主机（已是小写）等于根域名或以
/// `.<根域名>` 结尾；解析不了时为假。照 Node / App 唯一的判定 `resolveModelProviderFamilyIdByBaseURL`
/// （`packages/shared/src/model-provider-family.ts`），也是 F52 的用户决定（“只给 zai、bigmodel 的域名下的模型开”）。
///
/// 修复线 F58：以前有三种判定——ToolSearch 按这个域名规则，请求方案 `zhipu-official` 只认三种精确地址，内置配置的站点
/// 规则又是一套（那是 Node / App 的数据，不改）——`bigmodel.cn` 开了 ToolSearch，请求却按第三方形状发。
pub fn is_zhipu_endpoint(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url.trim()) else {
        return false;
    };
    url.host_str()
        .is_some_and(|host| ZHIPU_DOMAINS.iter().any(|domain| on_domain(host, domain)))
}

/// 主机是 `domain` 或它的子域名（主机已是小写）。
fn on_domain(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// OpenRouter 的端点（工作线 F64，模型请求另带 OpenRouter 的两个归因头）：去掉首尾空白后按 WHATWG URL 解析，`https`，
/// 主机是 `openrouter.ai` 或它的子域名。照 Node `isOpenRouterBaseUrl`（`packages/shared/src/openrouter-attribution.ts`）。
pub fn is_openrouter_endpoint(base_url: &str) -> bool {
    url::Url::parse(base_url.trim()).is_ok_and(|url| {
        url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(|host| on_domain(host, "openrouter.ai"))
    })
}

/// OpenCode Go 的端点（工作线 F64，模型请求另带 `x-opencode-session`）：主机是 `opencode.ai` 或它的子域名，路径去掉
/// 末尾的 `/`、转小写后是 `/zen/go/v1`。照 Node `isOpenCodeGoBaseUrl`（`adapters/src/model/opencode-session.ts`）。
pub fn is_opencode_go_endpoint(base_url: &str) -> bool {
    url::Url::parse(base_url.trim()).is_ok_and(|url| {
        url.host_str()
            .is_some_and(|host| on_domain(host, "opencode.ai"))
            && url.path().trim_end_matches('/').to_lowercase() == "/zen/go/v1"
    })
}

/// 按 WHATWG URL 解析再序列化（scheme 与 host 小写、去默认端口、空路径写成 `/`），去掉 path 末尾的全部 `/`，
/// query 与 fragment 原样接回；解析不了时为 None（Node 的 `new URL` 抛错）。
pub fn normalize(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value).ok()?;
    // Node 的 `search` / `hash` 在空 query、空 fragment 时是空串（`?`、`#` 不算），序列化里仍带着它们。
    let search = parsed
        .query()
        .filter(|query| !query.is_empty())
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let hash = parsed
        .fragment()
        .filter(|fragment| !fragment.is_empty())
        .map(|fragment| format!("#{fragment}"))
        .unwrap_or_default();
    let serialized = parsed.as_str();
    let suffix = format!("{search}{hash}");
    let endpoint = &serialized[..serialized.len() - suffix.len()];
    Some(format!("{}{suffix}", endpoint.trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
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
}
