//! `/resume` 的会话列表：打开与读到、按标题筛与词的回答、键、跟着草稿、Puffin host 的搜索规则。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::panels::{ProjectSession, ProjectSessions, SessionActivity};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{SessionOrder, SessionRecord};
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

const HERE: &str = "/Users/dev/project";

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn session(id: &str, title: &str, first: &str, last: &str) -> ProjectSession {
    ProjectSession {
        record: SessionRecord {
            id: SessionId::new(id),
            title: title.into(),
            root: EnvPath::new(HERE),
            created_at_ms: 0,
            updated_at_ms: 1_000,
            first_prompt: (!first.is_empty()).then(|| first.to_owned()),
        },
        activity: None,
        last_reply: (!last.is_empty()).then(|| last.to_owned()),
        branch: None,
        presence: None,
    }
}

fn sessions() -> ProjectSessions {
    ProjectSessions {
        sessions: vec![
            session("cur", "Current work", "", ""),
            session(
                "a",
                "Fix the footer wrap",
                "why does the footer wrap early on wide glyphs?",
                "",
            ),
            session(
                "b",
                "Port the renderer",
                "",
                "the renderer now draws glyphs in cells",
            ),
        ],
        more: false,
    }
}

fn list(app: &App) -> &SessionsList {
    match &app.overlay {
        Some(Overlay::Sessions(list)) => list,
        other => panic!("not the list: {other:?}"),
    }
}

fn ids(app: &App) -> Vec<String> {
    list(app).matches().iter().map(|x| x.id.clone()).collect()
}

#[test]
fn opening_writes_the_draft_and_selects_the_newest_other_session() {
    let mut app = app();
    app.session = Some("cur".into());
    let action = app.open_sessions(false);
    assert_eq!(app.composer.area.text(), "/resume ");
    assert_eq!(
        action,
        Action::Projects(Request::LoadResume {
            root: Some(app.view.workspace().root.as_str().to_owned()),
            order: SessionOrder::Updated,
        })
    );
    // 读到之前没有行。
    assert!(
        app.sessions_rows(80, std::time::Instant::now(), 0)
            .is_empty()
    );
    app.sessions_loaded(&sessions());
    assert_eq!(ids(&app), ["cur", "a", "b"]);
    assert_eq!(
        list(&app).choice,
        1,
        "the newest session that is not this one"
    );
}

#[test]
fn a_word_filters_titles_until_its_answer_arrives() {
    let mut app = app();
    app.open_sessions(false);
    app.sessions_loaded(&sessions());
    app.composer.set_text("/resume glyphs");
    let search = app.settle_sessions("/resume glyphs");
    assert_eq!(
        search,
        Some(Action::Projects(Request::Search {
            query: "glyphs".into(),
            root: Some(app.view.workspace().root.as_str().to_owned()),
            order: rust_cli_kernel_api::store::SessionOrder::Updated,
        }))
    );
    // 标题里没有这个词：回答到之前没有会话。
    assert!(ids(&app).is_empty());
    // 别的词的回答不用。
    app.sessions_found(
        "glyph".into(),
        vec![found_of(&sessions().sessions[1], "glyph").unwrap()],
    );
    assert!(ids(&app).is_empty());
    let found: Vec<Found> = sessions()
        .sessions
        .iter()
        .filter_map(|s| found_of(s, "glyphs"))
        .collect();
    app.sessions_found("glyphs".into(), found);
    assert_eq!(ids(&app), ["a", "b"]);
    // 同一个词再打一遍不再搜。
    assert_eq!(app.settle_sessions("/resume glyphs "), None);
    // 草稿不再以 `/resume ` 开头：列表关上。
    assert_eq!(app.settle_sessions("/resum"), None);
    assert!(app.overlay.is_none());
}

#[test]
fn the_host_rule_finds_titles_first_prompts_and_last_replies() {
    let all = sessions();
    // 标题含它：没有片段。
    assert_eq!(found_of(&all.sessions[1], "footer").unwrap().snippet, None);
    // 第一条输入含它：命中的那一段。
    assert_eq!(
        found_of(&all.sessions[1], "glyphs")
            .unwrap()
            .snippet
            .as_deref(),
        Some("why does the footer wrap early on wide glyphs?")
    );
    // 最后的回答含它。
    assert!(
        found_of(&all.sessions[2], "cells")
            .unwrap()
            .snippet
            .is_some()
    );
    assert_eq!(found_of(&all.sessions[0], "glyphs"), None);
    // 没有标题时是第一条输入的第一行；等你回答的会话有记号。
    let mut untitled = session("u", "", "first line\nsecond", "");
    untitled.activity = Some(SessionActivity::NeedsInput);
    let x = found_of(&untitled, "").unwrap();
    assert_eq!(x.title, "first line");
    assert!(x.waiting);
}

#[test]
fn arrows_wrap_enter_opens_and_esc_keeps_the_draft() {
    let mut app = app();
    app.session = Some("cur".into());
    app.open_sessions(false);
    app.sessions_loaded(&sessions());
    assert_eq!(app.sessions_key(key(KeyCode::Down)), Some(None));
    assert_eq!(list(&app).choice, 2);
    app.sessions_key(key(KeyCode::Down));
    assert_eq!(list(&app).choice, 0, "wraps to the top");
    // 现在的会话：只关上列表、清空草稿。
    assert_eq!(app.sessions_key(key(KeyCode::Enter)), Some(None));
    assert!(app.overlay.is_none());
    assert!(app.composer.is_empty());
    app.open_sessions(false);
    app.sessions_loaded(&sessions());
    assert_eq!(
        app.sessions_key(key(KeyCode::Enter)),
        Some(Some(Action::Projects(Request::Open {
            id: "a".into(),
            root: HERE.into(),
            cd: None,
            startup: false,
        })))
    );
    assert_eq!(
        app.said.quiet.as_deref(),
        Some("Opening “Fix the footer wrap”…")
    );
    // esc：关上，草稿留着；打字的键传下去。
    app.open_sessions(true);
    assert_eq!(app.sessions_key(key(KeyCode::Char('x'))), None);
    assert_eq!(app.sessions_key(key(KeyCode::Esc)), Some(None));
    assert!(app.overlay.is_none());
    assert_eq!(app.composer.area.text(), "/resume ");
}

fn found(id: &str, root: &str, updated: u64) -> Found {
    Found {
        presence: None,
        note: Default::default(),
        id: id.into(),
        root: root.into(),
        title: id.into(),
        created: 0,
        updated,
        waiting: false,
        activity: None,
        snippet: None,
        folder: String::new(),
        branch: None,
        prompt: String::new(),
        reply: String::new(),
    }
}

#[test]
fn folders_go_newest_activity_first_and_keep_the_order_inside() {
    // 进来的次序（比如 ⌃e 的创建时间）：o1 o2 在同一个文件夹，o2 更新；/p 与 /q 一样新，/p 先出现。
    let xs = [
        found("o1", "/o", 5),
        found("p1", "/p", 9),
        found("o2", "/o", 7),
        found("q1", "/q", 9),
    ];
    let got: Vec<&str> = by_folder(xs.iter().collect())
        .iter()
        .map(|x| x.id.as_str())
        .collect();
    assert_eq!(got, ["p1", "q1", "o1", "o2"]);
    assert!(by_folder(Vec::new()).is_empty());
}

fn texts(rows: &[Element]) -> Vec<String> {
    super::super::pick::lay(rows, 80)
        .iter()
        .map(|l| l.spans.iter().map(|s| &*s.text).collect::<String>())
        .map(|l| l.trim_end().to_owned())
        .collect()
}

#[test]
fn every_folder_draws_a_group_title_over_each_folder() {
    let at = |id: &str, title: &str, root: &str, updated: u64| {
        let mut s = session(id, title, "", "");
        s.record.root = EnvPath::new(root);
        s.record.updated_at_ms = updated;
        s
    };
    // 内核给的是创建时间的次序：别的文件夹的会话夹在中间，最近活动的文件夹是这里。
    let loaded = ProjectSessions {
        sessions: vec![
            at("p1", "Here newest", HERE, 9_000),
            at("o1", "Over there", "/Users/dev/other", 5_000),
            at("p2", "Here older", HERE, 1_000),
        ],
        more: false,
    };
    let mut app = app();
    let _ = app.open_sessions(false);
    let _ = app.sessions_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    app.sessions_loaded(&loaded);
    assert_eq!(ids(&app), ["p1", "p2", "o1"]);
    let rows = texts(&app.sessions_rows(80, std::time::Instant::now(), 10_000));
    let line = |text: &str| {
        rows.iter()
            .position(|l| l.contains(text))
            .unwrap_or_else(|| panic!("no {text:?} in {rows:#?}"))
    };
    // 第一行是看板的数量（sessions_status.rs）；组名同 `@` 面板：第 3 列起，只有文件夹；组之间不空行。
    assert_eq!(rows[0], "   ○ 3 inactive");
    assert_eq!(rows[1], "   ~/project");
    assert!(
        line("Here newest") == 2 && line("Here older") == 3,
        "{rows:#?}"
    );
    assert_eq!(rows[4], "   ~/other");
    assert_eq!(line("Over there"), 5, "{rows:#?}");
    // ↑↓ 按分组后的次序走。
    app.sessions_key(key(KeyCode::Down));
    assert_eq!(list(&app).choice, 1);
    assert_eq!(list(&app).chosen_found().map(|x| x.id.as_str()), Some("p2"));
    // ⇥ 回到这个文件夹：没有组名，同 Puffin。
    app.sessions_key(key(KeyCode::Tab));
    app.sessions_loaded(&ProjectSessions {
        sessions: vec![loaded.sessions[0].clone(), loaded.sessions[2].clone()],
        more: false,
    });
    let rows = texts(&app.sessions_rows(80, std::time::Instant::now(), 10_000));
    assert!(rows.iter().all(|l| !l.contains("~/project")), "{rows:#?}");
    assert!(rows[0].contains("Here newest"), "{rows:#?}");
}
