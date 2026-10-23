// 工作区信任对话框（docs/specs/rust-cli/workspace-trust.md §2.4、§2.5）：画面、
// 焦点、按键与退出码、后备询问、项目授权的列表、中文。终端是 `TestBackend`，按键经通道送进对话框的循环。
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use futures_util::stream;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use rust_cli_tui::trust::{Grants, TrustAnswer, TrustPrompt, drive};
use rust_cli_tui::ui::i18n::Lang;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::text_of;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn prompt() -> TrustPrompt {
    TrustPrompt {
        path: "/Users/dev/work".to_owned(),
        backstop: false,
        grants: None,
    }
}

fn grants() -> Grants {
    Grants {
        count: 4,
        rules: [
            "mcp__x__y",
            "Bash(ls:*)",
            "WebFetch(domain:example.com)",
            "Read",
        ]
        .map(str::to_owned)
        .to_vec(),
        sources: vec!["zcode.json".to_owned()],
    }
}

/// 跑对话框直到给出回答；返回回答与最后一帧（回答之后不再重画）。
async fn run(
    prompt: &TrustPrompt,
    lang: Lang,
    keys: Vec<Event>,
) -> (TrustAnswer, String, Terminal<TestBackend>) {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let answer = drive(prompt, palette, lang, &mut terminal, stream::iter(keys))
        .await
        .unwrap();
    let screen = text_of(terminal.backend().buffer());
    (answer, screen, terminal)
}

fn line_of(screen: &str, needle: &str) -> usize {
    screen
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not on screen:\n{screen}"))
}

#[tokio::test]
async fn the_dialog_shows_the_folder_and_the_fixed_text() {
    let (answer, screen, terminal) = run(&prompt(), Lang::En, vec![key(KeyCode::Esc)]).await;
    assert_eq!(answer, TrustAnswer::Exit(0));
    for text in [
        "Accessing workspace:",
        "/Users/dev/work",
        "Quick safety check: Is this a project you created or one you trust?",
        // 100 列下在固定的词处换行（workspace-trust.md §2.4）。
        "well-known open source project, or work from your team). If not, take a moment to review what's in",
        "this folder first.",
        "ZCode'll be able to read, edit, and execute files here.",
        "› No, exit",
        "Yes, I trust this folder",
        "Enter to confirm · Esc to cancel",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    // 取消项在前、焦点在取消项；没有序号；没有外部文档的链接（D1）。
    assert!(line_of(&screen, "No, exit") < line_of(&screen, "Yes, I trust this folder"));
    assert!(!screen.contains("1."), "{screen}");
    assert!(!screen.contains("Security guide"), "{screen}");
    assert!(!screen.contains("pre-approves"), "{screen}");
    // 顶上整行 `─` 与标题用主题的强调色（修复线 F26，用户决定；原来是黄色），见下一个测试。
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].symbol(), "─");
    assert_ne!(buffer[(0, 0)].fg, Color::Yellow);
}

/// 修复线 F26（用户决定，workspace-trust.md §2.4、tui.md §7.4）：顶上的线与标题是主题的蓝色强调色（与选中行的
/// 深色底色同一个常量），深浅两种背景各一种；项目授权的 `⚠` 标题仍是警告色。旧代码两处都是黄色。
#[tokio::test]
async fn the_dialog_uses_the_theme_accent_in_both_themes() {
    for (background, accent) in [
        ((0, 0, 0), Color::Rgb(99, 168, 248)),
        ((255, 255, 255), Color::Rgb(28, 100, 200)),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let palette = Palette::new(ColorLevel::TrueColor, Some(background));
        assert_eq!(palette.accent, accent);
        let backstop = TrustPrompt {
            backstop: true,
            grants: Some(grants()),
            ..prompt()
        };
        drive(
            &backstop,
            palette,
            Lang::En,
            &mut terminal,
            stream::iter(vec![key(KeyCode::Esc)]),
        )
        .await
        .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = text_of(buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "─");
        assert_eq!(buffer[(0, 0)].fg, accent, "the rule");
        assert_eq!(buffer[(99, 0)].fg, accent, "the whole rule");
        let title = line_of(&screen, "Accessing workspace:") as u16;
        let cell = &buffer[(2, title)];
        assert_eq!(cell.fg, accent, "the title");
        assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD));
        let warning = line_of(&screen, "⚠") as u16;
        assert_eq!(buffer[(2, warning)].fg, Color::Yellow, "the grants warning");
    }
}

#[tokio::test]
async fn the_answers_have_fixed_exit_codes() {
    let cases = [
        (
            vec![key(KeyCode::Down), key(KeyCode::Enter)],
            TrustAnswer::Trusted,
        ),
        (vec![key(KeyCode::Enter)], TrustAnswer::Exit(1)),
        (vec![key(KeyCode::Esc)], TrustAnswer::Exit(0)),
        (vec![ctrl('c'), ctrl('c')], TrustAnswer::Exit(1)),
        (vec![ctrl('d'), ctrl('d')], TrustAnswer::Exit(1)),
        // ↑ 到顶不绕回；↓ ↑ 回到取消项。
        (
            vec![key(KeyCode::Up), key(KeyCode::Enter)],
            TrustAnswer::Exit(1),
        ),
        (
            vec![key(KeyCode::Down), key(KeyCode::Up), key(KeyCode::Enter)],
            TrustAnswer::Exit(1),
        ),
        (
            vec![key(KeyCode::Down), key(KeyCode::Down), key(KeyCode::Enter)],
            TrustAnswer::Trusted,
        ),
        // 输入结束（终端关掉）当作拒绝。
        (Vec::new(), TrustAnswer::Exit(1)),
    ];
    for (keys, expected) in cases {
        let (answer, _, _) = run(&prompt(), Lang::En, keys.clone()).await;
        assert_eq!(answer, expected, "{keys:?}");
    }
}

#[tokio::test]
async fn the_first_ctrl_c_only_changes_the_hint() {
    let (answer, screen, _) = run(&prompt(), Lang::En, vec![ctrl('c'), key(KeyCode::Esc)]).await;
    assert_eq!(answer, TrustAnswer::Exit(0));
    assert!(screen.contains("Press Ctrl-C again to exit"), "{screen}");
    assert!(!screen.contains("Enter to confirm"), "{screen}");
}

#[tokio::test]
async fn the_backstop_lists_the_project_grants_and_continues_without_them() {
    let backstop = TrustPrompt {
        backstop: true,
        grants: Some(grants()),
        ..prompt()
    };
    let (answer, screen, _) = run(&backstop, Lang::En, vec![key(KeyCode::Esc)]).await;
    assert_eq!(answer, TrustAnswer::WithoutGrants);
    for text in [
        "⚠ This folder pre-approves 4 tool permissions in zcode.json:",
        "  mcp__x__y, Bash(ls:*), WebFetch(domain:example.com), and Read",
        "These will apply without asking. Only proceed if you trust this configuration.",
        "› No, continue without these permissions",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    assert!(!screen.contains("No, exit"), "{screen}");
    for (keys, expected) in [
        (vec![key(KeyCode::Enter)], TrustAnswer::WithoutGrants),
        (
            vec![key(KeyCode::Down), key(KeyCode::Enter)],
            TrustAnswer::Trusted,
        ),
        (vec![ctrl('c'), ctrl('c')], TrustAnswer::Exit(1)),
    ] {
        let (answer, _, _) = run(&backstop, Lang::En, keys.clone()).await;
        assert_eq!(answer, expected, "{keys:?}");
    }
}

#[tokio::test]
async fn the_grant_list_is_cut_after_eight_and_counts_in_the_singular() {
    let many = TrustPrompt {
        grants: Some(Grants {
            count: 10,
            rules: (1..=10).map(|n| format!("Tool{n}")).collect(),
            sources: vec!["zcode.json".to_owned(), ".zcode/config.json".to_owned()],
        }),
        ..prompt()
    };
    let (_, screen, _) = run(&many, Lang::En, vec![key(KeyCode::Esc)]).await;
    assert!(
        screen.contains("pre-approves 10 tool permissions in zcode.json and .zcode/config.json:"),
        "{screen}"
    );
    assert!(
        screen.contains("Tool1, Tool2, Tool3, Tool4, Tool5, Tool6, Tool7, Tool8, and 2 more"),
        "{screen}"
    );
    let one = TrustPrompt {
        grants: Some(Grants {
            count: 1,
            rules: vec!["Bash".to_owned()],
            sources: vec!["zcode.json".to_owned()],
        }),
        ..prompt()
    };
    let (_, screen, _) = run(&one, Lang::En, vec![key(KeyCode::Esc)]).await;
    assert!(
        screen.contains("pre-approves 1 tool permission in zcode.json:"),
        "{screen}"
    );
}

#[tokio::test]
async fn the_dialog_speaks_chinese() {
    let backstop = TrustPrompt {
        backstop: true,
        grants: Some(grants()),
        ..prompt()
    };
    let (_, screen, _) = run(&prompt(), Lang::Zh, vec![ctrl('c'), key(KeyCode::Esc)]).await;
    for text in [
        "访问工作区：",
        "快速安全检查：这是你自己创建或信任的项目吗？",
        "ZCode 将可以读取、编辑和执行这里的文件。",
        "› 否，退出",
        "是，我信任此文件夹",
        "再按一次 Ctrl-C 退出",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    let (_, screen, _) = run(&backstop, Lang::Zh, vec![key(KeyCode::Esc)]).await;
    for text in [
        "⚠ 此文件夹在 zcode.json 中预先批准了 4 项工具权限：",
        "这些权限会直接生效，不再询问。只有信任这份配置时才继续。",
        "› 否，不带这些权限继续",
        "Enter 确认 · Esc 取消",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
}

#[tokio::test]
async fn control_characters_in_the_path_are_replaced() {
    let odd = TrustPrompt {
        path: "/Users/dev/a\u{1b}[31mb".to_owned(),
        ..prompt()
    };
    let (_, screen, _) = run(&odd, Lang::En, vec![key(KeyCode::Esc)]).await;
    assert!(screen.contains("/Users/dev/a\u{fffd}[31mb"), "{screen}");
}
