//! `/stats` 标签页的正文（docs/specs/rust-cli/usage.md §5）：数据是内核给出的原始聚合（[`ActivityStats`]），
//! 这里只算派生值并排版。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
// 一天的毫秒数只定义在 base（F58：以前这里与 node-store 各写一份）。
use rust_cli_base::js::date::DAY_MS;
use rust_cli_kernel_api::clock::civil_date;
use rust_cli_kernel_api::metering::{
    ActivityDay, ActivityRange, ActivityStats, ModelTokens, format_duration, format_number,
};
use unicode_width::UnicodeWidthStr;

use super::i18n::Lang;
use super::i18n_usage::{self as text, UsageStrings};
use super::palette::Palette;

/// 两列：各宽 28，间隔 4。
const COLUMN: usize = 28;
const GAP: usize = 4;
/// Models 视图每页 4 个模型、两列各宽 36。
pub const MODELS_PER_PAGE: usize = 4;
const MODEL_COLUMN: usize = 36;

/// `YYYY-MM-DD` → 自 1970-01-01 的天数（days_from_civil）。
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn date_of(day: i64) -> String {
    civil_date(u64::try_from(day).unwrap_or(0) * DAY_MS)
}

/// （月, 日）。
fn month_day(date: &str) -> (u32, u32) {
    let mut parts = date.splitn(3, '-').skip(1).map(|p| p.parse().unwrap_or(1));
    (parts.next().unwrap_or(1), parts.next().unwrap_or(1))
}

/// 派生值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub total_days: u64,
    pub active_days: u64,
    pub current_streak: u64,
    pub longest_streak: u64,
    pub peak_day: Option<String>,
}

pub fn derive(stats: &ActivityStats) -> Derived {
    let total_days = match (stats.first_session_ms, stats.last_session_ms) {
        (Some(first), Some(last)) => last.saturating_sub(first).div_ceil(DAY_MS) + 1,
        _ => 0,
    };
    let numbers: Vec<i64> = stats
        .days
        .iter()
        .filter_map(|d| day_number(&d.date))
        .collect();
    let set: std::collections::BTreeSet<i64> = numbers.iter().copied().collect();
    // 以今天为终点往前数（今天没有消息时是 0）。
    let mut current = 0;
    if let Some(mut day) = day_number(&stats.today) {
        while set.contains(&day) {
            current += 1;
            day -= 1;
        }
    }
    let (mut longest, mut run, mut previous) = (0, 0, None);
    for day in &set {
        run = if previous == Some(day - 1) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        previous = Some(*day);
    }
    // 消息最多的一天，相同时取较早的。
    let peak_day = stats
        .days
        .iter()
        .fold(None::<&ActivityDay>, |best, day| match best {
            Some(best) if day.messages <= best.messages => Some(best),
            _ => Some(day),
        })
        .map(|day| day.date.clone());
    Derived {
        total_days,
        active_days: stats.days.len() as u64,
        current_streak: current,
        longest_streak: longest,
        peak_day,
    }
}

/// 输入加输出。
fn input_output(models: &[ModelTokens]) -> u64 {
    models.iter().map(|m| m.input + m.output).sum()
}

/// 候选句里按 `today` 确定地取一条（不随机，usage.md D12）。
pub fn fun_fact(stats: &ActivityStats, lang: Lang) -> Option<String> {
    let mut candidates = Vec::new();
    let tokens = input_output(&stats.models);
    if tokens > 0 {
        for (en, zh, size) in text::BOOKS.iter().filter(|book| tokens >= book.2) {
            let times = tokens / size;
            candidates.push(if times >= 2 {
                text::tokens_times(lang, times, (en, zh))
            } else {
                text::tokens_about(lang, (en, zh))
            });
        }
    }
    if let Some(longest) = stats.longest_session_ms {
        for (en, zh, minutes) in text::ACTIVITIES {
            let times = longest / (minutes * 60_000);
            if times >= 2 {
                candidates.push(text::session_times(lang, times, (en, zh)));
            }
        }
    }
    let pick = usize::try_from(day_number(&stats.today).unwrap_or(0).unsigned_abs()).unwrap_or(0);
    (!candidates.is_empty()).then(|| candidates.swap_remove(pick % candidates.len()))
}

/// 分位：非零的消息数排序后取 25%、50%、75% 处。
fn quartiles(days: &[ActivityDay]) -> Option<[u64; 3]> {
    let mut counts: Vec<u64> = days.iter().map(|d| d.messages).filter(|&n| n > 0).collect();
    if counts.is_empty() {
        return None;
    }
    counts.sort_unstable();
    let at = |fraction: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let index = (counts.len() as f64 * fraction).floor() as usize;
        counts[index.min(counts.len() - 1)]
    };
    Some([at(0.25), at(0.5), at(0.75)])
}

/// 0 是灰色的 `·`，1–4 是强调色的 `░▒▓█`。
fn cell(messages: u64, quartiles: Option<[u64; 3]>, palette: &Palette) -> Span<'static> {
    let level = match quartiles {
        Some(_) if messages == 0 => 0,
        None => 0,
        Some([p25, p50, p75]) => {
            if messages >= p75 {
                4
            } else if messages >= p50 {
                3
            } else if messages >= p25 {
                2
            } else {
                1
            }
        }
    };
    match level {
        0 => Span::styled("\u{b7}", palette.dim()),
        n => Span::styled(
            ["\u{2591}", "\u{2592}", "\u{2593}", "\u{2588}"][n - 1],
            Style::default().fg(palette.accent),
        ),
    }
}

/// 最多 52 周的方格图（宽度不够时 `宽度 − 4`，至少 10 周），从本周的周日往前，未来的日子留空。
pub fn heatmap(
    days: &[ActivityDay],
    today: &str,
    width: usize,
    palette: &Palette,
    lang: Lang,
) -> Vec<Line<'static>> {
    let Some(today) = day_number(today) else {
        return Vec::new();
    };
    let weeks = width.saturating_sub(4).clamp(10, 52);
    let counts: std::collections::BTreeMap<i64, u64> = days
        .iter()
        .filter_map(|d| day_number(&d.date).map(|n| (n, d.messages)))
        .collect();
    let quartiles = quartiles(days);
    // 1970-01-01 是周四：周日为 0。
    let weekday = (today + 4).rem_euclid(7);
    let start = today - weekday - (weeks as i64 - 1) * 7;
    let mut rows: Vec<Vec<Span<'static>>> = (0..7).map(|_| Vec::with_capacity(weeks)).collect();
    let mut months: Vec<u32> = Vec::new();
    let mut last_month = None;
    for week in 0..weeks {
        for (row, cells) in rows.iter_mut().enumerate() {
            let day = start + (week * 7 + row) as i64;
            if day > today {
                cells.push(Span::raw(" "));
                continue;
            }
            if row == 0 {
                let (month, _) = month_day(&date_of(day));
                if last_month != Some(month) {
                    months.push(month);
                    last_month = Some(month);
                }
            }
            cells.push(cell(
                counts.get(&day).copied().unwrap_or(0),
                quartiles,
                palette,
            ));
        }
    }
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // 按月份个数均分宽度，不按真实的周对齐。
    let slot = weeks / months.len().max(1);
    let labels: String = months
        .iter()
        .map(|m| {
            let name = match lang {
                Lang::En => {
                    NAMES[usize::try_from(m.saturating_sub(1)).unwrap_or(0).min(11)].to_owned()
                }
                Lang::Zh => format!("{m}月"),
            };
            let pad = slot.saturating_sub(name.width());
            format!("{name}{}", " ".repeat(pad))
        })
        .collect();
    let mut out = vec![Line::from(format!("    {labels}"))];
    const WEEKDAYS: [(&str, &str); 7] = [
        ("Sun", "日"),
        ("Mon", "一"),
        ("Tue", "二"),
        ("Wed", "三"),
        ("Thu", "四"),
        ("Fri", "五"),
        ("Sat", "六"),
    ];
    for (row, cells) in rows.into_iter().enumerate() {
        let label = if [1, 3, 5].contains(&row) {
            let (en, zh) = WEEKDAYS[row];
            match lang {
                Lang::En => format!("{en:<3}"),
                Lang::Zh => format!("{zh}  "),
            }
        } else {
            "   ".to_owned()
        };
        let mut spans = vec![Span::raw(format!("{label} "))];
        spans.extend(cells);
        out.push(Line::from(spans));
    }
    let strings = text::strings(lang);
    let shade = Style::default().fg(palette.accent);
    out.push(Line::default());
    out.push(Line::from(vec![
        Span::raw(format!("    {} ", strings.less)),
        Span::styled("\u{2591} \u{2592} \u{2593} \u{2588}", shade),
        Span::raw(format!(" {}", strings.more)),
    ]));
    out
}

/// `All time · Last 7 days · Last 30 days`，选中的粗体强调色，其余 dim；加载中在后面加 `…`。
pub fn range_bar(
    range: ActivityRange,
    loading: bool,
    strings: &UsageStrings,
    palette: &Palette,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (option, label)) in [
        (ActivityRange::All, strings.all_time),
        (ActivityRange::Week, strings.last_7),
        (ActivityRange::Month, strings.last_30),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            spans.push(Span::styled(" \u{b7} ", palette.dim()));
        }
        spans.push(if option == range {
            Span::styled(
                label,
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(label, palette.dim())
        });
    }
    if loading {
        spans.push(Span::styled(" \u{2026}", palette.dim()));
    }
    Line::from(spans)
}

/// 一列：标签加强调色的值，补到 `width` 宽。
fn column(label: &str, value: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(format!("{label} "))];
    spans.extend(value);
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    spans
}

fn row(left: Vec<Span<'static>>, right: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(GAP)));
    spans.extend(right);
    Line::from(spans)
}

fn value(text: String, palette: &Palette) -> Vec<Span<'static>> {
    vec![Span::styled(text, Style::default().fg(palette.accent))]
}

fn bold_value(text: String, palette: &Palette) -> Vec<Span<'static>> {
    vec![Span::styled(
        text,
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )]
}

/// 总 token。
fn total_tokens(models: &[ModelTokens]) -> u64 {
    models.iter().map(ModelTokens::total).sum()
}

/// token 行。
fn token_line(models: &[ModelTokens], lang: Lang) -> String {
    let sum = |f: fn(&ModelTokens) -> u64| format_number(models.iter().map(f).sum());
    text::token_line(
        lang,
        [
            &sum(|m| m.input),
            &sum(|m| m.output),
            &sum(|m| m.cache_read),
            &sum(|m| m.cache_write),
        ],
    )
}

/// Overview。`stats` 是所选范围，`all` 是全时段（方格图与当前连续天数用它）。
pub fn overview(
    stats: &ActivityStats,
    all: &ActivityStats,
    range: ActivityRange,
    loading: bool,
    width: usize,
    palette: &Palette,
    lang: Lang,
) -> Vec<Line<'static>> {
    let strings = text::strings(lang);
    let mut out = Vec::new();
    if !all.days.is_empty() {
        out.extend(heatmap(&all.days, &all.today, width, palette, lang));
        out.push(Line::default());
    }
    out.push(range_bar(range, loading, strings, palette));
    out.push(Line::default());
    let derived = derive(stats);
    let total = total_tokens(&stats.models);
    let favorite = stats
        .models
        .first()
        .map_or_else(Vec::new, |m| bold_value(m.model.clone(), palette));
    out.push(row(
        if stats.models.is_empty() {
            vec![Span::raw(" ".repeat(COLUMN))]
        } else {
            column(strings.favorite, favorite, COLUMN)
        },
        column(
            strings.total_tokens,
            value(format_number(total), palette),
            COLUMN,
        ),
    ));
    out.push(Line::default());
    let longest = stats.longest_session_ms.map_or_else(
        || vec![Span::raw("")],
        |ms| {
            column(
                strings.longest_session,
                value(format_duration(ms), palette),
                COLUMN,
            )
        },
    );
    out.push(row(
        column(
            strings.sessions,
            value(format_number(stats.sessions), palette),
            COLUMN,
        ),
        longest,
    ));
    let denominator = range.days().unwrap_or(derived.total_days);
    let mut active = value(derived.active_days.to_string(), palette);
    active.push(Span::styled(format!("/{denominator}"), palette.dim()));
    let mut longest_streak = bold_value(derived.longest_streak.to_string(), palette);
    longest_streak.push(Span::raw(days_word(lang, derived.longest_streak)));
    out.push(row(
        column(strings.active_days, active, COLUMN),
        column(strings.longest_streak, longest_streak, COLUMN),
    ));
    let current = derive(all).current_streak;
    let mut current_streak = bold_value(current.to_string(), palette);
    current_streak.push(Span::raw(days_word(lang, current)));
    let peak = derived.peak_day.as_deref().map_or_else(
        || vec![Span::raw(" ".repeat(COLUMN))],
        |date| {
            let (month, day) = month_day(date);
            column(
                strings.most_active,
                value(text::short_date(lang, month, day), palette),
                COLUMN,
            )
        },
    );
    out.push(row(
        peak,
        column(strings.current_streak, current_streak, COLUMN),
    ));
    if total > 0 {
        out.push(Line::from(Span::styled(
            token_line(&stats.models, lang),
            palette.dim(),
        )));
    }
    if let Some(fact) = fun_fact(stats, lang) {
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            fact,
            Style::default().fg(palette.accent),
        )));
    }
    out
}

/// ` day` / ` days`（数字之后，数字与单位之间留一个空格）。
fn days_word(lang: Lang, n: u64) -> String {
    let text = text::days(lang, n);
    text.split_once(' ')
        .map_or(text.clone(), |(_, unit)| format!(" {unit}"))
}

/// 一个模型占总数的百分比（一位小数）。
fn percent(model: &ModelTokens, total: u64) -> String {
    if total == 0 {
        return "0.0".to_owned();
    }
    #[allow(clippy::cast_precision_loss)]
    let value = model.total() as f64 / total as f64 * 100.0;
    format!("{value:.1}")
}

/// 一个模型的三行。
fn model_block(
    model: &ModelTokens,
    total: u64,
    palette: &Palette,
    lang: Lang,
) -> [Line<'static>; 3] {
    let (io, cache) = text::model_lines(
        lang,
        [
            &format_number(model.input),
            &format_number(model.output),
            &format_number(model.cache_read),
            &format_number(model.cache_write),
        ],
    );
    [
        Line::from(vec![
            Span::raw("\u{2022} "),
            Span::styled(
                model.model.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" ({}%)", percent(model, total)), palette.dim()),
        ]),
        Line::from(Span::styled(io, palette.dim())),
        Line::from(Span::styled(cache, palette.dim())),
    ]
}

/// 两列并排：左列补到 [`MODEL_COLUMN`] 宽。
fn side_by_side(left: Vec<Line<'static>>, right: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let height = left.len().max(right.len());
    (0..height)
        .map(|i| {
            let mut spans = left.get(i).map(|l| l.spans.clone()).unwrap_or_default();
            if let Some(line) = right.get(i) {
                let used: usize = spans.iter().map(|s| s.content.width()).sum();
                spans.push(Span::raw(
                    " ".repeat(MODEL_COLUMN.saturating_sub(used) + GAP),
                ));
                spans.extend(line.spans.clone());
            }
            Line::from(spans)
        })
        .collect()
}

/// Models（不画 `Tokens per Day` 折线图，usage.md D13）：每页 4 个模型、两列；`offset` 是第一个。
pub fn models(
    stats: &ActivityStats,
    range: ActivityRange,
    loading: bool,
    offset: usize,
    palette: &Palette,
    lang: Lang,
) -> Vec<Line<'static>> {
    let strings = text::strings(lang);
    if stats.models.is_empty() {
        return vec![Line::from(Span::styled(strings.no_models, palette.dim()))];
    }
    let mut out = vec![range_bar(range, loading, strings, palette), Line::default()];
    let total = total_tokens(&stats.models);
    let count = stats.models.len();
    let offset = offset.min(count.saturating_sub(MODELS_PER_PAGE));
    let page = &stats.models[offset..(offset + MODELS_PER_PAGE).min(count)];
    let split = page.len().div_ceil(2);
    let column = |models: &[ModelTokens]| -> Vec<Line<'static>> {
        models
            .iter()
            .flat_map(|m| model_block(m, total, palette, lang))
            .collect()
    };
    out.extend(side_by_side(column(&page[..split]), column(&page[split..])));
    if count > MODELS_PER_PAGE {
        let up = if offset > 0 { "\u{2191}" } else { " " };
        let down = if offset + MODELS_PER_PAGE < count {
            "\u{2193}"
        } else {
            " "
        };
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            format!(
                "{up} {down} {}",
                text::models_window(
                    lang,
                    offset + 1,
                    (offset + MODELS_PER_PAGE).min(count),
                    count
                )
            ),
            palette.dim(),
        )));
    }
    out
}

/// 没有会话时的提示（`No stats available yet…`，warning 色）。
pub fn empty(lang: Lang) -> Line<'static> {
    Line::from(Span::styled(
        text::strings(lang).empty,
        Style::default().fg(Color::Yellow),
    ))
}

#[cfg(test)]
#[path = "usage_stats_tests.rs"]
mod tests;
