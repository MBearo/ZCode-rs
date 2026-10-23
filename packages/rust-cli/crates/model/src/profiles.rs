//! 请求方案表（docs/specs/rust-cli/model.md §3.2，修复线 F23，用户决定 A6）。
//!
//! 本工程自己的一张小表，只管 Node 配置里没有的请求形状项：system 形状、一方专有项、流空闲的字节级缺省上限与
//! WebSearch 服务端工具的版本。能力（是否有 WebSearch、对话中间的 system、媒体）只看生效配置（§11），不在这里。
//! 写法同 Node 的 providerSiteRules：协议、baseUrl 正则（匹配规范化后的完整 URL，`^(?:…)$`）、模型 ID 正则（不区分
//! 大小写），按顺序取第一个命中。最后两条按协议兜底，所以总有一条命中。

use std::sync::OnceLock;
use std::time::Duration;

use rust_cli_kernel_api::provider::WireProtocol;

/// Anthropic 官方 API 在 Node 配置里的写法（只给测试用的声明按它查表，§6；入口查能力规则也用它，F55）。与显式覆盖的
/// 缺省 `AnthropicEndpoint::DEFAULT_BASE_URL`（SDK 的写法）由同一个 origin 拼出（F58）；方案表的正则两种都命中（测试）。
pub const ANTHROPIC_OFFICIAL_BASE_URL: &str = concat!(crate::endpoint::anthropic_origin!(), "/v1");

/// system 的分段（model.md §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemShape {
    /// context 给的原样：身份句无断点、静态段 `scope: global`、会话段。
    FirstParty,
    /// 不走全局缓存时的缺省分段：身份句 `ephemeral`，其余合成一段 `ephemeral`，不带 `scope`。
    ThirdParty,
}

/// WebSearch 服务端工具的版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebSearchTool {
    /// `web_search_20250305`。
    V20250305,
    /// Node（`@ai-sdk/anthropic` 3.0.81 的 `webSearch_20260209`）：`web_search_20260209`。
    V20260209,
    /// OpenAI 两种协议：不支持。
    Unsupported,
}

/// 方案命中的端点。
#[derive(Debug, Clone, Copy)]
enum Endpoint {
    /// 任意端点。
    Any,
    /// baseUrl 正则（匹配规范化后的完整 URL）。
    Pattern(&'static str),
    /// z.ai / bigmodel 的端点（base `rule_url::is_zhipu_endpoint`，与 ToolSearch 同一个判定，修复线 F58）。
    Zhipu,
}

/// 一个请求方案。
#[derive(Debug)]
pub(crate) struct Profile {
    pub(crate) name: &'static str,
    protocol: WireProtocol,
    endpoint: Endpoint,
    /// 模型 ID 正则；None 是任意。
    model: Option<&'static str>,
    pub(crate) system: SystemShape,
    /// 发一方专有项：卡片的 thinking / effort / context_management / 每轮 effort、`diagnostics`、全部按特性的 beta。
    pub(crate) first_party_extras: bool,
    /// 字节级看门狗的缺省上限（官方 Anthropic 端点 180 s，其他 300 s）。
    pub(crate) byte_idle: Duration,
    pub(crate) web_search: WebSearchTool,
}

const OFFICIAL_IDLE: Duration = Duration::from_secs(180);
/// 其他端点的字节级缺省与事件级上限的下限是同一个常量（F55：以前各写一份 300 s）。
const OTHER_IDLE: Duration = crate::idle::EVENT_IDLE_FLOOR;

/// 一方：第一方 system、一方专有项都发。
const fn official(
    name: &'static str,
    endpoint: Endpoint,
    model: &'static str,
    byte_idle: Duration,
    web_search: WebSearchTool,
) -> Profile {
    Profile {
        name,
        protocol: WireProtocol::AnthropicMessages,
        endpoint,
        model: Some(model),
        system: SystemShape::FirstParty,
        first_party_extras: true,
        byte_idle,
        web_search,
    }
}

/// 不发一方专有项的方案（其他 Anthropic 兼容与 OpenAI 两种）。
const fn other(name: &'static str, protocol: WireProtocol, web_search: WebSearchTool) -> Profile {
    Profile {
        name,
        protocol,
        endpoint: Endpoint::Any,
        model: None,
        system: SystemShape::ThirdParty,
        first_party_extras: false,
        byte_idle: OTHER_IDLE,
        web_search,
    }
}

/// 方案表（model.md §3.2 的表，顺序即优先级）。
///
/// 修复线 F58：智谱的端点以前是三种精确地址（照 zcode-builtin.json 的站点规则），与 ToolSearch 的域名规则不一致；
/// 现在用同一个判定（主机是 z.ai / bigmodel.cn 或子域名，Node / App 的 `resolveModelProviderFamilyIdByBaseURL`）。
pub(crate) static PROFILES: [Profile; 6] = [
    official(
        "anthropic-official",
        Endpoint::Pattern(r"https://api\.anthropic\.com(?:/v1)?"),
        ".*claude-.*",
        OFFICIAL_IDLE,
        WebSearchTool::V20250305,
    ),
    official(
        "zhipu-official",
        Endpoint::Zhipu,
        ".*glm.*",
        OTHER_IDLE,
        WebSearchTool::V20260209,
    ),
    official(
        "deepseek-official",
        Endpoint::Pattern(r"https://api\.deepseek\.com/anthropic"),
        ".*deepseek.*",
        OTHER_IDLE,
        WebSearchTool::V20260209,
    ),
    other(
        "anthropic-compatible",
        WireProtocol::AnthropicMessages,
        WebSearchTool::V20260209,
    ),
    other(
        "openai-chat",
        WireProtocol::OpenAiChatCompletions,
        WebSearchTool::Unsupported,
    ),
    other(
        "openai-responses",
        WireProtocol::OpenAiResponses,
        WebSearchTool::Unsupported,
    ),
];

/// 每个方案编译好的两个正则（与 [`PROFILES`] 同序）。
fn compiled() -> &'static [(Option<regress::Regex>, Option<regress::Regex>)] {
    static COMPILED: OnceLock<Vec<(Option<regress::Regex>, Option<regress::Regex>)>> =
        OnceLock::new();
    COMPILED.get_or_init(|| {
        let regex = |pattern: Option<&str>, flags: &str| {
            pattern.map(|pattern| {
                regress::Regex::with_flags(&format!("^(?:{pattern})$"), flags)
                    .unwrap_or_else(|error| panic!("请求方案表的正则无效：{pattern}：{error}"))
            })
        };
        PROFILES
            .iter()
            .map(|profile| {
                let pattern = match profile.endpoint {
                    Endpoint::Pattern(pattern) => Some(pattern),
                    Endpoint::Any | Endpoint::Zhipu => None,
                };
                (regex(pattern, ""), regex(profile.model, "i"))
            })
            .collect()
    })
}

/// 这次请求的方案：`base_url` 先按 Node 规范化（解析不了时只有不看 baseUrl 的方案能命中），`model` 是端点给的
/// 模型 ID。
pub(crate) fn select(protocol: WireProtocol, base_url: &str, model: &str) -> &'static Profile {
    let url = rust_cli_base::rule_url::normalize(base_url);
    let hit = |regex: &Option<regress::Regex>, subject: Option<&str>| {
        regex
            .as_ref()
            .is_none_or(|regex| subject.is_some_and(|subject| regex.find(subject).is_some()))
    };
    PROFILES
        .iter()
        .zip(compiled())
        .find(|(profile, (url_regex, model_regex))| {
            let endpoint = match profile.endpoint {
                Endpoint::Zhipu => rust_cli_base::rule_url::is_zhipu_endpoint(base_url),
                Endpoint::Any | Endpoint::Pattern(_) => hit(url_regex, url.as_deref()),
            };
            profile.protocol == protocol && endpoint && hit(model_regex, Some(model))
        })
        .map(|(profile, _)| profile)
        // 最后两条按协议兜底，Anthropic 协议有“其他 Anthropic 兼容”：不会走到这里。
        .unwrap_or(&PROFILES[3])
}

/// 经网关：兼容端点的 base URL 既不是 Anthropic 官方（`anthropic-official` 的端点正则）也不是智谱（model.md §3.1 的
/// “兼容端点”，工作线 F71）。只看 base URL，不看模型。
pub(crate) fn is_gateway(base_url: &str) -> bool {
    if rust_cli_base::rule_url::is_zhipu_endpoint(base_url) {
        return false;
    }
    let url = rust_cli_base::rule_url::normalize(base_url);
    let official = PROFILES
        .iter()
        .zip(compiled())
        .find(|(profile, _)| profile.name == "anthropic-official")
        .and_then(|(_, (url_regex, _))| url_regex.as_ref());
    !matches!(
        (official, url.as_deref()),
        (Some(regex), Some(url)) if regex.find(url).is_some()
    )
}

#[cfg(test)]
#[path = "profiles_select_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod snapshot_tests;
