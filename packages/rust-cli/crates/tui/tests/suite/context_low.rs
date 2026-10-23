//! 上下文快满的提示（R5-8、F14，docs/specs/rust-cli/tui.md §5「footer」、compact.md §2.1）：真实的内核组装（会话
//! actor 与压缩模块）在提交 assistant 与压缩结果之后发出 ContextNotice，footer 只在接近上限时
//! 显示。替身模型不在模型卡片里：窗口来源是 `unknown-model`（不是 `auto`），显示 `N% until auto-compact`；
//! 窗口 200,000、预留 20,000，有效窗口 180,000，阈值 167,000，提示线 147,000。
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};

use super::support::{GatedModel, Item, Setup, with_ui};

/// 一段文字回复，模型回报的输入用量是 `input`。
fn reply(text: &str, input: u64) -> Vec<Item> {
    vec![
        Ok(ModelEvent::MessageStart {
            id: "msg_api".to_owned(),
            model: "scripted".to_owned(),
            usage: Usage {
                input_tokens: input,
                ..Usage::default()
            },
        }),
        Ok(ModelEvent::BlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ModelEvent::BlockDelta {
            index: 0,
            delta: BlockDelta::Text(text.to_owned()),
        }),
        Ok(ModelEvent::BlockStop { index: 0 }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage {
                output_tokens: 1,
                ..Usage::default()
            },
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn the_footer_warns_only_when_the_context_is_nearly_full() {
    let model = GatedModel::scripted(vec![
        reply("small reply", 1_000),
        reply("big reply", 150_000),
    ]);
    let setup = Setup {
        width: 120,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("first");
        ui.wait_for("small reply").await;
        // 远离上限：不显示（level ok）。看回合结束时的画面：上下文的提示在回复提交之后才发。修复线 F32：原来断言的是
        // 回复文字出现的那一帧；事件一起到时提示与文字同帧，文字先画出时这一帧还没有提示，断言空过。
        let screen = ui
            .wait("the turn to end", |s| {
                s.contains("small reply") && !s.contains("esc to interrupt")
            })
            .await;
        assert!(!screen.contains("auto-compact"), "{screen}");
        ui.submit("second");
        // 150,001 token：离阈值剩 round(16,999 / 167,000 · 100) = 10%。修前按 `context used` 显示 `84% context used`。
        let screen = ui.wait_for("10% until auto-compact").await;
        assert!(screen.contains("big reply"), "{screen}");
        assert!(!screen.contains("context used"), "{screen}");
    })
    .await;
}

/// F14：压缩结果提交后提示随之更新（消息变化时重算）；修前要等下一条回复，压缩后仍显示旧的百分比。
#[tokio::test(flavor = "multi_thread")]
async fn the_warning_goes_away_once_the_conversation_is_compacted() {
    let model = GatedModel::scripted(vec![
        reply("big reply", 150_000),
        reply("<summary>short summary</summary>", 2_000),
    ]);
    let setup = Setup {
        width: 120,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("first");
        ui.wait_for("10% until auto-compact").await;
        ui.wait("the turn to end", |s| !s.contains("esc to interrupt"))
            .await;
        ui.submit("/compact");
        let screen = ui.wait_for("Conversation compacted").await;
        let screen = if screen.contains("until auto-compact") {
            ui.wait("the warning to go away", |s| {
                !s.contains("until auto-compact")
            })
            .await
        } else {
            screen
        };
        assert!(!screen.contains("auto-compact"), "{screen}");
    })
    .await;
}
