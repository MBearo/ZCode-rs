//! 请求方案表（docs/specs/rust-cli/model.md §3.2，修复线 F23，用户决定 A6）。
//!
//! 本工程自己的一张小表，只管 Node 配置里没有的请求形状项：system 形状、一方专有项与流空闲的字节级缺省上限。能力（是否
//! 有 WebSearch、对话中间的 system、媒体）只看生效配置（§11），不在这里。WebSearch 服务端工具的版本 F93 起一律是 Node 的
//! `web_search_20260209`（用户决定 2026-10-24，推翻 F23 给官方地址用 `web_search_20250305`），不再在表里。
//! 写法同 Node 的 providerSiteRules：协议、baseUrl 正则（匹配规范化后的完整 URL，`^(?:…)$`）、模型 ID 正则（不区分
//! 大小写），按顺序取第一个命中。最后两条按协议兜底，所以总有一条命中。

use std::sync::OnceLock;
use std::time::Duration;

use rust_cli_kernel_api::provider::WireProtocol;

use crate::errors::Vendor;

/// Anthropic 官方 API 在 Node 配置里的写法（只给测试用的声明按它查表，§6；入口查能力规则也用它，F55）。与显式覆盖的
/// 缺省 `AnthropicEndpoint::DEFAULT_BASE_URL`（SDK 的写法）由同一个 origin 拼出（F58；工作线 F121 起两种写法都在 base 的
/// `hosts`）；方案表两种都认（测试）。
pub const ANTHROPIC_OFFICIAL_BASE_URL: &str = rust_cli_base::hosts::ANTHROPIC_API_V1;

/// system 的分段（model.md §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemShape {
    /// context 给的原样：身份句无断点、静态段 `scope: global`、会话段。
    FirstParty,
    /// 不走全局缓存时的缺省分段：身份句 `ephemeral`，其余合成一段 `ephemeral`，不带 `scope`。
    ThirdParty,
}

/// 方案命中的端点。
#[derive(Debug, Clone, Copy)]
enum Endpoint {
    /// 任意端点。
    Any,
    /// 规范化后的完整 URL 等于其中之一（地址在 base 的 `hosts`，工作线 F121；以前是写死主机名的正则，含义相同）。
    Exact(&'static [&'static str]),
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
) -> Profile {
    Profile {
        name,
        protocol: WireProtocol::AnthropicMessages,
        endpoint,
        model: Some(model),
        system: SystemShape::FirstParty,
        first_party_extras: true,
        byte_idle,
    }
}

/// 不发一方专有项的方案（其他 Anthropic 兼容与 OpenAI 两种）。
const fn other(name: &'static str, protocol: WireProtocol) -> Profile {
    Profile {
        name,
        protocol,
        endpoint: Endpoint::Any,
        model: None,
        system: SystemShape::ThirdParty,
        first_party_extras: false,
        byte_idle: OTHER_IDLE,
    }
}

/// 方案表（model.md §3.2 的表，顺序即优先级）。
///
/// 修复线 F58：智谱的端点以前是三种精确地址（照 zcode-builtin.json 的站点规则），与 ToolSearch 的域名规则不一致；
/// 现在用同一个判定（主机是 z.ai / bigmodel.cn 或子域名，Node / App 的 `resolveModelProviderFamilyIdByBaseURL`）。
pub(crate) static PROFILES: [Profile; 6] = [
    official(
        "anthropic-official",
        Endpoint::Exact(&[
            rust_cli_base::hosts::ANTHROPIC_API,
            rust_cli_base::hosts::ANTHROPIC_API_V1,
        ]),
        ".*claude-.*",
        OFFICIAL_IDLE,
    ),
    official("zhipu-official", Endpoint::Zhipu, ".*glm.*", OTHER_IDLE),
    official(
        "deepseek-official",
        Endpoint::Exact(&[rust_cli_base::hosts::DEEPSEEK_ANTHROPIC]),
        ".*deepseek.*",
        OTHER_IDLE,
    ),
    other("anthropic-compatible", WireProtocol::AnthropicMessages),
    other("openai-chat", WireProtocol::OpenAiChatCompletions),
    other("openai-responses", WireProtocol::OpenAiResponses),
];

/// 每个方案编译好的模型 ID 正则（与 [`PROFILES`] 同序）。
fn compiled() -> &'static [Option<regress::Regex>] {
    static COMPILED: OnceLock<Vec<Option<regress::Regex>>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        PROFILES
            .iter()
            .map(|profile| {
                profile.model.map(|pattern| {
                    regress::Regex::with_flags(&format!("^(?:{pattern})$"), "i")
                        .unwrap_or_else(|error| panic!("请求方案表的正则无效：{pattern}：{error}"))
                })
            })
            .collect()
    })
}

/// 方案的端点列命中这个 base URL：`url` 是规范化后的（解析不了时为 None，只有不看地址的方案能命中）。
fn endpoint_hit(endpoint: Endpoint, base_url: &str, url: Option<&str>) -> bool {
    match endpoint {
        Endpoint::Any => true,
        Endpoint::Zhipu => rust_cli_base::rule_url::is_zhipu_endpoint(base_url),
        Endpoint::Exact(urls) => url.is_some_and(|url| urls.contains(&url)),
    }
}

/// 这次请求的方案：`base_url` 先按 Node 规范化（解析不了时只有不看 baseUrl 的方案能命中），`model` 是端点给的
/// 模型 ID。
pub(crate) fn select(protocol: WireProtocol, base_url: &str, model: &str) -> &'static Profile {
    let url = rust_cli_base::rule_url::normalize(base_url);
    PROFILES
        .iter()
        .zip(compiled())
        .find(|(profile, model_regex)| {
            profile.protocol == protocol
                && endpoint_hit(profile.endpoint, base_url, url.as_deref())
                && model_regex
                    .as_ref()
                    .is_none_or(|regex| regex.find(model).is_some())
        })
        .map(|(profile, _)| profile)
        // 最后两条按协议兜底，Anthropic 协议有“其他 Anthropic 兼容”：不会走到这里。
        .unwrap_or(&PROFILES[3])
}

/// Anthropic 官方的方案：只给测试用的声明 `_ZCODE_ASSUME_FIRST_PARTY_BASE_URL` 直接取它，不看模型 ID（工作线 F121：GLM 按
/// opus 处理，轨迹比对的本工程一侧用 GLM 的名字发，仍是官方形状；model.md §11.1）。
pub(crate) fn anthropic_official() -> &'static Profile {
    &PROFILES[0]
}

/// 经网关：兼容端点的 base URL 既不是 Anthropic 官方（`anthropic-official` 的端点列）也不是智谱（model.md §3.1 的
/// “兼容端点”，工作线 F71）。只看 base URL，不看模型。
pub(crate) fn is_gateway(base_url: &str) -> bool {
    if rust_cli_base::rule_url::is_zhipu_endpoint(base_url) {
        return false;
    }
    let url = rust_cli_base::rule_url::normalize(base_url);
    !endpoint_hit(anthropic_official().endpoint, base_url, url.as_deref())
}

/// 错误码表的厂商（docs/specs/rust-cli/provider-errors.md §2.2，修复线 F113）：只看方案表的端点列，写法同
/// [`is_gateway`]，不看模型与协议（错误码是服务方给的；智谱 OpenAI 协议的端点与 z.ai 上的非 GLM 模型也用智谱的码表，
/// D2 暂定）。
pub(crate) fn vendor(base_url: &str) -> Vendor {
    let zhipu = PROFILES
        .iter()
        .any(|profile| matches!(profile.endpoint, Endpoint::Zhipu))
        && rust_cli_base::rule_url::is_zhipu_endpoint(base_url);
    if zhipu {
        Vendor::Zhipu
    } else {
        Vendor::Generic
    }
}

#[cfg(test)]
#[path = "profiles_select_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod snapshot_tests;
