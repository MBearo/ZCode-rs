//! 目标的中性画法的用例（docs/specs/rust-cli/tui-kit.md §4.4 K5）：状态栏的一段、卡与它的键、用了多久的纯用例随画法从
//! Puffin 的 goals_tests.rs 搬来；检查行与恢复行按中性的 `Check` / `Resumed` 各一个代表的输入。Puffin 的 goals_tests.rs
//! 经条目映射与整屏测同样的规则。
use crate::app::layout::Layout;
use crate::render::line::StyledLine;
use crate::render::width::string_width;
use crate::render::{Style, attr};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::goal::{
    Check, GoalClock, GoalRound, GoalState, GoalVerdict, Resumed, SessionGoal, card, card_keys,
    check, enter_does, met_key, resumed, status_parts, time_used,
};

fn theme() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

/// 一行第 `col` 列的字符与样式。
fn cell(line: &StyledLine, col: usize) -> Option<(String, Style)> {
    let mut c = 0;
    for sp in &line.spans {
        for ch in sp.text.chars() {
            if c == col {
                return Some((ch.to_string(), sp.style));
            }
            c += string_width(&ch.to_string());
        }
    }
    None
}

fn round(n: u32, step: &str, verdict: GoalVerdict, done: u32, total: u32) -> GoalRound {
    GoalRound {
        round: n,
        step: step.to_owned(),
        verdict,
        done,
        total,
    }
}

fn goal(state: GoalState, round_n: u32, rounds: Vec<GoalRound>, reason: &str) -> SessionGoal {
    SessionGoal {
        target_id: "g-1".to_owned(),
        objective: "Make the footer links work on every page".to_owned(),
        title: "Footer links".to_owned(),
        status: String::new(),
        state,
        round: round_n,
        time_used_seconds: 468,
        running_since: 0,
        rounds,
        reason: reason.to_owned(),
    }
}

fn two_rounds() -> Vec<GoalRound> {
    vec![
        round(1, "Footer links", GoalVerdict::NotYet, 1, 2),
        round(2, "Fix the two links", GoalVerdict::None, 0, 0),
    ]
}

#[test]
fn a_not_yet_check_says_its_reason_and_the_next_step_on_the_rail() {
    let c = Check {
        round: 2,
        verdict: GoalVerdict::NotYet,
        title: "Footer links",
        reason: "Two links still 404.",
        next: "Fix the two links",
        time_used_seconds: 0,
    };
    let el = check(&theme(), &c, &GoalClock::default(), 0, 1000.0, None, false);
    assert_eq!(
        plain(&Layout::lay(&el, 80)),
        [
            " ◎ Goal  Footer links  not yet · round 2",
            "   │ Two links still 404.",
            "   │ Next  Fix the two links",
        ]
    );
    let r = Resumed {
        round: 3,
        title: "Footer links",
    };
    assert_eq!(
        plain(&Layout::lay(&resumed(&theme(), &r, i64::MAX / 2), 80)),
        [" ◎ Goal  Footer links  resumed · round 3"]
    );
}

#[test]
fn the_status_line_carries_one_mark_while_the_goal_runs_and_words_when_it_waits_for_you() {
    let t = theme();
    let parts =
        |state: GoalState, seen: bool| status_parts(&t, &goal(state, 2, two_rounds(), ""), seen);
    assert_eq!(
        parts(GoalState::Working, false),
        (vec![("◎".to_owned(), Style::fg(t.brand))], vec![])
    );
    assert_eq!(
        parts(GoalState::Paused, false),
        (
            vec![("◎".to_owned(), t.fg3)],
            vec![(" goal paused".to_owned(), t.fg3)]
        )
    );
    assert_eq!(
        parts(GoalState::Met, false).1,
        vec![(" goal met".to_owned(), t.fg2)]
    );
    assert_eq!(
        parts(GoalState::Met, true),
        (vec![], vec![]),
        "gone after the next send"
    );
    assert_eq!(met_key(&goal(GoalState::Met, 2, vec![], "")), "g-1#2");
}

// ---- 卡 ----

fn card_text(g: Option<&SessionGoal>, w: usize) -> Vec<String> {
    card(&theme(), g, 1000.0)
        .iter()
        .flat_map(|e| plain(&Layout::lay(e, w)))
        .collect()
}

#[test]
fn the_card_shows_the_objective_where_it_stands_and_a_row_per_round() {
    let g = goal(GoalState::Paused, 2, two_rounds(), "");
    assert_eq!(
        card_text(Some(&g), 80),
        vec![
            "   Goal   Make the footer links work on every page",
            "          round 2 · paused · 7m 48s",
            "",
            "    1  ◎  Footer links         1/2",
            "    2  ○  Fix the two links",
        ]
    );
    let line = &Layout::lay(&card(&theme(), Some(&g), 1000.0)[0], 80)[0];
    assert_eq!(
        cell(line, 3).map(|c| c.1),
        Some(theme().fg2.with_attr(attr::BOLD))
    );
}

#[test]
fn the_card_lists_at_most_eight_rounds_a_failed_check_says_its_error() {
    let many = (1..=10)
        .map(|i| {
            round(
                i,
                &format!("Step {i}"),
                if i < 10 {
                    GoalVerdict::NotYet
                } else {
                    GoalVerdict::Failed
                },
                0,
                0,
            )
        })
        .collect();
    let g = goal(GoalState::Failed, 10, many, "The checker timed out.");
    let t = card_text(Some(&g), 80);
    assert_eq!(t[1], "          round 10 · check failed · 7m 48s");
    let listed: Vec<&String> = t[3..].iter().take_while(|l| !l.is_empty()).collect();
    assert_eq!(listed[0], "   ↑ 3 earlier rounds");
    assert_eq!(
        listed[1..]
            .iter()
            .map(|l| l.trim().split(' ').next().unwrap_or("").to_owned())
            .collect::<Vec<_>>(),
        (4..=10).map(|i| i.to_string()).collect::<Vec<_>>()
    );
    assert_eq!(t[3 + listed.len() + 1], "   The checker timed out.");
}

#[test]
fn without_a_goal_the_card_says_how_to_set_one() {
    assert_eq!(
        card_text(None, 80),
        vec!["   No goal is set. /goal and what you want done sets one."]
    );
}

#[test]
fn enter_on_the_card_pauses_a_running_goal_and_resumes_a_waiting_one() {
    let g = |s: GoalState| goal(s, 2, vec![], "");
    assert_eq!(enter_does(Some(&g(GoalState::Working))), Some("pause"));
    assert_eq!(enter_does(Some(&g(GoalState::Checking))), Some("pause"));
    assert_eq!(enter_does(Some(&g(GoalState::Paused))), Some("resume"));
    assert_eq!(enter_does(Some(&g(GoalState::Met))), None);
    assert_eq!(enter_does(None), None);
    assert_eq!(
        card_keys(Some(&g(GoalState::Paused))),
        vec![
            ("⏎".to_owned(), "resume".to_owned()),
            ("esc".to_owned(), "close".to_owned())
        ]
    );
    assert_eq!(
        card_keys(None),
        vec![("esc".to_owned(), "close".to_owned())]
    );
}

#[test]
fn time_used_adds_the_stretch_that_is_running_now() {
    let mut g = goal(GoalState::Working, 2, vec![], "");
    assert_eq!(time_used(&g, 5000.0), 468_000.0);
    g.running_since = 2000;
    assert_eq!(time_used(&g, 5000.0), 471_000.0);
    assert_eq!(
        time_used(&g, 1000.0),
        468_000.0,
        "a clock behind the start adds nothing"
    );
}
