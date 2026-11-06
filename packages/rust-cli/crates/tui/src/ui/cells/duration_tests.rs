//! 回合结束那一行的排版（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）。
use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::turn_duration::TurnDuration;
use rust_cli_tui_kit::render::attr;

use super::*;
use crate::ui::cells::tests::ctx;
use crate::ui::i18n::Lang;
use crate::ui::lines::plain;
use crate::ui::probe::{ColorLevel, Colors};

/// 2026-10-30 04:52 UTC。
const AT: u64 = 1_793_335_920_000;

fn line(turn: &str, duration_ms: u64, waiting_agents: u32) -> TurnDuration {
    TurnDuration {
        turn: TurnId::new(turn),
        duration_ms,
        finished_at_ms: AT,
        utc_offset_minutes: 0,
        waiting_agents,
    }
}

fn shown(line: &TurnDuration, still: Option<&str>, lang: Lang, width: usize) -> Vec<String> {
    let colors = Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let ctx = Ctx {
        lang,
        width,
        ..ctx(&colors, false)
    };
    lines(line, still, &ctx).iter().map(plain).collect()
}

#[test]
fn the_verb_is_picked_by_the_string_hash_of_the_seed() {
    // 期望值与对齐对象的写法逐个核对过（`h = h·31 + 字符`，32 位回绕，取无符号后对 8 取余）。
    for (seed, verb_) in [
        ("", "Baked"),
        ("t", "Cooked"),
        ("abc", "Churned"),
        ("turn_1", "Worked"),
        ("turn_2", "Baked"),
        ("turn_3", "Brewed"),
        ("turn-1", "Brewed"),
        ("00000000-0000-4000-8000-000000000001", "Crunched"),
        ("中文", "Churned"),
    ] {
        assert_eq!(verb(seed), verb_, "{seed:?}");
    }
}

#[test]
fn the_done_time_is_local_in_twelve_or_twenty_four_hours() {
    let at = |hour: u64, minute: u64| (hour * 60 + minute) * 60_000 + 59_999;
    assert_eq!(clock(at(0, 0), 0, Lang::En), "12:00 AM");
    assert_eq!(clock(at(4, 52), 0, Lang::En), "4:52 AM");
    assert_eq!(clock(at(12, 5), 0, Lang::En), "12:05 PM");
    assert_eq!(clock(at(23, 59), 0, Lang::En), "11:59 PM");
    assert_eq!(clock(at(0, 0), 0, Lang::Zh), "0:00");
    assert_eq!(clock(at(16, 52), 0, Lang::Zh), "16:52");
    // 东八区跨过午夜，西五区退回前一天。
    assert_eq!(clock(at(20, 52), 480, Lang::Zh), "4:52");
    assert_eq!(clock(at(2, 0), -300, Lang::En), "9:00 PM");
}

/// 时刻的拆分收进 `ui::quantities` 之前的实现（逐字照抄）；两天里逐分钟、几个时区核对（constants.md §5.3）。
#[test]
fn the_done_time_matches_the_old_split() {
    fn old(at_ms: u64, offset_minutes: i32, lang: Lang) -> String {
        let minutes = i64::try_from(at_ms / 60_000).unwrap_or(i64::MAX) + i64::from(offset_minutes);
        let of_day = minutes.rem_euclid(24 * 60);
        let (hour, minute) = (of_day / 60, of_day % 60);
        match lang {
            Lang::En => {
                let half = if hour < 12 { "AM" } else { "PM" };
                let hour = match hour % 12 {
                    0 => 12,
                    hour => hour,
                };
                format!("{hour}:{minute:02} {half}")
            }
            Lang::Zh => format!("{hour}:{minute:02}"),
        }
    }
    for minute in 0..2 * 24 * 60 {
        for at in [AT + minute * 60_000, minute * 60_000 + 59_999] {
            for offset in [-720, -300, 0, 330, 345, 840] {
                for lang in [Lang::En, Lang::Zh] {
                    assert_eq!(
                        clock(at, offset, lang),
                        old(at, offset, lang),
                        "{at} {offset}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_finished_turn_reads_verb_duration_and_done_time() {
    assert_eq!(
        shown(&line("turn_2", 62_000, 0), None, Lang::En, 80),
        [" ✻ Baked for 1m 2s · done 4:52 AM"]
    );
    assert_eq!(
        shown(&line("turn_1", 400, 0), None, Lang::En, 80),
        [" ✻ Worked for 0s · done 4:52 AM"]
    );
    assert_eq!(
        shown(&line("turn_1", 3_723_000, 0), None, Lang::Zh, 80),
        [" ✻ 用时 1h 2m 3s · 完成于 4:52"]
    );
}

#[test]
fn running_background_commands_are_named_at_the_end() {
    assert_eq!(
        shown(&line("turn_3", 0, 0), Some("1 monitor"), Lang::En, 80),
        [" ✻ Brewed for 0s · done 4:52 AM · 1 monitor still running"]
    );
    assert_eq!(
        shown(
            &line("turn_3", 0, 0),
            Some("1 shell, 2 monitors"),
            Lang::En,
            80
        ),
        [" ✻ Brewed for 0s · done 4:52 AM · 1 shell, 2 monitors still running"]
    );
    assert_eq!(
        shown(
            &line("turn_3", 0, 0),
            Some("1 shell、2 monitor"),
            Lang::Zh,
            80
        ),
        [" ✻ 用时 0s · 完成于 4:52 · 1 shell、2 monitor 仍在运行"]
    );
}

#[test]
fn waiting_for_background_agents_replaces_the_line_with_a_bold_count() {
    assert_eq!(
        shown(&line("turn_1", 5_000, 1), Some("1 shell"), Lang::En, 80),
        [" ✻ Waiting for 1 background agent to finish"]
    );
    assert_eq!(
        shown(&line("turn_1", 5_000, 2), None, Lang::En, 80),
        [" ✻ Waiting for 2 background agents to finish"]
    );
    assert_eq!(
        shown(&line("turn_1", 5_000, 2), None, Lang::Zh, 80),
        [" ✻ 等待 2 个后台代理完成"]
    );
    let colors = Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let ctx = ctx(&colors, false);
    let rendered = lines(&line("turn_1", 0, 3), None, &ctx);
    let count = rendered[0]
        .spans
        .iter()
        .find(|span| span.text == "3")
        .unwrap();
    assert!(count.style.has_attr(attr::BOLD));
    assert_eq!(count.style.fg_color(), ctx.ink.theme.fg3.fg_color());
}

#[test]
fn a_narrow_terminal_wraps_under_the_text() {
    let wrapped = shown(&line("turn_3", 0, 0), Some("1 monitor"), Lang::En, 24);
    assert!(wrapped.len() > 1, "{wrapped:?}");
    assert!(wrapped[0].starts_with(" ✻ Brewed"));
    // 续行与首行的文字同一列（第 3 列），第 3 列是字不是空格。
    assert!(
        wrapped[1..]
            .iter()
            .all(|row| row.starts_with("   ") && !row.starts_with("    ")),
        "{wrapped:?}"
    );
    assert_eq!(
        wrapped
            .iter()
            .map(|row| row.trim_start_matches(" ✻ ").trim())
            .collect::<Vec<_>>()
            .join(" "),
        "Brewed for 0s · done 4:52 AM · 1 monitor still running"
    );
}

#[test]
fn the_line_wraps_at_the_body_width_on_a_wide_terminal() {
    let colors = Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let ctx = Ctx {
        lang: Lang::En,
        width: 200,
        ..ctx(&colors, false)
    };
    let still = "1 shell, ".repeat(20);
    let rendered = lines(&line("turn_3", 0, 0), Some(&still), &ctx);
    assert!(rendered.len() > 1);
    assert!(rendered.iter().all(|row| row.width() <= 99), "{rendered:?}");
}

/// 整行是主题的 fg3（kit 的 transcript 里安静的字；上游是 ratatui 的 DIM）：深浅主题各取各的 fg3。
#[test]
fn the_whole_line_is_quiet_in_both_themes() {
    for colors in [
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Colors::new(ColorLevel::TrueColor, Some((255, 255, 255))),
    ] {
        let ctx = ctx(&colors, false);
        let rendered = lines(&line("turn_3", 0, 0), Some("1 shell"), &ctx);
        for span in rendered.iter().flat_map(|line| line.spans.iter()) {
            if span.text.trim().is_empty() {
                continue;
            }
            assert_eq!(span.style, ctx.ink.theme.fg3, "{span:?}");
        }
    }
}
