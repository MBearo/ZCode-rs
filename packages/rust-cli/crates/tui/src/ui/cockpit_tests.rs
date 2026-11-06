//! 驾驶舱的分组、次序、数量、筛选、键与窄宽的行（tui-cockpit.md §6）。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::panels::{
    ProjectSession, ProjectSessions, SessionActivity, SessionPresence,
};
use rust_cli_kernel_api::peers::{PeerAddress, PeerAsk, PeerPresence};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::SessionRecord;
use rust_cli_kernel_api::testing::selection;

use super::super::app::{App, Overlay};
use super::super::i18n::Lang;
use super::super::probe::{ColorLevel, Colors};
use super::super::sessions_steps::Step;
use super::*;
use crate::view::Transcript;

fn app() -> App {
    let colors = Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let mut app = App::new(
        colors,
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.session = Some("current".to_owned());
    app
}

fn session(id: &str, updated: u64, activity: Option<SessionActivity>) -> ProjectSession {
    ProjectSession {
        record: SessionRecord {
            id: SessionId::new(id),
            title: format!("title {id}"),
            root: EnvPath::new(format!("/srv/{id}")),
            created_at_ms: 1,
            updated_at_ms: updated,
            first_prompt: Some(format!("first {id}")),
        },
        activity,
        last_reply: None,
        branch: Some("main".to_owned()),
        presence: None,
    }
}

fn elsewhere(mut s: ProjectSession, since: u64, ask: Option<&str>) -> ProjectSession {
    s.presence = Some(SessionPresence {
        here: false,
        address: PeerAddress::from_socket("/peers/2.sock"),
        since_ms: Some(since),
        presence: PeerPresence {
            ask: ask.map(|question| PeerAsk {
                question: question.to_owned(),
                detail: None,
                answers: Vec::new(),
            }),
            ..PeerPresence::default()
        },
        tmux: None,
    });
    s
}

fn open(app: &mut App, sessions: Vec<ProjectSession>, notes: HashMap<String, CockpitNote>) {
    let _ = app.open_cockpit();
    app.cockpit_notes(notes);
    app.sessions_loaded(&ProjectSessions {
        sessions,
        more: false,
    });
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    app.dispatch(KeyEvent::new(code, modifiers), Instant::now())
}

fn order(app: &App) -> Vec<String> {
    let Some(Overlay::Sessions(list)) = &app.overlay else {
        panic!("open");
    };
    list.matches().iter().map(|x| x.id.clone()).collect()
}

fn note(ended: Option<u64>, seen: Option<u64>, failed: Option<&str>) -> CockpitNote {
    CockpitNote {
        ended_at_ms: ended,
        failed: failed.map(str::to_owned),
        seen_at_ms: seen,
    }
}

fn fleet() -> (Vec<ProjectSession>, HashMap<String, CockpitNote>) {
    let sessions = vec![
        session("rest", 900, None),
        elsewhere(
            session("need-new", 10, Some(SessionActivity::NeedsInput)),
            800,
            Some("Which?"),
        ),
        elsewhere(
            session("need-old", 10, Some(SessionActivity::NeedsInput)),
            100,
            Some("Run?"),
        ),
        elsewhere(
            session("work", 10, Some(SessionActivity::Working)),
            500,
            None,
        ),
        session("new", 10, None),
        session("failed", 10, None),
        session("seen", 700, None),
    ];
    let notes = HashMap::from([
        ("new".to_owned(), note(Some(600), Some(500), None)),
        (
            "failed".to_owned(),
            note(Some(650), None, Some("Rate limited.")),
        ),
        ("seen".to_owned(), note(Some(600), Some(600), None)),
    ]);
    (sessions, notes)
}

#[test]
fn the_kinds_follow_the_table_and_the_order_puts_the_longest_wait_first() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    assert_eq!(
        order(&app),
        [
            "need-old", "need-new", "work", "failed", "new", "rest", "seen"
        ]
    );
}

#[test]
fn seen_equal_to_ended_is_not_new_and_a_rename_does_not_make_it_new() {
    let mut x = super::super::sessions_list::found_of(&session("a", 5_000, None), "").unwrap();
    x.note = note(Some(100), Some(100), None);
    assert_eq!(kind_of(&x), Kind::Rest);
    x.note = note(Some(101), Some(100), None);
    assert_eq!(kind_of(&x), Kind::New);
    assert_eq!(since_of(&x), 101);
}

#[test]
fn typed_words_filter_and_punctuation_opens_no_panel() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    for c in "/!@$".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        assert!(app.composer.popup.is_none());
        assert_eq!(app.composer.mode, crate::ui::composer::InputMode::Prompt);
    }
    assert_eq!(app.composer.area.text(), "/!@$");
    assert!(order(&app).is_empty());
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.cockpit_open(), "esc clears the words first");
    assert_eq!(app.composer.area.text(), "");
    for c in "work".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(order(&app), ["work"]);
}

#[test]
fn the_question_mark_on_an_empty_filter_opens_keys_and_any_key_closes_them() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    let step = |app: &App| match &app.overlay {
        Some(Overlay::Sessions(list)) => list.step.clone(),
        _ => panic!("open"),
    };
    assert_eq!(step(&app), Step::Keys);
    press(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(step(&app), Step::List);
    assert_eq!(app.composer.area.text(), "");
}

#[test]
fn left_goes_back_and_marks_the_current_session_seen() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    let action = press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert!(app.overlay.is_none());
    assert_eq!(
        action,
        Some(Action::Projects(Request::Seen("current".to_owned())))
    );
}

#[test]
fn enter_on_an_agent_elsewhere_asks_instead_of_opening_a_second_writer() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(action, None);
    let Some(Overlay::Sessions(list)) = &app.overlay else {
        panic!("open");
    };
    assert!(matches!(list.step, Step::Elsewhere { .. }));
}

#[test]
fn stop_on_a_resting_agent_says_nothing_to_stop() {
    let mut app = app();
    open(&mut app, vec![session("rest", 900, None)], HashMap::new());
    let action = press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(action, None);
    assert_eq!(app.said.quiet.as_deref(), Some("Nothing to stop."));
}

#[test]
fn stop_on_an_agent_elsewhere_goes_to_its_address() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    let action = press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(
        action,
        Some(Action::Projects(Request::Stop {
            address: Some(PeerAddress::from_socket("/peers/2.sock")),
        }))
    );
}

#[test]
fn tab_switches_to_folders_and_keeps_the_chosen_agent() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    let action = press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        action,
        Some(Action::Projects(Request::RememberGrouping(true)))
    );
    let Some(Overlay::Sessions(list)) = &app.overlay else {
        panic!("open");
    };
    assert_eq!(list.chosen_found().map(|x| x.id.as_str()), Some("need-new"));
}

/// 窗格从第 2 行起有 panel 地，字从第 3 行起；记号：等你 code、失败 err（§1.1、§2.2）。
#[test]
fn the_pane_sits_on_its_ground_and_the_marks_use_their_inks() {
    let mut app = app();
    app.ink = crate::ui::ink::Ink::new(
        ColorLevel::TrueColor,
        Some((255, 255, 255)),
        Some((0, 0, 0)),
    );
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    let theme = app.ink.theme;
    let panel = theme.panel.expect("a rich theme has a panel");
    let (lines, _) = app.cockpit_screen(120, 30, Instant::now(), 1_000);
    let on_ground = |line: &rust_cli_tui_kit::render::StyledLine| {
        line.spans.iter().any(|span| span.style.bg_color() == panel)
    };
    assert!(!on_ground(&lines[1]), "the head row has no ground");
    assert!(lines[2..].iter().all(on_ground));
    let text = |line: &rust_cli_tui_kit::render::StyledLine| {
        line.spans
            .iter()
            .map(|s| s.text.as_ref())
            .collect::<String>()
    };
    assert!(
        text(&lines[3]).contains("title need-old"),
        "the pane text starts on row 3"
    );
    let mark_of = |needle: &str| {
        lines
            .iter()
            .find(|line| text(line).contains(needle))
            .and_then(|line| {
                line.spans
                    .iter()
                    .find(|s| matches!(s.text.as_ref(), "●" | "✗"))
            })
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("no mark for {needle}"))
    };
    assert_eq!(mark_of("title need-new").fg_color(), theme.code);
    assert_eq!(mark_of("title failed").fg_color(), theme.err);
}

#[test]
fn the_screen_is_exactly_as_tall_as_asked_and_narrow_has_no_pane() {
    let mut app = app();
    let (sessions, notes) = fleet();
    open(&mut app, sessions, notes);
    let now = Instant::now();
    let text = |lines: &[rust_cli_tui_kit::render::StyledLine]| {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.text.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (wide, hits) = app.cockpit_screen(120, 30, now, 1_000);
    assert_eq!(wide.len(), 30);
    assert!(!hits.is_empty());
    let shown = text(&wide);
    assert!(shown.contains("Agent cockpit"));
    assert!(shown.contains("2 need you"));
    assert!(shown.contains("Needs you"));
    assert!(shown.contains("Asking you, for"));
    let (narrow, _) = app.cockpit_screen(80, 30, now, 1_000);
    let shown = text(&narrow);
    assert!(!shown.contains("2 need you"));
    assert!(
        shown.contains("Asking you, for"),
        "the chosen row grows in place"
    );
}

/// 槽的提示的数（§3.4）：按急的次序第一种不为 0 的；Working 与 Resting 不算；不数现在的会话；`kind_from` 同 `kind_of`。
#[test]
fn the_slot_counts_the_most_urgent_news_of_other_agents() {
    let kinds = |list: &[(&str, Kind)]| -> Vec<(String, Kind)> {
        list.iter().map(|(id, k)| ((*id).to_owned(), *k)).collect()
    };
    assert_eq!(news_of(&[], Some("current")), None);
    let quiet = kinds(&[("a", Kind::Work), ("b", Kind::Rest)]);
    assert_eq!(news_of(&quiet, Some("current")), None);
    let mixed = kinds(&[
        ("a", Kind::New),
        ("b", Kind::Failed),
        ("c", Kind::Need),
        ("current", Kind::Need),
    ]);
    assert_eq!(news_of(&mixed, Some("current")), Some((Kind::Need, 1)));
    assert_eq!(news_of(&mixed, None), Some((Kind::Need, 2)));
    let done = kinds(&[("a", Kind::New), ("b", Kind::Failed), ("c", Kind::New)]);
    assert_eq!(news_of(&done, Some("current")), Some((Kind::Failed, 1)));
    let fresh = kinds(&[("a", Kind::New), ("current", Kind::Failed)]);
    assert_eq!(news_of(&fresh, Some("current")), Some((Kind::New, 1)));
    // 笔记：结束得比看过的晚才算新；失败的是 Failed。
    let note = |ended: Option<u64>, seen: Option<u64>, failed: bool| CockpitNote {
        ended_at_ms: ended,
        failed: failed.then(|| "boom".to_owned()),
        seen_at_ms: seen,
    };
    assert_eq!(kind_from(None, &note(Some(5), Some(3), false)), Kind::New);
    assert_eq!(kind_from(None, &note(Some(5), Some(5), false)), Kind::Rest);
    assert_eq!(kind_from(None, &note(Some(5), None, true)), Kind::Failed);
    assert_eq!(
        kind_from(
            Some(SessionActivity::NeedsInput),
            &note(Some(5), None, true)
        ),
        Kind::Need
    );
}
