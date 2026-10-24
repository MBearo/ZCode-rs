//! 会话生命周期的界面（docs/specs/rust-cli/session-lifecycle.md §3.3.1、§5.4、§6.2）：esc 连按两次的 rewind
//! 选择器、`/clear` 之后立刻 `/resume` 回旧会话。`/fork` 在 `fork.rs`。
use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{environments, text_response};

use super::support::{GatedModel, Setup, with_ui};

/// esc 连按两次 → 选择器（Rewind）→ 选第一条 → `Restore conversation`：历史截掉，消息放回输入框。
#[tokio::test(flavor = "multi_thread")]
async fn double_esc_rewinds_the_conversation_and_restores_the_prompt() {
    let model = GatedModel::scripted(vec![text_response(&["first answer"])]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("first try");
        ui.wait_for("first answer").await;
        ui.wait("the turn to end", |s| !s.contains("esc to interrupt"))
            .await;
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Esc);
        let picker = ui.wait_for("Rewind").await;
        assert!(
            picker.contains("Restore the code and/or conversation"),
            "{picker}"
        );
        assert!(picker.contains("first try"), "{picker}");
        assert!(picker.contains("No code changes"), "{picker}");
        // 默认选中 `(current)`；上移到第一条消息。
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        let confirm = ui.wait_for("Restore conversation").await;
        assert!(
            confirm.contains("Confirm you want to restore the conversation"),
            "{confirm}"
        );
        assert!(confirm.contains("Never mind"), "{confirm}");
        ui.key(KeyCode::Enter);
        let after = ui
            .wait("the history to be rewound", |s| !s.contains("first answer"))
            .await;
        assert!(
            after.contains("first try"),
            "the prompt is back in the input: {after}"
        );
    })
    .await;
}

/// 修复线 F33（session-lifecycle.md §3.3.1）：`/clear` 在后台关旧会话、不等。关闭停在执行环境的 `close_session` 上
/// （actor 已退出、还没移出会话表）时 `/resume` 选回旧会话：以前打开从会话表拿到旧句柄，界面写
/// `the session is closed`；现在打开等关闭走完再冷加载，旧会话照常重开。
#[tokio::test(flavor = "multi_thread")]
async fn resume_right_after_clear_reopens_the_old_session() {
    use rust_cli_kernel_api::testing::ClosingEnvironment;
    use std::sync::Arc;

    let model = GatedModel::scripted(vec![text_response(&["first"])]);
    let env = Arc::new(ClosingEnvironment::held());
    let setup = Setup {
        environments: environments(env.clone()),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hello");
        ui.wait("the first reply", |s| {
            s.contains("⏺ first") && !s.contains("esc to interrupt")
        })
        .await;
        ui.submit("/clear");
        ui.wait_for("Started a new session.").await;
        // 旧会话的关闭停在 `close_session`：它的 actor 已经退出，条目还在会话表里。
        env.closing(1).await;
        ui.submit("/resume");
        ui.wait("the old session in the list", |s| {
            s.contains("Type to search") && s.contains("hello")
        })
        .await;
        ui.key(KeyCode::Enter);
        ui.wait("the page to close", |s| !s.contains("Type to search"))
            .await;
        // 选中之后的那一帧画出时打开已经开始；再等一会儿只为让旧代码确定地先用上旧句柄（报错），新代码不依赖它。
        ui.settle().await;
        env.release();
        let screen = ui
            .wait("the old session to reopen", |s| {
                s.contains("⏺ first") || s.contains("the session is closed")
            })
            .await;
        assert!(!screen.contains("the session is closed"), "{screen}");
        assert!(screen.contains("hello"), "{screen}");
    })
    .await;
}
