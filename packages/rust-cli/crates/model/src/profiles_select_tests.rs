//! 方案表的命中（model.md §3.2）。

use rust_cli_kernel_api::provider::WireProtocol;

use super::*;

fn name(protocol: WireProtocol, base_url: &str, model: &str) -> &'static str {
    select(protocol, base_url, model).name
}

#[test]
fn each_endpoint_lands_on_its_profile() {
    let anthropic = WireProtocol::AnthropicMessages;
    for (base_url, model, expected) in [
        (
            "https://api.anthropic.com",
            "claude-opus-5-5",
            "anthropic-official",
        ),
        (
            "https://api.anthropic.com/v1/",
            "claude-opus-5-5",
            "anthropic-official",
        ),
        (
            "https://API.anthropic.com:443",
            "Claude-Opus-5-5",
            "anthropic-official",
        ),
        // 官方 API 上不匹配 `.*claude-.*` 的模型、别的主机上的 claude-opus-5-5：都不是 Anthropic 官方。
        (
            "https://api.anthropic.com",
            "glm-5.1",
            "anthropic-compatible",
        ),
        (
            "https://api.anthropic.com.example.com",
            "claude-opus-5-5",
            "anthropic-compatible",
        ),
        (
            "http://127.0.0.1:8080",
            "claude-opus-5-5",
            "anthropic-compatible",
        ),
        (
            "https://gw.example.com/v1",
            "claude-opus-5-5",
            "anthropic-compatible",
        ),
        (
            "https://api.z.ai/api/anthropic",
            "glm-5.1",
            "zhipu-official",
        ),
        (
            "https://API.Z.AI/api/anthropic/",
            "GLM-5.2",
            "zhipu-official",
        ),
        (
            "https://open.bigmodel.cn/api/anthropic",
            "glm-5-turbo",
            "zhipu-official",
        ),
        (
            "https://zcode.z.ai/api/v1/zcode-plan/anthropic",
            "glm-5.1",
            "zhipu-official",
        ),
        (
            "https://zcode.z.ai/api/v1/highspeed/anthropic",
            "glm-5.1",
            "zhipu-official",
        ),
        (
            "https://zcode.z.ai/api/v1/off-peak/anthropic",
            "glm-5.1",
            "zhipu-official",
        ),
        // 智谱的站点上不是 GLM 的模型。
        (
            "https://api.z.ai/api/anthropic",
            "kimi-k3",
            "anthropic-compatible",
        ),
        (
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-pro",
            "deepseek-official",
        ),
        (
            "https://api.deepseek.com/anthropic",
            "claude-opus-5-5",
            "anthropic-compatible",
        ),
        ("not a url", "claude-opus-5-5", "anthropic-compatible"),
    ] {
        assert_eq!(
            name(anthropic, base_url, model),
            expected,
            "{base_url} {model}"
        );
    }
    assert_eq!(
        name(
            WireProtocol::OpenAiChatCompletions,
            "https://api.z.ai/api/paas/v4",
            "glm-5.1"
        ),
        "openai-chat"
    );
    assert_eq!(
        name(
            WireProtocol::OpenAiResponses,
            "https://api.openai.com/v1",
            "gpt-5"
        ),
        "openai-responses"
    );
}

/// 智谱的端点只有一个判定（修复线 F58，README §4.1）：主机是 z.ai / bigmodel.cn 或子域名（同 ToolSearch 与 Node 的
/// `resolveModelProviderFamilyIdByBaseURL`）。以前方案表只认三种精确地址：`bigmodel.cn`（`ZCODE_ENV=test` 的
/// BigModel）开了 ToolSearch，请求却按第三方形状发。
#[test]
fn every_zhipu_endpoint_shares_the_tool_search_rule() {
    let anthropic = WireProtocol::AnthropicMessages;
    for base_url in [
        "https://open.bigmodel.cn/api/anthropic",
        "https://zcode.z.ai/api/v1/new-plan/anthropic",
        "https://bigmodel.cn/api/anthropic",
        "https://z.ai/anthropic",
    ] {
        assert_eq!(
            name(anthropic, base_url, "glm-5.1"),
            "zhipu-official",
            "{base_url}"
        );
        assert_eq!(
            name(anthropic, base_url, "kimi-k3"),
            "anthropic-compatible",
            "{base_url}"
        );
    }
    for base_url in [
        "https://z.ai.example.com/api/anthropic",
        "https://notz.ai/api/anthropic",
        "https://api.example.site/api/anthropic",
        "https://example.com/api.z.ai/anthropic",
    ] {
        assert_eq!(
            name(anthropic, base_url, "glm-5.1"),
            "anthropic-compatible",
            "{base_url}"
        );
    }
}

#[test]
fn the_profiles_carry_the_table_values() {
    let official = select(
        WireProtocol::AnthropicMessages,
        "https://api.anthropic.com",
        "claude-opus-5-5",
    );
    assert_eq!(official.system, SystemShape::FirstParty);
    assert!(official.first_party_extras);
    assert_eq!(official.byte_idle, Duration::from_secs(180));
    let zhipu = select(
        WireProtocol::AnthropicMessages,
        "https://api.z.ai/api/anthropic",
        "glm-5.1",
    );
    assert_eq!(zhipu.system, SystemShape::FirstParty);
    assert!(zhipu.first_party_extras);
    assert_eq!(zhipu.byte_idle, Duration::from_secs(300));
    let gateway = select(
        WireProtocol::AnthropicMessages,
        "https://gw.example.com",
        "claude-opus-5-5",
    );
    assert_eq!(gateway.system, SystemShape::ThirdParty);
    assert!(!gateway.first_party_extras);
    assert_eq!(gateway.byte_idle, Duration::from_secs(300));
    let chat = select(
        WireProtocol::OpenAiChatCompletions,
        "https://api.openai.com/v1",
        "gpt-5",
    );
    assert_eq!(chat.system, SystemShape::ThirdParty);
    assert!(!chat.first_party_extras);
}

/// Anthropic 官方地址的两种写法（F58）：SDK 的缺省（不带 `/v1`）与 Node 配置的写法（带 `/v1`）出自同一个 origin，补
/// `/v1` 后相同，方案表都认作 `anthropic-official`。
#[test]
fn both_spellings_of_the_official_address_agree() {
    use crate::endpoint::{AnthropicEndpoint, anthropic_v1_base_url};
    assert_eq!(
        AnthropicEndpoint::DEFAULT_BASE_URL,
        "https://api.anthropic.com"
    );
    assert_eq!(ANTHROPIC_OFFICIAL_BASE_URL, "https://api.anthropic.com/v1");
    assert_eq!(
        anthropic_v1_base_url(AnthropicEndpoint::DEFAULT_BASE_URL),
        ANTHROPIC_OFFICIAL_BASE_URL
    );
    for base_url in [
        AnthropicEndpoint::DEFAULT_BASE_URL,
        ANTHROPIC_OFFICIAL_BASE_URL,
    ] {
        assert_eq!(
            name(WireProtocol::AnthropicMessages, base_url, "claude-opus-5-5"),
            "anthropic-official",
            "{base_url}"
        );
    }
}

/// 错误码表的厂商只看端点（provider-errors.md §2.2，D2 暂定）：智谱两种协议的端点、z.ai 上的非 GLM 模型与 ZCode 套餐网关
/// 都是智谱；DeepSeek、网关、OpenAI、Anthropic 官方没有厂商层。
#[test]
fn the_vendor_follows_the_endpoint_only() {
    for base_url in [
        "https://open.bigmodel.cn/api/anthropic",
        "https://open.bigmodel.cn/api/paas/v4",
        "https://api.z.ai/api/paas/v4",
        "https://api.z.ai/api/anthropic",
        "https://zcode.z.ai/api/v1",
    ] {
        assert_eq!(vendor(base_url), crate::errors::Vendor::Zhipu, "{base_url}");
    }
    for base_url in [
        "https://api.deepseek.com/anthropic",
        "https://gw.example.com",
        "https://api.openai.com/v1",
        ANTHROPIC_OFFICIAL_BASE_URL,
        "not a url",
    ] {
        assert_eq!(
            vendor(base_url),
            crate::errors::Vendor::Generic,
            "{base_url}"
        );
    }
}
