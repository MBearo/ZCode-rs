//! 门道（tui-kit.md §4.2「登录」）：打开在欢迎块里、选择与登录的键、密钥的粘贴、接好后说的、在 transcript 末尾。
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::transcript::Cache;
use crate::view::Transcript;

fn app_with(lang: Lang, model: &str) -> App {
    let mut chosen: ModelSelection = selection();
    chosen.model = model.to_owned();
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        lang,
        Transcript::new(
            chosen,
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// 排一帧，返回每行的文字（去掉行尾空白）。
fn screen(app: &mut App) -> Vec<String> {
    let mut cache = Cache::default();
    let now = app.started + Duration::from_secs(1);
    crate::ui::draw::draw(app, &mut cache, now, 0, 80, 30)
        .lines
        .iter()
        .map(|line| line.plain().trim_end().to_owned())
        .collect()
}

fn has(lines: &[String], text: &str) -> bool {
    lines.iter().any(|l| l.contains(text))
}

#[test]
fn the_door_opens_in_the_welcome_block_and_signs_in() {
    let mut app = app_with(Lang::En, "");
    app.open_door(None);
    let lines = screen(&mut app);
    assert!(has(&lines, "Connect a model to start."), "{lines:#?}");
    assert!(has(&lines, "1  Sign in to Z.ai"), "{lines:#?}");
    assert!(has(&lines, "2  Sign in to BigModel"), "{lines:#?}");
    assert!(has(&lines, "3  Use an API key"), "{lines:#?}");
    // 没有模型：esc 不关上，传下去（Puffin 同样）。
    assert_eq!(app.door_key(key(KeyCode::Esc)), None);
    assert!(app.door.is_some());
    // ↓ 选 BigModel，↑ 回到 Z.ai，再 ↑ 回绕到第三个。
    app.door_key(key(KeyCode::Down));
    app.door_key(key(KeyCode::Up));
    app.door_key(key(KeyCode::Up));
    assert_eq!(
        app.door.as_ref().map(|d| d.step.clone()),
        Some(Step::Choose { sel: 2 })
    );
    // 数字直接选定。
    assert_eq!(
        app.door_key(key(KeyCode::Char('1'))),
        Some(Some(Action::Login(Family::Zai)))
    );
    app.door_url(Family::Zai, "https://example.com/sign-in".into());
    let lines = screen(&mut app);
    assert!(has(&lines, "Opened the sign-in page"), "{lines:#?}");
    assert!(has(&lines, "Waiting for you to sign in"), "{lines:#?}");
    // `c` 复制网址，槽里说复制了，到下一个键。
    assert_eq!(
        app.door_key(key(KeyCode::Char('c'))),
        Some(Some(Action::Copy("https://example.com/sign-in".into())))
    );
    assert!(has(&screen(&mut app), "Copied the link"));
    // esc 取消，回到选择、选中 Z.ai。
    assert_eq!(
        app.door_key(key(KeyCode::Esc)),
        Some(Some(Action::CancelLogin))
    );
    assert_eq!(
        app.door.as_ref().map(|d| d.step.clone()),
        Some(Step::Choose { sel: 0 })
    );
    // 取消的回执晚到：已经回到选择，不再动。
    app.door_cancelled();
    assert_eq!(
        app.door.as_ref().map(|d| d.step.clone()),
        Some(Step::Choose { sel: 0 })
    );
}

#[test]
fn a_failed_sign_in_retries_and_success_says_who_in_the_welcome_block() {
    let mut app = app_with(Lang::En, "");
    app.open_door(None);
    app.door_key(key(KeyCode::Enter));
    app.door_failed("the server said no".into());
    assert!(has(&screen(&mut app), "the server said no"));
    assert_eq!(
        app.door_key(key(KeyCode::Enter)),
        Some(Some(Action::Login(Family::Zai)))
    );
    app.door_connected(signed_in_says(Family::Zai, Some("sam")));
    assert!(app.door.is_none());
    let lines = screen(&mut app);
    assert!(has(&lines, "✓ Signed in to Z.ai as sam."), "{lines:#?}");
    assert!(app.said.quiet.is_none());
}

#[test]
fn chinese_puts_bigmodel_first() {
    let mut app = app_with(Lang::Zh, "");
    app.open_door(None);
    assert!(has(&screen(&mut app), "1  Sign in to BigModel"));
    assert_eq!(
        app.door_key(key(KeyCode::Enter)),
        Some(Some(Action::Login(Family::Bigmodel)))
    );
}

#[test]
fn a_key_is_pasted_on_the_board_and_saved() {
    let mut app = app_with(Lang::En, "");
    app.open_door(None);
    app.door_key(key(KeyCode::Char('3')));
    assert_eq!(
        app.door.as_ref().map(|d| d.step.clone()),
        Some(Step::KeyFor { sel: 0 })
    );
    // `Another provider` 在这个版本里没有。
    app.door_key(key(KeyCode::Char('3')));
    assert!(has(
        &screen(&mut app),
        "This isn't available in this version yet"
    ));
    app.door_key(key(KeyCode::Char('2')));
    assert!(matches!(
        app.door.as_ref().map(|d| &d.step),
        Some(Step::Paste { .. })
    ));
    // 打字、粘贴、退格、^U 都改密钥，不改草稿。
    app.door_key(key(KeyCode::Char('x')));
    app.door_key(ctrl('u'));
    assert!(app.on_paste("  sk-1234\n").is_none());
    app.door_key(key(KeyCode::Char('5')));
    app.door_key(key(KeyCode::Backspace));
    assert!(app.composer.is_empty());
    let lines = screen(&mut app);
    assert!(has(&lines, "•••••••"), "{lines:#?}");
    assert!(!has(&lines, "sk-1234"), "{lines:#?}");
    assert_eq!(
        app.door_key(key(KeyCode::Enter)),
        Some(Some(Action::ConfigureKey {
            family: Family::Bigmodel,
            key: RedactedString::new("sk-1234".to_owned()),
        }))
    );
    // 拒绝的原因留在门道里，可以再改。
    app.door_failed("401".into());
    assert!(has(&screen(&mut app), "401"));
    assert_eq!(app.door_key(key(KeyCode::Esc)), Some(None));
    assert_eq!(
        app.door.as_ref().map(|d| d.step.clone()),
        Some(Step::KeyFor { sel: 1 })
    );
}

#[test]
fn with_a_model_esc_closes_and_after_turns_the_door_is_at_the_end() {
    let mut app = app_with(Lang::En, "scripted");
    // 有本地块：门道不在欢迎块里。
    app.info("one\ntwo");
    app.open_door(None);
    assert!(!app.door_in_welcome());
    let lines = screen(&mut app);
    let head = lines
        .iter()
        .position(|l| l.contains("Connect a model to start."));
    let local = lines.iter().position(|l| l.contains("two"));
    assert!(head > local, "{lines:#?}");
    assert_eq!(app.door_key(key(KeyCode::Esc)), Some(None));
    assert!(app.door.is_none());
    // 在末尾接好：说在输入框上方。
    app.open_door(None);
    app.door_connected(keyed_says(Family::Zai, "zai/glm-4.6"));
    assert_eq!(
        app.said.quiet.as_deref(),
        Some("✓ Connected to Z.ai with your key. glm-4.6 answered in 0.0 s.")
    );
}

/// 远程 shell（入口经 Puffin 的 `place` 判断）：链接一到就整段折出，不等浏览器打开失败（Puffin `place.remote`）。
#[test]
fn in_a_remote_shell_the_link_is_folded_out_in_full() {
    let mut app = app_with(Lang::En, "");
    app.remote_shell = true;
    app.open_door(None);
    app.door_key(key(KeyCode::Char('1')));
    app.door_url(Family::Zai, "https://example.com/sign-in".into());
    let lines = screen(&mut app);
    assert!(
        has(&lines, "Open this link on any device and sign in there."),
        "{lines:#?}"
    );
    assert!(has(&lines, "https://example.com/sign-in"), "{lines:#?}");
    assert!(!has(&lines, "Opened the sign-in page"), "{lines:#?}");
}
