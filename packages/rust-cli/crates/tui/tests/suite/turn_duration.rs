//! 回合结束那一行（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：真实内核，内核的时钟固定在 0（UTC 的
//! `12:00 AM`）。正常结束与出错结束的回合在最后有 `✻ <动词> for 0s · done 12:00 AM`，中断的没有；后台还有在跑的命令时
//! 接 ` · 1 monitor still running`（与 footer 的 pill 同一份文字）。修前没有这一行。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::testing::{environments, text_response};

use super::ledger::Ledger;
use super::support::{GatedModel, Setup, row_of, with_ui};

const LINE: &str = " for 0s · done 12:00 AM";

/// 画面上回合结束那一行（`✻ ` 开头、带 `LINE`）。
fn turn_line(screen: &str) -> Option<String> {
    screen
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("✻ ") && line.contains(LINE))
        .map(str::to_owned)
}

fn idle(screen: &str) -> bool {
    !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_turn_ends_with_the_duration_line() {
    let model = GatedModel::scripted(vec![text_response(&["hello there"])]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the duration line", |s| turn_line(s).is_some())
            .await;
        let line = turn_line(&screen).unwrap();
        let verb = line
            .trim_start_matches("✻ ")
            .split(' ')
            .next()
            .unwrap()
            .to_owned();
        assert!(
            [
                "Baked",
                "Brewed",
                "Churned",
                "Cogitated",
                "Cooked",
                "Crunched",
                "Sautéed",
                "Worked"
            ]
            .contains(&verb.as_str()),
            "{line}"
        );
        assert_eq!(line, format!("✻ {verb}{LINE}"));
        // 在回复下面，空一行。
        let reply = row_of(&screen, "hello there").unwrap();
        assert_eq!(row_of(&screen, LINE), Some(reply + 2), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_turn_also_ends_with_the_line_below_the_error() {
    let model = GatedModel::scripted(vec![vec![Err(ModelError::InvalidRequest {
        message: "bad request".into(),
    })]]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the error and the line", |s| {
                s.contains("bad request") && turn_line(s).is_some()
            })
            .await;
        let error = row_of(&screen, "bad request").unwrap();
        assert!(row_of(&screen, LINE).unwrap() > error, "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_turn_has_no_line() {
    let (model, senders) = GatedModel::new(1);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("hi");
        ui.wait_for("esc to interrupt").await;
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the interrupted turn", |s| {
                s.contains("Interrupted") && idle(s)
            })
            .await;
        assert!(turn_line(&screen).is_none(), "{screen}");
        assert!(!screen.contains(" for 0s"), "{screen}");
        drop(senders);
    })
    .await;
}

/// 后台有一条在跑的 monitor：这一行接 ` · 1 monitor still running`，footer 是 `1 monitor`。
#[tokio::test(flavor = "multi_thread")]
async fn a_running_monitor_is_named_at_the_end_of_the_line() {
    let model = GatedModel::scripted(vec![text_response(&["watching"])]);
    let setup = Setup {
        width: 100,
        environments: environments(Arc::new(Ledger(&["b2"]))),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("go");
        let screen = ui
            .wait("the still running segment", |s| {
                turn_line(s).is_some_and(|line| line.ends_with("· 1 monitor still running"))
            })
            .await;
        assert!(screen.contains("· 1 monitor · ↓ to manage"), "{screen}");
    })
    .await;
}

/// shell 与 monitor 都在跑：`· 1 shell, 1 monitor still running`；中文：`用时 0s · 完成于 0:00 · 1 shell、1 monitor 仍在运行`。
#[tokio::test(flavor = "multi_thread")]
async fn shells_and_monitors_both_show_in_english_and_chinese() {
    for (lang, wanted) in [
        ("en_US.UTF-8", "· 1 shell, 1 monitor still running"),
        (
            "zh_CN.UTF-8",
            "✻ 用时 0s · 完成于 0:00 · 1 shell、1 monitor 仍在运行",
        ),
    ] {
        let model = GatedModel::scripted(vec![text_response(&["watching"])]);
        let setup = Setup {
            width: 100,
            lang,
            environments: environments(Arc::new(Ledger(&["b1", "b2"]))),
            ..Setup::default()
        };
        with_ui(model, setup, |mut ui| async move {
            ui.submit("go");
            ui.wait(wanted, |s| {
                s.lines().any(|line| line.trim().ends_with(wanted))
            })
            .await;
        })
        .await;
    }
}
