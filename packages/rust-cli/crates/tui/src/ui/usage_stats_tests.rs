//! Stats 标签页的派生值与排版（usage.md §5）。
use ratatui::text::Line;
use rust_cli_base::models::{GLM_4_6, GLM_5_3};
use rust_cli_kernel_api::metering::{ActivityDay, ActivityRange, ActivityStats, ModelTokens};

use super::*;
use crate::ui::palette::{ColorLevel, Palette};

const DAY: u64 = 86_400_000;

fn palette() -> Palette {
    Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)))
}

fn plain(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .map(|line| line.trim_end().to_owned())
        .collect()
}

fn day(date: &str, messages: u64) -> ActivityDay {
    ActivityDay {
        date: date.into(),
        messages,
    }
}

fn model(name: &str, input: u64, output: u64) -> ModelTokens {
    ModelTokens {
        model: name.into(),
        input,
        output,
        cache_read: 10,
        cache_write: 0,
    }
}

/// 今天是 2026-10-18（周日）；四个活跃日，其中 10-17、10-18 连续。
fn stats() -> ActivityStats {
    let first = day_number("2026-10-08").unwrap() as u64 * DAY;
    ActivityStats {
        range: Some(ActivityRange::All),
        today: "2026-10-18".into(),
        days: vec![
            day("2026-10-08", 3),
            day("2026-10-12", 9),
            day("2026-10-17", 1),
            day("2026-10-18", 2),
        ],
        models: vec![
            model(GLM_5_3, 1_000_000, 200_000),
            model(GLM_4_6, 1_000, 500),
        ],
        sessions: 2,
        longest_session_ms: Some(2 * DAY + 60_000),
        first_session_ms: Some(first),
        last_session_ms: Some(first + 10 * DAY),
    }
}

#[test]
fn derived_values_come_from_the_raw_stats() {
    let derived = derive(&stats());
    assert_eq!(
        derived,
        Derived {
            total_days: 11,
            active_days: 4,
            current_streak: 2,
            longest_streak: 2,
            peak_day: Some("2026-10-12".into()),
        }
    );
    // 今天没有消息：当前连续是 0。
    let mut quiet = stats();
    quiet.today = "2026-10-19".into();
    assert_eq!(derive(&quiet).current_streak, 0);
    assert_eq!(derive(&ActivityStats::default()).total_days, 0);
}

#[test]
fn the_heatmap_has_weekday_rows_and_ends_on_today() {
    let s = stats();
    let lines = plain(&heatmap(&s.days, &s.today, 80, &palette(), Lang::En));
    // 月份行、7 行星期、空行、图例。
    assert_eq!(lines.len(), 10);
    assert!(lines[0].starts_with("    "), "{lines:?}");
    assert!(lines[0].contains("Oct"), "{lines:?}");
    assert!(lines[2].starts_with("Mon "), "{lines:?}");
    assert!(lines[1].starts_with("    "), "{lines:?}");
    assert_eq!(
        lines[9],
        "    Less \u{2591} \u{2592} \u{2593} \u{2588} More"
    );
    // 52 周：今天是周日，在最后一列的第一行；之后的日子留空。
    let sunday: Vec<char> = lines[1].chars().skip(4).collect();
    assert_eq!(sunday.len(), 52);
    // 2 条消息：分位是 [1, 2, 3, 9] 的 p25 = 2、p50 = 3、p75 = 9 → 第 2 级。
    assert_eq!(sunday[51], '\u{2592}');
    let saturday: Vec<char> = lines[7].chars().skip(4).collect();
    // 10-17（周六）1 条消息：第 1 级。
    assert_eq!(saturday[50], '\u{2591}');
    assert_eq!(saturday.len(), 51, "{:?}", lines[7]);
    // 窄屏至少 10 周。
    let narrow = plain(&heatmap(&s.days, &s.today, 8, &palette(), Lang::En));
    assert_eq!(narrow[1].chars().skip(4).count(), 10);
}

#[test]
fn the_overview_uses_fixed_labels_and_columns() {
    let s = stats();
    let lines = plain(&overview(
        &s,
        &s,
        ActivityRange::All,
        false,
        80,
        &palette(),
        Lang::En,
    ));
    let body = &lines[11..];
    assert_eq!(body[0], "All time \u{b7} Last 7 days \u{b7} Last 30 days");
    // 两列各宽 28、间隔 4：右列从第 32 列开始。
    let two = |left: &str, right: &str| format!("{left:<32}{right}");
    assert_eq!(
        body[2],
        two(&format!("Favorite model: {GLM_5_3}"), "Total tokens: 1.2m")
    );
    assert_eq!(body[4], two("Sessions: 2", "Longest session: 2d 0h 1m"));
    assert_eq!(body[5], two("Active days: 4/11", "Longest streak: 2 days"));
    assert_eq!(
        body[6],
        two("Most active day: Oct 12", "Current streak: 2 days")
    );
    assert_eq!(
        body[7],
        "Input 1.0m \u{b7} Output 200.5k \u{b7} Cache read 20 \u{b7} Cache write 0"
    );
    // 1 001 000 + 200 500 个输入与输出：比战争与和平（730k）多一倍以上的那些书里取一条。
    assert!(body[9].starts_with("Your "), "{body:?}");
}

#[test]
fn a_week_divides_active_days_by_seven_and_keeps_the_all_time_streak() {
    let all = stats();
    let mut week = stats();
    week.range = Some(ActivityRange::Week);
    week.days = vec![day("2026-10-12", 9)];
    let lines = plain(&overview(
        &week,
        &all,
        ActivityRange::Week,
        true,
        80,
        &palette(),
        Lang::En,
    ));
    let text = lines.join("\n");
    assert!(text.contains("Active days: 1/7"), "{text}");
    assert!(text.contains("Longest streak: 1 day"), "{text}");
    // 当前连续用全时段。
    assert!(text.contains("Current streak: 2 days"), "{text}");
    assert!(text.contains("Last 30 days \u{2026}"), "{text}");
}

#[test]
fn chinese_labels_keep_the_layout() {
    let s = stats();
    let lines = plain(&overview(
        &s,
        &s,
        ActivityRange::All,
        false,
        80,
        &palette(),
        Lang::Zh,
    ));
    let text = lines.join("\n");
    assert!(
        text.contains(&format!("最常用的模型： {GLM_5_3}")),
        "{text}"
    );
    assert!(text.contains("最活跃的一天： 10月12日"), "{text}");
    assert!(text.contains("当前连续： 2 天"), "{text}");
    assert!(lines[0].contains("10月"), "{text}");
}

#[test]
fn fun_facts_compare_tokens_and_the_longest_session() {
    let s = stats();
    let fact = fun_fact(&s, Lang::En).unwrap();
    assert!(
        fact.starts_with("Your input and output are ~")
            || fact.starts_with("Your longest session is ~"),
        "{fact}"
    );
    // 同一天同一份数据选同一条（不随机，usage.md D12）。
    assert_eq!(fun_fact(&s, Lang::En), Some(fact));
    let mut small = ActivityStats {
        today: "2026-10-18".into(),
        models: vec![model("m", 20_000, 3_000)],
        ..ActivityStats::default()
    };
    assert_eq!(
        fun_fact(&small, Lang::En).as_deref(),
        Some("Your input and output are about as many tokens as The Little Prince")
    );
    assert_eq!(
        fun_fact(&small, Lang::Zh).as_deref(),
        Some("你的输入与输出和《小王子》的 token 数差不多")
    );
    small.models.clear();
    assert_eq!(fun_fact(&small, Lang::En), None);
}

#[test]
fn models_show_four_per_page_in_two_columns() {
    let mut s = stats();
    s.models = (0..6)
        .map(|i| model(&format!("model-{i}"), 600 - i * 100, 0))
        .collect();
    let lines = plain(&models(
        &s,
        ActivityRange::All,
        false,
        0,
        &palette(),
        Lang::En,
    ));
    assert_eq!(lines[0], "All time \u{b7} Last 7 days \u{b7} Last 30 days");
    assert!(lines[2].starts_with("\u{2022} model-0 ("), "{lines:?}");
    assert!(lines[2].contains("\u{2022} model-2 ("), "{lines:?}");
    // 左列宽 36、间隔 4。
    assert_eq!(
        lines[3],
        format!(
            "{:<40}{}",
            "  In: 600 \u{b7} Out: 0", "  In: 400 \u{b7} Out: 0"
        )
    );
    assert!(
        lines
            .last()
            .unwrap()
            .ends_with("\u{2193} 1-4 of 6 models (\u{2191}/\u{2193} to scroll)"),
        "{lines:?}"
    );
    let scrolled = plain(&models(
        &s,
        ActivityRange::All,
        false,
        2,
        &palette(),
        Lang::En,
    ));
    assert!(
        scrolled[2].starts_with("\u{2022} model-2 ("),
        "{scrolled:?}"
    );
    assert!(
        scrolled.last().unwrap().starts_with("\u{2191}   3-6 of 6"),
        "{scrolled:?}"
    );
    s.models.clear();
    assert_eq!(
        plain(&models(
            &s,
            ActivityRange::All,
            false,
            0,
            &palette(),
            Lang::En
        )),
        ["No model usage data available"]
    );
}
