//! 终端通知（docs/specs/rust-cli/tui-manage.md §5）：用 tokio 的暂停时钟推进时间，不真的等待。
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

fn sent(ui: &Ui) -> Vec<String> {
    ui.notifications.lock().unwrap().clone()
}

#[tokio::test(start_paused = true)]
async fn a_waiting_permission_and_an_idle_prompt_notify_iterm2() {
    let setup = Setup {
        vars: vec![("TERM_PROGRAM", "iTerm.app")],
        ..Setup::default()
    };
    with_ui(bash_then_text(), setup, |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Bash command").await;
        // 5 s 时还不发；满 6 s 发一次。
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(sent(&ui).is_empty());
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(sent(&ui), ["\x1b]9;\n\nZCode needs your permission\x07"]);
        ui.key(KeyCode::Esc);
        ui.wait_for("file step done").await;
        // 回合结束 60 s 后空闲提醒（之间没有按键）。
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert_eq!(sent(&ui).len(), 1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            sent(&ui).last().map(String::as_str),
            Some("\x1b]9;\n\nZCode is waiting for your input\x07")
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn no_channel_and_keys_hold_the_notification() {
    // 没有渠道：不写。
    with_ui(bash_then_text(), Setup::default(), |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Bash command").await;
        tokio::time::sleep(Duration::from_secs(13)).await;
        assert!(sent(&ui).is_empty());
        ui.key(KeyCode::Esc);
        ui.wait_for("file step done").await;
    })
    .await;
    // 有按键时推迟到安静满 6 s 的下一次检查。
    let setup = Setup {
        vars: vec![("TERM_PROGRAM", "ghostty")],
        ..Setup::default()
    };
    with_ui(bash_then_text(), setup, |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Bash command").await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        ui.key(KeyCode::Down);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(sent(&ui).is_empty());
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(
            sent(&ui),
            ["\x1b]777;notify;ZCode;ZCode needs your permission\x07"]
        );
        ui.key(KeyCode::Esc);
        ui.wait_for("file step done").await;
    })
    .await;
}
