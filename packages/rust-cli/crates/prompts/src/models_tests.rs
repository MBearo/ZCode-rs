//! 每 token 字符数（context-usage.md §3，修复线 F10）。

use super::{card_effort, chars_per_token, mid_conversation_system};

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

/// 修复线 F23：effort 只随卡片写；没有卡片的模型（GLM）即使选了档位也不写（推理字段由 option map 写，同 Node）。
#[test]
fn only_carded_models_get_an_effort() {
    assert_eq!(
        card_effort("claude-opus-5-5", None).as_deref(),
        Some("medium")
    );
    assert_eq!(
        card_effort("claude-opus-5-5", Some("high")).as_deref(),
        Some("high")
    );
    assert_eq!(card_effort("glm-5.1", Some("enabled")), None);
    assert_eq!(card_effort("deepseek-v4-pro", Some("max")), None);
}

/// 对话中间的 system：生效配置给出时只看它；答不出时按卡片回退。
#[test]
fn the_config_decides_mid_conversation_system() {
    assert!(mid_conversation_system("glm-5.1", Some(true)));
    assert!(!mid_conversation_system("claude-opus-5-5", Some(false)));
    assert!(mid_conversation_system("claude-opus-5-5", None));
    assert!(!mid_conversation_system("glm-5.1", None));
}

/// 没有卡片的模型的缺省输出上限只有一个（F58）。
#[test]
fn the_unknown_model_output_limit_is_32000() {
    assert_eq!(super::UNKNOWN_MAX_OUTPUT_TOKENS, 32_000);
}
