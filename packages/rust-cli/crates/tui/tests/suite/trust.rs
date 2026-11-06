// 工作区信任对话框（docs/specs/rust-cli/workspace-trust.md §2.4、§2.5）：画面、
// 焦点、按键与退出码、后备询问、项目授权的列表、中文。启动前的信任页是 Puffin 的（tui-kit.md §2.3）；这里测会话里切项目
// 时的同一个对话框（`trust_view::Dialog`）：每个按键之前画一屏（100 × 30 的 kit 格子网），按键送进它，直到给出回答。
use crossterm::event::{Event, KeyCode, KeyEventKind};
use rust_cli_tui::trust::{Grants, TrustAnswer, TrustPrompt};
use rust_cli_tui::{ColorLevel, Ink, Lang, TrustDialog as Dialog};
use rust_cli_tui_kit::render::attr;

use super::support::{
    Shot, ctrl_event as ctrl, key_event as key, release_event as release, shot_of,
};

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

/// 跑对话框直到给出回答；返回回答与最后一帧（回答之后不再重画）。按键用完（终端关掉）当作 `Exit(1)`。
fn run_with(
    prompt: &TrustPrompt,
    ink: &Ink,
    lang: Lang,
    keys: Vec<Event>,
) -> (TrustAnswer, String, Shot) {
    let mut dialog = Dialog::new(prompt.clone());
    let mut keys = keys.into_iter();
    loop {
        let shot = shot_of(&dialog.lines(ink, lang, 100), 100, 30);
        let Some(event) = keys.next() else {
            let screen = shot.text();
            return (TrustAnswer::Exit(1), screen, shot);
        };
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
            && let Some(answer) = dialog.key(key, std::time::Instant::now())
        {
            let screen = shot.text();
            return (answer, screen, shot);
        }
    }
}

async fn run(prompt: &TrustPrompt, lang: Lang, keys: Vec<Event>) -> (TrustAnswer, String, Shot) {
    let ink = Ink::new(ColorLevel::TrueColor, None, Some((0, 0, 0)));
    run_with(prompt, &ink, lang, keys)
}

fn line_of(screen: &str, needle: &str) -> usize {
    screen
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not on screen:\n{screen}"))
}

#[tokio::test]
async fn the_dialog_shows_the_folder_and_the_fixed_text() {
    let (answer, screen, buffer) = run(&prompt(), Lang::En, vec![key(KeyCode::Esc)]).await;
    assert_eq!(answer, TrustAnswer::Exit(0));
    for text in [
        "Accessing workspace:",
        "/Users/dev/work",
        "Quick safety check: Is this a project you created or one you trust?",
        // 100 列下按 Puffin 的正文栏（98 列）折行（tui-kit.md §4.2）。
        "well-known open source project, or work from your team). If not, take a moment to review what's",
        "   in this folder first.",
        "ZCode'll be able to read, edit, and execute files here.",
        "   1  No, exit",
        "   2  Yes, I trust this folder",
        "⏎ confirm  ·  esc cancel",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    // 取消项在前、焦点在取消项；没有外部文档的链接（D1）。
    assert!(line_of(&screen, "No, exit") < line_of(&screen, "Yes, I trust this folder"));
    assert!(!screen.contains("Security guide"), "{screen}");
    assert!(!screen.contains("pre-approves"), "{screen}");
    // 照 Puffin 的信任页：顶上空一行，没有整行的线（以前是强调色的 `─`）。
    assert_eq!(screen.lines().next(), Some(""));
    assert_eq!(buffer[(0, 0)].symbol(), " ");
}

/// 颜色照 Puffin 的信任页（工作线 TK 的阶段 S，tui-kit.md §4.2；以前是修复线 F26 的强调色的线与标题）：路径粗体，项目授权
/// 的 `⚠` 是 amber（`code`），焦点所在回答的序号与名字是 brand、其余序号 fg3；深浅两种背景的 brand 不同。
#[tokio::test]
async fn the_dialog_uses_the_theme_accent_in_both_themes() {
    let mut brands = Vec::new();
    for (background, foreground) in [((0, 0, 0), (255, 255, 255)), ((255, 255, 255), (0, 0, 0))] {
        let ink = Ink::new(ColorLevel::TrueColor, Some(foreground), Some(background));
        let theme = ink.theme;
        brands.push(theme.brand);
        let backstop = TrustPrompt {
            backstop: true,
            grants: Some(grants()),
            ..prompt()
        };
        let (_, screen, buffer) = run_with(&backstop, &ink, Lang::En, vec![key(KeyCode::Esc)]);
        let path = line_of(&screen, "/Users/dev/work") as u16;
        assert!(buffer[(3, path)].has(attr::BOLD), "the path");
        let row = line_of(&screen, "⚠") as u16;
        assert_eq!(buffer[(3, row)].fg, theme.code, "the grants warning");
        let first = line_of(&screen, "1  No") as u16;
        assert_eq!(buffer[(3, first)].fg, theme.brand, "the focused number");
        assert_eq!(buffer[(6, first)].fg, theme.brand, "the focused answer");
        let second = line_of(&screen, "2  Yes") as u16;
        assert_eq!(
            buffer[(3, second)].fg,
            theme.fg3.fg_color(),
            "the other number"
        );
        assert_ne!(theme.code, theme.brand);
    }
    assert_ne!(brands[0], brands[1]);
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
        // 松开按键的事件不处理。
        (
            vec![release(KeyCode::Enter), key(KeyCode::Esc)],
            TrustAnswer::Exit(0),
        ),
        // 输入结束（终端关掉）当作拒绝。
        (Vec::new(), TrustAnswer::Exit(1)),
        // 序号直接回答（Puffin 的回答行，tui-kit.md §4.2）。
        (vec![key(KeyCode::Char('1'))], TrustAnswer::Exit(1)),
        (vec![key(KeyCode::Char('2'))], TrustAnswer::Trusted),
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
    assert!(screen.contains("^C again to exit"), "{screen}");
    assert!(!screen.contains("⏎ confirm"), "{screen}");
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
        "1  No, continue without these permissions",
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
        "1  否，退出",
        "2  是，我信任此文件夹",
        "^C 再按一次退出",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    let (_, screen, _) = run(&backstop, Lang::Zh, vec![key(KeyCode::Esc)]).await;
    for text in [
        "⚠ 此文件夹在 zcode.json 中预先批准了 4 项工具权限：",
        "这些权限会直接生效，不再询问。只有信任这份配置时才继续。",
        "1  否，不带这些权限继续",
        "⏎ 确认  ·  esc 取消",
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
