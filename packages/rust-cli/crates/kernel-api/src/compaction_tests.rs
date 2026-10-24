use super::{ContextNotice, ContextOccupancy, prompt_too_long_gap, used_percent};
use crate::session_api::KernelEvent;

/// 上下文超限的错误文本（Anthropic 的 400 `prompt is too long`）。
#[test]
fn the_gap_is_actual_minus_limit() {
    assert_eq!(
        prompt_too_long_gap("prompt is too long: 1000500 tokens > 1000000 maximum"),
        Some(500)
    );
    assert_eq!(
        prompt_too_long_gap("Prompt is too long: 210000 TOKENS>200000 maximum"),
        Some(10_000)
    );
    assert_eq!(
        prompt_too_long_gap("input: prompt is too long — 7 token > 5"),
        Some(2)
    );
}

/// 按错误文本的正则：没有这句话、不是 `N tokens > M`、差不大于 0 时都没有差。
#[test]
fn other_texts_have_no_gap() {
    for message in [
        "input length and max_tokens exceed context limit: 1000 + 3000 > 2500",
        "prompt is too long",
        "prompt is too long: 1000 words > 900",
        "prompt is too long: 1000 tokens > 1000 maximum",
        "prompt is too long: 900 tokens > 1000 maximum",
        "prompt is too long: 1000 tokens",
    ] {
        assert_eq!(prompt_too_long_gap(message), None, "{message}");
    }
}

/// 上下文提示的事件形状（compact.md §2.1）：变体名是 `kind`，缺省是不显示。
#[test]
fn context_notices_serialize_by_kind() {
    let event = KernelEvent::ContextNotice {
        notice: ContextNotice::Low {
            percent: 7,
            compact_hint: true,
        },
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "type": "context_notice",
            "notice": {"kind": "low", "percent": 7, "compact_hint": true},
        })
    );
    assert_eq!(serde_json::from_value::<KernelEvent>(json).unwrap(), event);
    assert_eq!(ContextNotice::default(), ContextNotice::Hidden);
    assert_eq!(
        serde_json::to_value(ContextNotice::UntilAutoCompact { percent: 10 }).unwrap(),
        serde_json::json!({"kind": "until_auto_compact", "percent": 10})
    );
}

/// footer 的 ctx（tui-display.md §2.6，F27）：与 `N% context used` 的提示同一个式子
/// `100 − max(0, round((W − token) / W · 100))`；窗口为 0 时是 100。
#[test]
fn the_used_percent_is_the_context_used_formula() {
    assert_eq!(used_percent(0, 180_000), 0);
    assert_eq!(used_percent(66_600, 180_000), 37);
    // 0.5 的边界按 JS `Math.round`：(180000 − 900) / 180000 · 100 = 99.5 → 剩 100%（JS 向 +∞），已用 0%。
    assert_eq!(used_percent(900, 180_000), 0);
    assert_eq!(used_percent(180_000, 180_000), 100);
    assert_eq!(used_percent(200_000, 180_000), 100);
    assert_eq!(used_percent(5, 0), 100);
    let occupancy = ContextOccupancy {
        used_tokens: 66_600,
        max_tokens: 180_000,
    };
    assert_eq!(occupancy.percent(), 37);
    let event = KernelEvent::ContextUsage {
        used_tokens: 1,
        max_tokens: 2,
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        serde_json::json!({"type": "context_usage", "used_tokens": 1, "max_tokens": 2})
    );
}

/// 阻塞线的缺省：编排 trait 的默认实现不拦；压缩附件带历史的入口缺省转到不带历史的那个。
#[test]
fn the_defaults_do_not_block_and_ignore_the_history() {
    use super::{CompactRequest, CompactResult, Compactor, PostCompactContext};
    use crate::message::{AttachmentMessage, Message};
    use crate::model::ModelSelection;
    use crate::orchestration::{ContextRequest, OrchestrationError};
    use async_trait::async_trait;
    use futures_util::FutureExt;
    use tokio_util::sync::CancellationToken;

    struct Plain;
    #[async_trait]
    impl Compactor for Plain {
        fn should_auto_compact(&self, _: &[Message], _: &ModelSelection) -> bool {
            false
        }
        async fn compact(
            &self,
            _: CompactRequest,
            _: CancellationToken,
        ) -> Result<CompactResult, OrchestrationError> {
            Err(OrchestrationError::Unsupported {
                what: "none".to_owned(),
            })
        }
    }
    #[async_trait]
    impl PostCompactContext for Plain {
        async fn post_compact(
            &self,
            _: ContextRequest,
            kept: &[Message],
        ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
            Ok(vec![AttachmentMessage {
                id: crate::ids::MessageId::new(format!("kept-{}", kept.len())),
                source: "x".to_owned(),
                text: String::new(),
                data: serde_json::Value::Null,
            }])
        }
    }
    let selection: ModelSelection =
        serde_json::from_value(serde_json::json!({"provider": "p", "model": "m"})).expect("model");
    assert_eq!(Plain.blocking_limit(&[], &selection, Some("x")), None);
    assert_eq!(super::PROMPT_TOO_LONG, "Prompt is too long");
    let request: ContextRequest = serde_json::from_value(serde_json::json!({
        "session": "s", "workspace": {"environment": "e", "root": "/w"}, "source": {"kind": "main_thread"},
        "mode": "default", "tool_names": [], "selection": {"provider": "p", "model": "m"}
    }))
    .expect("request");
    let found = Plain
        .post_compact_with_history(request, &[], &[])
        .now_or_never()
        .expect("ready")
        .expect("attachments");
    assert_eq!(found[0].id.as_str(), "kept-0");
}

/// 粗估：UTF-16 码元数 / 4，四舍五入。
#[test]
fn rough_tokens_round_the_utf16_length() {
    use super::rough_tokens;
    assert_eq!(rough_tokens(""), 0);
    assert_eq!(rough_tokens("ab"), 1);
    assert_eq!(rough_tokens("abcdef"), 2);
    assert_eq!(rough_tokens("😀😀"), 1);
}
