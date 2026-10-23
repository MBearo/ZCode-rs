//! 提问视图的焦点行、方框与“其他”行的光标（docs/specs/rust-cli/tui.md §7.6，工作线 F74，用户决定 2026-10-23）：
//! 焦点行不铺底色、指针与名字是强调色；多选勾上的 `[✔]` 是成功色；焦点在“其他”行时终端光标停在输入位置。
//! 深浅两种背景。旧代码的焦点行整行铺底色条、`[✔]` 不上色、界面上没有光标，这些测试都失败。
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use rust_cli_tui::ui::palette::{ColorLevel, Palette};
use serde_json::{Value, json};

use super::support::{GatedModel, Setup, Ui, request_text, with_ui};

type Rgb = (u8, u8, u8);

/// 一种背景下的期望颜色。
struct Theme {
    background: Rgb,
    foreground: Rgb,
    accent: Color,
    success: Color,
}

const THEMES: [Theme; 2] = [
    Theme {
        background: (0, 0, 0),
        foreground: (255, 255, 255),
        accent: Color::Rgb(99, 168, 248),
        success: Color::Rgb(78, 186, 101),
    },
    Theme {
        background: (255, 255, 255),
        foreground: (0, 0, 0),
        accent: Color::Rgb(28, 100, 200),
        success: Color::Rgb(44, 122, 57),
    },
];

fn setup(theme: &Theme, width: u16) -> Setup {
    Setup {
        width,
        height: 30,
        palette: Palette::new(ColorLevel::TrueColor, Some(theme.background))
            .with_foreground(Some(theme.foreground)),
        ..Setup::default()
    }
}

/// 模型先发一次提问，拿到回答后回一句文字。
fn asking(multi: bool) -> std::sync::Arc<GatedModel> {
    let questions: Value = json!([{
        "question": "Which database?",
        "header": "DB",
        "multiSelect": multi,
        "options": [
            {"label": "Postgres", "description": "a server"},
            {"label": "SQLite", "description": "a file"},
            {"label": "DuckDB", "description": "columns"},
        ],
    }]);
    GatedModel::scripted(vec![
        tool_use_response(
            "toolu_q",
            "AskUserQuestion",
            &json!({"questions": questions}),
        ),
        text_response(&["noted"]),
    ])
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

/// 等提问弹出并过了防误触的时间。
async fn open(ui: &mut Ui) {
    ui.submit("pick a database");
    ui.wait_for("Which database?").await;
    ui.wait_for("Postgres").await;
    ui.settle().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_focused_row_has_no_bar_and_uses_the_accent_color() {
    for theme in &THEMES {
        let accent = theme.accent;
        with_ui(asking(false), setup(theme, 80), |mut ui| async move {
            open(&mut ui).await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "1. Postgres").unwrap();
            assert_no_fill(&buffer, y);
            let pointer = &buffer[(x - 2, y)];
            assert_eq!(pointer.symbol(), "❯", "{pointer:?}");
            assert_eq!(pointer.fg, accent, "{pointer:?}");
            let number = &buffer[(x, y)];
            assert!(number.modifier.contains(Modifier::DIM), "{number:?}");
            let name = &buffer[(x + 3, y)];
            assert_eq!(name.fg, accent, "{name:?}");
            assert!(!name.modifier.contains(Modifier::BOLD), "{name:?}");
            // 其余行：没有指针，名字不上色。
            let (x2, y2) = find(&buffer, "2. SQLite").unwrap();
            assert_no_fill(&buffer, y2);
            assert_eq!(buffer[(x2 - 2, y2)].symbol(), " ");
            assert_eq!(buffer[(x2 + 3, y2)].fg, Color::Reset);
            // 焦点不在“其他”行：不显示光标。
            assert_eq!(ui.cursor(), None);
            // 焦点下移：指针与颜色跟着走，仍然没有底色。
            ui.key(KeyCode::Down);
            ui.wait("the pointer on SQLite", |s| s.contains("❯ 2. SQLite"))
                .await;
            let buffer = frame(&ui);
            assert_no_fill(&buffer, y2);
            assert_eq!(buffer[(x2 + 3, y2)].fg, accent);
            assert_eq!(buffer[(x + 3, y)].fg, Color::Reset);
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_select_ticks_use_the_success_color() {
    for theme in &THEMES {
        let success = theme.success;
        let model = asking(true);
        with_ui(model.clone(), setup(theme, 80), |mut ui| async move {
            open(&mut ui).await;
            // 勾上第一项（焦点在这一项）：`[✔]` 是成功色，没有底色。
            ui.key(KeyCode::Char('1'));
            ui.wait("the first box", |s| s.contains("[✔] Postgres"))
                .await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "[✔] Postgres").unwrap();
            assert_eq!(buffer[(x + 1, y)].fg, success, "{:?}", buffer[(x + 1, y)]);
            assert_no_fill(&buffer, y);
            // 焦点移到第二项：没聚焦的 `[✔]` 也是成功色。
            ui.key(KeyCode::Down);
            ui.wait("the pointer on SQLite", |s| s.contains("❯ 2. [ ] SQLite"))
                .await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "[✔] Postgres").unwrap();
            assert_no_fill(&buffer, y);
            for offset in 0..3 {
                let cell = &buffer[(x + offset, y)];
                assert_eq!(cell.fg, success, "{cell:?}");
            }
            let (x2, y2) = find(&buffer, "[ ] SQLite").unwrap();
            assert_no_fill(&buffer, y2);
            assert_eq!(buffer[(x2, y2)].fg, Color::Reset);
            // 再勾上第二项。
            ui.key(KeyCode::Char(' '));
            ui.wait("the second box", |s| s.contains("❯ 2. [✔] SQLite"))
                .await;
            let buffer = frame(&ui);
            assert_no_fill(&buffer, y2);
            assert_eq!(buffer[(x2 + 1, y2)].fg, success);
            // “其他”行也有方框：输入文字后勾上。
            ui.key(KeyCode::Char('4'));
            ui.type_text("Redis");
            ui.wait("the typed answer", |s| s.contains("4. [✔] Redis"))
                .await;
            let buffer = frame(&ui);
            let (x4, y4) = find(&buffer, "[✔] Redis").unwrap();
            assert_eq!(buffer[(x4 + 1, y4)].fg, success);
            assert_no_fill(&buffer, y4);
            ui.key(KeyCode::Enter);
            ui.wait_for("noted").await;
        })
        .await;
        let second = request_text(&model.requests()[1]);
        assert!(second.contains("Postgres, SQLite, Redis"), "{second}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_other_row_shows_the_cursor_where_the_text_goes() {
    for theme in &THEMES {
        let model = asking(false);
        with_ui(model.clone(), setup(theme, 40), |mut ui| async move {
            open(&mut ui).await;
            assert_eq!(ui.cursor(), None);
            // 焦点到“其他”行：光标在占位文字的第一个字上。
            ui.key(KeyCode::Char('4'));
            ui.wait("the pointer on the other row", |s| {
                s.contains("❯ 4. Type something.")
            })
            .await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "Type something.").unwrap();
            assert_no_fill(&buffer, y);
            assert_eq!(ui.cursor(), Some((x, y)));
            // 输入后在文字末尾。
            ui.type_text("Duck");
            ui.wait("the typed text", |s| s.contains("❯ 4. Duck")).await;
            assert_eq!(ui.cursor(), Some((x + 4, y)));
            // 放不下一行时折行，光标在最后一行的文字末尾（续行与文字对齐）。底部区域贴底，多了一行后
            // 整个视图上移一行，位置从这一帧重新找。
            ui.type_text(" lake on a laptop for analytics");
            ui.wait("the wrapped text", |s| s.contains("analytics"))
                .await;
            let buffer = frame(&ui);
            let (dx, dy) = find(&buffer, "Duck lake").unwrap();
            let (wx, wy) = find(&buffer, "analytics").unwrap();
            assert_eq!(wy, dy + 1, "the text wraps onto the next row");
            assert_eq!(wx, dx, "the next row lines up with the text");
            assert_eq!(ui.cursor(), Some((wx + 9, wy)));
            // 焦点离开“其他”行：光标隐藏。
            ui.key(KeyCode::Up);
            ui.wait("the pointer on DuckDB", |s| s.contains("❯ 3. DuckDB"))
                .await;
            assert_eq!(ui.cursor(), None);
            // 回到“其他”行并提交：回答是输入的文字。
            ui.key(KeyCode::Down);
            ui.wait("the pointer on the other row", |s| s.contains("❯ 4. Duck"))
                .await;
            assert_eq!(ui.cursor(), Some((wx + 9, wy)));
            ui.key(KeyCode::Enter);
            ui.wait_for("noted").await;
        })
        .await;
        let second = request_text(&model.requests()[1]);
        assert!(
            second.contains("Duck lake on a laptop for analytics"),
            "{second}"
        );
    }
}
