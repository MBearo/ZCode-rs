//! 会话的数字（docs/puffin-tui.md §4.2「会话的数字（/stats）」；docs/specs/rust-cli/tui-kit.md §4.4）与状态栏的「多少」：
//! 上下文的表与它的数和计数（`62%  124k/200k`，离压缩的位置近时是琥珀色），`/stats` 的卡（标题与回合、上下文的条与图例、
//! 缓存、速度的小图、token、结束了的子代理与合计）。纯的；入参是中性的 [`Usage`]、[`Window`] 与 [`Ended`]。
//! 移植自 Puffin 的 shell/stats.rs（`METER_CELLS`、`KEYS`、`parts`、`near`、`meter`、`percent`、`card` 与
//! 它的行、`working`）。
use std::collections::BTreeMap;

use crate::app::element::{Element, br, by_width, single_line, stack};
use crate::markdown::render::{INDENT, SETTLED};
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, attr};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{MEASURE_MARGIN, MEASURE_MAX, clip, count, pad, rate, round};
use super::status_bar::Part;

/// 上下文窗口：用掉的与窗口的 token，自动压缩的位置（没有时为 `None`），组成（来源与字符数，只在 `/stats` 打开时有）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub used: u64,
    pub max: u64,
    pub compact_at: Option<u64>,
    pub breakdown: Vec<(String, u64)>,
}

/// 缓存命中率（0–1）；`last_call` 是最近一次调用的。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cache {
    pub hit_rate: f64,
    pub last_call: Option<f64>,
}

/// 会话的用量：窗口（模型不说时为 `None`）、缓存、累计的输入与输出 token、最近一次的速度与每个回合的速度（最早的在前）、
/// 最近一次调用的 token、回合的个数与它们 activeMs 之和、回合是不是都算进来了。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub context: Option<Window>,
    pub cache: Option<Cache>,
    pub input: u64,
    pub output: u64,
    pub last_speed: Option<f64>,
    pub speeds: Vec<f64>,
    pub last_call_tokens: Option<u64>,
    pub turns: u32,
    pub active_ms: u64,
    pub complete: bool,
}

/// 结束了的子代理的合计：几个、token、工作的毫秒与各自的速度。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ended {
    pub count: usize,
    pub tokens: f64,
    pub work_ms: f64,
    pub speeds: Vec<f64>,
}

/// 状态栏表的格数。
pub const METER_CELLS: usize = 10;
/// 淡入按什么分：表与它的数和计数、速度、卡的 token 与缓存、子代理、合计。
pub const KEYS: [&str; 6] = ["context", "speed", "tokens", "cache", "subagents", "inall"];

// ---- 状态栏 ----

/// 「多少」的部分：表、它的数与计数（`124k/200k`），然后是速度。会话的 token 只在卡上。`age(key)`：那个部分变了多少
/// tick；`speed`：画好的速度，为空时不显示。
pub fn parts(
    theme: &Theme,
    context: Option<&Window>,
    age: &dyn Fn(&str) -> i64,
    speed: Vec<Span>,
) -> Vec<Part> {
    let fade =
        |key: &str, spans: Vec<Span>| fade::fade_in(theme, spans, age(key), fade::TICKS, None);
    let mut out = Vec::new();
    if let Some(c) = context {
        let ink = if near(c) {
            Style::fg(theme.code)
        } else {
            theme.fg2
        };
        out.push(Part {
            right: true,
            ..Part::new("meter", fade("context", meter(theme, c)), 50)
        });
        out.push(Part {
            right: true,
            glue: Some(vec![Span::new(" ", Style::DEFAULT)]),
            ..Part::new(
                "percent",
                fade("context", vec![Span::new(format!("{}%", percent(c)), ink)]),
                90,
            )
        });
        // 用掉的数与数同色；窗口的大小是附加的部分，永远 fg3：分母是窗口，压缩的位置由表的竖线说。
        out.push(Part {
            right: true,
            glue: Some(vec![Span::new("  ", Style::DEFAULT)]),
            after: Some("percent".to_owned()),
            ..Part::new(
                "used",
                fade("context", vec![Span::new(count(c.used as f64), ink)]),
                35,
            )
        });
        out.push(Part {
            right: true,
            glue: Some(Vec::new()),
            extra: true,
            after: Some("used".to_owned()),
            ..Part::new(
                "window",
                fade(
                    "context",
                    vec![Span::new(format!("/{}", count(c.max as f64)), theme.fg3)],
                ),
                25,
            )
        });
    }
    if !speed.is_empty() {
        out.push(Part {
            right: true,
            ..Part::new("speed", speed, 30)
        });
    }
    out
}

/// 离 agent 压缩的位置不到十个百分点（没有时离整个窗口）：表与它的数变成琥珀色。
pub fn near(c: &Window) -> bool {
    let max = c.max as f64;
    c.used as f64 >= c.compact_at.map_or(max, |a| a as f64) - 0.1 * max
}

/// 表：用掉的粗，空着的细，agent 压缩的位置一条竖线。
pub fn meter(theme: &Theme, c: &Window) -> Vec<Span> {
    let full = filled(c, METER_CELLS);
    let tick = tick_at(c, METER_CELLS);
    let used = if near(c) {
        Style::fg(theme.code)
    } else if theme.rich {
        theme.fg2
    } else {
        Style::attrs_only(attr::BOLD)
    };
    let cells: Vec<(&str, Style)> = (0..METER_CELLS)
        .map(|i| {
            if tick == Some(i) {
                (
                    if i < full { "┃" } else { "│" },
                    if i < full && theme.rich {
                        used
                    } else {
                        theme.fg3
                    },
                )
            } else if i < full {
                ("━", used)
            } else {
                ("─", theme.line)
            }
        })
        .collect();
    runs(&cells)
}

/// 压缩的位置在 `cells` 格里的哪一格；不在格里时为 `None`。
fn tick_at(c: &Window, cells: usize) -> Option<usize> {
    c.compact_at
        .map(|a| round(a as f64 / c.max as f64 * cells as f64))
        .filter(|t| *t >= 0 && (*t as usize) < cells)
        .map(|t| t as usize)
}

/// `cells` 格里用掉的格数：`used ÷ max`，四舍五入。
pub fn filled(c: &Window, cells: usize) -> usize {
    round(c.used as f64 / c.max as f64 * cells as f64).clamp(0, cells as i64) as usize
}

/// `used ÷ max` 的整百分数。
pub fn percent(c: &Window) -> i64 {
    round(c.used as f64 / c.max as f64 * 100.0)
}

/// 卡说的比例：整百分数。
pub fn share(r: f64) -> String {
    format!("{}%", round(r * 100.0))
}

/// 相邻的同墨格并成一段。
fn runs(cells: &[(&str, Style)]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (t, st) in cells {
        match out.last_mut() {
            Some(last) if last.style == *st => last.text.to_mut().push_str(t),
            _ => out.push(Span::new((*t).to_owned(), *st)),
        }
    }
    out
}

// ---- 卡 ----

/// `   Context    `：卡上一行的内容从哪里开始。
const LABEL: usize = 11;
/// 组成的名字，按合并的次序。
const SOURCES: [(&str, &[&str]); 5] = [
    ("system", &["system_prompt", "meta_user_context"]),
    ("tools", &["tool_prompt", "system_tool_schemas"]),
    ("MCP", &["mcp_tool_schemas"]),
    ("skills", &["skills"]),
    ("messages", &["messages"]),
];
/// 条的每一级从 brand 向板混合多少。
const STEPS: [f64; 5] = [0.0, 0.3, 0.5, 0.66, 0.8];
/// 组成的第 `i` 档（按大小排的名次，[`groups`]）的墨：brand 向板的底色混合 [`STEPS`]；plain 模式是 brand、brand、fg3、fg3、
/// line。卡的条与图例、上下文的页的方格图与大纲同一档（docs/specs/rust-cli/tui-panels.md §4.2）。
pub fn step(theme: &Theme, i: usize) -> Style {
    let i = i.min(STEPS.len() - 1);
    match (theme.brand, theme.panel) {
        (Color::TrueColor(r, g, b), Some(p)) if theme.rich => {
            Style::fg(mixed(Rgb::new(r, g, b), p, STEPS[i]))
        }
        _ => [
            Style::fg(theme.brand),
            Style::fg(theme.brand),
            theme.fg3,
            theme.fg3,
            theme.line,
        ][i],
    }
}

/// 组成按 [`SOURCES`] 的名字合并，最大的在前（稳定排序，同量时按 `SOURCES` 的次序，同 Scala 的 sortBy）；为 0 的不列。
/// 名次就是 [`step`] 的档。
pub fn groups(breakdown: &[(String, u64)]) -> Vec<(&'static str, u64)> {
    let mut sums: Vec<(&'static str, u64)> = SOURCES
        .iter()
        .map(|(name, sources)| {
            let n = breakdown
                .iter()
                .filter(|(source, _)| sources.contains(&source.as_str()))
                .map(|(_, n)| *n)
                .sum();
            (*name, n)
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    sums.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    sums
}

const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// 卡的旁注要这么多列。
const SIDE_FACTS: usize = 76;

fn width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// 卡：会话的标题，然后是上下文、缓存、速度与 token。`age(key)`：那个值变了多少 tick；它从板上淡入。
pub fn card(
    theme: &Theme,
    title: &str,
    u: &Usage,
    age: &dyn Fn(&str) -> i64,
    more: &Ended,
) -> Vec<Element> {
    let ages: BTreeMap<&'static str, i64> = KEYS.iter().map(|k| (*k, age(k))).collect();
    let theme = *theme;
    let title = title.to_owned();
    let u = u.clone();
    let more = more.clone();
    // 与回答一样至多 99 列宽：更宽的终端按 101 列算（§4.2「会话的数字（/stats）」）。
    vec![by_width(move |w| {
        let card = Card {
            theme: &theme,
            ages: &ages,
            cols: w.min(MEASURE_MAX + MEASURE_MARGIN),
        };
        stack(card.rows(&title, &u, &more))
    })]
}

/// 卡的 Context 行（条、用量与压缩的位置、图例），落定的墨（没有淡入）：上下文的页在计数到达之前与模型没有窗口时画它
/// （docs/specs/rust-cli/tui-panels.md §4.2）。`cols` 是可用的宽度，同 [`card`] 至多 101 列。
pub fn context_rows(theme: &Theme, u: &Usage, cols: usize) -> Vec<Vec<Span>> {
    let ages = BTreeMap::new();
    Card {
        theme,
        ages: &ages,
        cols: cols.min(MEASURE_MAX + MEASURE_MARGIN),
    }
    .context_rows(u)
}

/// 在一个宽度上画卡要的。
struct Card<'a> {
    theme: &'a Theme,
    ages: &'a BTreeMap<&'static str, i64>,
    cols: usize,
}

impl Card<'_> {
    fn fade(&self, key: &str, spans: Vec<Span>) -> Vec<Span> {
        let age = self.ages.get(key).copied().unwrap_or(SETTLED);
        fade::fade_in(self.theme, spans, age, fade::TICKS, self.theme.panel)
    }

    fn label(&self, t: &str) -> Vec<Span> {
        vec![
            Span::new(INDENT, Style::DEFAULT),
            Span::new(pad(t, LABEL), self.theme.fg2),
        ]
    }

    fn rows(&self, title: &str, u: &Usage, more: &Ended) -> Vec<Element> {
        let theme = self.theme;
        let cols = self.cols;
        // 右边结束在最后一列之前一格。
        let edge = cols as i64 - 1;
        let line = |spans: Vec<Span>| single_line(spans, false);
        let head = {
            let mut left = vec![
                Span::new(INDENT, Style::DEFAULT),
                Span::new("Stats", theme.fg2.with_attr(attr::BOLD)),
            ];
            if !title.is_empty() {
                left.push(Span::new("   ", Style::DEFAULT));
                left.push(Span::new(
                    clip(title, (cols as i64 - 40).max(8) as usize),
                    Style::DEFAULT,
                ));
            }
            let right = if u.turns == 0 {
                Vec::new()
            } else {
                let n = if u.turns == 1 {
                    "1 turn".to_owned()
                } else {
                    format!("{} turns", u.turns)
                };
                vec![
                    Span::new(if u.complete { n } else { format!("last {n}") }, theme.fg3),
                    Span::new(" · ", theme.line),
                    Span::new(
                        format!("{} working", working(u.active_ms as f64)),
                        theme.fg3,
                    ),
                ]
            };
            let gap = edge - width(&left) as i64 - width(&right) as i64;
            if !right.is_empty() && gap >= 3 {
                left.push(Span::new(" ".repeat(gap as usize), Style::DEFAULT));
                left.extend(right);
            }
            line(left)
        };
        let context: Vec<Element> = self.context_rows(u).into_iter().map(line).collect();
        let cache: Vec<Element> = u
            .cache
            .iter()
            .map(|c| {
                let mut spans = vec![
                    Span::new(share(c.hit_rate), Style::DEFAULT),
                    Span::new(" of input from cache", theme.fg3),
                ];
                if let Some(l) = c.last_call.filter(|_| cols >= SIDE_FACTS) {
                    spans.push(Span::new("   last call ", theme.fg3));
                    spans.push(Span::new(share(l), theme.fg2));
                }
                let mut row = self.label("Cache");
                row.extend(self.fade("cache", spans));
                line(row)
            })
            .collect();
        let speed: Vec<Element> = u
            .last_speed
            .iter()
            .map(|s| {
                let turns = &u.speeds;
                let keep = 4.max(cols as i64 - 40) as usize;
                let shown = &turns[turns.len().saturating_sub(keep)..];
                let top = shown.iter().copied().reduce(f64::max).unwrap_or(1.0);
                let older = match theme.brand {
                    Color::TrueColor(r, g, b) if theme.rich => theme
                        .panel
                        .map_or(theme.fg3, |p| Style::fg(mixed(Rgb::new(r, g, b), p, 0.45))),
                    _ => theme.fg3,
                };
                let mut spans: Vec<Span> = shown
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let h = round(v / top * 8.0).clamp(1, 8) as usize;
                        Span::new(
                            EIGHTHS[h].to_string(),
                            if i == shown.len() - 1 {
                                Style::fg(theme.brand)
                            } else {
                                older
                            },
                        )
                    })
                    .collect();
                if !spans.is_empty() {
                    spans.push(Span::new("   ", Style::DEFAULT));
                }
                spans.push(Span::new(rate(*s), Style::DEFAULT));
                spans.push(Span::new(" tok/s last call", theme.fg3));
                if cols >= SIDE_FACTS && !shown.is_empty() {
                    spans.push(Span::new("   median ", theme.fg3));
                    spans.push(Span::new(rate(middle(turns)), theme.fg2));
                }
                let mut row = self.label("Speed");
                row.extend(self.fade("speed", spans));
                line(row)
            })
            .collect();
        let mut tokens = self.label("Tokens");
        tokens.extend(self.fade(
            "tokens",
            vec![
                Span::new(count(u.input as f64), Style::DEFAULT),
                Span::new(" in", theme.fg3),
                Span::new("  ·  ", theme.line),
                Span::new(count(u.output as f64), Style::DEFAULT),
                Span::new(" out", theme.fg3),
            ],
        ));
        let mut out = vec![head, br()];
        out.extend(context);
        out.push(br());
        out.extend(cache);
        out.extend(speed);
        out.push(line(tokens));
        out.extend(self.others(u, more).into_iter().map(line));
        out
    }

    /// Tokens 之后的行（§4.2「会话的数字（/stats）」）：结束了的子代理，有它时还有合计。
    fn others(&self, u: &Usage, more: &Ended) -> Vec<Vec<Span>> {
        let theme = self.theme;
        let dot = || Span::new("  ·  ", theme.line);
        let e = more;
        if e.count == 0 {
            return Vec::new();
        }
        let mut parts = vec![vec![
            Span::new(e.count.to_string(), Style::DEFAULT),
            Span::new(" ran", theme.fg3),
        ]];
        if e.tokens > 0.0 {
            parts.push(vec![
                Span::new(count(e.tokens), Style::DEFAULT),
                Span::new(" tokens", theme.fg3),
            ]);
        }
        if e.work_ms > 0.0 {
            parts.push(vec![
                Span::new(working(e.work_ms), Style::DEFAULT),
                Span::new(" of work", theme.fg3),
            ]);
        }
        let mut spans: Vec<Span> = Vec::new();
        for (i, p) in parts.into_iter().enumerate() {
            if i > 0 {
                spans.push(dot());
            }
            spans.extend(p);
        }
        if !e.speeds.is_empty() {
            spans.push(Span::new("   median ", theme.fg3));
            spans.push(Span::new(rate(middle(&e.speeds)), theme.fg2));
            spans.push(Span::new(" tok/s", theme.fg3));
        }
        let mut subagents = self.label("Subagents");
        subagents.extend(self.fade("subagents", spans));
        let own = u.input as f64 + u.output as f64;
        let mut all = vec![
            Span::new(count(own + e.tokens), Style::DEFAULT),
            Span::new(" tokens", theme.fg3),
            Span::new("   ", Style::DEFAULT),
        ];
        let split = [("this session", own), ("subagents", e.tokens)];
        for (i, (name, n)) in split.iter().filter(|(_, n)| *n > 0.0).enumerate() {
            if i > 0 {
                all.push(Span::new(" · ", theme.line));
            }
            all.push(Span::new(format!("{name} "), theme.fg3));
            all.push(Span::new(count(*n), theme.fg2));
        }
        let mut in_all = self.label("In all");
        in_all.extend(self.fade("inall", all));
        vec![subagents, in_all]
    }

    /// 上下文一行与它的图例；没有窗口时说知道的。
    fn context_rows(&self, u: &Usage) -> Vec<Vec<Span>> {
        let theme = self.theme;
        let cols = self.cols as i64;
        let Some(c) = &u.context else {
            let mut known: Vec<Span> = u.last_call_tokens.map_or_else(Vec::new, |n| {
                vec![
                    Span::new(count(n as f64), Style::DEFAULT),
                    Span::new(" tokens", theme.fg3),
                    Span::new("   ", Style::DEFAULT),
                ]
            });
            known.push(Span::new("the model does not say its window", theme.fg3));
            let mut row = self.label("Context");
            row.extend(self.fade("context", known));
            return vec![row];
        };
        let edge = cols - 1;
        let bw = (cols - 52).clamp(10, 32) as usize;
        let used_cells = filled(c, bw);
        let tick = tick_at(c, bw);
        // 组成按名字合并，最大的在前；不到 1 % 的不进图例。
        let sums = groups(&c.breakdown);
        let total: f64 = sums.iter().map(|(_, n)| *n as f64).sum();
        let shares: Vec<(&str, f64)> = sums
            .into_iter()
            .map(|(name, n)| (name, if total > 0.0 { n as f64 / total } else { 0.0 }))
            .collect();
        let step = |i: usize| step(theme, i);
        let mut bar: Vec<(&str, Style)> = Vec::new();
        let mut at = 0;
        for (i, (_, sh)) in shares.iter().enumerate() {
            let n = round(sh * used_cells as f64).max(0) as usize;
            let mut k = 0;
            while k < n && at < used_cells {
                bar.push(("█", step(i)));
                k += 1;
                at += 1;
            }
        }
        while at < used_cells {
            bar.push(("█", step(0)));
            at += 1;
        }
        for i in used_cells..bw {
            bar.push(if tick == Some(i) {
                ("│", theme.fg3)
            } else {
                ("─", theme.line)
            });
        }
        let mut head = self.label("Context");
        head.push(Span::new(" ".repeat(bw), Style::DEFAULT));
        let amount = vec![
            Span::new("   ", Style::DEFAULT),
            Span::new(count(c.used as f64), Style::DEFAULT),
            Span::new(" of ", theme.fg3),
            Span::new(count(c.max as f64), theme.fg2),
        ];
        let compacts = c.compact_at.map_or_else(Vec::new, |a| {
            vec![
                Span::new("   compacts at ", theme.fg3),
                Span::new(count(a as f64), theme.fg2),
            ]
        });
        let (hw, aw, cw) = (
            width(&head) as i64,
            width(&amount) as i64,
            width(&compacts) as i64,
        );
        let tail: Vec<Span> = if hw + aw + cw <= edge {
            amount.into_iter().chain(compacts).collect()
        } else if hw + aw <= edge {
            amount
        } else {
            Vec::new()
        };
        let legend: Vec<Vec<Span>> = shares
            .iter()
            .enumerate()
            .filter(|(_, (_, sh))| *sh >= 0.01)
            .map(|(i, (name, sh))| {
                vec![
                    Span::new("■", step(i)),
                    Span::new(format!(" {name}"), theme.fg3),
                    Span::new(format!(" {}%", round(sh * 100.0)), theme.fg2),
                ]
            })
            .collect();
        let room = edge - INDENT.len() as i64 - LABEL as i64;
        let mut legend_rows: Vec<Vec<Span>> = Vec::new();
        for it in legend {
            match legend_rows.last_mut() {
                Some(last) if width(last) as i64 + 3 + width(&it) as i64 <= room => {
                    last.push(Span::new("   ", Style::DEFAULT));
                    last.extend(it);
                }
                _ => legend_rows.push(it),
            }
        }
        let mut first = self.label("Context");
        let mut body = runs(&bar);
        body.extend(tail);
        first.extend(self.fade("context", body));
        let pad = format!("{INDENT}{}", " ".repeat(LABEL));
        std::iter::once(first)
            .chain(legend_rows.into_iter().map(|r| {
                let mut row = vec![Span::new(pad.clone(), Style::DEFAULT)];
                row.extend(self.fade("context", r));
                row
            }))
            .collect()
    }
}

/// `ms` 的工作：一分钟以内是秒，一小时以内是整分钟，再长是时与分。
pub fn working(ms: f64) -> String {
    let secs = round(ms / 1000.0).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {:02}m", secs / 3600, secs % 3600 / 60)
    }
}

/// `xs` 的中位数（不为空）。
fn middle(xs: &[f64]) -> f64 {
    let mut s = xs.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

fn mixed(from: Rgb, to: Color, t: f64) -> Color {
    match to {
        Color::TrueColor(r, g, b) => from.mix(Rgb::new(r, g, b), t).color(),
        _ => from.color(),
    }
}
