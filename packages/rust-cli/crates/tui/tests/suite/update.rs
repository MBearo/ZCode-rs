// 自动更新的选择屏与会话里的提示（docs/specs/rust-cli/update.md §6、§7，工作线 F48）：画面、焦点与回绕、按键、
// 中文、深浅背景的强调色；会话头下面的提示框，启动时打开已有会话后仍在顶上。终端是 `TestBackend`，按键经 stream
// 送进选择屏的循环。
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use futures_util::stream;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::testing::MemoryStore;
use rust_cli_tui::ui::i18n::Lang;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};
use rust_cli_tui::update::{UpdateChoice, UpdateInfo, drive};

use super::sessions_seed::{Seed, seed};
use super::support::{GatedModel, Setup, text_of, with_ui};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn info() -> UpdateInfo {
    UpdateInfo {
        current: "0.1.46".to_owned(),
        latest: "0.1.47".to_owned(),
        command: "npm install -g @scope/zcode".to_owned(),
        notes_url: "https://www.npmjs.com/package/@scope/zcode".to_owned(),
    }
}

fn dark() -> Palette {
    Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)))
}

/// 跑选择屏直到选出一项；返回选择与最后一帧。
async fn run(
    lang: Lang,
    size: (u16, u16),
    palette: Palette,
    keys: Vec<Event>,
) -> (UpdateChoice, String, Terminal<TestBackend>) {
    let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    let choice = drive(&info(), palette, lang, &mut terminal, stream::iter(keys))
        .await
        .unwrap();
    let screen = text_of(terminal.backend().buffer());
    (choice, screen, terminal)
}

fn line_of(screen: &str, needle: &str) -> usize {
    screen
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not on screen:\n{screen}"))
}

#[tokio::test]
async fn the_prompt_shows_its_text_and_options() {
    let (choice, screen, terminal) = run(Lang::En, (80, 12), dark(), vec![key(KeyCode::Esc)]).await;
    assert_eq!(choice, UpdateChoice::Skip);
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    assert_eq!(
        &lines[1..9],
        [
            "  Update available · 0.1.46 → 0.1.47",
            "  Release notes: https://www.npmjs.com/package/@scope/zcode",
            "",
            "  › 1. Update now (runs `npm install -g @scope/zcode`)",
            "    2. Skip",
            "    3. Skip until next version",
            "",
            "  enter continue · esc skip",
        ],
        "{screen}"
    );
    let buffer = terminal.backend().buffer();
    // 顶上整行 `─` 与标题用强调色、标题粗体；焦点行铺选中的底色条（选择列表组件，tui.md §7.4）。
    assert_eq!(buffer[(0, 0)].symbol(), "─");
    assert_eq!(buffer[(79, 0)].fg, Color::Rgb(99, 168, 248));
    assert_eq!(buffer[(2, 1)].fg, Color::Rgb(99, 168, 248));
    assert!(buffer[(2, 1)].modifier.contains(Modifier::BOLD));
    let focused = line_of(&screen, "1. Update now") as u16;
    assert_ne!(buffer[(79, focused)].bg, Color::Reset, "the selection bar");
    assert_eq!(buffer[(79, focused + 1)].bg, Color::Reset);
    // 链接带下划线。
    let notes = line_of(&screen, "Release notes:") as u16;
    assert!(buffer[(20, notes)].modifier.contains(Modifier::UNDERLINED));
}

/// 深浅两种背景：强调色各一种（与工作区信任对话框相同）。
#[tokio::test]
async fn the_prompt_uses_the_theme_accent_in_both_themes() {
    for (background, accent) in [
        ((0, 0, 0), Color::Rgb(99, 168, 248)),
        ((255, 255, 255), Color::Rgb(28, 100, 200)),
    ] {
        let palette = Palette::new(ColorLevel::TrueColor, Some(background));
        let (_, _, terminal) = run(Lang::En, (80, 12), palette, vec![key(KeyCode::Esc)]).await;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].fg, accent);
        assert_eq!(buffer[(2, 1)].fg, accent);
    }
}

#[tokio::test]
async fn the_prompt_has_chinese_text() {
    let (_, screen, _) = run(Lang::Zh, (80, 12), dark(), vec![key(KeyCode::Esc)]).await;
    for text in [
        "有新版本 · 0.1.46 → 0.1.47",
        "更新说明：https://www.npmjs.com/package/@scope/zcode",
        "› 1. 立即更新（运行 `npm install -g @scope/zcode`）",
        "2. 跳过",
        "3. 跳过，直到下一个版本",
        "enter 继续 · esc 跳过",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
}

/// 按键：↑↓ 与 j / k 回绕，1 / 2 / 3 直接选，enter 选焦点，esc 与 ctrl+c / ctrl+d 跳过。
#[tokio::test]
async fn the_keys_wrap_pick_and_skip() {
    let cases: Vec<(Vec<Event>, UpdateChoice)> = vec![
        (vec![key(KeyCode::Enter)], UpdateChoice::UpdateNow),
        (
            vec![key(KeyCode::Down), key(KeyCode::Enter)],
            UpdateChoice::Skip,
        ),
        (
            vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)],
            UpdateChoice::SkipUntilNext,
        ),
        // 第一项再上移到最后一项，最后一项再下移回第一项。
        (
            vec![key(KeyCode::Up), key(KeyCode::Enter)],
            UpdateChoice::SkipUntilNext,
        ),
        (
            vec![key(KeyCode::Up), key(KeyCode::Down), key(KeyCode::Enter)],
            UpdateChoice::UpdateNow,
        ),
        (
            vec![key(KeyCode::Char('j')), key(KeyCode::Enter)],
            UpdateChoice::Skip,
        ),
        (
            vec![key(KeyCode::Char('k')), key(KeyCode::Enter)],
            UpdateChoice::SkipUntilNext,
        ),
        (vec![key(KeyCode::Char('1'))], UpdateChoice::UpdateNow),
        (vec![key(KeyCode::Char('2'))], UpdateChoice::Skip),
        (vec![key(KeyCode::Char('3'))], UpdateChoice::SkipUntilNext),
        (vec![key(KeyCode::Esc)], UpdateChoice::Skip),
        (vec![ctrl('c')], UpdateChoice::Skip),
        (vec![ctrl('d')], UpdateChoice::Skip),
        // 别的键不处理。
        (
            vec![
                key(KeyCode::Char('x')),
                key(KeyCode::Tab),
                key(KeyCode::Enter),
            ],
            UpdateChoice::UpdateNow,
        ),
        // 松开按键的事件不处理。
        (
            vec![
                Event::Key(KeyEvent {
                    code: KeyCode::Char('3'),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Release,
                    state: KeyEventState::NONE,
                }),
                key(KeyCode::Enter),
            ],
            UpdateChoice::UpdateNow,
        ),
        // 输入结束（终端关掉）：当作 Skip。
        (vec![key(KeyCode::Down)], UpdateChoice::Skip),
        (Vec::new(), UpdateChoice::Skip),
    ];
    for (keys, expected) in cases {
        let (choice, _, _) = run(Lang::En, (80, 12), dark(), keys.clone()).await;
        assert_eq!(choice, expected, "{keys:?}");
    }
}

/// 焦点移到第三项时它带 `›` 与底色条；窄终端（32 列）里长的命令折行，三个选项与按键提示都还在。
#[tokio::test]
async fn focus_moves_and_long_lines_wrap() {
    let mut terminal = Terminal::new(TestBackend::new(32, 16)).unwrap();
    let keys = vec![key(KeyCode::Up), key(KeyCode::Esc)];
    // 画两帧：第一帧焦点在第一项，第二帧在第三项；最后一帧是第二帧。
    let choice = drive(&info(), dark(), Lang::En, &mut terminal, stream::iter(keys))
        .await
        .unwrap();
    assert_eq!(choice, UpdateChoice::Skip);
    let screen = text_of(terminal.backend().buffer());
    assert!(
        screen.contains("  › 3. Skip until next version"),
        "{screen}"
    );
    assert!(screen.contains("    1. Update now (runs `npm"), "{screen}");
    // 续行缩进到序号之后。
    assert!(screen.contains("\n       install -g"), "{screen}");
    assert!(screen.contains("  enter continue · esc skip"), "{screen}");
}

fn update_setup(lang: &'static str, store: Option<MemoryStore>) -> Setup {
    let mut setup = Setup {
        width: 80,
        height: 30,
        lang,
        ..Setup::default()
    };
    setup.services.update = Some(info());
    if let Some(store) = store {
        setup.store = std::sync::Arc::new(store);
    }
    setup
}

/// 会话里的提示：会话头下面一个圆角框。
#[tokio::test(flavor = "multi_thread")]
async fn the_notice_box_sits_under_the_session_header() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model,
        update_setup("en_US.UTF-8", None),
        |mut ui| async move {
            let screen = ui.wait_for("Update available!").await;
            let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
            let top = line_of(&screen, "╭");
            assert!(line_of(&screen, "ZCode v0.1.0") < top, "{screen}");
            assert_eq!(
                &lines[top..top + 7],
                [
                    "╭────────────────────────────────────────────╮",
                    "│ ✨\u{200A}Update available! 0.1.46 -> 0.1.47      │",
                    "│ Run npm install -g @scope/zcode to update. │",
                    "│                                            │",
                    "│ See full release notes:                    │",
                    "│ https://www.npmjs.com/package/@scope/zcode │",
                    "╰────────────────────────────────────────────╯",
                ],
                "{screen}"
            );
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_notice_box_has_chinese_text() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model,
        update_setup("zh_CN.UTF-8", None),
        |mut ui| async move {
            let screen = ui.wait_for("有新版本！").await;
            for text in [
                // ✨ 后面是细空格（U+200A）。
                "✨\u{200A}有新版本！ 0.1.46 -> 0.1.47",
                "运行 npm install -g @scope/zcode 更新。",
                "完整的更新说明：",
            ] {
                assert!(screen.contains(text), "{text}:\n{screen}");
            }
        },
    )
    .await;
}

/// 启动时打开已有会话（`--resume`）：切换清掉旧视图后，提示仍在新视图的顶上（会话内容之前）。
#[tokio::test(flavor = "multi_thread")]
async fn the_notice_survives_resuming_a_session_at_startup() {
    let store = MemoryStore::default();
    seed(
        &store,
        &[Seed {
            id: "sess_resumed",
            root: "/Users/dev/project",
            title: "Old work",
            prompt: "earlier question",
            reply: "earlier answer",
            created: 1,
            updated: 2,
        }],
    )
    .await;
    let mut setup = update_setup("en_US.UTF-8", Some(store));
    setup.startup = rust_cli_tui::Startup::Session("sess_resumed".to_owned());
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, setup, |mut ui| async move {
        let screen = ui.wait_for("earlier answer").await;
        let notice = line_of(&screen, "Update available!");
        assert!(notice < line_of(&screen, "earlier question"), "{screen}");
        assert_eq!(screen.matches("Update available!").count(), 1, "{screen}");
    })
    .await;
}
