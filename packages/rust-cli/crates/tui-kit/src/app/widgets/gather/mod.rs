//! 一段工作收成一行（docs/puffin-tui.md §4.2「工作的收拢（Gather）」；docs/specs/rust-cli/tui-kit.md §4.4）：
//! `✓ Thought for 9s, explored 4 files, edited src/x.ts +6 −3, ran 1 command`，从不折行；进行中时下面是此刻的那一行
//! （`└`、四格的槽与在做的事），一批调用时是名册。纯的：界面保存动效与播放头；[`tools::tally`] 数调用。入参是中性的
//! [`Member`]（一段思考或一个 [`Call`]）。
//! 移植自 Puffin 的 shell/gather.rs 的纯函数部分（Gather.scala 第 1–308 行）。
use crate::app::element::{Element, by_width, single_line, stack};
use crate::markdown::render::{INDENT, SETTLED};
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::ease::Ink;
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{cut, measured, width};
use super::pen::{self, Pen};
use super::playhead::{self, Playhead};
use super::roster;
use super::subagent_cols::{self, Cells};
use super::tool_row::{self, ToolClock, ToolCtx};
use super::tools::{self, Call, Status};

pub mod words;

pub use words::{GATHER_WORDS, GatherWords};
use words::{clauses, fit as fit_words};

/// 一段思考：条目 id、到此的文字、是否结束、想了多久（毫秒）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thought<'a> {
    pub id: &'a str,
    pub text: &'a str,
    pub ended: bool,
    pub duration_ms: Option<u64>,
}

/// 一段工作里的一个条目：思考或调用。
#[derive(Clone, Copy, Debug, PartialEq)]
// 每帧现借现用的视图，不存起来：装箱只多出分配。
#[allow(clippy::large_enum_variant)]
pub enum Member<'a> {
    Thought(Thought<'a>),
    Call(Call<'a>),
}

impl<'a> Member<'a> {
    pub fn id(&self) -> &'a str {
        match self {
            Member::Thought(t) => t.id,
            Member::Call(c) => c.id,
        }
    }

    pub fn ended(&self) -> bool {
        match self {
            Member::Thought(t) => t.ended,
            Member::Call(c) => c.ended,
        }
    }

    pub fn call(&self) -> Option<&Call<'a>> {
        match self {
            Member::Call(c) => Some(c),
            Member::Thought(_) => None,
        }
    }

    fn is(&self, s: Status) -> bool {
        self.call().is_some_and(|c| c.status == s)
    }
}

/// 一段里有东西要画：一段思考，或一个不是管道的调用。
pub fn shows(members: &[Member<'_>]) -> bool {
    members.iter().any(|m| match m {
        Member::Thought(_) => true,
        Member::Call(c) => !tools::transparent(c),
    })
}

fn sp(text: impl Into<String>, style: Style) -> Span {
    Span::new(text.into(), style)
}

/// 思考的跑马灯至少这么多格，否则不显示。
const TICKER_MIN: i64 = 8;

/// 此刻在做的那一个，画法要的值都拷出来。
#[derive(Clone)]
enum Now {
    /// 一个调用：名称、输入是否还在写出、对象、在跑的时间、它到达的 tick 之后多久、子代理的格子。
    Call {
        name: Span,
        writing: bool,
        target: String,
        live: Vec<Span>,
        arrived: i64,
        cells: Option<Cells>,
    },
    /// 一段思考：文字、显示到哪里与它的播放头。
    Thought {
        text: String,
        shown: usize,
        head: Option<Playhead>,
    },
}

impl Now {
    /// 此刻在做的事，至多 `room` 格。
    fn doing(&self, theme: &Theme, room: i64, w: &GatherWords) -> Vec<Span> {
        match self {
            Now::Thought { text, shown, head } => {
                let lead = thinking_lead(theme, w);
                // 还在写的思考的字标成 agent 写的：最新的词落定到 fg3。
                let ink = if head.is_some() {
                    theme.fg3.with_attr(attr::WRITTEN)
                } else {
                    theme.fg3
                };
                if room - width(&lead) as i64 >= TICKER_MIN {
                    let mut v = lead;
                    v.push(sp(thought_words(text, *shown, room, theme, w), ink));
                    v
                } else {
                    cut(&lead[..1], room)
                }
            }
            Now::Call {
                name,
                writing,
                target,
                live,
                arrived,
                ..
            } => {
                // 输入还在写出：只有名称，还没有对象（§4.2「笔」的「调用」）。
                if *writing {
                    return cut(std::slice::from_ref(name), room);
                }
                let mut label = vec![name.clone()];
                if !target.is_empty() {
                    label.push(sp(format!("  {target}"), theme.fg3));
                }
                label.extend(live.iter().cloned());
                // 换成下一个调用时新的立即换上并淡入（§4.2「工具调用」的「折入」）。
                fade::fade_in(theme, cut(&label, room), *arrived, fade::LAND_TICKS, None)
            }
        }
    }
}

fn thinking_lead(theme: &Theme, w: &GatherWords) -> Vec<Span> {
    vec![
        sp(w.thinking, theme.fg3.with_attr(attr::ITALIC)),
        sp(" · ", theme.line),
    ]
}

/// `Thinking · ` 之后思考的字，两者至多 `room` 格。
fn thought_words(text: &str, shown: usize, room: i64, theme: &Theme, w: &GatherWords) -> String {
    ticker(text, shown, room - width(&thinking_lead(theme, w)) as i64)
}

/// 一行，进行中时它下面是此刻在做的事的一行（§4.2「工作的收拢」的「进行中」）与最新的在跑的调用挂在下面的东西。
///
/// - `clocks`：每个调用（按 id）的动效。
/// - `gathered`：这段结束的时刻，给标记渐变；进行中时没有。
/// - `caret`：笔骑着这一段时的笔；扣住的笔只点亮一批的末行。
/// - `playhead`：思考（按 id）的播放头，它显示到哪里。
#[allow(clippy::too_many_arguments)]
pub fn line(
    ctx: &ToolCtx<'_>,
    members: &[Member<'_>],
    clocks: &dyn Fn(&str) -> ToolClock,
    gathered: Option<i64>,
    closed: bool,
    caret: Option<&Pen>,
    playhead: &dyn Fn(&str) -> Option<Playhead>,
) -> Element {
    line_said(
        ctx,
        members,
        clocks,
        gathered,
        closed,
        caret,
        playhead,
        &GATHER_WORDS,
    )
}

/// [`line`]，句子的字是 `w` 的（别的界面语言）。
#[allow(clippy::too_many_arguments)]
pub fn line_said(
    ctx: &ToolCtx<'_>,
    members: &[Member<'_>],
    clocks: &dyn Fn(&str) -> ToolClock,
    gathered: Option<i64>,
    closed: bool,
    caret: Option<&Pen>,
    playhead: &dyn Fn(&str) -> Option<Playhead>,
    ws: &'static GatherWords,
) -> Element {
    if !shows(members) {
        return Element::Blank;
    }
    let theme = ctx.theme;
    let calls: Vec<Call<'_>> = members
        .iter()
        .filter_map(Member::call)
        .filter(|c| !tools::transparent(c))
        .copied()
        .collect();
    let batch = if closed {
        Vec::new()
    } else {
        roster::of(&calls)
    };
    if !batch.is_empty() {
        return with_roster(ctx, members, &batch, clocks, caret, ws);
    }
    // 等你回答时什么都不动：笔只点亮名单的末行（§4.2「笔」的「手停住」）。
    let pen: Option<Pen> = caret.filter(|p| !p.held).copied();
    let t = tools::tally(&calls, ctx.where_, ctx.todos);
    let busy = !closed || members.iter().any(|m| !m.ended());
    // 此刻在做的事：最新的已开始的思考或调用（等你回答的调用画在下面，不在这里）。
    // 排队的调用不是此刻在做的事：以前取最新的没结束的，同一批的 Bash 一个接一个跑时，
    // 这一行落在排队的调用上，运行中的那个的输出也挂不出来（§4.2「工作的收拢」的「进行中」）。
    let open: Vec<&Member<'_>> = members
        .iter()
        .filter(|m| !m.ended() && !m.is(Status::Asking))
        .collect();
    let now_item: Option<&Member<'_>> = open
        .iter()
        .rev()
        .find(|m| matches!(m, Member::Thought(_)) || m.is(Status::Running))
        .or(open.last())
        .copied();
    let mark: Vec<Span> = if calls.is_empty() {
        vec![sp(INDENT, Style::DEFAULT)]
    } else {
        let age = gathered.map_or(SETTLED, |g| ctx.now - g);
        let from = theme.fg3.fg_color();
        // 以前有调用在运行时这个 `○` 借笔的呼吸：2.4 s 一次的明暗看不出，长的命令像卡住了。
        // 现在它不呼吸，手停住时动的是此刻那一行开头的点阵（§4.2「笔」的「调用」）。
        let m = if busy {
            sp("○", theme.fg3)
        } else if t.ok > 0 || (t.failed.is_empty() && t.stopped.is_empty()) {
            sp(
                "✓",
                fade::ink(&theme, from, Style::fg(theme.ok), age, fade::TICKS),
            )
        } else if !t.failed.is_empty() {
            sp(
                "✗",
                fade::ink(&theme, from, Style::fg(theme.err), age, fade::TICKS),
            )
        } else {
            sp("–", theme.fg3)
        };
        vec![sp(" ", Style::DEFAULT), m, sp(" ", Style::DEFAULT)]
    };
    let said = clauses(&theme, members, &t, ws);
    let something = !said.is_empty() || !t.failed.is_empty();
    let now: Option<Now> = now_item.map(|m| match m {
        Member::Thought(th) => {
            let head = playhead(th.id);
            Now::Thought {
                text: th.text.to_owned(),
                shown: head.as_ref().map_or(th.text.len(), |h| h.shown),
                head,
            }
        }
        Member::Call(c) => Now::Call {
            name: sp(tools::name(c), theme.fg2),
            writing: tools::writing(c),
            target: tools::target(c, ctx.where_, ctx.tasks, ctx.todos),
            live: tools::live(c, ctx.now_ms)
                .iter()
                .map(|p| sp(format!(" {}", p.text), theme.fg3))
                .collect(),
            arrived: clocks(c.id).arrived.map_or(SETTLED, |a| ctx.now - a),
            cells: tools::subagent(c).then(|| (ctx.cells)(c, clocks(c.id).ended)),
        },
    });
    // 手停住时的点阵在此刻在做的事的开头，宽限期里这四格空着：点阵来去时字不移动（§4.2「笔」的「手停住」）。
    // 没有笔（等你回答、重试）时也留着这四格。
    let slot = pen.map_or_else(pen::blank, |p| p.slot());
    // 点阵的光带接着走过调用的名称（§4.2「笔」的「手停住」的「光带」）。
    let banded = move |runs: Vec<Span>| match pen {
        Some(p) => p.band(runs),
        None => runs,
    };
    let tick = ctx.now;
    let asking = members.iter().any(|m| m.is(Status::Asking));
    let has_now = now.is_some();
    // 此刻在做的事那一行，在 `prefix` 之后，`w` 格宽；在跑的子代理的数字分列结束它（§4.2「子代理的数字」的「此刻的那一行」）。
    let now_row = move |prefix: Vec<Span>, w: usize| -> Element {
        let w = w as i64;
        match &now {
            Some(n @ Now::Call { cells, .. }) => {
                let mut head = prefix;
                head.extend(slot.iter().cloned());
                let hw = width(&head) as i64;
                match cells {
                    None => {
                        head.extend(banded(n.doing(&theme, w - hw, ws)));
                        single_line(head, false)
                    }
                    Some(c) => {
                        let keep = subagent_cols::keep_for(
                            w - hw - width(&n.doing(&theme, i64::MAX / 4, ws)) as i64,
                        );
                        let cols = subagent_cols::columns(c, keep);
                        let own = w
                            - hw
                            - if keep > 0 {
                                (subagent_cols::cols_width(keep) + subagent_cols::GAP) as i64
                            } else {
                                0
                            };
                        head.extend(banded(n.doing(&theme, own, ws)));
                        single_line(
                            subagent_cols::right_at(&head, &cols, w.max(0) as usize),
                            false,
                        )
                    }
                }
            }
            Some(n @ Now::Thought { text, shown, head }) => {
                // 思考：笔尖跟在 ticker 后面；停在句中满 0.5 s 时点阵在笔尖的那一格（§4.2「笔」的「手停住」）。
                // ticker 一直给它留着四格，点阵来去时词不移动。
                let room =
                    (w - width(&prefix) as i64 - if pen.is_some() { pen::SLOT as i64 } else { 0 })
                        .max(1);
                // 停了多久按手的时钟算，不按这个播放头：刚出现、还没走过的播放头没有「上次移动」。
                let glyph: Vec<Span> = match pen {
                    None => Vec::new(),
                    Some(p) => match p.sign_age() {
                        None => vec![match head {
                            Some(h) => p.nib(Ink::Thought, h, tick),
                            None => p.rest(Ink::Thought),
                        }],
                        Some(age) => {
                            // 笔尖停在词后的空白之后：点阵就在那一格；紧跟着词时先空一格。
                            pen::sign_after(
                                &theme,
                                &thought_words(text, *shown, room, &theme, ws),
                                age,
                            )
                        }
                    },
                };
                let mut v = prefix;
                v.extend(n.doing(&theme, room, ws));
                v.extend(glyph);
                single_line(v, false)
            }
            // 等模型：停住的笔尖，手停住满 0.5 s 是等待行（§4.2「笔」的「手停住」）。
            None => {
                let mut v = prefix;
                if let Some(p) = pen {
                    v.extend(p.waiting());
                }
                single_line(v, false)
            }
        }
    };
    // 思考的 ticker 是这一行里唯一 agent 写的字：最新的词落定到 fg3。
    let thinking: Option<(String, Playhead)> = match now_item {
        Some(Member::Thought(th)) => playhead(th.id).map(|p| (th.text.to_owned(), p)),
        _ => None,
    };
    let pencilled = move |e: Element| -> Element {
        match &thinking {
            Some((text, p)) => pen::settled(&theme, e, text, p, tick),
            None => e,
        }
    };
    let mark_w = width(&mark) as i64;
    // 收拢的一段里有第一次 Always allow 的调用：句子之后指向 /permissions（spec §11.9）；放不下整句加它时不画。
    let pointer: Option<Vec<Span>> = ctx
        .pointed
        .filter(|id| members.iter().any(|m| m.id() == *id))
        .map(|_| {
            let mut v = vec![sp(" · ", theme.line)];
            v.extend(tool_row::pointer_words(&theme));
            v
        });
    let rows = by_width(move |w| {
        if !something {
            // 还没有做完的事：此刻在做的事就在这一行上，一段只有一行。
            return pencilled(now_row(mark.clone(), w));
        }
        let mut head_spans = mark.clone();
        let whole = fit_words(&theme, &said, &t, i64::MAX / 4, ws);
        match pointer
            .as_ref()
            .filter(|p| (width(&whole) + width(p)) as i64 <= w as i64 - mark_w)
        {
            Some(p) => {
                head_spans.extend(whole);
                head_spans.extend(p.iter().cloned());
            }
            None => head_spans.extend(fit_words(&theme, &said, &t, w as i64 - mark_w, ws)),
        }
        let head = single_line(head_spans, false);
        // 进行中时此刻在做的事有自己的一行，从第 5 列起，句子多长都不推动它；等模型时只有 `└` 与笔尖或等待行，行数不变。
        // 只有等你回答的调用时不画这一行：`●` 行就在它的位置上。
        if closed || (!has_now && (pen.is_none() || asking)) {
            head
        } else {
            stack([head, pencilled(now_row(now_hook(&theme), w))])
        }
    });
    // 没有可说的（只有等你回答的调用）时不画这一行：`●` 行就在它的位置上。
    let mut out = Vec::new();
    if something || now_item.is_some() {
        out.push(measured(rows));
    }
    // 运行中的调用挂在下面的东西（命令的最后几行、子代理的当前调用）画在此刻的那一行下面。
    if let Some(Member::Call(c)) = now_item {
        out.extend(tool_row::under(ctx, c, &clocks(c.id)));
    }
    stack(out)
}

/// 一批调用在外面时（§4.2「工作的收拢」的「一批调用」）：这一行说这一批之外做了的，名册在它下面。没有笔尖：手停住时点阵在
/// 名册末行的开头。
fn with_roster(
    ctx: &ToolCtx<'_>,
    members: &[Member<'_>],
    batch: &[Call<'_>],
    clocks: &dyn Fn(&str) -> ToolClock,
    pen: Option<&Pen>,
    ws: &'static GatherWords,
) -> Element {
    let theme = ctx.theme;
    // 这一批的调用在名单收起时一起并入句子：同一件事不在两处说。
    let rest: Vec<Member<'_>> = members
        .iter()
        .filter(|m| !batch.iter().any(|b| b.id == m.id()))
        .copied()
        .collect();
    let rest_calls: Vec<Call<'_>> = rest.iter().filter_map(Member::call).copied().collect();
    let t = tools::tally(&rest_calls, ctx.where_, ctx.todos);
    let said = clauses(&theme, &rest, &t, ws);
    let mark = vec![
        sp(" ", Style::DEFAULT),
        sp("○", theme.fg3),
        sp(" ", Style::DEFAULT),
    ];
    let n = batch.len();
    let head = by_width(move |w| {
        let room = w as i64 - width(&mark) as i64;
        let words = if !said.is_empty() || !t.failed.is_empty() {
            fit_words(&theme, &said, &t, room, ws)
        } else {
            cut(&[sp((ws.at_once)(n), theme.fg3)], room)
        };
        let mut v = mark.clone();
        v.extend(words);
        single_line(v, false)
    });
    let mut out = vec![measured(head)];
    out.extend(roster::rows(ctx, batch, clocks, pen));
    stack(out)
}

fn now_hook(theme: &Theme) -> Vec<Span> {
    vec![
        sp("   ", Style::DEFAULT),
        sp("└", theme.line),
        sp(" ", Style::DEFAULT),
    ]
}

/// 正在打出的思考的跑马灯（§4.2「思考」的收起）：当前段落到播放头所在的词的末尾，`room` 格宽的一页；下一个词放不下时这一行
/// 从左边重新开始。只有已显示的字母。`shown` 是字节偏移。
pub fn ticker(raw: &str, shown: usize, room: i64) -> String {
    let shown = shown.min(raw.len());
    let para_start = raw[..shown].rfind('\n').map_or(0, |i| i + 1);
    let para = clean(&raw[para_start..playhead::word_end(raw, shown)]);
    let typed = clean(&raw[para_start..shown]).len();
    let pages = pages(&para, room.max(1) as usize);
    let (from, to) = pages.last().copied().unwrap_or((0, 0));
    let end = from.max(typed.min(to));
    para.get(from..end).unwrap_or("").to_owned()
}

/// 思考行的页：`para` 一个词一个词装进 `room` 格的行，给出（起，止）字节偏移；比一行长的词自成一页。
pub fn pages(para: &str, room: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut start, mut end, mut i) = (0, 0, 0);
    while i <= para.len() {
        let j = para[i..].find(' ').map_or(para.len(), |k| i + k);
        if end > start && string_width(&para[start..j]) > room {
            out.push((start, end));
            start = i;
        }
        end = j;
        i = j + 1;
    }
    out.push((start, end.min(para.len()).max(start)));
    out
}

/// 给格子的纯文字：tab 与回车会弄乱它。
fn clean(s: &str) -> String {
    s.replace('\r', "").replace('\t', "    ")
}
