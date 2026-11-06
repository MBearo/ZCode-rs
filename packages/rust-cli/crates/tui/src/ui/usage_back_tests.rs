//! 失败回合的「回来的时刻」（Puffin `ask_back`、`usage_back_line`）：只问刚到的、被套餐的上限停下的失败；回答到了块多一行，
//! 从到达的 tick 起淡入；读不到时不多。
use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::Span;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn failed_app(message: &str) -> App {
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
    let turn = TurnId::new("t1");
    for (seq, event) in [
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
        },
        KernelEvent::TurnFinished {
            turn,
            outcome: TurnOutcome::Failed {
                message: message.to_owned(),
            },
            usage: None,
        },
    ]
    .into_iter()
    .enumerate()
    {
        app.view.apply(
            SequencedEvent {
                seq: seq as u64 + 1,
                revision: 0,
                event,
            },
            1_000,
        );
    }
    app
}

fn failure(app: &App) -> (u64, crate::view::Failure) {
    let row = app.view.rows().last().expect("a row");
    let Body::Failure(f) = &row.body else {
        panic!("not a failure: {:?}", row.body);
    };
    (row.id, (**f).clone())
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|s| &*s.text).collect()
}

/// 可以再试的限流不是套餐的上限：不问（Puffin `failures::capped`）。
#[test]
fn a_retryable_rate_limit_asks_nothing() {
    let mut app = failed_app("rate limited: slow down");
    assert!(app.ask_back().is_none());
}

/// 上限说了什么时候回来：块多一行，从到达的 tick 起淡入；没说时不多。
#[test]
fn the_answer_adds_the_comes_back_line_and_fades_it_in() {
    let mut app = failed_app("boom");
    let (row, f) = failure(&app);
    assert!(app.usage_back_line(row, &f).is_none(), "nothing asked yet");
    let at = f.at as f64 + 2.0 * 3_600_000.0;
    app.usage_back(row, Some(("weekly".to_owned(), at)));
    let (spans, age) = app.usage_back_line(row, &f).expect("the line");
    assert!(text(&spans).starts_with("The weekly limit comes back at "));
    assert_eq!(age, 0);
    assert!(app.usage_back_moving());
    app.usage_back(row, None);
    assert!(app.usage_back_line(row, &f).is_none());
    assert!(!app.usage_back_moving());
}
