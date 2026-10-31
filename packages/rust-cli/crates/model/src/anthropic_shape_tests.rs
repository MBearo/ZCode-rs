//! 按方案改写请求（model.md §3.2）：第三方 system 的形状；专有项的去留。与对照数据逐字节比较的用例在
//! `anthropic_shape_parity_tests.rs`（docs/specs/rust-cli/export.md §5）。

use rust_cli_base::{hosts, models};
use rust_cli_kernel_api::model::{
    CacheControl, ContextEdit, ModelRequest, SystemBlock, ThinkingConfig, TurnOutputConfig,
};
use rust_cli_kernel_api::provider::WireProtocol;

use super::*;
use crate::anthropic_request::tests::request;
use crate::profiles::select;

fn block(text: &str, cache_control: Option<CacheControl>) -> SystemBlock {
    SystemBlock {
        text: text.to_owned(),
        cache_control,
    }
}

/// 主线程的第一方 system：身份句（`ephemeral`）、`scope: global` 的静态段、会话段。
fn first_party() -> Vec<SystemBlock> {
    let global = CacheControl {
        kind: "ephemeral".to_owned(),
        ttl: None,
        scope: Some("global".to_owned()),
    };
    vec![
        block("identity", ephemeral()),
        block("static", Some(global)),
        block("session", None),
    ]
}

/// 第三方形状：身份句 `ephemeral`，静态段与会话段用空行连接成一段 `ephemeral`。
fn third_party() -> Vec<SystemBlock> {
    vec![
        block("identity", ephemeral()),
        block("static\n\nsession", ephemeral()),
    ]
}

#[test]
fn the_third_party_system_joins_the_global_sections() {
    let mut ours = first_party();
    third_party_system(&mut ours);
    assert_eq!(ours, third_party());
    // 身份句没有缓存断点时补一个 `ephemeral`。
    let mut bare = first_party();
    bare[0].cache_control = None;
    third_party_system(&mut bare);
    assert_eq!(bare, third_party());
}

/// 没有 `scope: global` 的 system（子代理、旁路请求）两种形状相同，不改。
#[test]
fn a_system_without_the_global_section_stays() {
    let mut subagent = third_party();
    third_party_system(&mut subagent);
    assert_eq!(subagent, third_party());
    let mut side = vec![SystemBlock {
        text: "title".to_owned(),
        cache_control: None,
    }];
    third_party_system(&mut side);
    assert_eq!(side[0].cache_control, None);
}

/// 卡片模型的主循环请求：卡片字段齐全。
fn carded() -> ModelRequest {
    let mut request = request(vec![rust_cli_kernel_api::model::ApiMessage {
        role: rust_cli_kernel_api::model::ApiRole::User,
        content: Vec::new(),
        output_config: None,
    }]);
    request.system = first_party();
    request.thinking = Some(ThinkingConfig::Adaptive {
        display: Some("updates".to_owned()),
    });
    request.options.effort = Some("medium".to_owned());
    request.options.context_edits = vec![ContextEdit::ClearThinking {
        keep: "all".to_owned(),
    }];
    request.messages[0].output_config = Some(TurnOutputConfig {
        effort: Some("medium".to_owned()),
    });
    request.betas = vec!["context-1m-2025-08-07".to_owned()];
    request
}

#[test]
fn official_endpoints_keep_the_first_party_extras() {
    for (base_url, model) in [
        (hosts::ANTHROPIC_API, models::OPUS_5_5),
        (hosts::ZAI_ANTHROPIC, models::GLM_5_1),
        (hosts::DEEPSEEK_ANTHROPIC, models::DEEPSEEK_V4_PRO),
    ] {
        let mut request = carded();
        apply(
            &mut request,
            select(WireProtocol::AnthropicMessages, base_url, model),
        );
        assert_eq!(request, carded(), "{base_url}");
    }
}

/// “其他 Anthropic 兼容”：第三方 system、没有 `scope`，卡片的 thinking / effort / context_management / 每轮 effort 与
/// 显式 beta 都去掉，其余不动。
#[test]
fn other_gateways_get_the_node_shape() {
    let mut request = carded();
    apply(
        &mut request,
        select(
            WireProtocol::AnthropicMessages,
            "https://gw.example.com",
            models::GLM_5_3,
        ),
    );
    assert_eq!(request.system, third_party());
    assert_eq!(request.thinking, None);
    assert_eq!(request.options.effort, None);
    assert!(request.options.context_edits.is_empty());
    assert!(request.betas.is_empty());
    assert!(request.messages.iter().all(|m| m.output_config.is_none()));
    let mut expected = carded();
    expected.system = request.system.clone();
    expected.thinking = None;
    expected.options.effort = None;
    expected.options.context_edits.clear();
    expected.betas.clear();
    expected.messages[0].output_config = None;
    assert_eq!(request, expected);
}
