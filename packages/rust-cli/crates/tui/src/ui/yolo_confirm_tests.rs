//! 切到完全访问的确认框（docs/specs/rust-cli/tui.md §7.8，工作线 F87）：什么时候弹、按键、不叠框、离开会话作废、排版。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;

use super::*;
use crate::ui::app::Local;
use crate::ui::i18n::Lang;
use crate::ui::lines::plain;
use rust_cli_tui_kit::render::width::string_width;

const FULL: PermissionMode = PermissionMode::BypassPermissions;

fn app(mode: PermissionMode) -> App {
    crate::ui::test_support::app_with(crate::ui::test_support::palette(), Lang::En, mode)
}

fn press(app: &mut App, code: KeyCode) -> Option<Action> {
    app.yolo_confirm_key(crate::ui::test_support::key(code))
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

/// 本地的提示：一行的在输入框上方（`App::said`，tui-kit.md §4.0），多行的进 transcript。
fn infos(app: &App) -> Vec<String> {
    let said = [&app.said.failed, &app.said.quiet]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    app.local
        .iter()
        .filter_map(|cell| match &cell.local {
            Local::Info(text) => Some(text.clone()),
            _ => None,
        })
        .chain(said)
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
        assert_eq!(dialog(&app), Some(&YoloConfirm::default()));
        assert!(infos(&app).is_empty());
    }
    // 已经是完全访问：照旧发命令并提示（提示的字是模式那一句，`i18n::mode_says`）。
    let mut full = app(FULL);
    assert_eq!(full.switch_mode(FULL, true), Some(Action::SetMode(FULL)));
    assert!(dialog(&full).is_none());
    assert_eq!(
        infos(&full),
        [crate::ui::i18n::mode_says(Lang::En, FULL).to_owned()]
    );
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
    assert_eq!(dialog(&guarded), Some(&YoloConfirm::default()));
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
        then: None,
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
    // 勾上之后确认：关掉框，发命令；不另说切了模式（Puffin 同，模式行说新的模式）。
    press(&mut app, KeyCode::Up);
    assert_eq!(press(&mut app, KeyCode::Enter), Some(Action::SetMode(FULL)));
    assert!(dialog(&app).is_none());
    assert!(infos(&app).is_empty());
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
            then: None,
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
    // 2 勾选后：焦点在勾选框上也直接确认，发命令。
    assert_eq!(
        press(&mut app, KeyCode::Char('2')),
        Some(Action::SetMode(FULL))
    );
    assert!(dialog(&app).is_none());
    assert!(infos(&app).is_empty());
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
    let ink = crate::ui::test_support::palette().ink();
    let view = View {
        ink: &ink,
        lang,
        width,
    };
    lines(confirm, &view, height)
        .lines
        .iter()
        .map(|line| plain(line).trim_end().to_owned())
        .collect()
}

/// 照 Puffin 的确认框（tui-kit.md §4.2）：kit 的 `yolo_rows` 在板的顶上，块之间不空行，高度不改排版；槽是框的键。
#[test]
fn the_dialog_is_a_board_without_gaps() {
    let confirm = YoloConfirm::default();
    let roomy = render(&confirm, Lang::En, 80, 40);
    assert_eq!(render(&confirm, Lang::En, 80, 10), roomy);
    assert_eq!(roomy[0], "   ⚠ Enable full access?");
    assert!(
        roomy.iter().all(|line| !line.trim().is_empty()),
        "{roomy:#?}"
    );
    assert_eq!(roomy.last().map(String::as_str), Some("   2  Confirm"));
    let ink = crate::ui::test_support::palette().ink();
    let view = View {
        ink: &ink,
        lang: Lang::En,
        width: 80,
    };
    let slot: String = lines(&confirm, &view, 40)
        .slot
        .iter()
        .map(|span| &*span.text)
        .collect();
    // 槽是 Puffin 的 `yolo_slot`：焦点在取消上时 `1–2 answer  esc cancel`。
    assert_eq!(slot.trim(), "1–2 answer  esc cancel", "{slot:?}");
    let zh = render(&confirm, Lang::Zh, 80, 16);
    assert_eq!(zh[0], "   ⚠ 要启用完全访问吗？");
}

#[test]
fn item_descriptions_line_up() {
    let confirm = YoloConfirm::default();
    let wide = render(&confirm, Lang::En, 100, 40);
    for (name, description) in words::items(Lang::En) {
        let row = wide.iter().find(|line| line.contains(name)).unwrap();
        assert_eq!(row.find(description), Some(23), "{row}");
    }
    // 窄时说明以 `…` 截断（Puffin 的回答行不折行），整行不超宽。
    let narrow = render(&confirm, Lang::En, 36, 60);
    assert!(
        narrow.iter().all(|line| string_width(line) <= 36),
        "{narrow:#?}"
    );
    assert!(
        narrow
            .iter()
            .any(|line| line.starts_with("   Files and folders") && line.ends_with('…'))
    );
}

#[test]
fn the_choices_follow_the_focus_and_the_checkbox() {
    let mut confirm = YoloConfirm::default();
    let ack = "I understand the risks and take responsibility for my data security.";
    let shown = render(&confirm, Lang::En, 90, 40);
    assert!(shown.contains(&format!("   □ {ack}")), "{shown:#?}");
    assert!(
        shown.iter().any(|l| l.starts_with("   1  Cancel")),
        "{shown:#?}"
    );
    assert!(
        shown.iter().any(|l| l.starts_with("   2  Confirm")),
        "{shown:#?}"
    );
    confirm.checked = true;
    confirm.focus = Focus::Acknowledge;
    let shown = render(&confirm, Lang::Zh, 90, 40);
    assert!(
        shown.contains(&"   ■ 我已了解风险，并对自己的数据安全负责。".to_owned()),
        "{shown:#?}"
    );
    assert!(
        shown.iter().any(|l| l.starts_with("   1  取消")),
        "{shown:#?}"
    );
    assert!(
        shown.iter().any(|l| l.starts_with("   2  确认启用")),
        "{shown:#?}"
    );
    // 窄时勾选框的文字折行，续行与文字对齐。
    let narrow = render(&confirm, Lang::En, 40, 60);
    let at = narrow
        .iter()
        .position(|line| line.starts_with("   ■ I understand"))
        .unwrap();
    assert!(narrow[at + 1].starts_with("     "), "{narrow:#?}");
}

/// 权限请求里选了 Full access（Puffin `YoloThen::Answer`）：确认后允许这一次再切模式；取消什么都不发。
#[test]
fn confirming_for_an_ask_allows_it_once_then_switches() {
    use rust_cli_kernel_api::orchestration::InteractionRequest;
    use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
    let ask = rust_cli_kernel_api::ids::InteractionId::new("i1");
    let mut app = app(PermissionMode::Default);
    // 那个请求开着（离开了框就作废，`settle_yolo`）。
    app.view.apply(
        SequencedEvent {
            seq: 1,
            revision: 0,
            event: KernelEvent::InteractionAsked {
                interaction: ask.clone(),
                request: InteractionRequest::Permission {
                    machine: Default::default(),
                    rule_seed: None,
                    tool_use: rust_cli_kernel_api::ids::ToolUseId::new("tu1"),
                    tool: "Bash".into(),
                    input: serde_json::json!({"command": "ls"}),
                    message: String::new(),
                    suggestions: Vec::new(),
                    asked_by: None,
                },
            },
        },
        0,
    );
    app.sync_interaction(Instant::now());
    app.confirm_yolo_for(ask.clone(), "fullAccess".into());
    assert_eq!(
        dialog(&app).and_then(|d| d.then.clone()),
        Some((ask.clone(), "fullAccess".to_owned()))
    );
    press(&mut app, KeyCode::Char('1'));
    assert!(dialog(&app).is_none());
    app.confirm_yolo_for(ask.clone(), "fullAccess".into());
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(
        press(&mut app, KeyCode::Char('2')),
        Some(Action::Many(vec![
            Action::Resolve {
                interaction: ask,
                answer: rust_cli_kernel_api::orchestration::Answer::Permission(
                    rust_cli_kernel_api::orchestration::PermissionAnswer::Allow {
                        remember: Vec::new(),
                    },
                ),
            },
            Action::SetMode(FULL),
        ]))
    );
}
