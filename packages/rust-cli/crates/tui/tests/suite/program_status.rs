//! 程序状态（OSC 7501，docs/specs/rust-cli/tui-manage.md §5.4）：支持的终端里一次带权限询问的回合依次报告
//! idle → working → blocked → working → done，离开时 clear；不支持的终端一个 7501 序列都不写。
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use serde_json::json;

use super::support::{GatedModel, Setup, finish, with_ui};

fn bash_then_text() -> Arc<GatedModel> {
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &json!({"command": "touch a.txt", "description": "create a file"}),
        ),
        text_response(&["file step done"]),
    ])
}

/// 写到终端的根记录的状态（不带 `id=` 的报告），连着相同的只留一个。
fn root_states(written: &[String]) -> Vec<String> {
    let mut states: Vec<String> = Vec::new();
    for sequence in written {
        for report in sequence.split("\x1b]7501;").skip(1) {
            let body = report.trim_end_matches("\x1b\\");
            if body.split(':').any(|pair| pair.starts_with("id=")) {
                continue;
            }
            let state = body
                .split(':')
                .find_map(|pair| pair.strip_prefix("state="))
                .unwrap_or_default()
                .to_owned();
            if states.last() != Some(&state) {
                states.push(state);
            }
        }
    }
    states
}

/// 跑一次带权限询问的回合（脚本写好的模型），返回写到终端的全部序列（含离开时的）。
async fn one_turn(setup: Setup) -> Vec<String> {
    let written = Arc::new(Mutex::new(None));
    let keep = written.clone();
    with_ui(bash_then_text(), setup, |mut ui| async move {
        *keep.lock().unwrap() = Some(ui.notifications.clone());
        ui.submit("make a file");
        ui.wait_for("Run this command?").await;
        // 回答的按键在请求打开 12 个 tick 之后才算（`ARM_TICKS`，真实时间）。
        ui.settle().await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for("file step done").await;
        ui.settle().await;
    })
    .await;
    let all = written.lock().unwrap().take().unwrap();
    all.lock().unwrap().clone()
}

/// 等到根记录最后报告的状态是 `state`。
async fn until_root(written: &Arc<Mutex<Vec<String>>>, state: &str) {
    let wait = async {
        while root_states(&written.lock().unwrap())
            .last()
            .map(String::as_str)
            != Some(state)
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the root never reported {state}: {:?}",
                written.lock().unwrap()
            )
        });
}

#[tokio::test]
async fn a_supporting_terminal_hears_the_turn_and_the_ask() {
    // 模型的两个响应由测试放行：回合在跑、等询问、再跑都能各画出一帧。
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    let setup = Setup {
        program_status: true,
        ..Setup::default()
    };
    let notifications = Arc::new(Mutex::new(None));
    let keep = notifications.clone();
    with_ui(model, setup, |mut ui| async move {
        let written = ui.notifications.clone();
        *keep.lock().unwrap() = Some(written.clone());
        until_root(&written, "idle").await;
        ui.submit("make a file");
        until_root(&written, "working").await;
        finish(
            first,
            tool_use_response(
                "toolu_1",
                "Bash",
                &json!({"command": "touch a.txt", "description": "create a file"}),
            ),
        );
        ui.wait_for("Run this command?").await;
        until_root(&written, "blocked").await;
        ui.settle().await;
        ui.key(KeyCode::Char('3'));
        until_root(&written, "working").await;
        finish(second, text_response(&["file step done"]));
        ui.wait_for("file step done").await;
        until_root(&written, "done").await;
    })
    .await;
    let written = notifications.lock().unwrap().take().unwrap();
    let written = written.lock().unwrap().clone();
    assert_eq!(
        root_states(&written),
        ["idle", "working", "blocked", "working", "done", "clear"],
        "{written:?}"
    );
    // 每条根记录都带 `app`；询问是权限，说法加上工具名。
    let blocked = written
        .iter()
        .find(|s| s.contains("state=blocked"))
        .unwrap();
    assert!(
        blocked.contains("state=blocked:kind=permission:app=zcode:msg="),
        "{blocked}"
    );
    let msg = blocked
        .split("msg=")
        .nth(1)
        .unwrap()
        .trim_end_matches("\x1b\\");
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(msg)
        .unwrap();
    assert_eq!(
        String::from_utf8(decoded).unwrap(),
        "ZCode needs your permission: Bash"
    );
}

#[tokio::test]
async fn other_terminals_get_no_7501() {
    let written = one_turn(Setup::default()).await;
    assert!(
        !written.iter().any(|s| s.contains("\x1b]7501;")),
        "{written:?}"
    );
}
