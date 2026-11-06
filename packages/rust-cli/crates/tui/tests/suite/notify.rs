//! 终端通知（docs/specs/rust-cli/tui-manage.md §5；请求的通知照 Puffin，tui-kit.md §4.2「终端通知」）：终端不在前台时每个
//! 新到的请求响一次铃（BEL）并发终端的通知，窗口标题换成 `● 名字`，请求都离开后换回来；在前台时什么都不发。回合结束后
//! 空闲 60 s 的提醒是旧界面才有的，照旧。用 tokio 的暂停时钟推进时间，不真的等待。
use std::time::Duration;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::support::{GatedModel, Setup, Ui, with_ui};

fn bash_then_text() -> std::sync::Arc<GatedModel> {
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &json!({"command": "touch a.txt", "description": "create a file"}),
        ),
        text_response(&["file step done"]),
    ])
}

/// 请求打开后 12 个 tick（396 ms）内回答没有点亮（`ARM_TICKS`），那是真实时间（`std::time::Instant`）；这里的测试时钟是暂停
/// 的，虚拟的几秒不让真实时间前进。按键之前阻塞本线程等过真实的 450 ms：暂停的时钟不动，计时的断言不变。
fn past_arming() {
    std::thread::sleep(Duration::from_millis(450));
}

/// 写到终端的铃与通知（以 BEL 开头的那些；窗口标题另算）。
fn rings(ui: &Ui) -> Vec<String> {
    ui.notifications
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.starts_with('\u{7}'))
        .cloned()
        .collect()
}

/// 写到终端的窗口标题（推、设、弹）。
fn titles(ui: &Ui) -> Vec<String> {
    ui.notifications
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.starts_with("\x1b[") || s.starts_with("\x1b]0;"))
        .cloned()
        .collect()
}

/// 等 runner 处理完排着的事件（暂停的时钟下 sleep 立即推进到下一个定时器）。
async fn tick() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test(start_paused = true)]
async fn an_ask_rings_while_the_terminal_is_away_and_the_title_follows() {
    let setup = Setup {
        vars: vec![("TERM_PROGRAM", "iTerm.app")],
        ..Setup::default()
    };
    with_ui(bash_then_text(), setup, |mut ui| async move {
        ui.focus(false);
        ui.submit("make a file");
        ui.wait_for("Run this command?").await;
        tick().await;
        // 一到就响，不等（以前要安静满 6 s）。
        assert_eq!(
            rings(&ui),
            ["\u{7}\x1b]9;\n\nZCode needs your permission\u{7}"]
        );
        let shown = titles(&ui);
        assert!(
            shown
                .iter()
                .any(|t| t.starts_with("\x1b[22;0t\x1b]0;● ") && t.ends_with('\u{7}')),
            "{shown:?}"
        );
        past_arming();
        ui.key(KeyCode::Char('3'));
        ui.wait_for("file step done").await;
        tick().await;
        // 请求离开了：标题换回会话的名字（没有名字时弹回终端原来的）。
        let shown = titles(&ui);
        assert_eq!(
            shown.last().map(String::as_str),
            Some("\x1b]0;make a file\u{7}"),
            "{shown:?}"
        );
        // 回合结束 60 s 后空闲提醒（之间没有按键；旧界面才有的，只发通知）。回答按播放头一个 tick 一个 tick 打出，暂停的
        // 时钟下画完时回合已经结束了约 8 s，所以在画完后 45 s 与 65 s 两头看。
        tokio::time::sleep(Duration::from_secs(45)).await;
        let idle = ui.notifications.lock().unwrap().len();
        assert_eq!(idle, 3, "{:?}", ui.notifications.lock().unwrap());
        tokio::time::sleep(Duration::from_secs(20)).await;
        let all = ui.notifications.lock().unwrap().clone();
        assert_eq!(
            all[idle..].last().map(String::as_str),
            Some("\x1b]9;\n\nZCode is waiting for your input\x07"),
            "{all:?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_focused_terminal_gets_no_ring_and_no_title() {
    let setup = Setup {
        vars: vec![("TERM_PROGRAM", "ghostty")],
        ..Setup::default()
    };
    with_ui(bash_then_text(), setup, |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Run this command?").await;
        tokio::time::sleep(Duration::from_secs(13)).await;
        assert!(rings(&ui).is_empty());
        assert!(!titles(&ui).iter().any(|t| t.contains("● ")));
        // 请求开着时离开终端：已经到过的请求不再响。
        ui.focus(false);
        tick().await;
        assert!(rings(&ui).is_empty());
        past_arming();
        ui.key(KeyCode::Char('3'));
        ui.wait_for("file step done").await;
    })
    .await;
    // 没有渠道：只响铃。
    with_ui(bash_then_text(), Setup::default(), |mut ui| async move {
        ui.focus(false);
        ui.submit("make a file");
        ui.wait_for("Run this command?").await;
        tick().await;
        assert_eq!(rings(&ui), ["\u{7}"]);
        past_arming();
        ui.key(KeyCode::Char('3'));
        ui.wait_for("file step done").await;
    })
    .await;
}
