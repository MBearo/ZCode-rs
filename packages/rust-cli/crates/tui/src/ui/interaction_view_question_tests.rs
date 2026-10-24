//! 提问视图的选项（docs/specs/rust-cli/tui.md §7.6，工作线 F74）：单选的已选项、确认页、“其他”行的光标。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use serde_json::json;

use super::super::View;
use crate::ui::i18n::{EN, Lang, ZH};
use crate::ui::interaction::Interaction;
use crate::ui::keyboard::Typing;
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::text::plain;
use rust_cli_kernel_api::orchestration::InteractionRequest;

const SUCCESS: Color = Color::Rgb(78, 186, 101);

fn two_questions() -> InteractionRequest {
    InteractionRequest::Question {
        tool_use: ToolUseId::new("tu1"),
        questions: json!([
            {"question": "Which database?", "header": "DB", "multiSelect": false,
             "options": [{"label": "Postgres"}, {"label": "SQLite"}]},
            {"question": "Which cache?", "header": "Cache", "multiSelect": false,
             "options": [{"label": "Redis"}, {"label": "None"}]},
        ]),
    }
}

/// 弹出后过了防误触的时间，依次按键。
fn pressed(request: &InteractionRequest, keys: &[KeyCode]) -> Interaction {
    let start = Instant::now();
    let mut ui = Interaction::new(InteractionId::new("i1"), request, start);
    for code in keys {
        ui.on_key(
            request,
            KeyEvent::new(*code, KeyModifiers::NONE),
            start + Duration::from_millis(200),
            Typing::default(),
        );
    }
    ui
}

fn row<'a>(lines: &'a [Line<'static>], needle: &str) -> &'a Line<'static> {
    lines
        .iter()
        .find(|line| plain(line).contains(needle))
        .unwrap_or_else(|| {
            panic!(
                "no row with {needle}: {:?}",
                lines.iter().map(plain).collect::<Vec<_>>()
            )
        })
}

#[test]
fn a_chosen_single_option_is_green_with_a_tick() {
    let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let view = View {
        palette: &palette,
        strings: &EN,
        lang: Lang::En,
        width: 60,
        height: 40,
        now_ms: 0,
    };
    let request = two_questions();
    // 第一题选 SQLite，到第二题后回到第一题。
    let ui = pressed(&request, &[KeyCode::Char('2'), KeyCode::Left]);
    let (lines, _) = ui.lines(&request, &view);
    let chosen = row(&lines, "SQLite");
    assert!(
        plain(chosen).ends_with("2. SQLite ✔"),
        "{:?}",
        plain(chosen)
    );
    let name = chosen
        .spans
        .iter()
        .find(|span| span.content == "SQLite")
        .unwrap();
    assert_eq!(name.style.fg, Some(SUCCESS));
    let tick = chosen
        .spans
        .iter()
        .find(|span| span.content == " ✔")
        .unwrap();
    assert_eq!(tick.style.fg, Some(SUCCESS));
    // 焦点回到第一项：强调色的指针，没有底色。
    let focused = row(&lines, "Postgres");
    assert!(
        plain(focused).starts_with("  ❯ 1. Postgres"),
        "{:?}",
        plain(focused)
    );
    assert!(lines.iter().all(|line| line.style.bg.is_none()));
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.style.bg.is_none())
    );
}

#[test]
fn the_review_page_has_green_answers_and_plain_choices() {
    let palette = Palette::new(ColorLevel::TrueColor, Some((255, 255, 255)));
    let view = View {
        palette: &palette,
        strings: &EN,
        lang: Lang::En,
        width: 60,
        height: 40,
        now_ms: 0,
    };
    let request = two_questions();
    let ui = pressed(&request, &[KeyCode::Char('1'), KeyCode::Char('1')]);
    let (lines, _) = ui.lines(&request, &view);
    let answer = row(&lines, "→ Postgres");
    let arrow = answer
        .spans
        .iter()
        .find(|span| span.content.starts_with('→'))
        .unwrap();
    assert_eq!(arrow.style.fg, Some(Color::Rgb(44, 122, 57)));
    let submit = row(&lines, "Submit answers");
    assert!(
        plain(submit).starts_with("  ❯ 1. Submit answers"),
        "{:?}",
        plain(submit)
    );
    assert!(submit.spans.iter().all(|span| span.style.bg.is_none()));
    let number = submit
        .spans
        .iter()
        .find(|span| span.content == "1. ")
        .unwrap();
    assert!(number.style.add_modifier.contains(Modifier::DIM));
    assert_eq!(ui.cursor(&request, &view), None);
}

#[test]
fn the_cursor_follows_the_other_row_and_stays_on_screen() {
    let palette = Palette::new(ColorLevel::Ansi16, None);
    let request = two_questions();
    // 中文的占位文字：光标在它的第一个字上（缩进 2、指针 2、序号 3）。
    let view = View {
        palette: &palette,
        strings: &ZH,
        lang: Lang::Zh,
        width: 30,
        height: 40,
        now_ms: 0,
    };
    let ui = pressed(&request, &[KeyCode::Char('3')]);
    let (lines, _) = ui.lines(&request, &view);
    let (at, column) = ui.cursor(&request, &view).unwrap();
    assert!(
        plain(&lines[at]).starts_with("  ❯ 3. 输入其他回答。"),
        "{:?}",
        plain(&lines[at])
    );
    assert_eq!(column, 7);
    // 最后一行正好占满：光标停在最后一格。
    let ui = pressed(
        &request,
        &[KeyCode::Char('3'), KeyCode::Char('a'), KeyCode::Char('b')],
    );
    let view = View { width: 9, ..view };
    let (lines, _) = ui.lines(&request, &view);
    let (at, column) = ui.cursor(&request, &view).unwrap();
    assert_eq!(plain(&lines[at]), "  ❯ 3. ab");
    assert_eq!(column, 8);
    // 焦点在别的行：没有光标。
    let ui = pressed(&request, &[KeyCode::Char('3'), KeyCode::Up]);
    assert_eq!(ui.cursor(&request, &view), None);
}
