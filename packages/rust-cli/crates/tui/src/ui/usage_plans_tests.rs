//! `/usage` 的卡与详情页（tui-kit.md §4.2「`/usage`」）：问一次、回答回来、←→ 换计划、⏎ 进页、esc 回卡、页的滚动。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::app::widgets::usage::{
    DEFAULT_RANGE, Family, PlanKind, PlanQuota, UsageModel, UsagePlan, range,
};

use super::{PlanAnswer, PlanAsk};
use crate::ui::app::{Action, App, Overlay};
use crate::ui::cards::Card;
use crate::ui::i18n::Lang;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

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

fn plan(id: &str, kind: PlanKind) -> UsagePlan {
    UsagePlan {
        id: id.into(),
        family: Family::Zai,
        kind,
        name: String::new(),
    }
}

fn asks(action: Option<Action>) -> Vec<PlanAsk> {
    match action {
        Some(Action::PlanUsage(asks)) => asks,
        None => Vec::new(),
        Some(other) => panic!("not a usage ask: {other:?}"),
    }
}

fn card(app: &App) -> Card {
    match &app.overlay {
        Some(Overlay::Card(card)) => card.clone(),
        other => panic!("no card: {other:?}"),
    }
}

/// 答出两个计划：个人的与 Start Plan（Start Plan 没有详情页）。
fn answered(app: &mut App) -> Vec<PlanAsk> {
    let plans = vec![
        plan("zai", PlanKind::Individual),
        plan("start", PlanKind::Start),
    ];
    let model = UsageModel {
        provider_id: "fixture".into(),
        official: true,
        ..UsageModel::default()
    };
    asks(app.plan_answered(
        PlanAnswer::Plans {
            provider: "fixture".into(),
            result: Ok((model, plans)),
        },
        0,
    ))
}

fn slot_text(app: &App) -> String {
    let Card::Usage { entry } = card(app) else {
        panic!("not the usage card");
    };
    let (_, slot) = app.usage_card_layer(entry, 80, 0, 0);
    slot.iter().map(|s| s.text.as_ref()).collect()
}

#[test]
fn opening_asks_for_the_plans_once_and_the_answer_asks_for_the_quota() {
    let mut app = app();
    app.composer.set_text("draft");
    let first = asks(app.open_plan_usage());
    assert_eq!(
        first,
        [PlanAsk::Plans {
            provider: "fixture".into()
        }]
    );
    assert!(app.composer.is_empty(), "the draft is cleared");
    assert_eq!(card(&app), Card::Usage { entry: 0 });
    // 回答还没到：再开一次不再问。
    assert!(asks(app.open_plan_usage()).is_empty());
    assert_eq!(answered(&mut app), [PlanAsk::Quota { plan: "zai".into() }]);
}

#[test]
fn an_answer_for_another_provider_is_dropped() {
    let mut app = app();
    asks(app.open_plan_usage());
    let stale = app.plan_answered(
        PlanAnswer::Plans {
            provider: "other".into(),
            result: Ok((UsageModel::default(), vec![plan("x", PlanKind::Individual)])),
        },
        0,
    );
    assert!(stale.is_none());
    assert!(app.usage_plans.plans.is_empty());
}

#[test]
fn a_failed_read_offers_try_again_and_enter_asks_again() {
    let mut app = app();
    asks(app.open_plan_usage());
    app.plan_answered(
        PlanAnswer::Plans {
            provider: "fixture".into(),
            result: Err("offline".into()),
        },
        0,
    );
    assert!(slot_text(&app).contains("try again"), "{}", slot_text(&app));
    let again = app.usage_card_key(key(KeyCode::Enter), 0).expect("taken");
    assert_eq!(
        asks(again),
        [PlanAsk::Plans {
            provider: "fixture".into()
        }]
    );
}

#[test]
fn arrows_change_the_plan_and_ask_for_its_quota() {
    let mut app = app();
    asks(app.open_plan_usage());
    answered(&mut app);
    let next = app.usage_card_key(key(KeyCode::Right), 0).expect("taken");
    assert_eq!(
        asks(next),
        [PlanAsk::Quota {
            plan: "start".into()
        }]
    );
    assert_eq!(card(&app), Card::Usage { entry: 1 });
    let back = app.usage_card_key(key(KeyCode::Right), 1).expect("taken");
    // 第一个计划的额度还在问：不再问。
    assert!(asks(back).is_empty());
    assert_eq!(card(&app), Card::Usage { entry: 0 });
    // 打字不归卡：交下去（进草稿、关上卡）。
    assert!(app.usage_card_key(key(KeyCode::Char('a')), 0).is_none());
    assert!(app.usage_card_key(key(KeyCode::Esc), 0).is_some());
    assert!(app.overlay.is_none());
}

#[test]
fn enter_opens_the_page_esc_goes_back_and_start_plan_has_no_page() {
    let mut app = app();
    asks(app.open_plan_usage());
    answered(&mut app);
    app.plan_answered(
        PlanAnswer::Quota {
            plan: "zai".into(),
            result: Ok(PlanQuota::default()),
        },
        0,
    );
    let open = app.usage_card_key(key(KeyCode::Enter), 0).expect("taken");
    assert_eq!(
        asks(open),
        [PlanAsk::Detail {
            plan: "zai".into(),
            range: range(DEFAULT_RANGE).0,
        }]
    );
    assert_eq!(
        card(&app),
        Card::UsagePage {
            entry: 0,
            range: DEFAULT_RANGE,
            top: 0
        }
    );
    app.plan_answered(
        PlanAnswer::Detail {
            plan: "zai".into(),
            range: range(DEFAULT_RANGE).0,
            result: Ok(Box::default()),
        },
        0,
    );
    let page = app.usage_page_body(80, 0, 0).expect("the page");
    let slot: String = page.slot.unwrap().iter().map(|s| s.text.as_ref()).collect();
    assert!(slot.contains("range"), "{slot}");
    // 只有一个计划有详情页：槽里没有 ⇥ plan（同 Puffin）；⇥ 走过它到「This computer」（usage_local.rs）。
    assert!(!slot.contains("plan"), "{slot}");
    let local = app
        .usage_page_key(key(KeyCode::Tab), (0, DEFAULT_RANGE, 0))
        .expect("taken");
    assert!(
        matches!(
            local,
            Some(Action::Manage(crate::ui::manage::Manage::Usage))
        ),
        "{local:?}"
    );
    app.usage_page_key(key(KeyCode::Esc), (0, DEFAULT_RANGE, 0));
    assert_eq!(card(&app), Card::Usage { entry: 0 });
    // Start Plan 没有详情页：⏎ 留在卡上。
    app.overlay = Some(Overlay::Card(Card::Usage { entry: 1 }));
    let stay = app.usage_card_key(key(KeyCode::Enter), 1).expect("taken");
    assert!(asks(stay).is_empty());
    assert_eq!(card(&app), Card::Usage { entry: 1 });
}

#[test]
fn the_page_scrolls_within_its_last_frame() {
    let mut app = app();
    asks(app.open_plan_usage());
    answered(&mut app);
    // 上一帧：正文 10 行，页 25 行，最多滚到 15。
    app.page_rows = (10, 25);
    let mut at = (0, DEFAULT_RANGE, 0);
    app.overlay = Some(Overlay::Card(Card::UsagePage {
        entry: 0,
        range: DEFAULT_RANGE,
        top: 0,
    }));
    for (code, want) in [
        (KeyCode::Down, 1),
        (KeyCode::PageDown, 10),
        (KeyCode::PageDown, 15),
        (KeyCode::Up, 14),
        (KeyCode::PageUp, 5),
        (KeyCode::PageUp, 0),
    ] {
        app.usage_page_key(key(code), at);
        let Card::UsagePage { top, .. } = card(&app) else {
            panic!("left the page");
        };
        assert_eq!(top, want, "{code:?}");
        at.2 = top;
    }
}
