//! `/usage` 的卡与详情页的中性画法的用例（docs/specs/rust-cli/tui-kit.md §4.4 K5）：措辞与本地日历的纯用例随画法从 Puffin
//! 的 usage/tests/pages.rs 搬来；卡与页各一个代表的输入。Puffin 的 usage/tests*.rs 经 `UsageUi` 与整屏测同样的规则。
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::app::element::Element;
use crate::app::layout::Layout;
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::calendar::{self, DAY_MS, HOUR_MS, MINUTE_MS};
use super::{
    Entry, Family, PlanKind, PlanQuota, PoolKind, QuotaState, Seen, Usage, UsagePlan, UsagePool,
    UtcOffset, page, view,
};

/// Puffin 的 rig 的墙钟：2026-10-22 16:00 UTC。
const NOW: f64 = 1_792_684_800_000.0;

fn utc() -> UtcOffset {
    Arc::new(|_| 0)
}

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn text(els: &[Element], w: usize) -> Vec<String> {
    els.iter()
        .flat_map(|e| Layout::lay(e, w))
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn zai() -> UsagePlan {
    UsagePlan {
        id: "p-zai".to_owned(),
        family: Family::Zai,
        kind: PlanKind::Individual,
        name: String::new(),
    }
}

fn quota() -> PlanQuota {
    PlanQuota {
        at: NOW as u64,
        state: QuotaState::Ok,
        level: "Pro".to_owned(),
        since: None,
        renews: None,
        ends: None,
        pools: vec![UsagePool {
            pool: PoolKind::FiveHour,
            left: 72.0,
            count: None,
            reset_at: Some((NOW + 2.0 * HOUR_MS + 14.0 * MINUTE_MS) as u64),
            parts: Vec::new(),
        }],
    }
}

#[test]
fn the_clock_of_the_tests_is_the_rig_s() {
    assert_eq!(
        calendar::from_local(2026, 10, 22, 16.0 * HOUR_MS, &utc()),
        NOW
    );
}

#[test]
fn the_card_has_the_plan_on_its_head_and_a_row_per_pool() {
    let plans = vec![zai()];
    let quotas = BTreeMap::from([("p-zai".to_owned(), Seen::default().got(quota(), 0))]);
    let details = BTreeMap::new();
    let usage = Usage {
        plans: &plans,
        plans_failed: None,
        quotas: &quotas,
        details: &details,
    };
    let entry = Entry::Plan(zai());
    let rows = text(
        &view::card(&rich(), &usage, Some(&entry), "glm", NOW, &utc(), 100),
        80,
    );
    assert_eq!(rows[0], "   Usage   Z.ai Coding Plan · Pro");
    assert_eq!(rows[1], "");
    assert!(rows[2].starts_with("   5-hour   "), "{}", rows[2]);
    assert!(
        rows[2].ends_with("72% left   resets in 2h 14m"),
        "{}",
        rows[2]
    );
    // 读计划失败：头是空的，下面说为什么。
    let failed = Usage {
        plans_failed: Some("offline"),
        ..usage
    };
    assert_eq!(
        text(
            &view::card(&rich(), &failed, None, "glm", NOW, &utc(), 0),
            80
        )[2],
        "   ✗ Could not read your usage: offline"
    );
}

#[test]
fn the_page_has_the_crumbs_left_now_and_asks_for_the_history() {
    let plans = vec![zai()];
    let quotas = BTreeMap::from([("p-zai".to_owned(), Seen::default().got(quota(), 0))]);
    let details = BTreeMap::new();
    let usage = Usage {
        plans: &plans,
        plans_failed: None,
        quotas: &quotas,
        details: &details,
    };
    let rows = text(
        &page::page(&rich(), &usage, &zai(), 1, NOW, &utc(), 100),
        80,
    );
    assert_eq!(rows[1], "   Usage  ›  Z.ai Coding Plan · Pro");
    assert_eq!(rows[3], "   Left now");
    assert!(
        rows.iter().any(|r| r == "   ○ Asking z.ai for the history"),
        "{rows:#?}"
    );
}

#[test]
fn words_when_how_far_compact_numbers_commas() {
    let zone = utc();
    assert_eq!(view::when(NOW + 38.0 * MINUTE_MS, NOW, &zone), "in 38m");
    assert_eq!(view::when(NOW + 2.0 * HOUR_MS, NOW, &zone), "in 2h");
    assert_eq!(
        view::how_far(NOW + 3.0 * DAY_MS + HOUR_MS, NOW, &zone),
        "3 days"
    );
    assert_eq!(
        view::how_far(NOW + HOUR_MS + 12.0 * MINUTE_MS, NOW, &zone),
        "1h 12m"
    );
    let compact: Vec<String> = [940.0, 12340.0, 41.2e6, 127e6, 1.9e9]
        .into_iter()
        .map(view::compact)
        .collect();
    assert_eq!(compact, ["940", "12.3K", "41.2M", "127M", "1.9B"]);
    assert_eq!(view::commas(1206.0), "1,206");
    assert_eq!(view::commas(-1_234_567.0), "-1,234,567");
    assert_eq!(view::percent(99.6), 99);
}

#[test]
fn the_local_calendar_follows_the_utc_offset() {
    let utc = utc();
    let east: UtcOffset = Arc::new(|_| 480);
    // 2026-10-22 16:00 UTC 是周四；东八区已是周五零点。
    assert_eq!(view::weekday_time(NOW, &utc), "Thu 16:00");
    assert_eq!(view::weekday_time(NOW, &east), "Fri 00:00");
    assert_eq!(view::month_day(NOW, &east), "Oct 23");
    assert_eq!(
        calendar::from_local(2026, 10, 23, 0.0, &east),
        NOW,
        "local midnight back to the instant"
    );
    // Tool calls 的窗口是日历上的一个月：3 月 31 日往前一个月按 js.Date 进位到 3 月 3 日。
    let end = calendar::from_local(2026, 3, 31, 9.0 * HOUR_MS, &utc);
    let start = calendar::from_local(2026, 3, 3, 9.0 * HOUR_MS, &utc);
    assert_eq!(view::window(PoolKind::Tools, end, &utc), Some(end - start));
    assert_eq!(
        calendar::day_of("2026-10-22"),
        Some(((NOW - 16.0 * HOUR_MS) / DAY_MS) as i64)
    );
    assert_eq!(page::month_row("2025-10-26", 0, 10), "Nov  Dec");
}
