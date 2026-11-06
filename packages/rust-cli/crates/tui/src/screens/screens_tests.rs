//! 启动前的信任页与更新选择屏（迁自已删除的 Puffin 界面的 `shell/kept/screens_tests.rs`；旧 tui 的 ui/trust_view.rs 第
//! 120–177 行、ui/update_view.rs 第 90–122 行的测试）。
use rust_cli_tui_kit::app::cmd::Cmd;
use rust_cli_tui_kit::app::element::stack;
use rust_cli_tui_kit::app::key::Key;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::screen::App;
use rust_cli_tui_kit::app::widgets::kept_rows::list;
use rust_cli_tui_kit::render::line::StyledLine;
use rust_cli_tui_kit::theme::theme::Theme;

use super::{TrustScreen, TrustUi, UpdateScreen, trust_key, trust_rows, update_key, update_rows};
use crate::trust::{Grants, TrustAnswer, TrustPrompt};
use crate::update::{UpdateChoice, UpdateInfo};

fn theme() -> Theme {
    Theme::choose(None, None, true)
}

fn text(rows: Vec<rust_cli_tui_kit::app::element::Element>) -> Vec<String> {
    Layout::lay(&stack(rows), 100)
        .iter()
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .collect()
}

fn prompt(backstop: bool, grants: Option<Grants>) -> TrustPrompt {
    TrustPrompt {
        path: "/Users/dev/project".into(),
        backstop,
        grants,
    }
}

fn info() -> UpdateInfo {
    UpdateInfo {
        current: "0.9.0".into(),
        latest: "0.10.0".into(),
        command: "npm install -g zcode".into(),
        notes_url: "https://example.com/notes".into(),
    }
}

/// 信任的键：焦点开始在取消项；↓ 到信任项；数字直接选；esc 未信任时退出（0），后备时不带授权继续；^C 连按两次退出（1）。
#[test]
fn trust_keys() {
    let mut ui = TrustUi::default();
    assert_eq!(
        trust_key(&mut ui, false, &Key::Enter),
        Some(TrustAnswer::Exit(1))
    );
    assert_eq!(
        trust_key(&mut ui, true, &Key::Enter),
        Some(TrustAnswer::WithoutGrants)
    );
    trust_key(&mut ui, false, &Key::Down);
    assert_eq!(
        trust_key(&mut ui, false, &Key::Enter),
        Some(TrustAnswer::Trusted)
    );
    trust_key(&mut ui, false, &Key::Up);
    assert_eq!(ui.focus, 0);
    assert_eq!(
        trust_key(&mut ui, false, &Key::Char('2')),
        Some(TrustAnswer::Trusted)
    );
    assert_eq!(
        trust_key(&mut ui, false, &Key::Esc),
        Some(TrustAnswer::Exit(0))
    );
    assert_eq!(
        trust_key(&mut ui, true, &Key::Esc),
        Some(TrustAnswer::WithoutGrants)
    );
    assert_eq!(trust_key(&mut ui, false, &Key::Ctrl('C')), None);
    assert_eq!(ui.armed, Some('C'));
    assert_eq!(
        trust_key(&mut ui, false, &Key::Ctrl('C')),
        Some(TrustAnswer::Exit(1))
    );
    // 中间按了别的键：重新计。
    let mut ui = TrustUi::default();
    trust_key(&mut ui, false, &Key::Ctrl('D'));
    trust_key(&mut ui, false, &Key::Char('x'));
    assert_eq!(trust_key(&mut ui, false, &Key::Ctrl('D')), None);
}

/// 信任页的行：标题与路径、安全检查、项目授权、两个回答、按键提示。
#[test]
fn trust_rows_read_like_the_old_dialog() {
    let grants = Grants {
        count: 9,
        rules: (1..=9).map(|i| format!("Bash(r{i})")).collect(),
        sources: vec![".zcode/settings.json".into()],
    };
    let lines = text(trust_rows(
        &theme(),
        &prompt(true, Some(grants)),
        &TrustUi::default(),
    ));
    assert_eq!(lines[0], "   Accessing workspace:");
    assert_eq!(lines[1], "   /Users/dev/project");
    assert!(
        lines
            .iter()
            .any(|l| l
                == "   ⚠ This folder pre-approves 9 tool permissions in .zcode/settings.json:"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("Bash(r8), and 1 more")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "   1  No, continue without these permissions")
    );
    assert!(lines.iter().any(|l| l == "   2  Yes, I trust this folder"));
    assert_eq!(
        lines.last().map(String::as_str),
        Some("   ⏎ confirm  ·  esc cancel")
    );
    let ui = TrustUi {
        focus: 0,
        armed: Some('C'),
    };
    let lines = text(trust_rows(&theme(), &prompt(false, None), &ui));
    assert!(lines.iter().any(|l| l == "   1  No, exit"));
    assert_eq!(
        lines.last().map(String::as_str),
        Some("   ^C again to exit")
    );
    assert_eq!(list(&["a".into(), "b".into()], None), "a and b");
    assert_eq!(
        list(&["a".into(), "b".into(), "c".into()], None),
        "a, b, and c"
    );
}

/// 启动前的信任页是一个小应用：回答之后退出，回答在最终状态里。
#[test]
fn the_trust_screen_quits_with_its_answer() {
    let screen = TrustScreen {
        prompt: prompt(false, None),
        theme: theme(),
    };
    let (mut state, _) = screen.init();
    assert!(matches!(screen.update(Key::Down, &mut state), Cmd::None));
    assert!(matches!(screen.update(Key::Enter, &mut state), Cmd::Quit));
    assert_eq!(state.answer, Some(TrustAnswer::Trusted));
    assert_eq!(screen.subscriptions(&state).len(), 1);
}

/// 更新的键：焦点开始在 Update now，↑↓ 与 k / j 回绕，数字直接选，esc 与 ^C 是 Skip。
#[test]
fn update_keys() {
    let mut focus = 0;
    update_key(&mut focus, &Key::Up);
    assert_eq!(focus, 2);
    update_key(&mut focus, &Key::Char('j'));
    assert_eq!(focus, 0);
    update_key(&mut focus, &Key::Char('j'));
    assert_eq!(
        update_key(&mut focus, &Key::Enter),
        Some(UpdateChoice::Skip)
    );
    assert_eq!(
        update_key(&mut focus, &Key::Char('3')),
        Some(UpdateChoice::SkipUntilNext)
    );
    assert_eq!(
        update_key(&mut focus, &Key::Char('1')),
        Some(UpdateChoice::UpdateNow)
    );
    assert_eq!(update_key(&mut focus, &Key::Esc), Some(UpdateChoice::Skip));
    assert_eq!(
        update_key(&mut focus, &Key::Ctrl('D')),
        Some(UpdateChoice::Skip)
    );
    assert_eq!(update_key(&mut focus, &Key::Char('x')), None);
}

/// 更新选择屏的行与小应用。
#[test]
fn the_update_screen() {
    let lines = text(update_rows(&theme(), &info(), 0));
    assert_eq!(lines[0], "   Update available   0.9.0 → 0.10.0");
    assert_eq!(lines[1], "   Release notes: https://example.com/notes");
    assert_eq!(
        lines[3],
        "   1  Update now                runs `npm install -g zcode`"
    );
    assert_eq!(lines[4], "   2  Skip");
    assert_eq!(lines[5], "   3  Skip until next version");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("   ⏎ continue  ·  esc skip")
    );
    let screen = UpdateScreen {
        info: info(),
        theme: theme(),
    };
    let (mut state, _) = screen.init();
    assert!(matches!(
        screen.update(Key::Char('2'), &mut state),
        Cmd::Quit
    ));
    assert_eq!(state.choice, Some(UpdateChoice::Skip));
}
