//! `$` 面板（tui-input.md §1.4）：打开、过滤、↑↓、⏎ 插入、esc 关上，与槽。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::skill_mention::{From, MentionSkill};
use crate::view::Transcript;

fn app() -> App {
    let mut app = App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    let skill = |name: &str, description: &str, from: From| MentionSkill {
        name: name.into(),
        description: description.into(),
        path: String::new(),
        from,
    };
    app.composer.mentions.catalog = Some(skill_mention::catalog(vec![
        skill("release-notes", "Write release notes.", From::User),
        skill("bench", "Run the bench.", From::User),
        skill("triage", "Read a crash log.", From::User),
    ]));
    app
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE), Instant::now());
}

fn rows(app: &App) -> Vec<String> {
    super::super::pick::lay(&app.skill_lines(120, 1_000), 120)
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

#[test]
fn dollar_opens_the_panel_and_the_preview_is_on_the_right() {
    let mut app = app();
    press(&mut app, KeyCode::Char('$'));
    let shown = rows(&app);
    assert!(
        shown[0].starts_with("   $bench") && shown[0].ends_with("$bench  yours"),
        "{shown:?}"
    );
    assert!(shown[2].ends_with("Run the bench."), "{shown:?}");
    assert_eq!(app.skill_slot(), [("⏎".to_owned(), "insert".to_owned())]);
    press(&mut app, KeyCode::Char('r'));
    press(&mut app, KeyCode::Char('e'));
    assert!(rows(&app)[0].ends_with("$release-notes  yours"));
}

#[test]
fn enter_inserts_the_mention_and_esc_closes_until_the_word_goes() {
    let mut app = app();
    press(&mut app, KeyCode::Char('$'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.composer.area.text(), "$release-notes ");
    assert!(app.composer.popup.is_none());
    // 认识的提及是技能的墨色。
    let inks = app.skill_inks(app.composer.area.text(), 1_000);
    assert_eq!(inks.len(), 1);
    assert_eq!((inks[0].0, inks[0].1), (0, 14));
    press(&mut app, KeyCode::Char('$'));
    assert!(app.composer.popup.is_some());
    press(&mut app, KeyCode::Esc);
    assert!(app.composer.popup.is_none());
    press(&mut app, KeyCode::Char('b'));
    assert!(app.composer.popup.is_none(), "esc closed it for this word");
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Char('$'));
    assert!(app.composer.popup.is_some(), "a new word opens it again");
}
