//! Windows 终端的输入（docs/specs/rust-cli/tui-input.md §9，修复线 F65）：平台注入成 Windows，在本机跑同一个事件循环。
//! AltGr 的字符（crossterm 报成 Ctrl+Alt）进输入框；alt+小键盘码（只在松开事件里）进输入框；没有括号粘贴时，很快的
//! 一串按键合并成一次粘贴，换行不发送。注入 Unix 时同样的按键照旧。
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{Os, WindowsRelease};

use super::support::{GatedModel, Setup, request_text, with_ui};

fn windows() -> Setup {
    Setup {
        os: Os::Windows(Some(WindowsRelease {
            major: 10,
            build: 22631,
        })),
        ..Setup::default()
    }
}

/// 按键打出来的文字（Windows 的终端这样送粘贴）：`\n` 是 enter。
fn keys(ui: &super::support::Ui, text: &str) {
    for c in text.chars() {
        match c {
            '\n' => ui.key(KeyCode::Enter),
            c => ui.key(KeyCode::Char(c)),
        }
    }
}

fn release(key: KeyEvent) -> KeyEvent {
    KeyEvent::new_with_kind(key.code, key.modifiers, KeyEventKind::Release)
}

fn altgr(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::ALT)
}

#[tokio::test(flavor = "multi_thread")]
async fn altgr_characters_and_alt_codes_reach_the_input_box() {
    let model = GatedModel::scripted(vec![]);
    with_ui(model, windows(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 每个键都有按下与松开（Windows 的控制台）：配对的松开不重复插入。
        ui.send(altgr('@'));
        ui.send(release(altgr('@')));
        ui.wait_for("❯ @").await;
        ui.send(altgr('{'));
        // AltGr 先松开：字母键的松开不带修饰键、字符也变了，照样配对。
        ui.send(release(KeyEvent::new(
            KeyCode::Char('7'),
            KeyModifiers::NONE,
        )));
        ui.wait_for("❯ @{").await;
        // alt+小键盘码：字符只在 Alt 键的松开事件里，前面没有它的按下。
        ui.send(KeyEvent::new_with_kind(
            KeyCode::Char('é'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        ui.wait_for("❯ @{é").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn other_platforms_ignore_ctrl_alt_characters_and_releases() {
    let model = GatedModel::scripted(vec![]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.send(altgr('@'));
        ui.send(KeyEvent::new_with_kind(
            KeyCode::Char('é'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        ui.type_text("x");
        let screen = ui.wait_for("❯ x").await;
        assert!(!screen.contains('@') && !screen.contains('é'), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paste_burst_folds_and_enter_inside_it_does_not_submit() {
    let model = GatedModel::scripted(vec![text_response(&["got it"])]);
    with_ui(model.clone(), windows(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // Windows 上粘贴的三行是一串很快的字符键与 enter（一次送进事件循环）。
        keys(&ui, "line one\nline two\nline three");
        ui.wait_for("[Pasted text #1 +3 lines]").await;
        // 缓存里的 enter 没有发送；过了 enter 当作换行的窗口（120 ms）再按 enter 才发送。
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let screen = ui.screen();
        assert!(screen.contains("[Pasted text #1 +3 lines]"), "{screen}");
        ui.key(KeyCode::Enter);
        ui.wait_for("got it").await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        request_text(&requests[0]).contains("line one\\nline two\\nline three"),
        "{}",
        request_text(&requests[0])
    );
}
