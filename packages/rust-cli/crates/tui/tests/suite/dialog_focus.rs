//! 权限询问与计划审批的焦点行（docs/specs/rust-cli/tui.md §7.7，工作线 F76，用户决定 2026-10-23）：与提问视图（§7.6）
//! 相同，焦点行不铺底色条，指针 `❯` 与名字是强调色，序号 dim；输入行（“否”行的意见、“继续计划”）聚焦时终端光标停在
//! 输入位置。深浅两种背景。旧代码的焦点行整行铺底色条、界面上没有光标，这些测试都失败。扣住的同伴消息的批准框在
//! peers.rs。
use std::sync::Arc;

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, environments, text_response, tool_use_response,
};
use rust_cli_tui::ui::palette::{ColorLevel, Palette};
use serde_json::json;

use super::support::{GatedModel, Setup, Ui, request_text, with_ui};

type Rgb = (u8, u8, u8);

struct Theme {
    background: Rgb,
    foreground: Rgb,
    accent: Color,
}

const THEMES: [Theme; 2] = [
    Theme {
        background: (0, 0, 0),
        foreground: (255, 255, 255),
        accent: Color::Rgb(99, 168, 248),
    },
    Theme {
        background: (255, 255, 255),
        foreground: (0, 0, 0),
        accent: Color::Rgb(28, 100, 200),
    },
];

fn setup(theme: &Theme) -> Setup {
    Setup {
        width: 80,
        height: 40,
        palette: Palette::new(ColorLevel::TrueColor, Some(theme.background))
            .with_foreground(Some(theme.foreground)),
        environments: environments(Arc::new(MemoryEnvironment::new(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            additional_directories: Vec::new(),
            home: Some(EnvPath::new("/Users/dev")),
        }))),
        ..Setup::default()
    }
}

fn frame(ui: &Ui) -> Buffer {
    ui.buffer.lock().unwrap().clone().unwrap()
}

/// 第一处 `needle` 所在的（列，行）。
fn find(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let area = buffer.area;
    (area.top()..area.bottom()).find_map(|y| {
        let cells: Vec<(u16, String)> = (area.left()..area.right())
            .map(|x| (x, buffer[(x, y)].symbol().to_owned()))
            .collect();
        let row: String = cells.iter().map(|(_, s)| s.as_str()).collect();
        let at = row.find(needle)?;
        let mut bytes = 0;
        cells
            .iter()
            .find(|(_, s)| {
                let hit = bytes >= at;
                bytes += s.len();
                hit
            })
            .map(|(x, _)| (*x, y))
    })
}

/// 整行没有底色。
fn assert_no_fill(buffer: &Buffer, y: u16) {
    for x in buffer.area.left()..buffer.area.right() {
        let cell = &buffer[(x, y)];
        assert_eq!(cell.bg, Color::Reset, "column {x} of row {y}: {cell:?}");
    }
}

/// `number`（如 `1. Yes`）所在的行是焦点行：没有底色，`❯` 与名字是强调色、不加粗，序号 dim。
fn assert_focused(buffer: &Buffer, number: &str, accent: Color) {
    let (x, y) = find(buffer, number).unwrap_or_else(|| panic!("no {number}"));
    assert_no_fill(buffer, y);
    let pointer = &buffer[(x - 2, y)];
    assert_eq!(pointer.symbol(), "❯", "{pointer:?}");
    assert_eq!(pointer.fg, accent, "{pointer:?}");
    assert!(
        buffer[(x, y)].modifier.contains(Modifier::DIM),
        "the number is dim"
    );
    let name = &buffer[(x + 3, y)];
    assert_eq!(name.fg, accent, "{name:?}");
    assert!(!name.modifier.contains(Modifier::BOLD), "{name:?}");
}

/// 没聚焦的行：没有底色、没有指针，名字不上色。
fn assert_plain(buffer: &Buffer, number: &str) {
    let (x, y) = find(buffer, number).unwrap_or_else(|| panic!("no {number}"));
    assert_no_fill(buffer, y);
    assert_eq!(buffer[(x - 2, y)].symbol(), " ");
    assert_eq!(buffer[(x + 3, y)].fg, Color::Reset);
}

fn touching() -> Arc<GatedModel> {
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Bash",
            &json!({"command": "touch made.txt", "description": "create a file"}),
        ),
        text_response(&["done"]),
    ])
}

#[tokio::test(flavor = "multi_thread")]
async fn the_permission_ask_has_no_bar_and_shows_the_cursor_while_typing() {
    for theme in &THEMES {
        let accent = theme.accent;
        let model = touching();
        with_ui(model.clone(), setup(theme), |mut ui| async move {
            ui.submit("make a file");
            ui.wait_for("Do you want to proceed?").await;
            ui.settle().await;
            let buffer = frame(&ui);
            assert_focused(&buffer, "1. Yes", accent);
            assert_plain(&buffer, "2. No");
            assert_eq!(ui.cursor(), None);
            ui.key(KeyCode::Down);
            ui.wait("the pointer on No", |s| s.contains("❯ 2. No"))
                .await;
            let buffer = frame(&ui);
            assert_focused(&buffer, "2. No", accent);
            assert_plain(&buffer, "1. Yes");
            assert_eq!(ui.cursor(), None);
            // tab 补充意见：这一行变成输入，光标停在占位文字的第一个字上。
            ui.key(KeyCode::Tab);
            let screen = ui.wait_for("tell ZCode what to do differently").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "tell ZCode").unwrap();
            assert_no_fill(&buffer, y);
            assert_eq!(ui.cursor(), Some((x, y)), "{screen}");
            // 输入之后光标在文字末尾。
            ui.type_text("use make");
            ui.wait_for("2. use make").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "use make").unwrap();
            assert_no_fill(&buffer, y);
            assert_eq!(ui.cursor(), Some((x + 8, y)));
            ui.key(KeyCode::Enter);
            ui.wait_for("done").await;
        })
        .await;
        let second = request_text(&model.requests()[1]);
        assert!(second.contains("use make"), "{second}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plan_approval_has_no_bar_and_shows_the_cursor_on_keep_planning() {
    for theme in &THEMES {
        let accent = theme.accent;
        let model = GatedModel::scripted(vec![
            tool_use_response("toolu_1", "EnterPlanMode", &json!({})),
            tool_use_response("toolu_2", "ExitPlanMode", &json!({})),
            text_response(&["revising"]),
        ]);
        with_ui(model.clone(), setup(theme), |mut ui| async move {
            ui.submit("plan first");
            ui.wait_for("Ready to code?").await;
            ui.settle().await;
            let buffer = frame(&ui);
            assert_focused(&buffer, "1. Yes, exit plan mode", accent);
            assert_plain(&buffer, "2. No, keep planning");
            assert_eq!(ui.cursor(), None);
            // 第 2 行直接是输入：聚焦时光标在占位文字上，输入后在文字末尾。
            ui.key(KeyCode::Down);
            ui.wait_for("Tell ZCode what to change").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "Tell ZCode what to change").unwrap();
            assert_no_fill(&buffer, y);
            assert_eq!(buffer[(x - 5, y)].symbol(), "❯");
            assert_eq!(ui.cursor(), Some((x, y)));
            assert_plain(&buffer, "1. Yes, exit plan mode");
            ui.type_text("smaller steps");
            ui.wait_for("2. smaller steps").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "smaller steps").unwrap();
            assert_eq!(ui.cursor(), Some((x + 13, y)));
            // 回到第 1 行：光标隐藏。
            ui.key(KeyCode::Up);
            ui.wait("the pointer on approve", |s| s.contains("❯ 1. Yes"))
                .await;
            assert_eq!(ui.cursor(), None);
            ui.key(KeyCode::Down);
            ui.wait("the pointer on keep planning", |s| {
                s.contains("❯ 2. smaller")
            })
            .await;
            ui.key(KeyCode::Enter);
            ui.wait_for("revising").await;
        })
        .await;
        let last = request_text(&model.requests()[2]);
        assert!(last.contains("smaller steps"), "{last}");
    }
}

/// 长的意见按宽度折行，光标跟着到最后一行。
#[tokio::test(flavor = "multi_thread")]
async fn long_feedback_wraps_and_the_cursor_follows() {
    let model = touching();
    let narrow = Setup {
        width: 40,
        ..setup(&THEMES[0])
    };
    with_ui(model, narrow, |mut ui| async move {
        ui.submit("make a file");
        ui.wait_for("Do you want to proceed?").await;
        ui.settle().await;
        ui.key(KeyCode::Tab);
        ui.wait_for("tell ZCode").await;
        ui.type_text("alpha beta gamma delta epsilon zeta eta");
        ui.wait_for("zeta eta").await;
        let buffer = frame(&ui);
        let (_, first) = find(&buffer, "alpha").unwrap();
        let (x, y) = find(&buffer, "zeta eta").unwrap();
        assert!(y > first, "the input wrapped");
        // 续行与文字对齐（序号之后）。
        let (start, _) = find(&buffer, "alpha").unwrap();
        assert_eq!(x, start);
        assert_eq!(ui.cursor(), Some((x + 8, y)));
    })
    .await;
}
