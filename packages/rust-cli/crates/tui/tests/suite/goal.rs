//! `/goal`（docs/specs/rust-cli/goal.md §2、§6，工作线 L26）：设定后模型开始工作，停止前评估模型判断条件；
//! 对话里显示命令、`Goal set` 与评估结果，不带参数时打开目标面板。深浅主题、中英文。

use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::{GatedModel, Setup, request_text, with_ui};

#[tokio::test(flavor = "multi_thread")]
async fn a_goal_is_set_checked_and_shown_in_the_panel() {
    // 主请求的回复，之后是评估请求的回复（同一个模型端口，按到达顺序）。
    let model = GatedModel::scripted(vec![
        text_response(&["Working on it."]),
        text_response(&["{\"ok\": true, \"reason\": \"the tests pass\"}"]),
    ]);
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 还没有会话：面板说没有目标。
        ui.submit("/goal");
        ui.wait_for("No goal set").await;
        ui.key(KeyCode::Esc);
        ui.submit("/goal tests pass");
        let screen = ui.wait_for("Goal achieved").await;
        assert!(screen.contains("❯ /goal tests pass"), "{screen}");
        assert!(screen.contains("Goal set: tests pass"), "{screen}");
        assert!(screen.contains("Working on it."), "{screen}");
        assert!(!screen.contains("session-scoped Stop hook"), "{screen}");
        ui.wait("the turn to end", |s| !s.contains("esc to interrupt"))
            .await;
        ui.submit("/goal");
        let panel = ui.wait_for("✓ Goal achieved").await;
        assert!(panel.contains("Goal: tests pass"), "{panel}");
        assert!(
            panel.contains("/goal <condition> to set another"),
            "{panel}"
        );
        ui.key(KeyCode::Esc);
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[0]).contains("session-scoped Stop hook"));
    let evaluation = request_text(&requests[1]);
    assert!(evaluation.contains("Condition: tests pass"), "{evaluation}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_goal_rows_follow_the_language_and_both_themes() {
    for background in [(0, 0, 0), (255, 255, 255)] {
        let model = GatedModel::scripted(vec![
            text_response(&["第一次"]),
            text_response(&["{\"ok\": false, \"reason\": \"还没有\"}"]),
            text_response(&["第二次"]),
            text_response(&["{\"ok\": true, \"reason\": \"好了\"}"]),
        ]);
        let setup = Setup {
            lang: "zh_CN.UTF-8",
            palette: Palette::new(ColorLevel::TrueColor, Some(background)),
            ..Setup::default()
        };
        with_ui(model, setup, |mut ui| async move {
            ui.wait_for("变更前确认").await;
            ui.submit("/goal 测试通过");
            let screen = ui.wait_for("目标已达成").await;
            assert!(screen.contains("目标尚未达成……继续"), "{screen}");
            assert!(screen.contains("第二次"), "{screen}");
        })
        .await;
    }
}
