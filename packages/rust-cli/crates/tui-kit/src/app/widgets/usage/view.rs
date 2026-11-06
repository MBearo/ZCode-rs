//! `/usage` 的措辞、计量条与卡（docs/puffin-tui.md §4.2「用量（/usage）」）。详情页与失败回合的一行在 `page.rs`。
//! 移植自 Puffin 的 shell/usage/view.rs（UsageView.scala 第 1–311 行；docs/specs/rust-cli/tui-kit.md §4.4 K5）。
//! 纯函数：壳持有回答与时钟，这里只措辞与画；读到的经中性的 [`Usage`] 交进来。
//!
//! 与 Scala 的不同：本地时间按注入的 `UtcOffset` 算（Scala 用 `js.Date`）；池、计划的种类是投影的枚举。
use crate::app::element::{Element, Wrap, br, by_width, single_line, stack, wrap_text};
use crate::app::widgets::fade;
use crate::render::ansi::RESET;
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, attr};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::calendar::{DAY_MS, HOUR_MS, MINUTE_MS, from_local, local};
use super::{
    Entry, Family, PlanKind, PlanQuota, PoolKind, PoolPart, QuotaState, Seen, Usage, UsageModel,
    UsagePlan, UsagePool, UtcOffset,
};

const METER_MAX: i64 = 32;
const METER_MIN: i64 = 10;
pub const LEAD: &str = "   ";
/// 超过所有淡入的年龄：照原样画。
pub const SETTLED: i64 = i64::MAX / 2;

// ---- 措辞（第 24–124 行） ----

pub fn label(pool: PoolKind) -> &'static str {
    match pool {
        PoolKind::FiveHour => "5-hour",
        PoolKind::Weekly => "Weekly",
        PoolKind::Tools => "Tool calls",
        PoolKind::Mcp => "ZCode MCP",
        PoolKind::Today => "Today",
        PoolKind::Once => "One-time",
    }
}

pub fn family(f: Family) -> &'static str {
    match f {
        Family::Zai => "Z.ai",
        Family::Bigmodel => "BigModel",
    }
}

/// 计划的数字从哪个站来（槽这样说它）。
pub fn site(f: Family) -> &'static str {
    match f {
        Family::Zai => "z.ai",
        Family::Bigmodel => "open.bigmodel.cn",
    }
}

/// 页的标签页里计划的名字。
pub fn short_name(p: &UsagePlan) -> String {
    match p.kind {
        PlanKind::Team if !p.name.is_empty() => p.name.clone(),
        PlanKind::Team => "Team plan".to_owned(),
        PlanKind::Start => format!("{} Start Plan", family(p.family)),
        PlanKind::Individual => format!("{} plan", family(p.family)),
    }
}

/// 卡的头与页的面包屑里计划的名字。
pub fn title(theme: &Theme, p: &UsagePlan, level: &str) -> Vec<Span> {
    let dot = || Span::new(" · ", theme.line);
    let lv = if level.is_empty() {
        Vec::new()
    } else {
        vec![dot(), Span::new(level.to_owned(), theme.fg2)]
    };
    let mut out = match p.kind {
        PlanKind::Team => vec![
            Span::plain(if p.name.is_empty() {
                "Team".to_owned()
            } else {
                p.name.clone()
            }),
            dot(),
            Span::new(format!("{} team plan", family(p.family)), theme.fg2),
        ],
        PlanKind::Start => return vec![Span::plain(format!("{} Start Plan", family(p.family)))],
        PlanKind::Individual => vec![Span::plain(format!("{} Coding Plan", family(p.family)))],
    };
    out.extend(lv);
    out
}

/// 有详情页：Coding Plan，个人的或团队的。
pub fn detailed(p: &UsagePlan) -> bool {
    p.kind != PlanKind::Start
}

/// 卡写的剩余：向下取整，用了 0.4 % 读作 99 %。
pub fn percent(left: f64) -> i64 {
    (left.floor() as i64).clamp(0, 100)
}

pub fn used_up(p: &UsagePool) -> bool {
    p.left <= 0.0
}

/// 25 % 或更少：琥珀色。
pub fn low(p: &UsagePool) -> bool {
    p.left <= 25.0
}

/// `72%`，或 Tool calls 剩下的次数。
pub fn amount(p: &UsagePool) -> String {
    match p.count.filter(|_| p.pool == PoolKind::Tools) {
        Some(c) => commas(c as f64),
        None => format!("{}%", percent(p.left)),
    }
}

/// `at` 离 `now` 多远：`in 38m`、`in 2h 14m`、`Mon 09:00`、`Nov 14`。
pub fn when(at: f64, now: f64, zone: &UtcOffset) -> String {
    let d = at - now;
    if d < HOUR_MS {
        format!("in {}m", ((d / MINUTE_MS).ceil() as i64).max(0))
    } else if d < DAY_MS {
        let mins = (d / MINUTE_MS).ceil() as i64;
        if mins % 60 == 0 {
            format!("in {}h", mins / 60)
        } else {
            format!("in {}h {}m", mins / 60, mins % 60)
        }
    } else if d < 7.0 * DAY_MS {
        weekday_time(at, zone)
    } else {
        month_day(at, zone)
    }
}

/// 失败回合那一行说的远近：`38m`、`1h 12m`、`3 days`。
pub fn how_far(at: f64, from: f64, zone: &UtcOffset) -> String {
    let d = at - from;
    if d < DAY_MS {
        let w = when(at, from, zone);
        w.strip_prefix("in ").map_or(w.clone(), str::to_owned)
    } else {
        let days = (d / DAY_MS).floor() as i64;
        if days == 1 {
            "1 day".to_owned()
        } else {
            format!("{days} days")
        }
    }
}

/// `HH:MM`，本地时间。
pub fn clock(ms: f64, zone: &UtcOffset) -> String {
    let c = local(ms, zone);
    format!("{:02}:{:02}", c.hour, c.minute)
}

pub const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
pub const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub fn weekday_time(ms: f64, zone: &UtcOffset) -> String {
    format!(
        "{} {}",
        DAYS[local(ms, zone).weekday as usize],
        clock(ms, zone)
    )
}

pub fn month_day(ms: f64, zone: &UtcOffset) -> String {
    let c = local(ms, zone);
    format!("{} {}", MONTHS[(c.month - 1) as usize], c.day)
}

/// Java 的 `math.round`（向上取半）。
pub fn jround(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// `1,206`。
pub fn commas(n: f64) -> String {
    let s = format!("{}", jround(n) as i64);
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.as_str()),
    };
    let chars: Vec<char> = digits.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    if neg { format!("-{out}") } else { out }
}

/// 用很少的数字写 token：`940`、`12.3K`、`41.2M`、`127M`、`1.9B`。
pub fn compact(n: f64) -> String {
    let one = |x: f64, unit: &str| {
        if x < 100.0 {
            let v = jround(x * 10.0) / 10.0;
            format!("{v}{unit}")
        } else {
            format!("{}{unit}", jround(x) as i64)
        }
    };
    if n < 1000.0 {
        format!("{}", jround(n) as i64)
    } else if n < 1e6 {
        one(n / 1e3, "K")
    } else if n < 1e9 {
        one(n / 1e6, "M")
    } else {
        one(n / 1e9, "B")
    }
}

// ---- 匀速的刻度（第 126–146 行） ----

/// 池在 `reset_at` 之前的窗口：5 小时、7 天、日历上的一个月（Tool calls）、一天。
pub fn window(pool: PoolKind, reset_at: f64, zone: &UtcOffset) -> Option<f64> {
    match pool {
        PoolKind::FiveHour => Some(5.0 * HOUR_MS),
        PoolKind::Weekly => Some(7.0 * DAY_MS),
        PoolKind::Mcp | PoolKind::Today => Some(DAY_MS),
        PoolKind::Tools => {
            // js.Date 的 setMonth(月 − 1)：本地的同一天同一时刻，日子越界时进位。
            let c = local(reset_at, zone);
            let month = i64::from(c.month);
            let day = i64::from(c.day);
            let of_day = reset_at - from_local(c.year, month, day, 0.0, zone);
            let before = from_local(c.year, month - 1, day, of_day, zone);
            Some(reset_at - before)
        }
        PoolKind::Once => None,
    }
}

/// 匀速用到现在该剩的份额，0–1；池没有刻度时为 `None`。
pub fn pace(p: &UsagePool, now: f64, zone: &UtcOffset) -> Option<f64> {
    if used_up(p) {
        return None;
    }
    let at = p.reset_at? as f64;
    let w = window(p.pool, at, zone)?;
    Some(1.0 - ((now - (at - w)) / w).clamp(0.0, 1.0))
}

// ---- 计量条（第 148–206 行） ----

pub fn mixed(theme: &Theme, c: Color, t: f64) -> Color {
    match (c, theme.bg) {
        (Color::TrueColor(r, g, b), Some(bg)) if theme.rich => Rgb::new(r, g, b).mix(bg, t).color(),
        _ => c,
    }
}

/// 粗线上的刻度：前景向背景混 15 %；plain 时是终端自己的颜色。
pub fn tick_ink(theme: &Theme) -> Style {
    theme.fg.map_or(Style::DEFAULT, |fg| {
        Style::fg(mixed(theme, fg.color(), 0.15))
    })
}

/// 剩下的画成一条细条：粗线是剩下的，细线是用掉的。
pub fn meter(theme: &Theme, p: &UsagePool, cells: usize, pace_at: Option<f64>) -> Vec<Span> {
    let accent = (!used_up(p)).then(|| if low(p) { theme.code } else { theme.brand });
    let heavy = accent.map_or(theme.line, |c| {
        if theme.rich {
            Style::fg(c)
        } else {
            Style::fg(c).with_attr(attr::BOLD)
        }
    });
    let exact = p.left.clamp(0.0, 100.0) / 100.0 * cells as f64;
    let full = exact.floor() as usize;
    let half = exact - full as f64 >= 0.5;
    let mut chars: Vec<(&str, Style)> = (0..cells)
        .map(|i| {
            if i < full {
                ("━", heavy)
            } else if i == full && half {
                ("╸", heavy)
            } else {
                ("─", theme.line)
            }
        })
        .collect();
    if let Some(share) = pace_at
        && cells > 0
    {
        let at = (jround(share * cells as f64).max(0.0) as usize).min(cells - 1);
        chars[at] = if (at as f64) < exact {
            ("┃", tick_ink(theme))
        } else {
            ("│", theme.fg3)
        };
    }
    chars.into_iter().map(|(c, st)| Span::new(c, st)).collect()
}

/// `pools` 在 `width` 下的列宽：名字、剩余、重置与条（放不下时为 0）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub label: usize,
    pub amount: usize,
    pub reset: usize,
    pub bar: usize,
}

pub fn geometry(pools: &[UsagePool], width: usize, now: f64, zone: &UtcOffset) -> Geometry {
    let lw = pools.iter().map(|p| label(p.pool).len()).max().unwrap_or(0);
    let aw = pools
        .iter()
        .map(|p| {
            if used_up(p) {
                7
            } else {
                string_width(&amount(p)) + 5
            }
        })
        .max()
        .unwrap_or(0);
    let rw = pools
        .iter()
        .map(|p| widths(&reset_spans(&Theme::PLAIN, p, now, zone)))
        .max()
        .unwrap_or(0);
    let room = width as i64 - 1 - LEAD.len() as i64 - lw as i64 - 3 - 3 - aw as i64 - 3 - rw as i64;
    let bar = METER_MAX.min(room);
    Geometry {
        label: lw,
        amount: aw,
        reset: rw,
        bar: if bar >= METER_MIN { bar as usize } else { 0 },
    }
}

/// 池什么时候回来：`resets in 2h 14m`，用完之后 `back in 1h 12m · 23:00`。
pub fn reset_spans(theme: &Theme, p: &UsagePool, now: f64, zone: &UtcOffset) -> Vec<Span> {
    let Some(at) = p.reset_at.map(|t| t as f64) else {
        return Vec::new();
    };
    let w = when(at, now, zone);
    if !used_up(p) {
        vec![Span::new(format!("resets {w}"), theme.fg3)]
    } else if at - now < DAY_MS {
        vec![
            Span::new(format!("back {w}"), theme.fg2),
            Span::new(" · ", theme.line),
            Span::new(clock(at, zone), theme.fg3),
        ]
    } else {
        vec![Span::new(format!("back {w}"), theme.fg2)]
    }
}

/// 一个池的行：名字、条、剩余、回来的时刻。`age` 让它从 `ground` 淡入。
pub fn pool_row(
    theme: &Theme,
    p: &UsagePool,
    g: Geometry,
    now: f64,
    zone: &UtcOffset,
    age: i64,
    ground: Option<Color>,
) -> Element {
    let mut rest = Vec::new();
    if g.bar > 0 {
        rest.extend(meter(theme, p, g.bar, pace(p, now, zone)));
        rest.push(Span::plain("   "));
    }
    let left = if used_up(p) {
        vec![Span::new("used up", Style::fg(theme.err))]
    } else {
        vec![
            Span::new(
                amount(p),
                if low(p) {
                    Style::fg(theme.code)
                } else {
                    Style::DEFAULT
                },
            ),
            Span::new(" left", theme.fg3),
        ]
    };
    let pad = " ".repeat(g.amount.saturating_sub(widths(&left)) + 3);
    rest.extend(left);
    rest.push(Span::plain(pad));
    rest.extend(reset_spans(theme, p, now, zone));
    let mut spans = vec![
        Span::plain(LEAD),
        Span::new(pad_to(label(p.pool), g.label), theme.fg2),
        Span::plain("   "),
    ];
    spans.extend(fade::fade_in(theme, rest, age, fade::TICKS, ground));
    single_line(spans, false)
}

/// 比上一次回答变了的行淡入，别的不动。
pub fn changed(p: &UsagePool, before: Option<&PlanQuota>) -> bool {
    before
        .and_then(|b| b.pools.iter().find(|x| x.pool == p.pool))
        .is_none_or(|b| {
            percent(b.left) != percent(p.left) || b.count != p.count || b.reset_at != p.reset_at
        })
}

/// `   Usage   plan …           renews Nov 14`：卡的头；放不下时右边去掉。
pub fn head(theme: &Theme, left: Vec<Span>, right: Vec<Span>) -> Element {
    let theme = *theme;
    by_width(move |w| {
        let mut l = vec![
            Span::plain(LEAD),
            Span::new("Usage", theme.fg2.with_attr(attr::BOLD)),
            Span::plain("   "),
        ];
        l.extend(left.iter().cloned());
        single_line(spread(l, right.clone(), w, 3), false)
    })
}

/// `left`，然后 `right` 结束在第 `width − 1` 列；两边近于 `gap` 时只要 `left`。
pub fn spread(left: Vec<Span>, right: Vec<Span>, width: usize, gap: i64) -> Vec<Span> {
    let room = width as i64 - 1 - widths(&left) as i64 - widths(&right) as i64;
    if right.is_empty() || room < gap {
        left
    } else {
        let mut out = left;
        out.push(Span::plain(" ".repeat(room as usize)));
        out.extend(right);
        out
    }
}

pub fn widths(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// 按码点补空格到 `n`（Scala 的 `padTo`）。
pub fn pad_to(s: &str, n: usize) -> String {
    let len = s.chars().count();
    format!("{s}{}", " ".repeat(n.saturating_sub(len)))
}

/// 订阅的下一个日子：`renews Nov 14`，否则 `ends Nov 14`。
pub fn renewal(theme: &Theme, q: &PlanQuota, zone: &UtcOffset) -> Vec<Span> {
    q.renews
        .map(|t| format!("renews {}", month_day(t as f64, zone)))
        .or_else(|| {
            q.ends
                .map(|t| format!("ends {}", month_day(t as f64, zone)))
        })
        .into_iter()
        .map(|t| Span::new(t, theme.fg3))
        .collect()
}

/// 计划不在时卡在行的位置说的。
pub fn missing(theme: &Theme, p: &UsagePlan, q: &PlanQuota) -> Vec<Vec<Span>> {
    match q.state {
        QuotaState::NoPlan => vec![
            vec![Span::new("This account has no Coding Plan.", theme.fg2)],
            vec![
                Span::new("/login", theme.fg2),
                Span::new(" with another account, or get one on the web.", theme.fg3),
            ],
        ],
        QuotaState::TeamEnded => vec![vec![Span::new("This team plan has ended.", theme.fg2)]],
        QuotaState::NotOnTeam => {
            vec![vec![Span::new("You are not on this team plan.", theme.fg2)]]
        }
        QuotaState::Expired => vec![
            vec![Span::new(
                format!("Your {} sign-in has expired.", family(p.family)),
                theme.fg2,
            )],
            vec![
                Span::new("/login", theme.fg2),
                Span::new(" signs in again.", theme.fg3),
            ],
        ],
        QuotaState::Ok => vec![vec![Span::new(
            "The plan did not say how much is left.",
            theme.fg3,
        )]],
    }
}

pub fn said(lines: Vec<Vec<Span>>) -> Vec<Element> {
    lines
        .into_iter()
        .map(|l| {
            let mut spans = vec![Span::plain(LEAD)];
            spans.extend(l);
            single_line(spans, false)
        })
        .collect()
}

pub fn asking(theme: &Theme, words: &str) -> Element {
    single_line(
        vec![
            Span::plain(LEAD),
            Span::new("○", theme.fg3),
            Span::new(format!(" {words}"), theme.fg3),
        ],
        false,
    )
}

pub fn failed(theme: &Theme, words: &str) -> Element {
    single_line(
        vec![
            Span::plain(LEAD),
            Span::new("✗", Style::fg(theme.err)),
            Span::new(format!(" {words}"), theme.fg2),
        ],
        false,
    )
}

/// 当前模型不在 ZCode 的计划上：它的用量在哪里，按宽度折行。
pub fn elsewhere(theme: &Theme, m: &UsageModel, model: &str) -> Vec<Element> {
    let seq = |st: Style, t: &str| format!("{}{t}{RESET}", st.set_sequence());
    let who = if model.is_empty() {
        "This model"
    } else {
        model
    };
    let first = seq(
        theme.fg2,
        &format!("{who} runs on your {} API key.", m.name),
    );
    let site = if m.site.is_empty() {
        String::new()
    } else {
        seq(theme.fg2, " Its usage is at ")
            + &seq(Style::fg(theme.brand).with_attr(attr::UNDERLINE), &m.site)
            + &seq(theme.fg2, ".")
    };
    vec![wrap_text(LEAD, LEAD, first + &site, Wrap::Word)]
}

// ---- 卡（第 255–311 行） ----

/// 条目 `entry` 的卡：`model` 是当前模型的 id。
pub fn card(
    theme: &Theme,
    ui: &Usage<'_>,
    entry: Option<&Entry>,
    model: &str,
    now: f64,
    zone: &UtcOffset,
    clock: i64,
) -> Vec<Element> {
    let ground = theme.panel;
    match entry {
        None => {
            let body = match ui.plans_failed {
                None => asking(theme, "Asking how much is left"),
                Some(why) => failed(theme, &format!("Could not read your usage: {why}")),
            };
            vec![head(theme, Vec::new(), Vec::new()), br(), body]
        }
        Some(Entry::Model(m)) => {
            let mut out = vec![
                head(theme, vec![Span::plain(m.name.clone())], Vec::new()),
                br(),
            ];
            out.extend(elsewhere(theme, m, model));
            out
        }
        Some(Entry::Plan(p)) => {
            let seen = ui.quotas.get(&p.id).cloned().unwrap_or_default();
            match &seen.value {
                None => {
                    let body = match &seen.failed {
                        None => asking(
                            theme,
                            &format!("Asking {} how much is left", site(p.family)),
                        ),
                        Some(why) => failed(theme, &format!("Could not read your usage: {why}")),
                    };
                    vec![head(theme, title(theme, p, ""), Vec::new()), br(), body]
                }
                Some(q) => {
                    let mut out = vec![
                        head(theme, title(theme, p, &q.level), renewal(theme, q, zone)),
                        br(),
                    ];
                    out.extend(quota_rows(
                        theme,
                        p,
                        &seen,
                        (now, zone),
                        clock,
                        ground,
                        false,
                    ));
                    out
                }
            }
        }
    }
}

/// 额度的行（页上带 Tool calls 的明细与图例）。`at` 是（墙钟，时区）。
pub fn quota_rows(
    theme: &Theme,
    p: &UsagePlan,
    seen: &Seen<PlanQuota>,
    at: (f64, &UtcOffset),
    clock: i64,
    ground: Option<Color>,
    parts: bool,
) -> Vec<Element> {
    let Some(q) = &seen.value else {
        return Vec::new();
    };
    if q.state != QuotaState::Ok || q.pools.is_empty() {
        return said(missing(theme, p, q));
    }
    let (now, zone) = (at.0, at.1.clone());
    let theme = *theme;
    let seen = seen.clone();
    vec![by_width(move |w| {
        let Some(q) = &seen.value else {
            return stack([]);
        };
        let g = geometry(&q.pools, w, now, &zone);
        let indent = || Span::plain(" ".repeat(LEAD.len() + g.label + 3));
        let mut rows = Vec::new();
        for pool in &q.pools {
            let age = if changed(pool, seen.before.as_ref()) {
                clock - seen.arrived
            } else {
                SETTLED
            };
            rows.push(pool_row(&theme, pool, g, now, &zone, age, ground));
            if parts && pool.pool == PoolKind::Tools && !pool.parts.is_empty() {
                let pairs = pool
                    .parts
                    .iter()
                    .map(|x| (part_name(x), commas(x.used as f64)))
                    .collect();
                let mut spans = vec![indent()];
                spans.extend(joined(&theme, pairs));
                rows.push(single_line(spans, false));
            }
        }
        if parts && g.bar > 0 && q.pools.iter().any(|x| pace(x, now, &zone).is_some()) {
            rows.push(single_line(
                vec![
                    indent(),
                    Span::new("┃", tick_ink(&theme)),
                    Span::new(
                        " an even pace. Past it, the pool lasts until it resets.",
                        theme.fg3,
                    ),
                ],
                false,
            ));
        }
        stack(rows)
    })]
}

/// 工具调用的名字：ZCode 认得的三个，其余用计划的叫法。
pub fn part_name(x: &PoolPart) -> String {
    match x.code.as_str() {
        "search-prime" => "Web search".to_owned(),
        "web-reader" => "Web reader".to_owned(),
        "zread" => "Zread".to_owned(),
        code if x.name.is_empty() => code.to_owned(),
        _ => x.name.clone(),
    }
}

/// `name value  ·  name value`：名字 fg2，值 fg3。
pub fn joined(theme: &Theme, pairs: Vec<(String, String)>) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, (n, v)) in pairs.into_iter().enumerate() {
        if i > 0 {
            out.push(Span::new("  ·  ", theme.line));
        }
        out.push(Span::new(n, theme.fg2));
        out.push(Span::new(format!(" {v}"), theme.fg3));
    }
    out
}

/// `value label  ·  value label`：值是 fg，标签 fg3。
pub fn facts(theme: &Theme, pairs: Vec<(String, String)>) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, (v, l)) in pairs.into_iter().enumerate() {
        if i > 0 {
            out.push(Span::new("  ·  ", theme.line));
        }
        out.push(Span::plain(v));
        out.push(Span::new(format!(" {l}"), theme.fg3));
    }
    out
}
