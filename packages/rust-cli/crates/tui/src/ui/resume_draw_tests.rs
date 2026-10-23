//! `/resume` 页的排版（F29，tui-manage.md §10.2、§10.3）：间隙收起、工具栏的宽窄、选中行的底色（深浅两种背景）、
//! Filter=All 的路径、footer 的进度与按优先级丢掉的提示、展开的预览、删除确认与目录选择。
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use super::*;
use crate::ui::app::Overlay;
use crate::ui::resume::{Confirm, CwdPrompt, ResumePage};
use crate::ui::session_rows::SessionRow;

mod shared {
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::testing::selection;

    use crate::ui::app::{App, Overlay};
    use crate::ui::i18n::Lang;
    use crate::ui::palette::{ColorLevel, Palette};
    use crate::ui::session_rows::SessionRow;
    use crate::view::Transcript;

    pub fn row(id: &str, root: &str, title: &str) -> SessionRow {
        SessionRow {
            id: id.into(),
            title: title.into(),
            prompt: format!("prompt {id}"),
            root: root.into(),
            created: 0,
            updated: 0,
            branch: Some("main".into()),
            activity: None,
            reply: String::new(),
            current: false,
        }
    }

    pub fn app() -> App {
        App::new(
            Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
            Lang::En,
            Transcript::new(
                selection(),
                PermissionMode::Default,
                crate::view::test_workspace(),
            ),
            Some("/Users/dev".into()),
            "0.1.0".into(),
        )
    }

    pub fn with_rows(app: &mut App, rows: Vec<SessionRow>) {
        app.open_resume_page(false, 0);
        if let Some(Overlay::Resume(page)) = app.overlay.as_mut() {
            page.rows = Some(rows);
        }
    }
}

/// 画一帧，返回每行的文字与选中行（`›` 或 `⌄`）第一格的底色。
fn render(app: &mut App, width: u16, height: u16) -> (Vec<String>, Option<Color>) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let Some(Overlay::Resume(mut page)) = app.overlay.take() else {
        panic!("no resume page");
    };
    terminal
        .draw(|frame| draw(frame, app, &mut page, 10 * 86_400_000))
        .unwrap();
    app.overlay = Some(Overlay::Resume(page));
    let buffer = terminal.backend().buffer().clone();
    let mut lines = Vec::new();
    let mut fill = None;
    for y in 0..height {
        let mut text = String::new();
        for x in 0..width {
            let cell = &buffer[(x, y)];
            if (cell.symbol() == "›" || cell.symbol() == "⌄") && fill.is_none() && x < 4 {
                fill = Some(cell.bg);
            }
            text.push_str(cell.symbol());
        }
        lines.push(text.trim_end().to_owned());
    }
    (lines, fill)
}

fn rows() -> Vec<SessionRow> {
    let mut far = shared::row("sess_far", "/Users/dev/other/place", "Far away");
    far.updated = 10 * 86_400_000 - 2 * 3_600_000;
    let mut near = shared::row("sess_near", "/Users/dev/project", "Near");
    near.updated = 10 * 86_400_000 - 35 * 60_000;
    vec![near, far]
}

fn page(app: &mut App) -> &mut ResumePage {
    match app.overlay.as_mut() {
        Some(Overlay::Resume(page)) => page,
        _ => panic!(),
    }
}

#[test]
fn the_layout_follows_codex_and_gaps_collapse() {
    let mut app = shared::app();
    shared::with_rows(&mut app, rows());
    let (lines, _) = render(&mut app, 130, 24);
    assert_eq!(lines[0], " Resume a previous session");
    assert_eq!(lines[1], "");
    assert_eq!(lines[2], " Filter:  Cwd  All    Sort:  Updated  Created");
    assert_eq!(lines[4], " Type to search");
    assert!(lines[6].starts_with("  › 35m ago     Near"), "{lines:?}");
    assert!(
        lines[7].starts_with("    2h ago      Far away"),
        "{lines:?}"
    );
    assert!(
        lines[20].starts_with("────") && lines[20].ends_with(" 1 / 2 · 100% ─"),
        "{lines:?}"
    );
    assert_eq!(
        lines[21],
        " enter resume   ⌃r rename   ⌃d delete   esc exit   ⌃c exit   tab focus sort/filter   ←/→ change option"
    );
    // 矮的终端：三个间隙都收起，列表仍至少 6 行。
    let (lines, _) = render(&mut app, 130, 13);
    assert_eq!(lines[1], " Filter:  Cwd  All    Sort:  Updated  Created");
    assert_eq!(lines[2], " Type to search");
    assert!(lines[3].starts_with("  › 35m ago"), "{lines:?}");
}

#[test]
fn narrow_terminals_shorten_the_toolbar_and_drop_hints_by_priority() {
    let mut app = shared::app();
    shared::with_rows(&mut app, rows());
    let (lines, _) = render(&mut app, 40, 24);
    assert_eq!(lines[2], " Filter: Cwd  Sort: Updated");
    // 第一行留优先级高的：enter（0）、esc（1），然后是优先级 2 的。
    assert!(lines[21].starts_with(" enter resume"), "{lines:?}");
    assert!(lines[21].contains("esc exit"), "{lines:?}");
    assert!(!lines[21].contains("tab"), "{lines:?}");
    let (lines, _) = render(&mut app, 24, 24);
    assert_eq!(lines[2], " Filter: Cwd");
}

#[test]
fn the_selected_row_uses_the_selection_fill_in_both_themes() {
    for (background, fill) in [
        ((0, 0, 0), crate::ui::tokens::SELECTION_FILL_DARK),
        ((255, 255, 255), crate::ui::tokens::SELECTION_FILL_LIGHT),
    ] {
        let mut app = shared::app();
        app.palette = crate::ui::palette::Palette::new(
            crate::ui::palette::ColorLevel::TrueColor,
            Some(background),
        );
        shared::with_rows(&mut app, rows());
        let (_, bg) = render(&mut app, 100, 24);
        assert_eq!(
            bg,
            Some(Color::Rgb(fill.0, fill.1, fill.2)),
            "{background:?}"
        );
    }
}

#[test]
fn all_shows_paths_and_comfortable_adds_a_metadata_line() {
    let mut app = shared::app();
    shared::with_rows(&mut app, rows());
    page(&mut app).all = true;
    let (lines, _) = render(&mut app, 100, 24);
    assert!(
        lines[7].contains("Far away") && lines[7].ends_with("⌁ ~/other/place"),
        "{lines:?}"
    );
    page(&mut app).comfortable = true;
    let (lines, _) = render(&mut app, 100, 24);
    assert_eq!(lines[6], "  › Near");
    assert!(
        lines[7].starts_with("    35m ago       ⌁ ~/project  ⎇ main"),
        "{lines:?}"
    );
    assert_eq!(lines[8], "");
    assert!(lines[22].contains("⌃o dense "), "{lines:?}");
}

#[test]
fn the_expanded_preview_lists_details_and_the_conversation() {
    let mut app = shared::app();
    shared::with_rows(&mut app, rows());
    {
        let page = page(&mut app);
        page.expanded = Some("sess_near".into());
        page.previews.insert(
            "sess_near".into(),
            crate::ui::resume::Preview::Loaded(vec![
                (true, "hello".into()),
                (false, "hi back".into()),
            ]),
        );
    }
    let (lines, _) = render(&mut app, 100, 30);
    assert!(lines[6].starts_with("  ⌄ 35m ago"), "{lines:?}");
    assert_eq!(lines[7], "    │ Session:    Near (sess_near)");
    assert!(
        lines[8].starts_with("    │ Created:    10 days ago · 1970-01-01 00:00:00"),
        "{lines:?}"
    );
    assert_eq!(lines[10], "    │ Directory:  ~/project");
    assert_eq!(lines[11], "    │ Branch:     ⎇ main");
    assert_eq!(lines[13], "    │ Conversation:");
    assert_eq!(lines[14], "    │ hello");
    assert_eq!(lines[15], "    └ hi back");
}

#[test]
fn confirmations_and_the_directory_prompt() {
    let mut app = shared::app();
    shared::with_rows(&mut app, rows());
    page(&mut app).confirm = Some(Confirm {
        id: "sess_far".into(),
        name: "Far away".into(),
        focus: 0,
    });
    let (lines, _) = render(&mut app, 100, 24);
    let at = lines
        .iter()
        .position(|l| l.contains("Permanently delete “Far away”?"))
        .unwrap();
    assert_eq!(lines[at + 1], "  This cannot be undone.");
    assert_eq!(lines[at + 2], "  › Cancel");
    assert_eq!(lines[at + 3], "    Permanently delete session");
    assert!(lines[21].contains("enter select · esc cancel"), "{lines:?}");
    page(&mut app).confirm = None;
    page(&mut app).cwd = Some(CwdPrompt {
        row: rows()[1].clone(),
        focus: 1,
    });
    let (lines, _) = render(&mut app, 100, 24);
    let at = lines
        .iter()
        .position(|l| l.contains("Working directory · resume"))
        .unwrap();
    assert_eq!(
        lines[at + 1],
        "  Session = latest cwd recorded in the resumed session"
    );
    assert_eq!(
        lines[at + 4],
        "    1. Use session directory (~/other/place)"
    );
    assert_eq!(lines[at + 5], "  › 2. Use current directory (~/project)");
    assert_eq!(
        lines[at + 7],
        "  enter continue · esc use session · ⌃c exit"
    );
}
