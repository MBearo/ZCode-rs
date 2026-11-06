//! 输入框上方的列表、命令架、栈、状态栏与 `/stats` 的卡（docs/specs/rust-cli/tui-kit.md §4.4 K4）：每个从 Puffin 搬来的画法
//! 一个有代表性的中性输入，铺出来看行。Puffin 的 picker、modes、resume、rewind、key_hints、status_line、stats 的测试经它的
//! 映射测同样的规则；时间的说法与改动的纯用例随画法从 Puffin 搬来。
use crate::app::element::{Element, stack};
use crate::app::layout::Layout;
use crate::app::widgets::key_hints::{self, Slot};
use crate::app::widgets::model_picker::{self, Model, Picker};
use crate::app::widgets::modes::{self, Mode};
use crate::app::widgets::resume_list::{self, Changes, Mark, Session};
use crate::app::widgets::rewind_rows::{self, Point, PointFrom};
use crate::app::widgets::stats::{self, Ended, Usage, Window};
use crate::app::widgets::status_bar::{self, Left};
use crate::app::widgets::{export_list, lists, queue_stack, shelf, theme_list, when};
use crate::render::{Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

const NOW: f64 = 1_792_080_000_000.0;
const HOUR: f64 = 3_600_000.0;

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn utc(_: f64) -> i32 {
    0
}

fn lines(rows: Vec<Element>, w: usize) -> Vec<String> {
    Layout::lay(&stack(rows), w)
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|sp| sp.text.as_ref()).collect()
}

#[test]
fn a_window_follows_the_choice_and_the_filter_lights_what_matched() {
    // 一端有藏起的行时它占一行 more：显示 rows - 1 行。
    assert_eq!(lists::window(20, 0, 5), (0, 4));
    assert_eq!(lists::window(20, 19, 5), (16, 20));
    assert_eq!(lists::window(3, 2, 5), (0, 3));
    let t = rich();
    let lit = lists::matched(&t, "glm-5", "5");
    assert_eq!(text(&lit), "glm-5");
    assert!(
        lit.iter()
            .any(|s| s.text == "5" && s.style == Style::DEFAULT)
    );
}

#[test]
fn the_shelf_lists_commands_a_greyed_one_says_why() {
    assert_eq!(shelf::word("/re"), Some("re"));
    assert!(shelf::matches("quit", &[("exit", "quit")], "ex"));
    assert!(!shelf::matches("quit", &[("exit", "quit")], "zz"));
    let rows = [
        shelf::ShelfRow {
            name: "resume",
            description: "Pick up an earlier session",
            unavailable: None,
        },
        shelf::ShelfRow {
            name: "rewind",
            description: "Go back to a message",
            unavailable: Some("Nothing to go back to yet"),
        },
    ];
    let got = lines(shelf::rows(&rich(), &rows, 0), 100);
    assert!(got[0].contains("/resume") && got[0].contains("Pick up an earlier session"));
    assert!(got.iter().any(|l| l.contains("Nothing to go back to yet")));
}

#[test]
fn the_stack_shows_the_queued_messages_under_its_label() {
    let got = lines(queue_stack::rows(&rich(), &["first", "second"], 80), 80);
    assert!(got.iter().any(|l| l.contains(queue_stack::STACK_LABEL)));
    assert!(got.iter().any(|l| l.contains("first")));
    assert!(got.iter().any(|l| l.contains("second")));
}

#[test]
fn the_model_picker_lists_models_by_provider_then_the_efforts() {
    let models = [
        Model {
            provider_id: "zai",
            provider_name: "Z.ai",
            model_id: "glm-5",
            current: true,
        },
        Model {
            provider_id: "zai",
            provider_name: "Z.ai",
            model_id: "glm-4.6",
            current: false,
        },
    ];
    let efforts = ["low".to_owned(), "high".to_owned()];
    let p = Picker {
        any: true,
        widest_provider: 4,
        matches: &models,
        open: true,
        choice: 0,
        filter: "",
        efforts: &efforts,
        level: Some(1),
        named_age: 100,
    };
    let got = lines(model_picker::rows(&rich(), &p, 100), 100);
    assert!(
        got.iter()
            .any(|l| l.contains("glm-5") && l.contains("Z.ai"))
    );
    assert!(got.iter().any(|l| l.contains("glm-4.6")));
    assert!(model_picker::is_off("none") && !model_picker::is_off("high"));
}

#[test]
fn the_mode_list_marks_the_current_mode() {
    assert_eq!(modes::label(Mode::Yolo), "Full access");
    let got = lines(
        modes::list_rows(&rich(), 0, Some((Mode::Build, false))),
        100,
    );
    assert_eq!(got.len(), modes::MODE_ROWS.len());
    assert!(got.iter().any(|l| l.contains("Full access")));
}

#[test]
fn the_theme_list_filters_by_name() {
    let all = theme_list::matches("");
    assert!(all.len() > 1);
    let first = all[0];
    let got = lines(theme_list::rows(&rich(), Some(first), first, ""), 100);
    assert!(got.iter().any(|l| l.contains(first)));
    assert!(theme_list::matches("zzzz-no-theme").is_empty());
}

#[test]
fn the_export_list_has_its_rows() {
    let got = lines(export_list::rows(&rich(), 0), 100);
    assert_eq!(got.len(), export_list::EXPORT_ROWS.len());
}

#[test]
fn ages_days_for_conversations_a_clock_in_the_preview() {
    use crate::app::widgets::when::{day_age, day_age_long};
    assert_eq!(day_age(NOW, NOW - HOUR, &utc), "today");
    assert_eq!(day_age(NOW, NOW - 20.0 * HOUR, &utc), "yesterday");
    assert_eq!(day_age(NOW, NOW - 72.0 * HOUR, &utc), "3 days");
    assert_eq!(day_age(NOW, NOW - 70.0 * 24.0 * HOUR, &utc), "2 months");
    assert_eq!(day_age_long(NOW, 1_792_055_100_000.0, &utc), "today, 09:05");
    assert_eq!(day_age_long(NOW, NOW - 72.0 * HOUR, &utc), "3 days ago");
    assert_eq!(when::hh_mm(1_792_055_100_000.0, &utc), "09:05");
}

#[test]
fn the_resume_list_shows_each_session_with_its_age_and_changes() {
    let s = Session {
        title: "Fix the login".to_owned(),
        created_at: NOW - 3.0 * HOUR,
        updated_at: NOW - HOUR,
        model: "glm-5",
        effort: "high",
        mark: Mark::Unread,
        changes: Some(Changes {
            added: 12,
            removed: 3,
            files: 2,
        }),
        snippet: None,
        folder: None,
        current: false,
        dash: None,
    };
    let matches = [s];
    let list = resume_list::List {
        earlier: true,
        matches: &matches,
        choice: 0,
        query: "",
        named_age: 100,
    };
    let got = lines(resume_list::rows(&rich(), &list, 100, NOW, &utc), 100);
    assert!(got.iter().any(|l| l.contains("Fix the login")));
    let none = resume_list::List {
        earlier: false,
        ..list
    };
    assert_eq!(
        lines(resume_list::rows(&rich(), &none, 100, NOW, &utc), 100),
        ["   No earlier sessions in this folder."]
    );
}

fn points() -> Vec<Point> {
    vec![
        Point {
            at: NOW as u64 - 3_600_000,
            from: PointFrom::Start,
            added: 0,
            removed: 0,
        },
        Point {
            at: NOW as u64 - 1_800_000,
            from: PointFrom::Answer,
            added: 27,
            removed: 2,
        },
    ]
}

#[test]
fn rewind_lists_your_messages_newest_first() {
    assert_eq!(rewind_rows::ordinal(22), "22nd");
    assert_eq!(rewind_rows::where_(&points(), 0), "the start");
    let texts = ["Plan the trip", "Book a hotel in Tokyo"];
    let words = |k: usize| texts[k].to_owned();
    assert_eq!(rewind_rows::matches(&points(), &words, "tokyo"), vec![1]);
    let ui = rewind_rows::List {
        query: "",
        choice: 0,
    };
    let got = lines(
        rewind_rows::list_rows(&rich(), &points(), &words, &ui, NOW, &utc),
        100,
    );
    let tokyo = got.iter().position(|l| l.contains("Tokyo"));
    let trip = got.iter().position(|l| l.contains("Plan the trip"));
    assert!(tokyo.is_some() && trip.is_some() && tokyo < trip);
}

#[test]
fn change_shows_only_the_parts_there_are() {
    let theme = rich();
    let t = |a, r| text(&rewind_rows::change(&theme, a, r));
    assert_eq!(t(27, 2), "+27 −2");
    assert_eq!(t(5, 0), "+5");
    assert_eq!(t(0, 3), "−3");
    assert_eq!(t(0, 0), "");
}

#[test]
fn the_slot_says_its_keys_and_the_sheet_lists_the_groups() {
    let t = rich();
    let slot = Slot::keys(&[("⏎", "send"), ("esc", "close")]);
    let said = text(&key_hints::slot_spans(&t, &slot, 100));
    assert!(said.contains("send") && said.contains("close"));
    let sheet = lines(
        key_hints::sheet(
            &t,
            key_hints::newline_key(false),
            key_hints::paste_key(),
            120,
        ),
        120,
    );
    assert!(sheet.iter().any(|l| l.contains("new line")));
}

#[test]
fn the_status_bar_drops_the_lowest_ranks_first_when_narrow() {
    let t = rich();
    let left = status_bar::left(
        &t,
        &Left {
            mode: Some((Mode::Build, false, 100)),
            goal: None,
            model: Some(("glm-5", "high", 100)),
            cwd: "~/project",
            branch: Some(("main", 100)),
            machines: Some("@box"),
        },
    );
    let wide = status_bar::rows(&left, &[], &[], 140, 40, t.line);
    let w = text(&wide.concat());
    assert!(w.contains("glm-5") && w.contains("~/project") && w.contains("main"));
    assert!(w.contains("@box"));
    let narrow = text(&status_bar::rows(&left, &[], &[], 30, 40, t.line).concat());
    assert!(!narrow.contains("@box"));
}

#[test]
fn the_stats_card_counts_the_session_and_the_context_meter_fills() {
    let t = rich();
    let window = Window {
        used: 124_000,
        max: 200_000,
        compact_at: None,
        breakdown: Vec::new(),
    };
    let parts = stats::parts(&t, Some(&window), &|_| 100, Vec::new());
    let shown: String = parts.iter().map(|p| text(&p.spans)).collect();
    assert!(shown.contains("62%"), "{shown}");
    let u = Usage {
        context: Some(window),
        cache: None,
        input: 1200,
        output: 300,
        last_speed: None,
        speeds: Vec::new(),
        last_call_tokens: None,
        turns: 3,
        active_ms: 60_000,
        complete: true,
    };
    let ended = Ended {
        count: 0,
        tokens: 0.0,
        work_ms: 0.0,
        speeds: Vec::new(),
    };
    let got = lines(stats::card(&t, "This session", &u, &|_| 100, &ended), 100);
    assert!(got.iter().any(|l| l.contains("This session")));
}
