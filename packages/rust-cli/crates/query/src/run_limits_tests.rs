//! 入口的覆盖（修复线 F42，query.md §1.5、cli-args.md §9.1、§9.2）：`ZCODE_MAX_OUTPUT_TOKENS` 与 `MAX_THINKING_TOKENS`
//! 怎样改主请求。

use std::sync::Arc;

use rust_cli_base::models::{DEEPSEEK_V4_PRO, GLM_5_3, OPUS_5_5};
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::model::{ModelRequest, ModelSelection, QuerySource};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};

use super::tests::{parts, request, scope};
use crate::{RequestLimits, ThinkingTokens};

async fn first_request(model: &str, limits: RequestLimits, source: QuerySource) -> ModelRequest {
    let scripted = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut request = request("hi");
    request.selection = ModelSelection {
        provider: "anthropic".to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    };
    request.source = source;
    let mut parts = parts(scripted.clone(), Vec::new());
    parts.limits = limits;
    let (scope, _sink) = scope(parts, &request, Abort::new());
    super::run(scope, request).await.unwrap();
    scripted.requests().remove(0)
}

fn output(value: Option<u32>) -> RequestLimits {
    RequestLimits {
        max_output_tokens: value,
        ..RequestLimits::default()
    }
}

/// `max_tokens`：有卡片的模型缺省与上限都是卡片的 128000（卡片自己的上限，只有 Anthropic 的卡片模型有）；没有卡片时
/// 缺省 32000、上限 128000；设的值不超过上限。借形状的 GLM 不用卡片的上限：模型端口答不出能力时同没有卡片。
#[tokio::test]
async fn the_output_limit_follows_zcode_max_output_tokens() {
    let max = async |model: &str, value: Option<u32>| {
        first_request(model, output(value), QuerySource::MainThread)
            .await
            .max_tokens
    };
    assert_eq!(max(OPUS_5_5, None).await, 128_000);
    assert_eq!(max(OPUS_5_5, Some(4096)).await, 4096);
    assert_eq!(max(OPUS_5_5, Some(500_000)).await, 128_000);
    assert_eq!(max(DEEPSEEK_V4_PRO, None).await, 32_000);
    assert_eq!(max(DEEPSEEK_V4_PRO, Some(64_000)).await, 64_000);
    assert_eq!(max(DEEPSEEK_V4_PRO, Some(500_000)).await, 128_000);
    assert_eq!(max(GLM_5_3, None).await, 32_000);
    // 子代理与压缩摘要经同一个 `model_shape`（输出上限对每个请求相同）。
    let agent = first_request(
        GLM_5_3,
        output(Some(64_000)),
        QuerySource::Subagent("general-purpose".to_owned()),
    )
    .await;
    assert_eq!(agent.max_tokens, 64_000);
    let compact = first_request(GLM_5_3, output(Some(64_000)), QuerySource::Compact).await;
    assert_eq!(compact.max_tokens, 64_000);
}

/// `MAX_THINKING_TOKENS` 让 thinking 关：有卡片形状的模型（GLM 借 opus 的，F121）不发 thinking 与 clear_thinking（卡片
/// 模型拒绝 `{type: disabled}`），effort 不变；大于 0 时照旧。
#[tokio::test]
async fn max_thinking_tokens_turns_thinking_off_for_card_models() {
    let limits = |thinking| RequestLimits {
        thinking,
        ..RequestLimits::default()
    };
    let off = first_request(
        GLM_5_3,
        limits(ThinkingTokens::Disabled),
        QuerySource::MainThread,
    )
    .await;
    assert_eq!(off.thinking, None);
    assert!(off.options.context_edits.is_empty());
    assert_eq!(off.options.effort.as_deref(), Some("medium"));
    for thinking in [ThinkingTokens::Unset, ThinkingTokens::Budget(8000)] {
        let on = first_request(GLM_5_3, limits(thinking), QuerySource::MainThread).await;
        assert!(on.thinking.is_some(), "{thinking:?}");
        assert_eq!(on.options.context_edits.len(), 1, "{thinking:?}");
    }
    // 没有卡片形状的模型本来就不发 thinking（model.md §7）。
    let plain = first_request(
        DEEPSEEK_V4_PRO,
        limits(ThinkingTokens::Budget(8000)),
        QuerySource::MainThread,
    )
    .await;
    assert_eq!(plain.thinking, None);
}

/// 用尽时的报错里是本工程读的名字（F42）。
#[test]
fn the_exhausted_output_limit_names_zcode_max_output_tokens() {
    let text = crate::texts::output_limit_exhausted(4096);
    assert!(
        text.ends_with(
            "exceeded the 4096 output token maximum. To configure this behavior, set the ZCODE_MAX_OUTPUT_TOKENS environment variable."
        ),
        "{text}"
    );
}
