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

/// OpenAI 的句式（兼容端点也回它，修复线 F108，compact.md §3.1）：差是消息里的输入减上限。
#[test]
fn the_openai_wording_gives_input_minus_limit() {
    assert_eq!(
        prompt_too_long_gap(
            "This model's maximum context length is 1048576 tokens. However, you requested 1484576 tokens (1100000 in the messages, 384576 in the completion)."
        ),
        Some(1_100_000 - 1_048_576)
    );
    assert_eq!(
        prompt_too_long_gap(
            "this model's Maximum Context Length is 8192 tokens. However, your messages resulted in 9000 tokens (9000 in the messages, 0 in the completion)."
        ),
        Some(808)
    );
    // 输入没超、是加上输出上限才超（用户遇到的那句）：没有差。
    assert_eq!(
        prompt_too_long_gap(
            "This model's maximum context length is 1048576 tokens. However, you requested 1055008 tokens (671008 in the messages, 384000 in the completion)."
        ),
        None
    );
    // 没有输入的数：没有差。
    assert_eq!(
        prompt_too_long_gap(
            "This model's maximum context length is 8192 tokens. However, your messages resulted in 9000 tokens."
        ),
        None
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

fn user(text: &str) -> crate::message::Message {
    crate::message::Message::User(crate::message::UserMessage {
        id: crate::ids::MessageId::new("u"),
        content: vec![crate::message::ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: crate::message::UserOrigin::Human,
    })
}

/// 上下文 token 以最近一次带用量的响应为锚点（输入 + 缓存读写 + 输出），加之后消息的粗估；没有锚点时全部粗估。
#[test]
fn context_tokens_anchor_on_the_last_usage() {
    use crate::message::{AssistantMessage, ContentBlock, Message, Usage};
    let assistant = Message::Assistant(AssistantMessage {
        id: crate::ids::MessageId::new("a"),
        content: vec![ContentBlock::Text {
            text: "x".repeat(400),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_creation_input_tokens: 5,
            cache_read_input_tokens: 5,
        }),
    });
    let before = user(&"y".repeat(4000));
    let after = user(&"z".repeat(40));
    assert_eq!(
        super::context_tokens(&[&before, &assistant, &after]),
        120 + 10
    );
    assert_eq!(super::context_tokens(&[&before]), 1000);
    assert_eq!(super::context_tokens(&[]), 0);
}

/// 压缩边界是锚点 0：边界之前的用量不算。
#[test]
fn a_compact_boundary_anchors_at_zero() {
    use crate::ids::MessageId;
    use crate::message::{CompactBoundary, CompactTrigger, Message, SystemMessage};
    let boundary = Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("b"),
        trigger: CompactTrigger::Auto,
        pre_tokens: 900_000,
        summary: MessageId::new("s"),
        preserved: None,
        discovered_tools: Vec::new(),
    }));
    let after = user(&"z".repeat(40));
    assert_eq!(super::context_tokens(&[&boundary, &after]), 10);
}

/// 带内容块的 attachment（`@` 提及的图片）：图片按估算值计入；system 消息不算。
#[test]
fn attachment_blocks_count_like_message_blocks() {
    use crate::message::{ATTACHMENT_BLOCKS, AttachmentMessage, Message};
    let attachment = Message::Attachment(AttachmentMessage {
        id: crate::ids::MessageId::new("a"),
        source: "file".to_owned(),
        text: "abcdefgh".to_owned(),
        data: serde_json::json!({
            ATTACHMENT_BLOCKS: [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "x"}},
                {"type": "text", "text": "abcd"},
            ],
        }),
    });
    assert_eq!(
        super::message_tokens(&attachment),
        2 + super::IMAGE_TOKENS + 1
    );
}
