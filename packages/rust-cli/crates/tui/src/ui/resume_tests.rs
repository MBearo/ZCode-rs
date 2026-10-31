//! `/resume` 页的按键（F29，tui-manage.md §10.4–§10.6）：直接打字搜索、焦点到头停住、工具栏的焦点与切换、预览与
//! 全屏对话的请求、改名、删除确认（数字只移到删除）、当前会话的限制、别的目录先选目录、启动形态的 ctrl+c。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::palette::{ColorLevel, Palette};

fn row(id: &str, root: &str, title: &str) -> SessionRow {
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

fn app() -> App {
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

fn with_rows(app: &mut App, rows: Vec<SessionRow>) {
    app.open_resume_page(false, 0);
    if let Some(Overlay::Resume(page)) = app.overlay.as_mut() {
        page.rows = Some(rows);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn page(app: &mut App) -> &mut ResumePage {
    match app.overlay.as_mut() {
        Some(Overlay::Resume(page)) => page,
        other => panic!("not the resume page: {other:?}"),
    }
}

const HERE: &str = "/Users/dev/project";

#[test]
fn opening_asks_for_the_current_directory_and_toggles_reload() {
    let mut app = app();
    let action = app.open_resume_page(false, 0);
    assert_eq!(
        action,
        Action::Projects(Request::LoadResume {
            root: Some(HERE.into()),
            order: SessionOrder::Updated
        })
    );
    // ←/→ 切换焦点控件（Filter）的值；tab 换到 Sort。
    assert_eq!(
        app.resume_key(key(KeyCode::Right)),
        Some(Action::Projects(Request::LoadResume {
            root: None,
            order: SessionOrder::Updated
        }))
    );
    app.resume_key(key(KeyCode::Tab));
    assert_eq!(
        app.resume_key(key(KeyCode::Left)),
        Some(Action::Projects(Request::LoadResume {
            root: None,
            order: SessionOrder::Created
        }))
    );
    assert!(
        page(&mut app).rows.is_none(),
        "a toggle reloads from scratch"
    );
}

#[test]
fn typing_searches_and_esc_clears_then_closes() {
    let mut app = app();
    with_rows(
        &mut app,
        vec![row("sess_1", HERE, "Alpha"), row("sess_2", HERE, "Beta")],
    );
    for c in "j k".chars() {
        app.resume_key(key(KeyCode::Char(c)));
    }
    assert_eq!(page(&mut app).query, "j k");
    app.resume_key(key(KeyCode::Esc));
    assert_eq!(page(&mut app).query, "");
    // 会话 ID 与分支也搜。
    for c in "SESS_2".chars() {
        app.resume_key(key(KeyCode::Char(c)));
    }
    assert_eq!(page(&mut app).filtered().len(), 1);
    app.resume_key(key(KeyCode::Backspace));
    assert_eq!(page(&mut app).filtered().len(), 2);
    app.resume_key(key(KeyCode::Esc));
    app.resume_key(key(KeyCode::Esc));
    assert!(app.overlay.is_none());
}

#[test]
fn moving_stops_at_the_ends() {
    let mut app = app();
    with_rows(&mut app, vec![row("a", HERE, "A"), row("b", HERE, "B")]);
    app.resume_key(key(KeyCode::Up));
    assert_eq!(page(&mut app).selected, 0);
    app.resume_key(ctrl('n'));
    app.resume_key(key(KeyCode::Down));
    assert_eq!(page(&mut app).selected, 1);
}

#[test]
fn previews_and_transcripts_are_requested_once() {
    let mut app = app();
    with_rows(&mut app, vec![row("a", HERE, "A")]);
    assert_eq!(
        app.resume_key(ctrl('e')),
        Some(Action::Projects(Request::Preview("a".into())))
    );
    assert_eq!(page(&mut app).previews.get("a"), Some(&Preview::Loading));
    app.resume_key(ctrl('e'));
    assert_eq!(page(&mut app).expanded, None);
    page(&mut app)
        .previews
        .insert("a".into(), Preview::Loaded(Vec::new()));
    // 已有预览：再展开不重取。
    assert_eq!(app.resume_key(ctrl('e')), None);
    assert_eq!(page(&mut app).expanded.as_deref(), Some("a"));
    assert_eq!(
        app.resume_key(ctrl('t')),
        Some(Action::Projects(Request::Transcript("a".into())))
    );
    assert_eq!(
        page(&mut app).transcript,
        Some(TranscriptState::Loading("a".into()))
    );
    // 读取中只认 esc：取消后迟到的快照丢弃。
    app.resume_key(key(KeyCode::Esc));
    assert_eq!(page(&mut app).transcript, None);
    app.resume_snapshot("a", true, Err("late".into()), 0);
    assert_eq!(page(&mut app).transcript, None);
    assert_eq!(page(&mut app).error, None);
}

#[test]
fn delete_needs_confirmation_and_the_current_session_is_refused() {
    let mut app = app();
    let mut current = row("cur", HERE, "Current");
    current.current = true;
    with_rows(&mut app, vec![row("a", HERE, "Old"), current]);
    app.resume_key(ctrl('d'));
    assert_eq!(
        page(&mut app)
            .confirm
            .as_ref()
            .map(|c| (c.name.as_str(), c.focus)),
        Some(("Old", 0))
    );
    // 数字 2 只移到删除（删除要明确确认），enter 才删。
    assert_eq!(app.resume_key(key(KeyCode::Char('2'))), None);
    assert_eq!(
        app.resume_key(key(KeyCode::Enter)),
        Some(Action::Projects(Request::Delete("a".into())))
    );
    // 取消项上 enter：什么都不做。
    app.resume_key(ctrl('d'));
    assert_eq!(app.resume_key(key(KeyCode::Enter)), None);
    assert!(page(&mut app).confirm.is_none());
    app.resume_key(key(KeyCode::Down));
    app.resume_key(ctrl('d'));
    assert!(page(&mut app).confirm.is_none());
    assert_eq!(
        page(&mut app).error.as_deref(),
        Some("Use /clear to leave the current session before deleting it.")
    );
    // 删除的回执：行去掉、焦点留在原下标。
    app.resume_deleted("a");
    let page = page(&mut app);
    assert_eq!(page.filtered().len(), 1);
    assert_eq!(page.selected, 0);
}

#[test]
fn rename_submits_the_trimmed_name() {
    let mut app = app();
    with_rows(&mut app, vec![row("a", HERE, "Old")]);
    app.resume_key(ctrl('r'));
    assert_eq!(page(&mut app).rename, Some(("a".into(), "Old".into())));
    for _ in 0..3 {
        app.resume_key(key(KeyCode::Backspace));
    }
    app.resume_paste("  New\nname ");
    assert_eq!(
        app.resume_key(key(KeyCode::Enter)),
        Some(Action::Projects(Request::Rename {
            id: "a".into(),
            title: "New name".into()
        }))
    );
    // 空名字不提交。
    app.resume_key(ctrl('r'));
    for _ in 0..3 {
        app.resume_key(key(KeyCode::Backspace));
    }
    assert_eq!(app.resume_key(key(KeyCode::Enter)), None);
}

#[test]
fn another_directory_asks_first_and_the_current_session_is_already_viewed() {
    let mut app = app();
    let mut current = row("cur", HERE, "Current");
    current.current = true;
    with_rows(
        &mut app,
        vec![row("far", "/Users/dev/other", "Far"), current],
    );
    assert_eq!(app.resume_key(key(KeyCode::Enter)), None);
    assert!(page(&mut app).cwd.is_some());
    // 选 2（当前目录）：打开后换到当前目录，不做信任检查。
    app.resume_key(key(KeyCode::Char('2')));
    assert_eq!(
        app.resume_key(key(KeyCode::Enter)),
        Some(Action::Projects(Request::Open {
            id: "far".into(),
            root: "/Users/dev/other".into(),
            cd: Some(HERE.into()),
            startup: false
        }))
    );
    assert!(app.overlay.is_none());
    // esc 是会话目录：交给 runner 做信任检查。
    with_rows(&mut app, vec![row("far", "/Users/dev/other", "Far")]);
    app.resume_key(key(KeyCode::Enter));
    assert_eq!(
        app.resume_key(key(KeyCode::Esc)),
        Some(Action::Projects(Request::Open {
            id: "far".into(),
            root: "/Users/dev/other".into(),
            cd: None,
            startup: false
        }))
    );
    let mut current = row("cur", HERE, "Current");
    current.current = true;
    with_rows(&mut app, vec![current]);
    assert_eq!(app.resume_key(key(KeyCode::Enter)), None);
    assert!(app.overlay.is_none());
}

/// 启动形态选中的会话带上 `startup`（打开时不写续写占位，session-lifecycle.md §6.3.1，工作线 F79）：当前目录的会话也
/// 交给 runner 的打开请求；应用内的页面照旧直接切过去。
#[test]
fn the_startup_page_marks_what_it_opens() {
    let mut app = app();
    with_rows(&mut app, vec![row("near", HERE, "Near")]);
    assert_eq!(
        app.resume_key(key(KeyCode::Enter)),
        Some(Action::Resume("near".into()))
    );
    app.open_resume_page(true, 0);
    page(&mut app).rows = Some(vec![row("near", HERE, "Near")]);
    assert_eq!(
        app.resume_key(key(KeyCode::Enter)),
        Some(Action::Projects(Request::Open {
            id: "near".into(),
            root: HERE.into(),
            cd: None,
            startup: true
        }))
    );
    assert!(app.overlay.is_none());
    // 别的目录：目录选择之后同样带上。
    app.open_resume_page(true, 0);
    page(&mut app).rows = Some(vec![row("far", "/Users/dev/other", "Far")]);
    assert_eq!(app.resume_key(key(KeyCode::Enter)), None);
    assert_eq!(
        app.resume_key(key(KeyCode::Esc)),
        Some(Action::Projects(Request::Open {
            id: "far".into(),
            root: "/Users/dev/other".into(),
            cd: None,
            startup: true
        }))
    );
}

#[test]
fn ctrl_c_quits_only_at_startup() {
    let mut app = app();
    with_rows(&mut app, Vec::new());
    assert_eq!(app.resume_key(ctrl('c')), None);
    assert!(app.overlay.is_none());
    app.open_resume_page(true, 0);
    assert_eq!(app.resume_key(ctrl('c')), Some(Action::Quit));
}
