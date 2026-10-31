//! 回合结束那一行（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：真实内核，内核的时钟固定在 0（UTC 的
//! `12:00 AM`）。正常结束与出错结束的回合在最后有 `✻ <动词> for 0s · done 12:00 AM`，中断的没有；后台还有在跑的命令时
//! 接 ` · 1 monitor still running`（与 footer 的 pill 同一份文字）。修前没有这一行。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent};
use rust_cli_kernel_api::testing::{environments, text_response};

use super::ledger::Ledger;
use super::support::{GatedModel, Setup, row_of, send, with_ui};

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
    // 修 bug（工作线 F124 顺带，2026-10-31）：以前模型一个字没吐就按 esc，回合以中断结束后 esc 撤回（F38）随即把这条
    // 输入删掉、放回输入框，屏幕上的 `Interrupted` 只是撤回之前的一帧；撤回快、那一帧没画出来时这里等不到（ZCode-rs 的
    // 验证链里 10 秒超时一次）。先流出半句：输入算回答过，不撤回，中断的标记一直留在屏幕上。
    let (model, senders) = GatedModel::new(1);
    let first = senders.into_iter().next().unwrap();
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("hi");
        send(
            &first,
            vec![
                Ok(ModelEvent::MessageStart {
                    id: "msg_api".to_owned(),
                    model: "scripted".to_owned(),
                    usage: Default::default(),
                }),
                Ok(ModelEvent::BlockStart {
                    index: 0,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                }),
                // 带换行：流式中没写完的最后一行不显示。
                Ok(ModelEvent::BlockDelta {
                    index: 0,
                    delta: BlockDelta::Text("partial reply\n".to_owned()),
                }),
            ],
        );
        ui.wait_for("partial reply").await;
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the interrupted turn", |s| {
                s.contains("Interrupted") && idle(s)
            })
            .await;
        assert!(turn_line(&screen).is_none(), "{screen}");
        assert!(!screen.contains(" for 0s"), "{screen}");
        drop(first);
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
