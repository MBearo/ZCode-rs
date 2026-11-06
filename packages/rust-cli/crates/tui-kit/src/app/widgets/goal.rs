//! 目标（docs/puffin-tui.md §4.2「目标」；docs/specs/rust-cli/tui-kit.md §4.4 K5）：会话的目标的数据，每一轮末尾的检查行
//! （`✓` / `✗` / `◎` 与结果，下面竖线上的理由与 `Next`）、恢复的一轮的开头一行、状态栏上目标的记号与 `/goal` 打开的卡。
//! 只画；时钟在壳里。检查行与开头一行的入参是中性的 [`Check`] 与 [`Resumed`]。
//! 移植自 Puffin 的 shell/goals.rs（Goals.scala 第 1–228 行）；目标的数据（`SessionGoal`、`GoalState`、
//! `GoalRound`、`GoalVerdict`）从 Puffin 的投影（host/projection.rs）搬来。按条目认目标的 `is_goal`、
//! `rings` 留在壳。
use serde::{Deserialize, Serialize};

use crate::app::element::{
    Clip, ClipFocus, Element, Wrap, br, by_width, measure, single_line, stack, wrap_text,
};
use crate::app::widgets::fade;
use crate::app::widgets::fit::{MEASURE_MARGIN, MEASURE_MAX, clip, pad, thought_time};
use crate::app::widgets::gather::pages;
use crate::markdown::render::{INDENT, SETTLED};
use crate::render::ansi::RESET;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

/// 会话的当前目标（docs/puffin-tui.md §5 `goal`；由 KernelHost 从 `GoalView` 换算，见 `fold::goal_of`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionGoal {
    pub target_id: String,
    pub objective: String,
    /// 短名；没有时为 ""。
    pub title: String,
    /// 内核的状态原样（`active`、`verified`、`failed`）。
    pub status: String,
    pub state: GoalState,
    /// 当前一轮，从 1 起。
    pub round: u32,
    pub time_used_seconds: u64,
    /// 正在进行的这一段的开始时刻（epoch ms）；没有时为 0。
    pub running_since: u64,
    pub rounds: Vec<GoalRound>,
    /// 最近一次检查的理由；没有时为 ""。
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalState {
    #[default]
    Working,
    Checking,
    Paused,
    Met,
    Stalled,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GoalRound {
    pub round: u32,
    pub step: String,
    pub verdict: GoalVerdict,
    pub done: u32,
    pub total: u32,
}

/// 一轮检查的结论；`None`（JSON 为 ""）是还没检查过。`Checking` 只出现在 `goalCheck` 条目上。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalVerdict {
    #[default]
    #[serde(rename = "")]
    None,
    Checking,
    Met,
    NotYet,
    Stalled,
    Failed,
    Stopped,
}

/// 一轮末尾的一次检查（投影的 `GoalCheckItem` 的中性写法）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Check<'a> {
    pub round: u32,
    pub verdict: GoalVerdict,
    pub title: &'a str,
    pub reason: &'a str,
    pub next: &'a str,
    pub time_used_seconds: u64,
}

/// `/goal resume` 开始的一轮（投影的 `GoalResumedItem` 的中性写法）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resumed<'a> {
    pub round: u32,
    pub title: &'a str,
}

/// 检查的理由先显示这么多行，其余说成 `… N more lines`。
const REASON_ROWS: usize = 4;
/// 卡列出的轮数；更早的是一行 `↑ n earlier rounds`。
const CARD_ROUNDS: usize = 8;
/// 卡上目标的行数。
const OBJECTIVE_ROWS: usize = 3;
/// `   Goal   `：卡上目标从这一列开始。
const CARD_LEAD: usize = 10;
/// 检查开始这么久之后 `checking` 后面接上计时。
const TIMER_AFTER_MS: f64 = 3000.0;

/// 一个目标行的动效：出现与检查给出结论的 tick，与第一次见到的墙钟毫秒（检查的计时用）。快照里的行没有 tick。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct GoalClock {
    pub arrived: Option<i64>,
    pub decided: Option<i64>,
    pub since: f64,
}

/// 它的动效在 `now` 结束了：可以提交。
pub fn settled(clock: &GoalClock, now: i64) -> bool {
    clock
        .arrived
        .into_iter()
        .chain(clock.decided)
        .all(|t| now - t >= fade::TICKS)
}

/// 目标在进行：在做一轮或在检查一轮（Scala `GoalInfo.running`）。
pub fn running(goal: &SessionGoal) -> bool {
    matches!(goal.state, GoalState::Working | GoalState::Checking)
}

// ---- 检查行 ----

/// 检查行上结论的写法（`GoalVerdict::None` 按 Scala 的缺省 `checking`）。
fn verdict_of(it: &Check<'_>) -> GoalVerdict {
    match it.verdict {
        GoalVerdict::None => GoalVerdict::Checking,
        v => v,
    }
}

/// 一个结论的记号，检查行与卡都这样画。
pub fn mark(theme: &Theme, verdict: GoalVerdict) -> Span {
    match verdict {
        GoalVerdict::Met => Span::new("✓", Style::fg(theme.ok)),
        GoalVerdict::NotYet => Span::new("◎", Style::fg(theme.brand)),
        GoalVerdict::Stalled => Span::new("◎", theme.fg3),
        GoalVerdict::Failed => Span::new("✗", Style::fg(theme.err)),
        GoalVerdict::Stopped => Span::new("–", theme.fg3),
        GoalVerdict::None | GoalVerdict::Checking => Span::new("○", theme.fg3),
    }
}

/// 结果：开头的字用记号的墨色，其余 fg3。
fn outcome(theme: &Theme, it: &Check<'_>, elapsed_ms: f64) -> Vec<Span> {
    let round = format!(" · round {}", it.round);
    match verdict_of(it) {
        GoalVerdict::None | GoalVerdict::Checking => {
            let mut out = vec![Span::new("checking", theme.fg3)];
            if elapsed_ms >= TIMER_AFTER_MS {
                out.push(Span::new(
                    format!(" · {}", thought_time(elapsed_ms)),
                    theme.fg3,
                ));
            }
            out
        }
        GoalVerdict::NotYet => vec![
            Span::new("not yet", Style::fg(theme.brand)),
            Span::new(round, theme.fg3),
        ],
        GoalVerdict::Met => {
            let secs = it.time_used_seconds;
            let time = if secs > 0 {
                format!(" · {}", thought_time(secs as f64 * 1000.0))
            } else {
                String::new()
            };
            vec![
                Span::new("met", Style::fg(theme.ok)),
                Span::new(format!("{round}{time}"), theme.fg3),
            ]
        }
        GoalVerdict::Stalled => vec![Span::new("not met", theme.fg2), Span::new(round, theme.fg3)],
        GoalVerdict::Failed => vec![
            Span::new("check failed", Style::fg(theme.err)),
            Span::new(round, theme.fg3),
        ],
        GoalVerdict::Stopped => vec![
            Span::new("check stopped", theme.fg3),
            Span::new(round, theme.fg3),
        ],
    }
}

/// 一次检查与它下面说的话。`clock` 给出它在 tick `now` 的动效；`breath` 给出时是它的 `○` 借的工作光标的墨色；`open`（^O）
/// 显示整个理由；`now_ms` 走检查的计时。
pub fn check(
    theme: &Theme,
    it: &Check<'_>,
    clock: &GoalClock,
    now: i64,
    now_ms: f64,
    breath: Option<Style>,
    open: bool,
) -> Element {
    let verdict = verdict_of(it);
    let arrived = clock.arrived.map_or(SETTLED, |t| now - t);
    let decided = clock.decided.map_or(SETTLED, |t| now - t);
    let base = mark(theme, verdict);
    // 结果到达时同工具调用的落定：有颜色的标记从 fg3 渐变过去，结果的字与行下面的各行淡入。
    let mark_span = match verdict {
        GoalVerdict::None | GoalVerdict::Checking => breath.map_or(base, |st| Span::new("○", st)),
        GoalVerdict::Stalled | GoalVerdict::Stopped => base,
        _ => Span::new(
            base.text.clone(),
            fade::ink(
                theme,
                theme.fg3.fg_color(),
                base.style,
                decided,
                fade::TICKS,
            ),
        ),
    };
    let name = Span::new(
        "Goal",
        if verdict == GoalVerdict::Stopped {
            theme.fg3
        } else {
            theme.fg2
        }
        .with_attr(attr::BOLD),
    );
    let elapsed = if clock.since > 0.0 {
        now_ms - clock.since
    } else {
        0.0
    };
    let what = outcome(theme, it, elapsed);
    let checking = matches!(verdict, GoalVerdict::None | GoalVerdict::Checking);
    let what_shown = if checking {
        what.clone()
    } else {
        fade::fade_in(theme, what.clone(), decided, fade::TICKS, None)
    };
    // 标题按 `check stopped · round N` 与当前结果中较长的一种留位：结果换字时标题不动。
    let reserve = width(&what).max(string_width(&format!("check stopped · round {}", it.round)));
    let lead = fade::fade_in(theme, vec![mark_span, name], arrived, fade::TICKS, None);
    let head = row(
        theme,
        lead,
        it.title.to_owned(),
        what_shown,
        reserve,
        arrived,
    );
    let under = rail(theme, it, verdict, open, decided);
    if under.is_empty() {
        head
    } else {
        stack(std::iter::once(head).chain(under))
    }
}

/// `◎ Goal  标题  resumed · round 3`：`/goal resume` 开始的一轮。
pub fn resumed(theme: &Theme, it: &Resumed<'_>, age: i64) -> Element {
    let what = vec![
        Span::new("resumed", Style::fg(theme.brand)),
        Span::new(format!(" · round {}", it.round), theme.fg3),
    ];
    let reserve = width(&what);
    let lead = fade::fade_in(
        theme,
        vec![
            Span::new("◎", Style::fg(theme.brand)),
            Span::new("Goal", theme.fg2.with_attr(attr::BOLD)),
        ],
        age,
        fade::TICKS,
        None,
    );
    let shown = fade::fade_in(theme, what, age, fade::TICKS, None);
    row(theme, lead, it.title.to_owned(), shown, reserve, age)
}

/// 一行：记号、`Goal`、标题与结果。放不下时先用 `…` 截标题，再去掉它；结果保持完整。
fn row(
    theme: &Theme,
    lead0: Vec<Span>,
    title: String,
    what: Vec<Span>,
    reserve: usize,
    age: i64,
) -> Element {
    let mut it = lead0.into_iter();
    let (m, name) = (
        it.next().unwrap_or_else(|| Span::plain("")),
        it.next().unwrap_or_else(|| Span::plain("")),
    );
    let lead = vec![
        Span::new(" ", Style::DEFAULT),
        m,
        Span::new(" ", Style::DEFAULT),
        name,
    ];
    let tail: Vec<Span> = std::iter::once(Span::new("  ", Style::DEFAULT))
        .chain(what.iter().cloned())
        .collect();
    let fixed = width(&lead) + 2 + reserve.max(width(&what));
    let theme = *theme;
    by_width(move |w| {
        let room = w as i64 - fixed as i64 - 2;
        let shown = if (string_width(&title) as i64) <= room {
            title.clone()
        } else if room >= 8 {
            cut(&title, room as usize)
        } else {
            String::new()
        };
        let mut spans = lead.clone();
        if !shown.is_empty() {
            spans.extend(fade::fade_in(
                &theme,
                vec![Span::new("  ", Style::DEFAULT), Span::new(shown, theme.fg3)],
                age,
                fade::TICKS,
                None,
            ));
        }
        spans.extend(tail.iter().cloned());
        single_line(spans, false)
    })
}

/// 检查在下面说的：理由，然后下一步或现在该做什么。
fn rail(theme: &Theme, it: &Check<'_>, verdict: GoalVerdict, open: bool, age: i64) -> Vec<Element> {
    let ink = |st: Style| -> Style {
        fade::fade_in(theme, vec![Span::new("", st)], age, fade::TICKS, None)
            .first()
            .map_or(st, |s| s.style)
    };
    let lead = format!("{INDENT}{}│{RESET} ", ink(theme.line).set_sequence());
    let lead_spans = vec![
        Span::new(INDENT, Style::DEFAULT),
        Span::new("│", ink(theme.line)),
        Span::new(" ", Style::DEFAULT),
    ];
    let reason = it.reason.replace('\r', "").replace('\t', "    ");
    let paras: Vec<&str> = reason
        .split('\n')
        .filter(|p| !p.trim().is_empty())
        .collect();
    let mut out = Vec::new();
    if !paras.is_empty()
        && !matches!(
            verdict,
            GoalVerdict::Stopped | GoalVerdict::Checking | GoalVerdict::None
        )
    {
        let fg2 = ink(theme.fg2).set_sequence();
        let rows = stack(paras.iter().map(|p| {
            wrap_text(
                lead.clone(),
                lead.clone(),
                format!("{fg2}{p}{RESET}"),
                Wrap::Word,
            )
        }));
        let body = if open {
            rows
        } else {
            Clip::new(rows, REASON_ROWS, ClipFocus::Head)
                .with_lead(lead_spans.clone())
                .with_ink(ink(theme.fg3))
                .into()
        };
        out.push(measure(body, MEASURE_MAX, MEASURE_MARGIN));
    }
    let resume = |before: &str| -> Element {
        let mut spans = lead_spans.clone();
        spans.extend([
            Span::new(before.to_owned(), ink(theme.fg3)),
            Span::new("/goal resume", ink(theme.fg2)),
            Span::new(" tries again.", ink(theme.fg3)),
        ]);
        measure(single_line(spans, false), MEASURE_MAX, MEASURE_MARGIN)
    };
    match verdict {
        GoalVerdict::NotYet if !it.next.trim().is_empty() => {
            let next = it.next.replace('\n', " ").trim().to_owned();
            out.push(measure(
                wrap_text(
                    format!("{lead}{}Next{RESET}  ", ink(theme.fg3).set_sequence()),
                    format!("{lead}      "),
                    format!("{}{next}{RESET}", ink(theme.fg2).set_sequence()),
                    Wrap::Word,
                ),
                MEASURE_MAX,
                MEASURE_MARGIN,
            ));
        }
        GoalVerdict::Stalled => out.push(resume("The goal stopped here. ")),
        GoalVerdict::Failed => out.push(resume("")),
        _ => {}
    }
    out
}

// ---- 状态栏 ----

/// 状态栏上的一段：（文字，样式）的片段。
pub type Parts = Vec<(String, Style)>;

/// 模式后面目标的记号，与它等你时的字（docs/puffin-tui.md §4.2「目标」的「状态栏」）。`met_seen`：达成的目标的字随下一次
/// 发送消失了。
pub fn status_parts(theme: &Theme, goal: &SessionGoal, met_seen: bool) -> (Parts, Parts) {
    let one = |t: &str, st: Style| vec![(t.to_owned(), st)];
    match goal.state {
        GoalState::Paused => (one("◎", theme.fg3), one(" goal paused", theme.fg3)),
        GoalState::Stalled => (one("◎", theme.fg3), one(" goal not met", theme.fg2)),
        GoalState::Failed => (
            one("◎", theme.fg3),
            one(" goal check failed", Style::fg(theme.err)),
        ),
        GoalState::Met if met_seen => (Vec::new(), Vec::new()),
        GoalState::Met => (one("✓", Style::fg(theme.ok)), one(" goal met", theme.fg2)),
        GoalState::Working | GoalState::Checking => (one("◎", Style::fg(theme.brand)), Vec::new()),
    }
}

/// 达成的目标的字按这个键记下见过：发送时它们消失。
pub fn met_key(goal: &SessionGoal) -> String {
    format!("{}#{}", goal.target_id, goal.round)
}

// ---- 卡 ----

/// ⏎ 在卡上做什么：`pause`、`resume`，或什么都不做。
pub fn enter_does(goal: Option<&SessionGoal>) -> Option<&'static str> {
    match goal.map(|g| g.state) {
        Some(GoalState::Working | GoalState::Checking) => Some("pause"),
        Some(GoalState::Paused | GoalState::Stalled | GoalState::Failed) => Some("resume"),
        _ => None,
    }
}

/// 卡的键，槽从中挑选。
pub fn card_keys(goal: Option<&SessionGoal>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = enter_does(goal)
        .map(|d| ("⏎".to_owned(), d.to_owned()))
        .into_iter()
        .collect();
    out.push(("esc".to_owned(), "close".to_owned()));
    out
}

/// 目标跑了多久：agent 记的时间加上现在正在跑的这一段。
pub fn time_used(goal: &SessionGoal, now_ms: f64) -> f64 {
    let running = if goal.running_since > 0 {
        (now_ms - goal.running_since as f64).max(0.0)
    } else {
        0.0
    };
    goal.time_used_seconds as f64 * 1000.0 + running
}

/// 卡：目标、它到了哪里、每一轮一行。
pub fn card(theme: &Theme, goal: Option<&SessionGoal>, now_ms: f64) -> Vec<Element> {
    let indent = Span::new(INDENT, Style::DEFAULT);
    let line = |spans: Vec<Span>| measure(single_line(spans, false), MEASURE_MAX, MEASURE_MARGIN);
    let Some(g) = goal else {
        return vec![line(vec![
            indent,
            Span::new("No goal is set. ", theme.fg3),
            Span::new("/goal", Style::fg(theme.brand)),
            Span::new(" and what you want done sets one.", theme.fg3),
        ])];
    };
    let pad_span = Span::new(" ".repeat(CARD_LEAD), Style::DEFAULT);
    let th = *theme;
    let objective_text = g
        .objective
        .replace('\r', "")
        .replace('\n', " ")
        .trim()
        .to_owned();
    let (ind, padded) = (indent.clone(), pad_span.clone());
    let objective = measure(
        by_width(move |w| {
            let text = &objective_text;
            let room = w.saturating_sub(CARD_LEAD).max(1);
            let pgs = pages(text, room);
            let shown: Vec<String> = pgs
                .iter()
                .take(OBJECTIVE_ROWS)
                .enumerate()
                .map(|(i, (from, to))| {
                    let words = &text[*from..*to];
                    let last = i == OBJECTIVE_ROWS - 1 && pgs.len() > OBJECTIVE_ROWS;
                    if !last {
                        words.to_owned()
                    } else if string_width(words) + 2 <= room {
                        format!("{words} …")
                    } else {
                        clip(words, room)
                    }
                })
                .collect();
            stack(shown.iter().enumerate().map(|(i, t)| {
                let mut spans = if i == 0 {
                    vec![
                        ind.clone(),
                        Span::new("Goal", th.fg2.with_attr(attr::BOLD)),
                        Span::new("   ", Style::DEFAULT),
                    ]
                } else {
                    vec![padded.clone()]
                };
                spans.push(Span::new(fitted(t, room), Style::DEFAULT));
                single_line(spans, false)
            }))
        }),
        MEASURE_MAX,
        MEASURE_MARGIN,
    );
    let (word, word_ink) = match g.state {
        GoalState::Checking => ("checking", Style::fg(theme.brand)),
        GoalState::Paused => ("paused", theme.fg3),
        GoalState::Met => ("met", Style::fg(theme.ok)),
        GoalState::Failed => ("check failed", Style::fg(theme.err)),
        GoalState::Stalled => ("not met", theme.fg2),
        GoalState::Working => ("working", Style::fg(theme.brand)),
    };
    let stands = line(vec![
        pad_span,
        Span::new(format!("round {}", g.round), theme.fg3),
        Span::new(" · ", theme.line),
        Span::new(word, word_ink),
        Span::new(" · ", theme.line),
        Span::new(thought_time(time_used(g, now_ms)), theme.fg3),
    ]);
    let shown_rounds: Vec<_> = if g.rounds.len() > CARD_ROUNDS {
        g.rounds[g.rounds.len() - (CARD_ROUNDS - 1)..].to_vec()
    } else {
        g.rounds.clone()
    };
    let earlier = g.rounds.len() - shown_rounds.len();
    let (current_round, paused) = (g.round, g.state == GoalState::Paused);
    let rounds = measure(
        by_width(move |w| {
            let step_max = w.saturating_sub(16).max(8);
            let step_w = step_max.min(
                shown_rounds
                    .iter()
                    .map(|r| string_width(&r.step))
                    .max()
                    .unwrap_or(0),
            );
            let mut rows = Vec::new();
            if earlier > 0 {
                rows.push(single_line(
                    vec![
                        indent.clone(),
                        Span::new(
                            format!(
                                "↑ {earlier} earlier round{}",
                                if earlier == 1 { "" } else { "s" }
                            ),
                            th.fg3,
                        ),
                    ],
                    false,
                ));
            }
            for r in &shown_rounds {
                let current = r.round == current_round;
                let m = if r.verdict != GoalVerdict::None {
                    mark(&th, r.verdict)
                } else {
                    Span::new("○", if paused { th.fg3 } else { th.fg2 })
                };
                let step = Span::new(
                    pad(&clip(&r.step, step_w), step_w),
                    if current { Style::DEFAULT } else { th.fg2 },
                );
                let mut spans = vec![
                    indent.clone(),
                    Span::new(format!("{:>2}", r.round), th.fg3),
                    Span::new("  ", Style::DEFAULT),
                    m,
                    Span::new("  ", Style::DEFAULT),
                    step,
                ];
                if r.total > 0 {
                    spans.push(Span::new("   ", Style::DEFAULT));
                    spans.push(Span::new(
                        format!("{:>4}", format!("{}/{}", r.done, r.total)),
                        th.fg3,
                    ));
                }
                rows.push(single_line(spans, false));
            }
            stack(rows)
        }),
        MEASURE_MAX,
        MEASURE_MARGIN,
    );
    let mut out = vec![objective, stands, br(), rounds];
    if g.state == GoalState::Failed && !g.reason.is_empty() {
        out.push(br());
        out.push(measure(
            wrap_text(
                INDENT,
                INDENT,
                format!(
                    "{}{}{RESET}",
                    theme.fg3.with_attr(attr::ITALIC).set_sequence(),
                    g.reason.replace('\n', " ")
                ),
                Wrap::Word,
            ),
            MEASURE_MAX,
            MEASURE_MARGIN,
        ));
    }
    out
}

/// `text` 至多 `cols` 格：比一行长的词用 `…` 截。
fn fitted(text: &str, cols: usize) -> String {
    if string_width(text) <= cols {
        text.to_owned()
    } else {
        clip(text, cols)
    }
}

/// `text` 截到 `cols` 格，以 `…` 结尾，前面不留空格。
fn cut(text: &str, cols: usize) -> String {
    let clipped = clip(text, cols);
    let body = clipped.strip_suffix('…').unwrap_or(&clipped);
    format!("{}…", body.trim_end())
}

fn width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}
