//! ← 的看板（tui-panels.md §2）：内核的状态到四种状态、每行的记号与字、数量的一行、细节的三行、⌃b 的请求、重读时选中的留着。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::{SessionId, TurnId};
use rust_cli_kernel_api::panels::{ProjectSession, ProjectSessions, SessionActivity};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::store::{SessionOrder, SessionRecord};
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::render::{Span, Style};

use super::*;
use crate::ui::app::Action;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::projects::Request;
use crate::view::Transcript;

const HERE: &str = "/Users/dev/project";
const OTHER: &str = "/Users/dev/other";

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

struct Row {
    id: &'static str,
    root: &'static str,
    title: &'static str,
    updated: u64,
    activity: Option<SessionActivity>,
    prompt: &'static str,
    reply: &'static str,
    branch: Option<&'static str>,
}

fn session(r: &Row) -> ProjectSession {
    ProjectSession {
        record: SessionRecord {
            id: SessionId::new(r.id),
            title: r.title.into(),
            root: EnvPath::new(r.root),
            created_at_ms: 0,
            updated_at_ms: r.updated,
            first_prompt: (!r.prompt.is_empty()).then(|| r.prompt.to_owned()),
        },
        activity: r.activity,
        last_reply: (!r.reply.is_empty()).then(|| r.reply.to_owned()),
        branch: r.branch.map(str::to_owned),
        presence: None,
    }
}

/// 现在的会话在跑，这里另一个等你回答，别的文件夹一个只在库里（有分支）。
fn rows() -> [Row; 3] {
    [
        Row {
            id: "cur",
            root: HERE,
            title: "Current work",
            updated: 9_000,
            activity: Some(SessionActivity::Working),
            prompt: "start   the\nwork",
            reply: "working on it",
            branch: Some("main"),
        },
        Row {
            id: "w",
            root: HERE,
            title: "Wait on me",
            updated: 8_000,
            activity: Some(SessionActivity::NeedsInput),
            prompt: "",
            reply: "",
            branch: Some("main"),
        },
        Row {
            id: "o1",
            root: OTHER,
            title: "Over there",
            updated: 5_000,
            activity: None,
            prompt: "look over there",
            reply: "",
            branch: Some("feat/x"),
        },
    ]
}

fn loaded(rows: &[Row]) -> ProjectSessions {
    ProjectSessions {
        sessions: rows.iter().map(session).collect(),
        more: false,
    }
}

fn opened() -> App {
    let mut app = app();
    app.session = Some("cur".into());
    let _ = app.open_sessions(false);
    let _ = app.sessions_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    app.sessions_loaded(&loaded(&rows()));
    app
}

fn list(app: &App) -> &SessionsList {
    match &app.overlay {
        Some(Overlay::Sessions(list)) => list,
        other => panic!("not the list: {other:?}"),
    }
}

fn choose(app: &mut App, id: &str) {
    let at = list(app)
        .matches()
        .iter()
        .position(|x| x.id == id)
        .unwrap_or_else(|| panic!("no {id}"));
    if let Some(Overlay::Sessions(list)) = app.overlay.as_mut() {
        list.choice = at;
    }
}

fn lines(app: &App) -> Vec<Vec<Span>> {
    let rows: Vec<Element> = app.sessions_rows(100, std::time::Instant::now(), 10_000);
    super::super::pick::lay(&rows, 100)
        .into_iter()
        .map(|l| l.spans)
        .collect()
}

fn texts(app: &App) -> Vec<String> {
    lines(app)
        .iter()
        .map(|l| l.iter().map(|s| &*s.text).collect::<String>())
        .map(|l| l.trim_end().to_owned())
        .collect()
}

fn line_of<'a>(texts: &'a [String], needle: &str) -> &'a str {
    texts
        .iter()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no {needle:?} in {texts:#?}"))
}

#[test]
fn the_kernel_activity_maps_to_the_four_states() {
    use rust_cli_tui_kit::app::widgets::resume_list::State;
    let cases = [
        (
            Some(SessionActivity::NeedsInput),
            Status::NeedsYou,
            State::NeedsYou,
        ),
        (
            Some(SessionActivity::Working),
            Status::Working,
            State::Working,
        ),
        (Some(SessionActivity::Ready), Status::Ready, State::Ready),
        (None, Status::Inactive, State::Inactive),
    ];
    for (activity, status, state) in cases {
        assert_eq!(Status::of(activity), status);
        assert_eq!(status.state(), state);
        assert!(status.admits(activity));
    }
    assert!(!Status::Inactive.admits(Some(SessionActivity::Ready)));
    assert_eq!(State::NeedsYou.word(), "needs you");
}

#[test]
fn each_row_carries_its_state_mark_and_word() {
    let app = opened();
    let texts = texts(&app);
    assert!(
        line_of(&texts, "Current work").ends_with("working · current"),
        "{texts:#?}"
    );
    assert!(
        line_of(&texts, "Wait on me").ends_with("needs you"),
        "{texts:#?}"
    );
    assert!(
        line_of(&texts, "Over there").ends_with("inactive"),
        "{texts:#?}"
    );
    // 记号是状态的墨：等你回答 ● code（err 只给失败，tui-cockpit.md §5）、在跑 ● ok、没打开 ○ fg3。
    let theme = app.ink.theme;
    let mark = |needle: &str| {
        lines(&app)
            .into_iter()
            .find(|l| l.iter().any(|s| s.text.contains(needle)))
            .and_then(|l| l.into_iter().find(|s| s.text == "●" || s.text == "○"))
            .unwrap_or_else(|| panic!("no mark for {needle}"))
    };
    assert_eq!(mark("Wait on me").style, Style::fg(theme.code));
    assert_eq!(mark("Current work").style, Style::fg(theme.ok));
    assert_eq!(mark("Over there").text, "○");
    assert_eq!(mark("Over there").style, theme.fg3);
}

#[test]
fn the_first_row_counts_each_state_and_the_filter_shows() {
    let mut app = opened();
    let texts = texts(&app);
    assert_eq!(texts[0], "   ● 1 needs you   ● 1 working   ○ 1 inactive");
    assert_eq!(texts[1], "   ~/project");
    // ⌃f 选中的状态总是写（数量为 0 时也写），字是 fg2。
    for _ in 0..3 {
        app.sessions_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    }
    assert_eq!(list(&app).status, Some(Status::Ready));
    let texts = self::texts(&app);
    assert_eq!(
        texts[0],
        "   ● 1 needs you   ● 1 working   ○ 0 ready   ○ 1 inactive"
    );
    assert_eq!(texts[1], "   No session matches “ready”.");
    // 只列这个文件夹时没有这一行（同 Puffin）。
    app.sessions_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.sessions_loaded(&loaded(&rows()[..2]));
    assert!(self::texts(&app).iter().all(|l| !l.contains("needs you")));
}

#[test]
fn the_details_add_the_branch_the_first_prompt_and_the_last_reply() {
    let mut app = opened();
    choose(&mut app, "o1");
    let texts = texts(&app);
    let n = texts.len();
    assert_eq!(texts[n - 2], "            ~/other  ·  feat/x", "{texts:#?}");
    assert_eq!(texts[n - 1], "            › look over there", "{texts:#?}");
    // 打开着的会话还有最后的回复；空白并成一格。
    choose(&mut app, "cur");
    let texts = self::texts(&app);
    let n = texts.len();
    assert_eq!(texts[n - 3], "            ~/project  ·  main");
    assert_eq!(texts[n - 2], "            › start the work");
    assert_eq!(texts[n - 1], "            working on it");
}

#[test]
fn ctrl_b_starts_a_new_session_in_the_chosen_folder() {
    let mut app = opened();
    choose(&mut app, "o1");
    let action = app.sessions_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert_eq!(
        action,
        Some(Some(Action::Projects(Request::New {
            root: OTHER.to_owned()
        })))
    );
    assert!(app.overlay.is_none());
    assert!(app.composer.is_empty());
    // 没有行时是这个工作区。
    let mut app = self::app();
    let _ = app.open_sessions(false);
    let _ = app.sessions_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    app.sessions_loaded(&ProjectSessions::default());
    let action = app.sessions_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    let here = app.view.workspace().root.as_str().to_owned();
    assert_eq!(
        action,
        Some(Some(Action::Projects(Request::New { root: here })))
    );
}

#[test]
fn a_refresh_keeps_the_chosen_session_and_updates_the_answers() {
    let mut app = opened();
    assert_eq!(app.sessions_refreshing(), Some(SessionOrder::Updated));
    // 词的回答里也有等你回答的那个。
    if let Some(Overlay::Sessions(list)) = app.overlay.as_mut() {
        list.query = "wait".into();
    }
    let answer = found_of(&session(&rows()[1]), "wait").unwrap();
    app.sessions_found("wait".into(), vec![answer]);
    choose(&mut app, "w");
    // 回答了：内核说它打开着、空闲；别的文件夹的会话更新了，排到前面。
    let mut now = rows();
    now[1].activity = Some(SessionActivity::Ready);
    now[2].updated = 9_500;
    app.sessions_refreshed(&loaded(&now));
    let list = list(&app);
    assert_eq!(list.chosen_found().map(|x| x.id.as_str()), Some("w"));
    assert_eq!(
        list.chosen_found().and_then(|x| x.activity),
        Some(SessionActivity::Ready)
    );
    // 只列这个文件夹时不重读。
    let mut app = opened();
    app.sessions_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.sessions_refreshing(), None);
}

#[test]
fn turns_asks_and_background_work_change_the_state() {
    let started = KernelEvent::TurnStarted {
        turn: TurnId::new("t1"),
        command: None,
    };
    assert!(touches_activity(&started));
    let work = KernelEvent::BackgroundWork {
        work: "w1".into(),
        status: rust_cli_kernel_api::env_shell::BackgroundStatus::Running,
    };
    assert!(touches_activity(&work));
    let renamed = KernelEvent::TitleChanged { title: "x".into() };
    assert!(!touches_activity(&renamed));
}
