//! `/usage` 的详情页与失败回合的一行（docs/puffin-tui.md §4.2「用量（/usage）」「回合失败」的「回来的时刻」）。
//! 移植自 Puffin 的 shell/usage/page.rs（UsageView.scala 第 312–538 行；docs/specs/rust-cli/tui-kit.md §4.4 K5）。
//!
//! 与 Scala 的不同：日历按注入的 `UtcOffset` 算；热力图的级别是 `i8`（-1 是没有那一天）。
use crate::app::element::{Element, br, by_width, single_line, stack};
use crate::app::widgets::fade;
use crate::render::{Color, Span, Style, attr};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::calendar::{DAY_MS, civil_from_days, day_of, local, weekday_of};
use super::view::{
    DAYS, LEAD, MONTHS, asking, clock, commas, compact, detailed, facts, failed, how_far, joined,
    jround, mixed, month_day, pad_to, quota_rows, short_name, site, spread, title, used_up,
    weekday_time,
};
use super::{PlanDetail, PlanQuota, PoolKind, Usage, UsagePlan, UtcOffset, range, range_name};

fn line(theme: &Theme, spans: Vec<Span>, age: i64) -> Element {
    single_line(fade::fade_in(theme, spans, age, fade::TICKS, None), false)
}

fn heading(theme: &Theme, text: &str) -> Vec<Span> {
    vec![
        Span::plain(LEAD),
        Span::new(text.to_owned(), theme.fg2.with_attr(attr::BOLD)),
    ]
}

fn headed(theme: &Theme, text: &str, right: Vec<Span>) -> Element {
    let left = heading(theme, text);
    by_width(move |w| single_line(spread(left.clone(), right.clone(), w, 2), false))
}

fn chosen(theme: &Theme) -> Style {
    if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::fg(theme.brand).with_attr(attr::BOLD)
    }
}

/// 计划 `p` 的详情页，从上到下；壳滚动它（第 319–352 行）。
pub fn page(
    theme: &Theme,
    ui: &Usage<'_>,
    p: &UsagePlan,
    range_at: usize,
    now: f64,
    zone: &UtcOffset,
    clock_tick: i64,
) -> Vec<Element> {
    let seen = ui.quotas.get(&p.id).cloned().unwrap_or_default();
    let level = seen.value.as_ref().map_or("", |q| q.level.as_str());
    let tabs: Vec<&UsagePlan> = ui.plans.iter().filter(|t| detailed(t)).collect();
    let mut tab_spans = Vec::new();
    if tabs.len() >= 2 {
        for (i, t) in tabs.iter().enumerate() {
            if i > 0 {
                tab_spans.push(Span::plain("   "));
            }
            tab_spans.push(Span::new(
                short_name(t),
                if t.id == p.id {
                    chosen(theme)
                } else {
                    theme.fg3
                },
            ));
        }
    }
    let mut crumb_left = vec![
        Span::plain(LEAD),
        Span::new("Usage", Style::DEFAULT.with_attr(attr::BOLD)),
        Span::new("  ›  ", theme.line),
    ];
    crumb_left.extend(title(theme, p, level));
    let crumb =
        by_width(move |w| single_line(spread(crumb_left.clone(), tab_spans.clone(), w, 3), false));
    let since_and_renews: Vec<String> = seen.value.as_ref().map_or_else(Vec::new, |q| {
        let since = q.since.map(|t| {
            let lead = if q.level.is_empty() {
                "since ".to_owned()
            } else {
                format!("{} since ", q.level)
            };
            lead + &month_day(t as f64, zone)
        });
        let next = q
            .renews
            .map(|t| format!("renews {}", month_day(t as f64, zone)))
            .or_else(|| {
                q.ends
                    .map(|t| format!("ends {}", month_day(t as f64, zone)))
            });
        since.into_iter().chain(next).collect()
    });
    let mut dots = Vec::new();
    for (i, t) in since_and_renews.into_iter().enumerate() {
        if i > 0 {
            dots.push(Span::new("  ·  ", theme.line));
        }
        dots.push(Span::new(t, theme.fg3));
    }
    let mut out = vec![br(), crumb, br(), headed(theme, "Left now", dots)];
    if seen.value.is_some() {
        out.extend(quota_rows(
            theme,
            p,
            &seen,
            (now, zone),
            clock_tick,
            None,
            true,
        ));
    } else {
        out.push(match &seen.failed {
            None => asking(
                theme,
                &format!("Asking {} how much is left", site(p.family)),
            ),
            Some(why) => failed(theme, &format!("Could not read your usage: {why}")),
        });
    }
    let r = range(range_at);
    let hist = ui
        .details
        .get(&(p.id.clone(), range_name(r.0).to_owned()))
        .cloned()
        .unwrap_or_default();
    out.push(br());
    match &hist.value {
        None => out.push(match &hist.failed {
            None => asking(theme, &format!("Asking {} for the history", site(p.family))),
            Some(why) => failed(theme, &format!("Could not read the history: {why}")),
        }),
        Some(d) => {
            let age = clock_tick - hist.arrived;
            out.extend(activity(theme, d, age));
            out.push(br());
            out.extend(bars(theme, d, range_at, now, zone, age));
            out.push(br());
            out.extend(totals(theme, d, age));
            out.push(br());
            out.extend(speed(theme, d, age));
        }
    }
    out
}

// ---- 热力图（第 354–415 行） ----

/// 热力图的墨：空的一天只比背景亮一点，然后是 brand 向背景混 72 %、48 %、22 % 与 0 %。
fn heat_inks(theme: &Theme) -> [Color; 5] {
    let fg = theme.fg.map_or(Color::Default, Rgb::color);
    [
        mixed(theme, fg, 0.93),
        mixed(theme, theme.brand, 0.72),
        mixed(theme, theme.brand, 0.48),
        mixed(theme, theme.brand, 0.22),
        theme.brand,
    ]
}

pub fn activity(theme: &Theme, d: &PlanDetail, age: i64) -> Vec<Element> {
    let a = &d.activity;
    let mut pairs = Vec::new();
    if a.total > 0 {
        pairs.push((compact(a.total as f64), "total".to_owned()));
    }
    if a.best_day > 0 {
        pairs.push((compact(a.best_day as f64), "best day".to_owned()));
    }
    if a.streak > 0 {
        pairs.push((format!("{}-day", a.streak), "streak".to_owned()));
    }
    let head = headed(theme, "Tokens, last 52 weeks", facts(theme, pairs));
    if a.levels.is_empty() || a.levels.iter().all(|w| w.iter().all(|l| *l <= 0)) {
        vec![
            head,
            line(
                theme,
                vec![
                    Span::plain(LEAD),
                    Span::new("No activity yet.", theme.fg3.with_attr(attr::ITALIC)),
                ],
                age,
            ),
        ]
    } else {
        let theme = *theme;
        let d = d.clone();
        vec![
            head,
            by_width(move |w| stack(heat_rows(&theme, &d, w, age))),
        ]
    }
}

/// 周从左到右，最新的在右；每格两天（`▀`：上一天是墨，下一天是底）。
pub fn heat_rows(theme: &Theme, d: &PlanDetail, width: usize, age: i64) -> Vec<Element> {
    let levels = &d.activity.levels;
    let shown = (width as i64 - 1 - LEAD.len() as i64 - 5).clamp(1, 52) as usize;
    let first = levels.len().saturating_sub(shown);
    let weeks = &levels[first..];
    let level = |w: usize, day: usize| -> i8 { weeks[w].get(day).copied().unwrap_or(-1) };
    let mut rows = Vec::new();
    if theme.rich {
        let inks = heat_inks(theme);
        let ink = |l: i8| inks[l.clamp(0, 4) as usize];
        let labels = ["Mon", "Wed", "Fri", ""];
        for (r, name) in labels.iter().enumerate() {
            let mut spans = vec![Span::plain(LEAD), Span::new(pad_to(name, 5), theme.fg3)];
            for w in 0..weeks.len() {
                let a = level(w, 2 * r);
                let b = if 2 * r + 1 < 7 {
                    level(w, 2 * r + 1)
                } else {
                    -1
                };
                spans.push(if a < 0 && b < 0 {
                    Span::plain(" ")
                } else if b < 0 {
                    Span::new("▀", Style::fg(ink(a)))
                } else if a < 0 {
                    Span::new("▄", Style::fg(ink(b)))
                } else {
                    Span::new("▀", Style::fg(ink(a)).with_bg(ink(b)))
                });
            }
            rows.push(line(theme, spans, age));
        }
    } else {
        let glyph = [
            ("·", theme.line),
            ("▪", theme.fg3),
            ("▪", Style::DEFAULT),
            ("■", Style::DEFAULT),
            ("■", Style::fg(theme.brand)),
        ];
        let labels = ["", "Mon", "", "Wed", "", "Fri", ""];
        for (day, name) in labels.iter().enumerate() {
            let mut spans = vec![Span::plain(LEAD), Span::new(pad_to(name, 5), theme.fg3)];
            for w in 0..weeks.len() {
                let lv = level(w, day);
                spans.push(if lv < 0 {
                    Span::plain(" ")
                } else {
                    let (g, st) = glyph[lv.min(4) as usize];
                    Span::new(g, st)
                });
            }
            rows.push(line(theme, spans, age));
        }
    }
    rows.push(line(
        theme,
        vec![
            Span::plain(format!("{LEAD}     ")),
            Span::new(month_row(&d.activity.start, first, weeks.len()), theme.fg3),
        ],
        age,
    ));
    rows
}

/// 月名写在含 1 号的那一周；会被截断或会撞上另一个的名字不写。
pub fn month_row(start: &str, first: usize, weeks: usize) -> String {
    let Some(sunday) = day_of(start) else {
        return String::new();
    };
    let mut out = vec![' '; weeks];
    let mut free = 0;
    for w in 0..weeks {
        let firsts = (0..7)
            .map(|k| sunday + 7 * (first + w) as i64 + k)
            .find(|d| civil_from_days(*d).2 == 1);
        if let Some(day) = firsts {
            let name = MONTHS[(civil_from_days(day).1 - 1) as usize];
            if w >= free && w + name.len() <= weeks {
                for (k, c) in name.chars().enumerate() {
                    out[w + k] = c;
                }
                free = w + name.len() + 1;
            }
        }
    }
    out.into_iter().collect::<String>().trim_end().to_owned()
}

// ---- 每天的 token（第 417–472 行） ----

const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
const BAR_ROWS: usize = 5;

fn seg_inks(theme: &Theme) -> [Style; 3] {
    if theme.rich {
        [
            Style::fg(mixed(theme, theme.brand, 0.6)),
            Style::fg(theme.brand),
            Style::fg(theme.work),
        ]
    } else {
        [theme.fg3, Style::fg(theme.brand), Style::fg(Color::CYAN)]
    }
}

pub fn bars(
    theme: &Theme,
    d: &PlanDetail,
    range_at: usize,
    now: f64,
    zone: &UtcOffset,
    age: i64,
) -> Vec<Element> {
    let name = range(range_at).1;
    let today = name == "today";
    let mut head = heading(
        theme,
        if today {
            "Tokens by hour"
        } else {
            "Tokens by day"
        },
    );
    for (i, (_, _, label)) in super::RANGES.iter().enumerate() {
        head.push(Span::plain(if i == 0 { "   " } else { "  " }));
        head.push(Span::new(
            *label,
            if i == range_at {
                chosen(theme)
            } else {
                theme.fg3
            },
        ));
    }
    let head = line(theme, head, age);
    let (cell, gap) = if name == "7d" { (3, 2) } else { (1, 1) };
    let hour_now = local(now, zone).hour as usize;
    // 今天还没到的小时是空的。
    let points: Vec<Option<[f64; 3]>> = (0..d.axis.len())
        .map(|i| {
            let get = |v: &Vec<u64>| v.get(i).copied().unwrap_or(0) as f64;
            (!(today && i > hour_now)).then(|| [get(&d.cached), get(&d.input), get(&d.output)])
        })
        .collect();
    let max = points
        .iter()
        .flatten()
        .map(|v| v.iter().sum::<f64>())
        .fold(0.0, f64::max);
    let inks = seg_inks(theme);
    let mut rows = vec![head];
    for r in (0..BAR_ROWS).rev() {
        let tag = if r == BAR_ROWS - 1 {
            compact(max)
        } else if r == 0 {
            "0".to_owned()
        } else {
            String::new()
        };
        let mut spans = vec![Span::plain(LEAD), Span::new(pad_to(&tag, 5), theme.fg3)];
        for (i, v) in points.iter().enumerate() {
            spans.push(Span::plain(" ".repeat(if i == 0 { gap / 2 } else { gap })));
            let total = v.map_or(0.0, |v| v.iter().sum());
            let h8 = if max > 0.0 {
                total / max * (BAR_ROWS * 8) as f64
            } else {
                0.0
            };
            let lower = (r * 8) as f64;
            for _ in 0..cell {
                match v {
                    Some(vs) if h8 > lower + 0.5 => {
                        let fill = (h8 - lower).min(8.0);
                        // 这一格的颜色取它已填部分顶端所在的那一段。
                        let y = (lower + fill - 0.5) / (BAR_ROWS * 8) as f64 * max;
                        let seg = if y < vs[0] {
                            0
                        } else if y < vs[0] + vs[1] {
                            1
                        } else {
                            2
                        };
                        let k = (jround(fill) as usize).clamp(1, 8);
                        spans.push(Span::new(EIGHTHS[k], inks[seg]));
                    }
                    _ => spans.push(Span::plain(" ")),
                }
            }
        }
        rows.push(line(theme, spans, age));
    }
    let step = cell + gap;
    let start = LEAD.len() + 5 + gap / 2;
    let mut labels = vec![' '; start + points.len() * step + 8];
    let mut put = |at: usize, text: &str| {
        for (k, c) in text.chars().enumerate() {
            if at + k < labels.len() {
                labels[at + k] = c;
            }
        }
    };
    match name {
        "7d" => {
            for (i, key) in d.axis.iter().enumerate() {
                if let Some(day) = day_of(key) {
                    put(start + i * step, DAYS[weekday_of(day) as usize]);
                }
            }
        }
        "30d" => {
            for (i, key) in d.axis.iter().enumerate() {
                if i % 7 == 0
                    && let Some(day) = day_of(key)
                {
                    let (_, m, dd) = civil_from_days(day);
                    put(
                        start + i * step,
                        &format!("{} {dd}", MONTHS[(m - 1) as usize]),
                    );
                }
            }
        }
        _ => {
            for h in [0, 6, 12, 18] {
                put(start + h * step, &format!("{h:02}"));
            }
        }
    }
    let label_row: String = labels.into_iter().collect();
    rows.push(line(
        theme,
        vec![Span::new(label_row.trim_end().to_owned(), theme.fg3)],
        age,
    ));
    let key_of = |ink: Style, name: &str| {
        vec![
            Span::new("■", ink),
            Span::new(format!(" {name}"), theme.fg3),
        ]
    };
    let mut legend = vec![Span::plain(format!("{LEAD}     "))];
    legend.extend(key_of(inks[0], "cached input"));
    legend.push(Span::plain("   "));
    legend.extend(key_of(inks[1], "input"));
    legend.push(Span::plain("   "));
    legend.extend(key_of(inks[2], "output"));
    rows.push(line(theme, legend, age));
    rows
}

/// 这个范围的 Total、Models 与 Tool calls；没有可说的行不写。
pub fn totals(theme: &Theme, d: &PlanDetail, age: i64) -> Vec<Element> {
    let row = |name: &str, spans: Vec<Span>| -> Option<Element> {
        (!spans.is_empty()).then(|| {
            let mut all = vec![Span::plain(LEAD), Span::new(pad_to(name, 12), theme.fg3)];
            all.extend(spans);
            line(theme, all, age)
        })
    };
    let t = &d.total;
    let mut pairs = Vec::new();
    if t.tokens > 0 {
        pairs.push((compact(t.tokens as f64), "tokens".to_owned()));
    }
    if let Some(r) = t.cache_rate {
        pairs.push((
            format!("{}%", jround(r * 100.0) as i64),
            "from cache".to_owned(),
        ));
    }
    if t.credits > 0.0 {
        pairs.push((commas(t.credits), "credits".to_owned()));
    }
    let models = d
        .models
        .iter()
        .map(|m| (m.name.clone(), compact(m.count as f64)))
        .collect();
    let tools = d
        .tools
        .iter()
        .map(|m| (m.name.clone(), commas(m.count as f64)))
        .collect();
    [
        row("Total", facts(theme, pairs)),
        row("Models", joined(theme, models)),
        row("Tool calls", joined(theme, tools)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

// ---- 解码速度（第 490–522 行） ----

const SPARK_MAX: i64 = 56;

/// `points` 放进 `cells` 格：每格是它那些点的平均；点比格少时一点一格。
pub fn resample(points: &[f64], cells: usize) -> Vec<f64> {
    if points.len() <= cells {
        return points.to_vec();
    }
    (0..cells)
        .map(|c| {
            let from = c * points.len() / cells;
            let until = (from + 1).max((c + 1) * points.len() / cells);
            let part = &points[from..until];
            part.iter().sum::<f64>() / part.len() as f64
        })
        .collect()
}

pub fn speed(theme: &Theme, d: &PlanDetail, age: i64) -> Vec<Element> {
    let head = line(theme, heading(theme, "Decode speed, 7 days"), age);
    let h = &d.health;
    if h.pro_max.iter().all(|v| *v <= 0.0) && h.lite.iter().all(|v| *v <= 0.0) {
        return vec![
            head,
            line(
                theme,
                vec![
                    Span::plain(LEAD),
                    Span::new("No speed data yet.", theme.fg3.with_attr(attr::ITALIC)),
                ],
                age,
            ),
        ];
    }
    let series = [
        ("Max & Pro", h.pro_max.clone(), Style::fg(theme.brand)),
        (
            "Lite",
            h.lite.clone(),
            if theme.rich {
                Style::fg(theme.code)
            } else {
                theme.fg3
            },
        ),
    ];
    let lw = series.iter().map(|s| s.0.len()).max().unwrap_or(0);
    let theme = *theme;
    vec![
        head,
        by_width(move |w| {
            let cells = (w as i64 - 1 - LEAD.len() as i64 - lw as i64 - 3 - 3 - 14)
                .clamp(1, SPARK_MAX) as usize;
            let shown: Vec<(&str, Vec<f64>, Style, &Vec<f64>)> = series
                .iter()
                .map(|(n, pts, ink)| (*n, resample(pts, cells), *ink, pts))
                .collect();
            let max = shown
                .iter()
                .flat_map(|s| s.1.iter().copied())
                .fold(0.0, f64::max);
            stack(shown.into_iter().map(|(name, pts, ink, raw)| {
                let mut spans = vec![
                    Span::plain(LEAD),
                    Span::new(pad_to(name, lw), theme.fg2),
                    Span::plain("   "),
                ];
                for v in pts {
                    spans.push(if v <= 0.0 || max <= 0.0 {
                        Span::plain(" ")
                    } else {
                        let k = (jround(v / max * 8.0) as usize).clamp(1, 8);
                        Span::new(EIGHTHS[k], ink)
                    });
                }
                if let Some(v) = raw.iter().rev().find(|v| **v > 0.0) {
                    spans.push(Span::plain("   "));
                    spans.push(Span::plain(format!("{}", jround(*v) as i64)));
                    spans.push(Span::new(" tokens/s", theme.fg3));
                }
                line(&theme, spans, age)
            }))
        }),
    ]
}

// ---- 失败回合的一行（第 524–538 行） ----

/// `The 5-hour limit comes back at 23:00, in 1h 12m.`，从 `from` 的失败算起。
pub fn comes_back(theme: &Theme, pool: &str, at: f64, from: f64, zone: &UtcOffset) -> Vec<Span> {
    let name = if pool == "weekly" {
        "The weekly limit"
    } else {
        "The 5-hour limit"
    };
    let d = at - from;
    let (word, time) = if d < DAY_MS {
        ("at", clock(at, zone))
    } else if d < 7.0 * DAY_MS {
        ("on", weekday_time(at, zone))
    } else {
        ("on", month_day(at, zone))
    };
    vec![
        Span::new(format!("{name} comes back {word} "), theme.fg2),
        Span::plain(time),
        Span::new(format!(", in {}.", how_far(at, from, zone)), theme.fg2),
    ]
}

/// 停住计划的上限：用完了的 5 小时与每周两个池里，回来得最晚的那个（池名是 host 的写法）。
pub fn wall(q: &PlanQuota) -> Option<(String, f64)> {
    q.pools
        .iter()
        .filter(|p| matches!(p.pool, PoolKind::FiveHour | PoolKind::Weekly) && used_up(p))
        .filter_map(|p| {
            p.reset_at.map(|at| {
                let name = if p.pool == PoolKind::Weekly {
                    "weekly"
                } else {
                    "fiveHour"
                };
                (name.to_owned(), at as f64)
            })
        })
        .fold(None, |best: Option<(String, f64)>, x| match best {
            Some(b) if b.1 >= x.1 => Some(b),
            _ => Some(x),
        })
}
