//! 切到完全访问的确认框（docs/specs/rust-cli/tui.md §7.8，工作线 F87）：什么时候弹、按键、不叠框、离开会话作废、排版。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::app::Local;
use crate::ui::i18n::{Lang, strings};
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;
use crate::view::Transcript;

const FULL: PermissionMode = PermissionMode::BypassPermissions;

fn app(mode: PermissionMode) -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(selection(), mode, crate::view::test_workspace()),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

fn press(app: &mut App, code: KeyCode) -> Option<Action> {
    app.yolo_confirm_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn dialog(app: &App) -> Option<&YoloConfirm> {
    match &app.overlay {
        Some(Overlay::YoloConfirm(confirm)) => Some(confirm),
        _ => None,
    }
}

fn focus(app: &App) -> Focus {
    dialog(app).unwrap().focus
}

fn infos(app: &App) -> Vec<String> {
    app.local
        .iter()
        .filter_map(|cell| match &cell.local {
            Local::Info(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn only_switching_into_full_access_from_another_mode_asks() {
    for from in [
        PermissionMode::Default,
        PermissionMode::Guarded,
        PermissionMode::AcceptEdits,
    ] {
        let mut app = app(from);
        assert_eq!(app.switch_mode(FULL, true), None, "{from:?}");
        let opened = YoloConfirm {
            checked: false,
            focus: Focus::Cancel,
            announce: true,
        };
        assert_eq!(dialog(&app), Some(&opened));
        assert!(infos(&app).is_empty());
    }
    // 已经是完全访问：照旧发命令并提示。
    let mut full = app(FULL);
    assert_eq!(full.switch_mode(FULL, true), Some(Action::SetMode(FULL)));
    assert!(dialog(&full).is_none());
    assert_eq!(infos(&full), ["Mode switched to Full access."]);
    // 别的模式照旧。
    let mut build = app(PermissionMode::Default);
    let guarded = PermissionMode::Guarded;
    assert_eq!(
        build.switch_mode(guarded, false),
        Some(Action::SetMode(guarded))
    );
    assert!(infos(&build).is_empty());
}

#[test]
fn shift_tab_asks_only_on_the_way_into_full_access() {
    let back_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    let mut guarded = app(PermissionMode::Guarded);
    assert_eq!(guarded.on_key(back_tab, Instant::now()), None);
    assert_eq!(dialog(&guarded).map(|d| d.announce), Some(false));
    // 从完全访问回到变更前确认不问。
    let mut full = app(FULL);
    assert_eq!(
        full.on_key(back_tab, Instant::now()),
        Some(Action::SetMode(PermissionMode::Default))
    );
    assert!(dialog(&full).is_none());
}

/// 框开着时再请求一次（`/mode yolo`、显式入口）：同一个框，勾选与焦点不变，不提示。
#[test]
fn a_second_request_does_not_stack_or_reset() {
    let mut app = app(PermissionMode::Guarded);
    app.switch_mode(FULL, false);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.mode_command("yolo"), None);
    assert_eq!(app.switch_mode(FULL, true), None);
    let kept = YoloConfirm {
        checked: true,
        focus: Focus::Acknowledge,
        announce: false,
    };
    assert_eq!(dialog(&app), Some(&kept));
    assert!(infos(&app).is_empty());
}

#[test]
fn keys_move_check_and_confirm() {
    let mut app = app(PermissionMode::Default);
    app.switch_mode(FULL, true);
    // ↑↓ 到头回绕：取消 → 确认 → 勾选框 → 取消。
    for (code, expected) in [
        (KeyCode::Down, Focus::Confirm),
        (KeyCode::Down, Focus::Acknowledge),
        (KeyCode::Down, Focus::Cancel),
        (KeyCode::Up, Focus::Acknowledge),
        (KeyCode::Up, Focus::Confirm),
    ] {
        press(&mut app, code);
        assert_eq!(focus(&app), expected);
    }
    // 没勾选时确认不可用；Space 只在勾选框上起作用。
    assert_eq!(press(&mut app, KeyCode::Enter), None);
    press(&mut app, KeyCode::Char(' '));
    assert!(!dialog(&app).unwrap().checked);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    assert!(dialog(&app).unwrap().checked);
    // 勾选框上 Enter 同 Space。
    press(&mut app, KeyCode::Enter);
    assert!(!dialog(&app).unwrap().checked);
    press(&mut app, KeyCode::Enter);
    // 别的键不起作用。
    let before = dialog(&app).cloned();
    for code in [
        KeyCode::BackTab,
        KeyCode::Tab,
        KeyCode::Char('x'),
        KeyCode::Left,
    ] {
        assert_eq!(press(&mut app, code), None);
    }
    assert_eq!(dialog(&app).cloned(), before);
    // 勾上之后确认：关掉框，提示并发命令。
    press(&mut app, KeyCode::Up);
    assert_eq!(press(&mut app, KeyCode::Enter), Some(Action::SetMode(FULL)));
    assert!(dialog(&app).is_none());
    assert_eq!(infos(&app), ["Mode switched to Full access."]);
}

/// 数字键（工作线 F92）：`1` 同在“取消”上按 Enter、`2` 同在“确认启用”上按 Enter，都不看焦点；没勾选时 `2` 不起作用、
/// 焦点不动；别的数字不起作用。
#[test]
fn digits_choose_the_numbered_option() {
    let set = |app: &mut App, checked: bool, focus: Focus| {
        if let Some(Overlay::YoloConfirm(confirm)) = app.overlay.as_mut() {
            confirm.checked = checked;
            confirm.focus = focus;
        }
    };
    // 1：焦点在哪、勾没勾选都是取消，不发命令、不提示。
    for checked in [false, true] {
        for focus in Focus::ORDER {
            let mut app = app(PermissionMode::Guarded);
            app.switch_mode(FULL, true);
            set(&mut app, checked, focus);
            assert_eq!(press(&mut app, KeyCode::Char('1')), None);
            assert!(dialog(&app).is_none(), "{checked} {focus:?}");
            assert!(infos(&app).is_empty());
        }
    }
    // 2 没勾选：不起作用，焦点不动。
    let mut app = app(PermissionMode::Guarded);
    app.switch_mode(FULL, true);
    for focus in Focus::ORDER {
        set(&mut app, false, focus);
        assert_eq!(press(&mut app, KeyCode::Char('2')), None);
        let kept = YoloConfirm {
            checked: false,
            focus,
            announce: true,
        };
        assert_eq!(dialog(&app), Some(&kept));
    }
    // 别的数字不起作用（勾选了也一样）。
    set(&mut app, true, Focus::Acknowledge);
    let before = dialog(&app).cloned();
    for digit in ['0', '3', '4', '9'] {
        assert_eq!(press(&mut app, KeyCode::Char(digit)), None, "{digit}");
        assert_eq!(dialog(&app).cloned(), before, "{digit}");
    }
    // 2 勾选后：焦点在勾选框上也直接确认，提示并发命令。
    assert_eq!(
        press(&mut app, KeyCode::Char('2')),
        Some(Action::SetMode(FULL))
    );
    assert!(dialog(&app).is_none());
    assert_eq!(infos(&app), ["Mode switched to Full access."]);
}

#[test]
fn cancelling_sends_nothing() {
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    // esc、ctrl+c、取消上的 Enter（勾选了也一样）。
    for key in [esc, ctrl_c, enter] {
        let mut app = app(PermissionMode::Guarded);
        app.switch_mode(FULL, true);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Down);
        assert_eq!(app.yolo_confirm_key(key), None, "{key:?}");
        assert!(dialog(&app).is_none());
        assert!(infos(&app).is_empty());
    }
}

/// 离开会话（`/clear`、切换会话）：框作废，不发命令。
#[test]
fn leaving_the_session_drops_the_dialog() {
    let mut app = app(PermissionMode::Guarded);
    app.switch_mode(FULL, false);
    app.reset();
    assert!(dialog(&app).is_none());
}

fn render(confirm: &YoloConfirm, lang: Lang, width: usize, height: usize) -> Vec<String> {
    let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let view = View {
        palette: &palette,
        strings: strings(lang),
        lang,
        width,
        height: 24,
        now_ms: 0,
    };
    lines(confirm, &view, height)
        .iter()
        .map(|line| plain(line).trim_end().to_owned())
        .collect()
}

/// 空行在哪些行的前面（行的开头几个字）。
fn blanks_before(lines: &[String]) -> Vec<String> {
    lines
        .windows(2)
        .filter(|pair| pair[0].is_empty())
        .map(|pair| pair[1].trim_start().chars().take(5).collect())
        .collect()
}

#[test]
fn blank_lines_go_when_the_dialog_does_not_fit() {
    let confirm = YoloConfirm::default();
    // 英文 80 列：不算空行 13 行。
    let roomy = render(&confirm, Lang::En, 80, 40);
    assert_eq!(roomy.len(), 17, "{roomy:#?}");
    assert_eq!(blanks_before(&roomy), ["Files", "These", "[ ] I", "↑/↓ t"]);
    // 80×24 的底部区域是 16 行：先去掉风险段前面的空行。
    let tight = render(&confirm, Lang::En, 80, 16);
    assert_eq!(blanks_before(&tight), ["Files", "[ ] I", "↑/↓ t"]);
    let tighter = render(&confirm, Lang::En, 80, 14);
    assert_eq!(blanks_before(&tighter), ["[ ] I"]);
    let none = render(&confirm, Lang::En, 80, 10);
    assert_eq!(none.len(), 13, "{none:#?}");
    assert!(none[0].starts_with("────"));
    assert_eq!(none[1], "  ⚠ Enable full access?");
    // 中文 80 列：16 行放得下全部空行。
    let zh = render(&confirm, Lang::Zh, 80, 16);
    assert_eq!(zh.len(), 16, "{zh:#?}");
    assert_eq!(blanks_before(&zh).len(), 4, "{zh:#?}");
}

#[test]
fn item_descriptions_line_up_or_move_below() {
    let confirm = YoloConfirm::default();
    let wide = render(&confirm, Lang::En, 80, 40);
    for (name, description) in words::items(Lang::En) {
        let row = wide.iter().find(|line| line.contains(name)).unwrap();
        assert_eq!(row.find(description), Some(21), "{row}");
    }
    // 说明那一列超过宽度一半：说明另起一行、缩进 4 列。
    let narrow = render(&confirm, Lang::En, 36, 60);
    let at = narrow
        .iter()
        .position(|line| line == "  Files and folders")
        .unwrap();
    assert_eq!(narrow[at + 1], "    Read, create, change, and delete");
    assert!(narrow.iter().all(|line| line.width() <= 36), "{narrow:#?}");
}

#[test]
fn the_choices_follow_the_focus_and_the_checkbox() {
    let mut confirm = YoloConfirm::default();
    let ack = "I understand the risks and take responsibility for my data security.";
    let shown = render(&confirm, Lang::En, 90, 40);
    assert!(shown.contains(&format!("    [ ] {ack}")), "{shown:#?}");
    assert!(shown.contains(&"  ❯ 1. Cancel".to_owned()), "{shown:#?}");
    assert!(shown.contains(&"    2. Confirm".to_owned()), "{shown:#?}");
    confirm.checked = true;
    confirm.focus = Focus::Acknowledge;
    let shown = render(&confirm, Lang::Zh, 90, 40);
    assert!(
        shown.contains(&"  ❯ [✔] 我已了解风险，并对自己的数据安全负责。".to_owned()),
        "{shown:#?}"
    );
    assert!(shown.contains(&"    1. 取消".to_owned()), "{shown:#?}");
    assert!(shown.contains(&"    2. 确认启用".to_owned()), "{shown:#?}");
    // 窄时勾选框的文字折行，续行与文字对齐。
    let narrow = render(&confirm, Lang::En, 40, 60);
    let at = narrow
        .iter()
        .position(|line| line.starts_with("  ❯ [✔] I understand"))
        .unwrap();
    assert!(narrow[at + 1].starts_with("        "), "{narrow:#?}");
}
