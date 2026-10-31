//! 每 token 字符数（context-usage.md §3，修复线 F10）。

use rust_cli_base::models::{DEEPSEEK_V4_PRO, GLM_5_1, GLM_5_3, GLM_5_3_ACCOUNT, OPUS_5_5};

use super::{
    card, card_effort, card_output_limit, chars_per_token, mid_conversation_system, shape,
};

#[test]
fn current_and_unknown_models_use_three_chars_per_token() {
    for model in [
        "claude-opus-5-5",
        "claude-opus-5-5[1m]",
        "glm-4.6",
        "gpt-5",
        "claude-opus-4-8",
    ] {
        assert_eq!(chars_per_token(model), 3, "{model}");
    }
}

#[test]
fn the_fourteen_legacy_models_use_four() {
    for model in [
        "claude-3-opus",
        "claude-3-5-sonnet-20241022",
        "claude-3.7-sonnet",
        "claude-opus-4-1",
        "claude-opus-4-6[1m]",
        "claude-sonnet-4-5-20250929",
        "claude_haiku_4_5",
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "claude-opus-4-5@20251101",
    ] {
        assert_eq!(chars_per_token(model), 4, "{model}");
    }
}

#[test]
fn a_missing_model_uses_four_and_lookalikes_do_not_match() {
    assert_eq!(chars_per_token(""), 4);
    // 只在旧模型 ID 的边界上认：更长的版本号不是旧模型。
    assert_eq!(chars_per_token("claude-opus-4-10"), 3);
    assert_eq!(chars_per_token("xclaude-3-opus"), 3);
}

/// 修复线 F23：effort 只随卡片的行为形状写；没有形状的模型即使选了档位也不写（推理字段由 option map 写，同 Node）。
/// 工作线 F121：GLM 借 opus 的形状，档位是卡片认识的就用它，目录给的档位（`enabled`）不是 effort，用卡片的缺省。
#[test]
fn models_with_a_shape_get_an_effort() {
    assert_eq!(card_effort(OPUS_5_5, None).as_deref(), Some("medium"));
    assert_eq!(card_effort(OPUS_5_5, Some("high")).as_deref(), Some("high"));
    assert_eq!(card_effort(GLM_5_3, None).as_deref(), Some("medium"));
    assert_eq!(card_effort(GLM_5_3, Some("high")).as_deref(), Some("high"));
    assert_eq!(
        card_effort(GLM_5_1, Some("enabled")).as_deref(),
        Some("medium")
    );
    assert_eq!(card_effort(DEEPSEEK_V4_PRO, Some("max")), None);
}

/// 对话中间的 system：生效配置给出时只看它；答不出时按行为形状回退（GLM 借 opus 的形状，F121）。
#[test]
fn the_config_decides_mid_conversation_system() {
    assert!(mid_conversation_system(GLM_5_1, Some(true)));
    assert!(!mid_conversation_system(OPUS_5_5, Some(false)));
    assert!(mid_conversation_system(OPUS_5_5, None));
    assert!(mid_conversation_system(GLM_5_1, None));
    assert!(!mid_conversation_system(DEEPSEEK_V4_PRO, None));
}

/// 工作线 F121（model.md §11.1）：GLM 借 opus 那张卡片的行为形状，身份与上限不借——没有自己的卡片，输出上限只按目录
/// （`configured`）；有卡片的模型的上限是卡片的值；没有形状的模型两样都没有。
#[test]
fn glm_borrows_the_shape_but_not_the_identity_or_the_limits() {
    let opus = shape(OPUS_5_5).unwrap();
    assert_eq!(shape(GLM_5_3), Some(opus));
    assert_eq!(shape(GLM_5_3_ACCOUNT), Some(opus));
    assert!(opus.adaptive_thinking && opus.system_turns && opus.per_turn_effort);
    assert_eq!(opus.default_effort, Some("medium"));
    assert_eq!(shape(DEEPSEEK_V4_PRO), None);
    assert!(card(GLM_5_3).is_none());
    assert_eq!(card_output_limit(OPUS_5_5, Some(16_384)), Some(128_000));
    assert_eq!(card_output_limit(GLM_5_3, Some(131_072)), Some(131_072));
    assert_eq!(card_output_limit(GLM_5_3, None), None);
    assert_eq!(card_output_limit(DEEPSEEK_V4_PRO, Some(384_000)), None);
}

/// GLM 与 DeepSeek 不在旧模型清单里：每 token 3 个字符。
#[test]
fn glm_and_deepseek_use_three_chars_per_token() {
    for model in [GLM_5_3, GLM_5_3_ACCOUNT, DEEPSEEK_V4_PRO] {
        assert_eq!(chars_per_token(model), 3, "{model}");
    }
}

/// 没有卡片的模型的缺省输出上限只有一个（F58）。
#[test]
fn the_unknown_model_output_limit_is_32000() {
    assert_eq!(super::UNKNOWN_MAX_OUTPUT_TOKENS, 32_000);
}
