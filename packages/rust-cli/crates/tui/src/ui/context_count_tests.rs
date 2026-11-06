//! 上下文的计数与它的页（tui-panels.md §4.1、§4.3）：卡或页开着时才问，答案只收现在看的会话的，数变了的淡入（第一次的
//! 答案不淡），卡上 ⏎ 打开页、esc 回卡、打字关上两者，选中的行离页的边至少一行。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::StyledLine;

use super::*;
use crate::ui::context_tree::tests::sample;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::stats_page::Tab;
use crate::view::Transcript;

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
    app.session = Some("sess_main".into());
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn page_text(app: &App) -> Vec<String> {
    app.stats_page_body(100, 0)
        .expect("the page")
        .lines
        .iter()
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .collect()
}

fn page(app: &App) -> ContextPage {
    match &app.overlay {
        Some(Overlay::Card(Card::StatsPage(page))) => page.context.clone(),
        other => panic!("not the page: {other:?}"),
    }
}

/// 卡与页开着时才问，问的是主会话；卡的组成在答案到了之后才有（之前卡没有图例）。
#[test]
fn the_count_is_asked_while_the_card_or_page_is_open() {
    let mut app = app();
    assert_eq!(app.context_wanted(), None);
    assert!(matches!(app.open_stats(), Action::CountContext));
    assert_eq!(
        app.context_wanted(),
        Some(Some(SessionId::new("sess_main")))
    );
    assert!(app.context_breakdown().is_empty());
    let stale = app.context_counted(
        Some(SessionId::new("sess_main")),
        Ok(sample()),
        Instant::now(),
        0,
    );
    assert!(!stale);
    assert_eq!(app.context_breakdown().len(), 8);
    app.overlay = None;
    assert_eq!(app.context_wanted(), None);
}

/// 答案问的不是现在看的会话（切了会话）：丢掉，要 runner 再问。
#[test]
fn an_answer_for_another_session_is_dropped() {
    let mut app = app();
    let _ = app.open_stats();
    let stale = app.context_counted(
        Some(SessionId::new("sess_old")),
        Ok(sample()),
        Instant::now(),
        0,
    );
    assert!(stale);
    assert!(app.context_now().is_none());
}

/// 第一次的答案不淡；之后变了的数（按路径，图例按组）淡入，没变的不淡；淡完了不再重画。
#[test]
fn changed_numbers_fade_but_the_first_answer_does_not() {
    let mut app = app();
    let _ = app.open_stats();
    let session = Some(SessionId::new("sess_main"));
    app.context_counted(session.clone(), Ok(sample()), Instant::now(), 0);
    assert!(app.context_count.changed.is_empty());
    let mut next = sample();
    for c in &mut next.categories {
        if c.name == "Messages" {
            c.tokens += 1_000;
        }
    }
    next.messages[1].tokens += 1_000;
    app.context_counted(session, Ok(next), Instant::now(), 10);
    let keys: Vec<&str> = app
        .context_count
        .changed
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["legend/messages", "messages", "messages/Replies"]);
    let _ = app.open_stats_page(Tab::Context);
    assert!(app.cards_fading());
}

/// 失败时页上代替 `Counting…` 写原因；答案到之前是卡的 Context 行与 `Counting…`。
#[test]
fn the_page_counts_then_fails_or_shows_the_answer() {
    let mut app = app();
    let _ = app.open_stats();
    app.card_key(key(KeyCode::Enter));
    let text = page_text(&app);
    assert!(text[1].contains("Stats  ›  Context"), "{text:#?}");
    assert!(
        text.iter()
            .any(|l| l == &format!("{}Counting…", " ".repeat(14))),
        "{text:#?}"
    );
    app.context_counted(
        Some(SessionId::new("sess_main")),
        Err("the kernel is busy".into()),
        Instant::now(),
        0,
    );
    assert!(
        page_text(&app)
            .iter()
            .any(|l| l.contains("✗ the kernel is busy"))
    );
    app.context_counted(
        Some(SessionId::new("sess_main")),
        Ok(sample()),
        Instant::now(),
        0,
    );
    let text = page_text(&app);
    assert!(
        text.iter().any(|l| l.contains("126k of 200k   62%")),
        "{text:#?}"
    );
}

/// 卡的槽是 `⏎ context`；页上的槽按选中的行：能开的 `⏎ expand`，开着的 `⏎ collapse`；esc 回卡，打字关上两者。
#[test]
fn enter_opens_the_page_and_esc_goes_back_to_the_card() {
    let mut app = app();
    let _ = app.open_stats();
    let slot: String = app
        .card_layer(100, 0, 0)
        .expect("the card")
        .1
        .iter()
        .map(|s| s.text.as_ref())
        .collect();
    assert!(slot.contains("context"), "{slot}");
    app.context_counted(
        Some(SessionId::new("sess_main")),
        Ok(sample()),
        Instant::now(),
        0,
    );
    app.card_key(key(KeyCode::Enter));
    let slot = |app: &App| -> String {
        app.stats_page_body(100, 0)
            .and_then(|b| b.slot)
            .unwrap_or_default()
            .iter()
            .map(|s| s.text.as_ref())
            .collect()
    };
    assert!(slot(&app).contains("expand"), "{}", slot(&app));
    app.card_key(key(KeyCode::Right));
    assert!(slot(&app).contains("collapse"), "{}", slot(&app));
    assert_eq!(page(&app).open.len(), 1);
    app.card_key(key(KeyCode::Esc));
    assert!(matches!(app.overlay, Some(Overlay::Card(Card::Stats(_)))));
    app.card_key(key(KeyCode::Enter));
    assert!(page(&app).open.is_empty(), "the page opens closed");
    // 页不拿字：键去草稿，打了字卡与页一起关上。
    assert!(app.card_key(key(KeyCode::Char('x'))).is_none());
    app.composer.set_text("x");
    app.settle_card();
    assert!(app.overlay.is_none());
}

/// 页跟着选中滚：选中的行离一屏的上下边至少一行，夹到最后一屏。
#[test]
fn the_page_follows_the_choice_with_a_margin() {
    assert_eq!(follow(0, 5, 10, 40), 0);
    assert_eq!(follow(0, 9, 10, 40), 1);
    assert_eq!(follow(10, 10, 10, 40), 9);
    assert_eq!(follow(0, 39, 10, 40), 30);
    assert_eq!(follow(20, 0, 10, 40), 0);
}
