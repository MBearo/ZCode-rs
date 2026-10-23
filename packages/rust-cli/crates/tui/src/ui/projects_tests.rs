//! ← 项目页的视图状态（F29，tui-panels.md §2）：分组与排序、筛选与搜索、组头的计数、焦点的移动、改名输入。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

fn row(
    id: &str,
    root: &str,
    title: &str,
    updated: u64,
    activity: Option<SessionActivity>,
) -> SessionRow {
    SessionRow {
        id: id.into(),
        title: title.into(),
        prompt: format!("prompt of {id}"),
        root: root.into(),
        created: 0,
        updated,
        branch: None,
        activity,
        reply: String::new(),
        current: false,
    }
}

fn page() -> ProjectsPage {
    ProjectsPage {
        rows: Some(vec![
            row("a", "/w/zeta", "Zeta one", 10, None),
            row("b", "/w/alpha", "Alpha old", 5, None),
            row(
                "c",
                "/w/alpha",
                "Alpha new",
                50,
                Some(SessionActivity::Working),
            ),
            row("d", "/w/here", "Here", 1, Some(SessionActivity::Ready)),
        ]),
        ..ProjectsPage::default()
    }
}

fn ids(page: &ProjectsPage, indices: &[usize]) -> Vec<String> {
    indices
        .iter()
        .map(|&i| page.rows.as_ref().unwrap()[i].id.clone())
        .collect()
}

#[test]
fn the_current_project_comes_first_then_paths_then_recency() {
    let page = page();
    assert_eq!(ids(&page, &page.ordered("/w/here")), ["d", "c", "b", "a"]);
    let entries = page.entries("/w/here");
    assert_eq!(
        entries,
        [
            Entry::Group {
                row: 3,
                shown: 1,
                total: 1
            },
            Entry::Session(3),
            Entry::Gap,
            Entry::Group {
                row: 2,
                shown: 2,
                total: 2
            },
            Entry::Session(2),
            Entry::Session(1),
            Entry::Gap,
            Entry::Group {
                row: 0,
                shown: 1,
                total: 1
            },
            Entry::Session(0),
        ]
    );
}

#[test]
fn filters_and_search_narrow_the_rows_and_counts_show_x_of_y() {
    let mut page = page();
    page.filter = 2; // Working
    assert_eq!(ids(&page, &page.visible("/w/here")), ["c"]);
    page.filter = 0;
    page.search = Some("OLD".into());
    assert_eq!(ids(&page, &page.visible("/w/here")), ["b"]);
    assert!(page.entries("/w/here").contains(&Entry::Group {
        row: 1,
        shown: 1,
        total: 2
    }));
    // 路径与首条输入也搜。
    page.search = Some("zeta".into());
    assert_eq!(ids(&page, &page.visible("/w/here")), ["a"]);
    page.search = Some("prompt of d".into());
    assert_eq!(ids(&page, &page.visible("/w/here")), ["d"]);
    // 改名中的那一个总可见。
    page.rename = Some(("c".into(), String::new()));
    assert_eq!(ids(&page, &page.visible("/w/here")), ["d", "c"]);
}

#[test]
fn more_rows_end_with_a_note() {
    let mut page = page();
    page.more = true;
    assert_eq!(page.entries("/w/here").last(), Some(&Entry::More));
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn open(app: &mut App) -> &mut ProjectsPage {
    match app.overlay.as_mut() {
        Some(Overlay::Projects(page)) => page,
        other => panic!("not the projects page: {other:?}"),
    }
}

#[test]
fn keys_move_with_wraparound_and_r_edits_the_name() {
    let mut app = app();
    let root = app.workspace().to_owned();
    let mut page = page();
    for row in page.rows.as_mut().unwrap() {
        if row.id == "d" {
            row.root = root.clone();
        }
    }
    app.overlay = Some(Overlay::Projects(Box::new(page)));
    // 焦点从第一个可见的会话开始；↑ 绕回到最后一个（Codex）。
    assert_eq!(open(&mut app).focused_row(&root).unwrap().id, "d");
    app.projects_key(key(KeyCode::Up));
    assert_eq!(open(&mut app).focused_row(&root).unwrap().id, "a");
    app.projects_key(key(KeyCode::Down));
    assert_eq!(open(&mut app).focused_row(&root).unwrap().id, "d");
    app.projects_key(key(KeyCode::Down));
    assert_eq!(open(&mut app).focused_row(&root).unwrap().id, "c");
    // r：改名输入从原名开始，enter 提交去掉首尾空白后的名字。
    app.projects_key(key(KeyCode::Char('r')));
    assert_eq!(
        open(&mut app).rename,
        Some(("c".into(), "Alpha new".into()))
    );
    for c in " x ".chars() {
        app.projects_key(key(KeyCode::Char(c)));
    }
    let action = app.projects_key(key(KeyCode::Enter));
    assert_eq!(
        action,
        Some(Action::Projects(Request::Rename {
            id: "c".into(),
            title: "Alpha new x".into()
        }))
    );
    // n：在焦点行的项目新建；别的目录的会话 enter 交给 runner（页面开着，先做信任检查）。
    assert_eq!(
        app.projects_key(key(KeyCode::Char('n'))),
        Some(Action::Projects(Request::New {
            root: "/w/alpha".into()
        }))
    );
    assert_eq!(
        app.projects_key(key(KeyCode::Enter)),
        Some(Action::Projects(Request::Open {
            id: "c".into(),
            root: "/w/alpha".into(),
            cd: None
        }))
    );
    assert!(app.overlay.is_some());
    // 同一个工作区的会话直接切过去。
    app.projects_key(key(KeyCode::Home));
    assert_eq!(
        app.projects_key(key(KeyCode::Enter)),
        Some(Action::Resume("d".into()))
    );
    assert!(app.overlay.is_none());
}
