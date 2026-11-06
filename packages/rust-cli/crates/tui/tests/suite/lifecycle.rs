//! 会话生命周期的界面（docs/specs/rust-cli/session-lifecycle.md §3.3.1、§5.4、§6.2）：esc 连按两次挑选回退的点、`/clear` 之后立刻 `/resume` 回旧会话。`/fork` 在 `fork.rs`。
use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{environments, text_response};

use super::support::{GatedModel, Setup, with_ui};

/// esc 连按两次 → 在最后一条消息上挑选回退的点（Puffin 的挑选）→ esc 离开，什么都没有发生。
#[tokio::test(flavor = "multi_thread")]
async fn double_esc_picks_the_last_message_to_go_back_to() {
    let model = GatedModel::scripted(vec![text_response(&["first answer"])]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("first try");
        ui.wait_for("first answer").await;
        ui.wait("the turn to end", |s| !s.contains("esc stop"))
            .await;
        // 第一次 esc 槽里说 `esc rewind`，第二次在最后一条消息上开始挑选（docs/puffin-tui.md §4.2「回退（/rewind）」）：
        // 切线在它之前，草稿是它的原文，`↶` 代替 `›`。
        ui.key(KeyCode::Esc);
        ui.wait_for("esc rewind").await;
        ui.key(KeyCode::Esc);
        let picking = ui.wait_for("↶ Back to the start").await;
        assert!(
            picking.contains("the turn below stays behind, in /resume"),
            "{picking}"
        );
        assert!(picking.contains(" ↶ first try"), "{picking}");
        // esc 离开挑选：什么都没有发生，输入框是空的。
        ui.key(KeyCode::Esc);
        let after = ui
            .wait("picking to end", |s| !s.contains("↶ Back to"))
            .await;
        assert!(after.contains("first answer"), "{after}");
        assert!(after.contains("Send a message"), "{after}");
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
            s.contains("   first") && !s.contains("esc stop")
        })
        .await;
        ui.submit("/clear");
        ui.wait_for("Started a new session.").await;
        // 旧会话的关闭停在 `close_session`：它的 actor 已经退出，条目还在会话表里。
        env.closing(1).await;
        // `/resume` 的会话列表（Puffin 的列表，板上）：旧会话的行是它的第一条输入。
        ui.submit("/resume");
        ui.wait("the old session in the list", |s| {
            s.contains("› /resume") && s.lines().any(|l| l.trim_end().ends_with("   hello"))
        })
        .await;
        ui.key(KeyCode::Enter);
        ui.wait("the list to close", |s| !s.contains("› /resume"))
            .await;
        // 选中之后的那一帧画出时打开已经开始；再等一会儿只为让旧代码确定地先用上旧句柄（报错），新代码不依赖它。
        ui.settle().await;
        env.release();
        let screen = ui
            .wait("the old session to reopen", |s| {
                s.contains("   first") || s.contains("the session is closed")
            })
            .await;
        assert!(!screen.contains("the session is closed"), "{screen}");
        assert!(screen.contains("hello"), "{screen}");
    })
    .await;
}
