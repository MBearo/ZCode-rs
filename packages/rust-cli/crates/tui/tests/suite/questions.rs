//! 提问的页（docs/specs/rust-cli/tui.md §5.2、§7.6；tui-kit.md §4.2「权限询问、提问、计划审批」，照 Puffin）：回答在
//! panel 底色的板上，焦点行的序号与名字是 brand，其余序号 fg3、名字 fg2，没有指针与底色条；多选勾上的 `■` 是 brand；
//! 自己的回答在真的输入框里打（以前的“其他”行与它的终端光标删掉）。深浅两种背景。
use super::support::Shot;
use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::{text_response, tool_use_response};
use rust_cli_tui::{ColorLevel, Colors, Ink};
use rust_cli_tui_kit::render::{Color, attr};
use serde_json::{Value, json};

use super::support::{GatedModel, Setup, Ui, request_text, with_ui};

type Rgb = (u8, u8, u8);

/// 深浅两种背景。
struct Theme {
    background: Rgb,
    foreground: Rgb,
}

const THEMES: [Theme; 2] = [
    Theme {
        background: (0, 0, 0),
        foreground: (255, 255, 255),
    },
    Theme {
        background: (255, 255, 255),
        foreground: (0, 0, 0),
    },
];

/// 一种背景下的期望颜色（主题 token，tui-kit.md §3.4）：`brand`、`fg2`、`fg3` 与板的 `panel` 底色。
struct Inks {
    brand: Color,
    fg2: Color,
    fg3: Color,
    panel: Color,
}

fn inks(theme: &Theme) -> Inks {
    let ink = Ink::new(
        ColorLevel::TrueColor,
        Some(theme.foreground),
        Some(theme.background),
    );
    let theme = ink.theme;
    let Some(panel) = theme.panel else {
        panic!("rich 的主题有板的底色");
    };
    Inks {
        brand: theme.brand,
        fg2: theme.fg2.fg_color(),
        fg3: theme.fg3.fg_color(),
        panel,
    }
}

fn setup(theme: &Theme, width: u16) -> Setup {
    Setup {
        width,
        height: 30,
        colors: Colors::new(ColorLevel::TrueColor, Some(theme.background))
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

fn frame(ui: &Ui) -> Shot {
    ui.frame()
}

/// 第一处 `needle` 所在的（列，行）。
fn find(buffer: &Shot, needle: &str) -> Option<(u16, u16)> {
    buffer.find(needle)
}

/// 整行铺板的底色（Puffin 的板，tui-kit.md §4.2）。
fn assert_panel(buffer: &Shot, y: u16, panel: Color) {
    for x in buffer.area.left()..buffer.area.right() {
        let cell = &buffer[(x, y)];
        assert_eq!(cell.bg, panel, "column {x} of row {y}: {cell:?}");
    }
}

/// 等到最近一帧满足条件（焦点移动只改颜色与光标，画面的文字不变）；等待有上限，只为让失败的测试结束。
async fn until(ui: &Ui, what: &str, done: impl Fn(&Ui) -> bool) {
    let wait = async {
        while !done(ui) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("timed out waiting for {what}; screen:\n{}", ui.screen());
    }
}

/// 等提问弹出（第一个回答在板上）并过了回答点亮的时间。
async fn open(ui: &mut Ui, first: &str) {
    ui.submit("pick a database");
    ui.wait_for(first).await;
    ui.settle().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_focused_row_uses_the_brand_color() {
    for theme in &THEMES {
        let Inks {
            brand,
            fg2,
            fg3,
            panel,
        } = inks(theme);
        with_ui(asking(false), setup(theme, 80), |mut ui| async move {
            open(&mut ui, "1  Postgres").await;
            let screen = ui.screen();
            assert!(screen.contains("1–3 pick  esc stop"), "{screen}");
            assert!(screen.contains("Or type your own answer"), "{screen}");
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "1  Postgres").unwrap();
            assert_eq!(x, 3, "the number starts at column 3");
            assert_panel(&buffer, y, panel);
            let number = &buffer[(x, y)];
            assert_eq!(number.fg, brand, "{number:?}");
            let name = &buffer[(x + 3, y)];
            assert_eq!(name.fg, brand, "{name:?}");
            assert!(!name.has(attr::BOLD), "{name:?}");
            // 其余行：序号 fg3，名字 fg2。
            let (x2, y2) = find(&buffer, "2  SQLite").unwrap();
            assert_panel(&buffer, y2, panel);
            assert_eq!(buffer[(x2, y2)].fg, fg3);
            assert_eq!(buffer[(x2 + 3, y2)].fg, fg2);
            // 焦点下移：颜色跟着走。
            ui.key(KeyCode::Down);
            until(&ui, "the focus on SQLite", |ui| {
                ui.frame()[(x2 + 3, y2)].fg == brand
            })
            .await;
            let buffer = frame(&ui);
            assert_eq!(buffer[(x2, y2)].fg, brand);
            assert_eq!(buffer[(x + 3, y)].fg, fg2);
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_select_ticks_use_the_brand_color() {
    for theme in &THEMES {
        let Inks {
            brand, fg3, panel, ..
        } = inks(theme);
        let model = asking(true);
        with_ui(model.clone(), setup(theme, 80), |mut ui| async move {
            open(&mut ui, "□ Postgres").await;
            assert!(
                ui.screen().contains("space tick  esc stop"),
                "{}",
                ui.screen()
            );
            // 勾上第一项（焦点在这一项）：`■` 是 brand，行铺板的底色。
            ui.key(KeyCode::Char(' '));
            ui.wait_for("■ Postgres").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "■ Postgres").unwrap();
            assert_eq!(buffer[(x, y)].fg, brand, "{:?}", buffer[(x, y)]);
            assert_panel(&buffer, y, panel);
            let (x2, y2) = find(&buffer, "□ SQLite").unwrap();
            assert_eq!(buffer[(x2, y2)].fg, fg3);
            // 焦点移到第二项，再勾上它；没聚焦的 `■` 仍是 brand。
            ui.key(KeyCode::Down);
            ui.key(KeyCode::Char(' '));
            ui.wait_for("■ SQLite").await;
            let buffer = frame(&ui);
            assert_eq!(buffer[(x, y)].fg, brand);
            assert_eq!(buffer[(x2, y2)].fg, brand);
            // 输入框里的字也是这一页的回答（Puffin：勾上的与自己的话一起交出）。
            ui.type_text("Redis");
            ui.wait_for("› Redis").await;
            ui.key(KeyCode::Enter);
            ui.wait_for("noted").await;
        })
        .await;
        let second = request_text(&model.requests()[1]);
        assert!(second.contains("Postgres, SQLite, Redis"), "{second}");
    }
}

/// 自己的回答：输入框有字时数字是字，槽只剩 `esc stop`，⏎ 交出这些字。
#[tokio::test(flavor = "multi_thread")]
async fn your_own_words_answer_the_page() {
    for theme in &THEMES {
        let model = asking(false);
        with_ui(model.clone(), setup(theme, 40), |mut ui| async move {
            open(&mut ui, "1  Postgres").await;
            ui.type_text("Duck lake on 2 laptops");
            ui.wait_for("› Duck lake").await;
            let screen = ui.screen();
            assert!(!screen.contains("1–3 pick"), "{screen}");
            ui.key(KeyCode::Enter);
            ui.wait_for("noted").await;
        })
        .await;
        let second = request_text(&model.requests()[1]);
        assert!(second.contains("Duck lake on 2 laptops"), "{second}");
    }
}
