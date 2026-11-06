//! `/resume` 列表上旧界面才有的几步：⌃r 改名、⌃d 删除、⇥ 所有文件夹、⌃t 读对话（sessions_steps.rs）。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::panels::{ProjectSession, ProjectSessions};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{SessionOrder, SessionRecord};
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};

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
    app.session = Some("cur".into());
    let _ = app.open_sessions(false);
    let session = |id: &str, title: &str, root: &str, at: u64| ProjectSession {
        record: SessionRecord {
            id: SessionId::new(id),
            title: title.into(),
            root: EnvPath::new(root),
            created_at_ms: 0,
            updated_at_ms: at,
            first_prompt: None,
        },
        activity: None,
        last_reply: None,
        branch: None,
        presence: None,
    };
    app.sessions_loaded(&ProjectSessions {
        sessions: vec![
            session("cur", "Current work", "/Users/dev/project", 3_000),
            session("a", "Fix the footer", "/Users/dev/other", 2_000),
        ],
        more: false,
    });
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn step(app: &App) -> &Step {
    match &app.overlay {
        Some(Overlay::Sessions(list)) => &list.step,
        other => panic!("not the list: {other:?}"),
    }
}

fn texts(rows: &[Element]) -> Vec<String> {
    super::super::pick::lay(rows, 80)
        .iter()
        .map(|l| l.spans.iter().map(|s| &*s.text).collect::<String>())
        .map(|l| l.trim_end().to_owned())
        .collect()
}

#[test]
fn ctrl_r_renames_in_the_draft_and_esc_puts_the_words_back() {
    let mut app = app();
    assert_eq!(app.sessions_key(ctrl('r')), Some(None));
    assert_eq!(app.composer.area.text(), "Fix the footer");
    let Some(Overlay::Sessions(list)) = &app.overlay else {
        unreachable!()
    };
    assert_eq!(
        texts(&app.sessions_rows(80, std::time::Instant::now(), 0)),
        ["   Rename “Fix the footer”"]
    );
    assert!(list.step != Step::List);
    // 别的键编辑草稿；草稿不以 `/resume ` 开头时列表也不关上。
    assert_eq!(app.sessions_key(key(KeyCode::Char('x'))), None);
    app.composer.set_text("Fix the wrap");
    assert_eq!(app.settle_sessions("Fix the wrap"), None);
    assert!(app.overlay.is_some());
    assert_eq!(
        app.sessions_key(key(KeyCode::Enter)),
        Some(Some(Action::Projects(Request::Rename {
            id: "a".into(),
            title: "Fix the wrap".into(),
        })))
    );
    assert_eq!(app.composer.area.text(), "/resume ");
    assert_eq!(step(&app), &Step::List);
    // esc 不改名。
    app.sessions_key(ctrl('r'));
    assert_eq!(app.sessions_key(key(KeyCode::Esc)), Some(None));
    assert_eq!(app.composer.area.text(), "/resume ");
}

#[test]
fn ctrl_d_asks_with_numbered_answers() {
    let mut app = app();
    assert_eq!(app.sessions_key(ctrl('d')), Some(None));
    assert_eq!(
        texts(&app.sessions_rows(80, std::time::Instant::now(), 0)),
        [
            "   Delete “Fix the footer”?",
            "   1  Delete   it can't be opened again",
            "   2  Keep",
        ]
    );
    // 2 留着，回到列表。
    assert_eq!(app.sessions_key(key(KeyCode::Char('2'))), Some(None));
    assert_eq!(step(&app), &Step::List);
    app.sessions_key(ctrl('d'));
    assert_eq!(
        app.sessions_key(key(KeyCode::Enter)),
        Some(Some(Action::Projects(Request::Delete("a".into()))))
    );
    assert_eq!(
        app.sessions_reload(),
        Some(Request::LoadResume {
            root: Some(app.view.workspace().root.as_str().to_owned()),
            order: SessionOrder::Updated,
        })
    );
}

#[test]
fn tab_lists_every_folder_and_the_detail_names_it() {
    let mut app = app();
    assert_eq!(
        app.sessions_key(key(KeyCode::Tab)),
        Some(Some(Action::Many(vec![Action::Projects(
            Request::LoadResume {
                root: None,
                order: SessionOrder::Updated,
            }
        )])))
    );
    assert_eq!(
        app.sessions_reload()
            .map(|r| matches!(r, Request::LoadResume { root: None, .. })),
        Some(true)
    );
    app.sessions_loaded(&ProjectSessions {
        sessions: vec![ProjectSession {
            record: SessionRecord {
                id: SessionId::new("a"),
                title: "Fix the footer".into(),
                root: EnvPath::new("/Users/dev/other"),
                created_at_ms: 0,
                updated_at_ms: 2_000,
                first_prompt: None,
            },
            activity: None,
            last_reply: None,
            branch: None,
            presence: None,
        }],
        more: false,
    });
    let rows = texts(&app.sessions_rows(80, std::time::Instant::now(), 2_000));
    assert_eq!(rows.last().map(String::as_str), Some("            ~/other"));
}

#[test]
fn ctrl_t_reads_the_session_as_a_page_and_esc_goes_back() {
    let mut app = app();
    assert_eq!(
        app.sessions_key(ctrl('t')),
        Some(Some(Action::Projects(Request::Transcript("a".into()))))
    );
    let page = app.sessions_page_body(80, 0).expect("a page");
    let head: String = page.lines[1].spans.iter().map(|s| &*s.text).collect();
    assert_eq!(head, "   Sessions  ›  Fix the footer");
    // 读回之前和别的会话的回答都不换上。
    app.sessions_read("b", Err("no".into()), 0);
    assert!(matches!(step(&app), Step::Reading { .. }));
    app.sessions_read("a", Err("gone".into()), 0);
    assert_eq!(step(&app), &Step::List);
    app.sessions_key(ctrl('t'));
    assert_eq!(app.sessions_key(key(KeyCode::Esc)), Some(None));
    assert_eq!(step(&app), &Step::List);
    assert!(app.overlay.is_some());
}

#[test]
fn ctrl_e_asks_for_the_created_order_and_back() {
    let mut app = app();
    let root = Some(app.view.workspace().root.as_str().to_owned());
    let load = |order| {
        Some(Some(Action::Many(vec![Action::Projects(
            Request::LoadResume {
                root: root.clone(),
                order,
            },
        )])))
    };
    assert_eq!(app.sessions_key(ctrl('e')), load(SessionOrder::Created));
    // 旧的行留着，直到新的顺序到了（不闪空）；改名、删除之后的重读用同一个顺序。
    assert!(app.sessions_rows(80, std::time::Instant::now(), 0).len() > 1);
    assert_eq!(
        app.sessions_reload(),
        Some(Request::LoadResume {
            root: root.clone(),
            order: SessionOrder::Created,
        })
    );
    assert_eq!(app.sessions_key(ctrl('e')), load(SessionOrder::Updated));
}

#[test]
fn ctrl_f_filters_by_status_and_says_it_when_nothing_is_left() {
    let mut app = app();
    let ids = |app: &App| -> Vec<String> {
        match &app.overlay {
            Some(Overlay::Sessions(list)) => list.matches().iter().map(|x| x.id.clone()).collect(),
            _ => Vec::new(),
        }
    };
    assert_eq!(ids(&app), ["cur", "a"]);
    // 两个会话都没打开（内核没有状态）：等你回答、在跑、打开着空闲都筛空，那一句写这一步。
    assert_eq!(app.sessions_key(ctrl('f')), Some(None));
    assert!(ids(&app).is_empty());
    assert_eq!(
        texts(&app.sessions_rows(80, std::time::Instant::now(), 0)),
        ["   No session matches “needs you”."]
    );
    app.sessions_key(ctrl('f'));
    app.sessions_key(ctrl('f'));
    assert!(ids(&app).is_empty());
    // 没打开的：两个都在；再按回到全部。
    app.sessions_key(ctrl('f'));
    assert_eq!(ids(&app), ["cur", "a"]);
    app.sessions_key(ctrl('f'));
    assert_eq!(ids(&app), ["cur", "a"]);
    let Some(Overlay::Sessions(list)) = &app.overlay else {
        unreachable!()
    };
    assert_eq!(list.status, None);
}
