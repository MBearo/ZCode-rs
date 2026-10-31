//! 规则匹配用的 base URL 规范化（Node `normalizeBaseURLForRuleMatch`，`packages/provider/src/config/model-config.ts`
//! 548；docs/specs/rust-cli/platform.md §3.2 第 7 条）。平台层的 providerSiteRules 与模型适配器的请求方案表共用。
//!
//! 智谱端点的判定也在这里（[`is_zhipu_endpoint`]，修复线 F58）：平台层的 ToolSearch 与模型适配器的请求方案共用一个。

/// 智谱的两个根域名（Node / App `MODEL_PROVIDER_FAMILY_SPECS` 的 `rootDomain`；定义在 `hosts`，工作线 F121）。
pub const ZHIPU_DOMAINS: [&str; 2] = [crate::hosts::ZAI_DOMAIN, crate::hosts::BIGMODEL_DOMAIN];

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
                .is_some_and(|host| on_domain(host, crate::hosts::OPENROUTER_DOMAIN))
    })
}

/// OpenCode Go 的端点（工作线 F64，模型请求另带 `x-opencode-session`）：主机是 `opencode.ai` 或它的子域名，路径去掉
/// 末尾的 `/`、转小写后是 `/zen/go/v1`。照 Node `isOpenCodeGoBaseUrl`（`adapters/src/model/opencode-session.ts`）。
pub fn is_opencode_go_endpoint(base_url: &str) -> bool {
    url::Url::parse(base_url.trim()).is_ok_and(|url| {
        url.host_str()
            .is_some_and(|host| on_domain(host, crate::hosts::OPENCODE_DOMAIN))
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
#[path = "rule_url_tests.rs"]
mod tests;
