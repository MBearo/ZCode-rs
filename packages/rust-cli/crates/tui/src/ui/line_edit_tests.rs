//! 单行输入（架构整理 U12，用户决定 Q8）：backspace 按字素删除，每个单行输入都收粘贴（换行压成空格）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::{InteractionId, SessionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{DetachedAsk, DetachedEvent, InteractionRequest};
use serde_json::json;

use crate::ui::app::{App, Overlay};
use crate::ui::export::ExportView;
use crate::ui::list::{ListView, Shape};
use crate::ui::test_support;
use crate::ui::test_support::key;

/// 一个字素、两个字符：e 加组合的锐音符。
const ACCENTED: &str = "e\u{301}";
/// 一个字素、两个字符：竖大拇指加肤色。
const THUMBS: &str = "\u{1f44d}\u{1f3fd}";

#[test]
fn backspace_removes_one_grapheme_and_paste_flattens_lines() {
    let mut query = format!("a{ACCENTED}");
    assert!(super::backspace(&mut query));
    assert_eq!(query, "a");
    let mut query = "搜索".to_owned();
    super::backspace(&mut query);
    assert_eq!(query, "搜");
    let mut empty = String::new();
    assert!(!super::backspace(&mut empty));
    let mut line = "x".to_owned();
    super::paste(&mut line, "one\r\ntwo\nthree\rfour");
    assert_eq!(line, "xone two three four");
}

fn app() -> App {
    let mut app = test_support::app();
    app.composer.skills_loaded = true;
    app
}

fn backspace(app: &mut App) {
    app.on_key(key(KeyCode::Backspace), Instant::now());
}

fn filtered_list() -> ListView {
    ListView::new(Shape::OneLine, "Pick".into(), Vec::new(), "none".into()).filterable()
}

fn list_filter(app: &App) -> &str {
    match &app.overlay {
        Some(Overlay::List(list)) => list.filter.as_deref().unwrap_or_default(),
        other => panic!("not a list: {other:?}"),
    }
}

/// 修前 backspace 是 `String::pop`，删掉一个字符：组合字符与带肤色的表情只删了一半。
#[test]
fn backspace_deletes_a_whole_grapheme_in_every_single_line_input() {
    // `/export` 的文件名。
    let mut app = app();
    app.overlay = Some(Overlay::Export(ExportView::Name(format!("a{ACCENTED}"))));
    backspace(&mut app);
    assert!(matches!(&app.overlay, Some(Overlay::Export(ExportView::Name(name))) if name == "a"));
    // 列表的过滤。
    let mut list = filtered_list();
    list.filter = Some(format!("a{THUMBS}"));
    app.overlay = Some(Overlay::List(list));
    backspace(&mut app);
    assert_eq!(list_filter(&app), "a");
    // 详细视图的搜索。
    app.overlay = None;
    app.open_detail();
    app.on_key(key(KeyCode::Char('/')), Instant::now());
    for c in format!("a{ACCENTED}").chars() {
        app.on_key(key(KeyCode::Char(c)), Instant::now());
    }
    backspace(&mut app);
    match &app.overlay {
        Some(Overlay::Detail(detail)) => {
            assert_eq!(detail.search.as_ref().map(|s| s.query.as_str()), Some("a"));
        }
        other => panic!("not the detail view: {other:?}"),
    }
    // 输入历史的搜索（ctrl+r）。
    app.overlay = None;
    app.on_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    for c in format!("a{THUMBS}").chars() {
        app.on_key(key(KeyCode::Char(c)), Instant::now());
    }
    backspace(&mut app);
    assert_eq!(
        app.composer.search.as_ref().map(|s| s.0.as_str()),
        Some("a")
    );
    // 询问框的意见。
    let mut app = self::app();
    ask(&mut app);
    app.on_key(key(KeyCode::Tab), Instant::now());
    app.interaction.as_mut().unwrap().text = format!("a{ACCENTED}");
    backspace(&mut app);
    assert_eq!(app.interaction.as_ref().unwrap().text, "a");
}

/// 修前列表的过滤、详细视图的搜索与输入历史的搜索不收粘贴（丢掉，或者进了被盖住的输入框）。
#[test]
fn every_single_line_input_takes_a_paste() {
    let mut app = app();
    app.overlay = Some(Overlay::List(filtered_list()));
    app.on_paste("glm\n5");
    assert_eq!(list_filter(&app), "glm 5");
    app.overlay = None;
    app.open_detail();
    app.on_key(key(KeyCode::Char('/')), Instant::now());
    app.on_paste("needle");
    match &app.overlay {
        Some(Overlay::Detail(detail)) => {
            assert_eq!(
                detail.search.as_ref().map(|s| s.query.as_str()),
                Some("needle")
            );
        }
        other => panic!("not the detail view: {other:?}"),
    }
    app.overlay = None;
    app.on_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    app.on_paste("git\npush");
    assert_eq!(
        app.composer.search.as_ref().map(|s| s.0.as_str()),
        Some("git push")
    );
    assert!(app.composer.is_empty(), "{:?}", app.composer.area.text());
}

fn ask(app: &mut App) {
    app.detached_changed(DetachedEvent::Asked {
        ask: Box::new(DetachedAsk {
            interaction: InteractionId::new("i1"),
            session: SessionId::new("sess_old"),
            title: String::new(),
            request: InteractionRequest::Permission {
                tool_use: ToolUseId::new("tu1"),
                tool: "Bash".into(),
                input: json!({"command": "ls"}),
                message: "needs approval".into(),
                suggestions: Vec::new(),
                rule_seed: None,
                asked_by: None,
                machine: Default::default(),
            },
        }),
    });
    app.sync_interaction(Instant::now() - Duration::from_secs(1));
}
