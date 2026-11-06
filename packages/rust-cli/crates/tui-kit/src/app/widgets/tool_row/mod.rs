//! 一个工具调用的画法与它的动效（docs/puffin-tui.md §4.2「工具调用」；docs/specs/rust-cli/tui-kit.md §4.4）：行（标记、名称、
//! 对象、行尾的结果）、挂在下面的证据（diff 的行号与底色、命令的尾巴、原因、报告、待办）、子代理的数字列。入参是中性的
//! [`Call`] 与 [`ToolCtx`]；一段读的折叠在 `explore.rs`。
//! 移植自 Puffin 的 shell/tool_view.rs 的纯函数部分（ToolView.scala 第 1–320 行）。
use crate::app::element::{Element, beside, by_width, single_line, stack};
use crate::markdown::render::{INDENT, SETTLED};
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{self, measured};
use super::subagent_cols::{self, Cells};
use super::todo_step;
use super::tools::{self, Call, DiffRow, Ink, Part, Steps, Under, Where};

mod explore;

pub use explore::{run, run_settled, settle_tick, swap};

/// 很久以前：快照里的东西按它们的样子画，不动。
pub const PAST: i64 = i64::MIN / 4;

/// 一个调用的动效（tick）：出现、结束、输出行落下 `(行数, tick)`、一段读结束（在它第一个调用上）、一段工作结束（在它第一个
/// 条目上，§4.2「工作的收拢」）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ToolClock {
    pub arrived: Option<i64>,
    pub ended: Option<i64>,
    pub landed: Vec<(usize, i64)>,
    pub closed: Option<i64>,
    pub gathered: Option<i64>,
}

impl ToolClock {
    /// 输出的第 `i` 行（从第一行数起）到达的 tick。
    pub fn landed_at(&self, i: usize) -> Option<i64> {
        self.landed.iter().find(|(n, _)| *n > i).map(|(_, t)| *t)
    }
}

/// 画一个调用除了它本身还要的：主题、路径怎样写短、后台任务的命令（按任务 id）、tick、墙钟、^O、每个 `Todos` 调用之前的
/// 清单（按调用 id）、子代理调用的数字格（按调用与它结束的 tick）、调用在哪台远端机器上与是否在等它重连回来（spec §11.1；
/// 在本机时为 `None`）、行尾指向 `/permissions` 的调用（spec §11.9）、展开时宿主挂在调用下面的清单（`steps`）。
pub struct ToolCtx<'a> {
    pub theme: Theme,
    pub where_: &'a Where,
    pub tasks: &'a dyn Fn(&str) -> Option<String>,
    pub now: i64,
    pub now_ms: f64,
    pub details: bool,
    pub todos: &'a dyn Fn(&str) -> Option<Steps>,
    /// 展开（`details`）时挂在一个调用下面的清单，画法同 `Todos` 的新清单。Puffin 没有（`|_| None`）；旧界面的任务工具
    /// （TaskCreate / TaskUpdate）一次只动一项，它把内核此刻的整个清单挂在最新的那一次下面（tui-display.md §2.2）。
    pub steps: &'a dyn Fn(&Call<'_>) -> Option<Steps>,
    pub cells: &'a dyn Fn(&Call<'_>, Option<i64>) -> Cells,
    pub machine: &'a dyn Fn(&Call<'_>) -> Option<(String, bool)>,
    pub pointed: Option<&'a str>,
}

/// 它的动效在 `now` 结束了：可以提交。
pub fn settled(ctx: &ToolCtx<'_>, c: &Call<'_>, clock: &ToolClock) -> bool {
    c.ended && clock.ended.is_none_or(|t| ctx.now - t >= fade::TICKS)
}

/// `at` 以来的 tick；没有时是很久以前。
pub fn age(at: Option<i64>, now: i64) -> i64 {
    at.map_or(SETTLED, |t| now - t)
}

pub(crate) fn sp(text: impl Into<String>, style: Style) -> Span {
    Span::new(text.into(), style)
}

pub(crate) fn gap(n: usize) -> Span {
    Span::new(" ".repeat(n), Style::DEFAULT)
}

// ---- 一个调用 ----

/// 一个调用：它的行与挂在下面的东西。`asking` 是等你时 `●` 的样式。它的 `○` 不呼吸（§4.2「笔」的「调用」）。
pub fn call(ctx: &ToolCtx<'_>, c: &Call<'_>, clock: &ToolClock, asking: Option<Style>) -> Element {
    let row = call_row(ctx, c, clock, asking);
    let rows = under(ctx, c, clock);
    if rows.is_empty() {
        row
    } else {
        stack(std::iter::once(row).chain(rows))
    }
}

/// 一个调用的行：`mark Name  target  end`；子代理的行在行尾有数字列。
pub fn call_row(
    ctx: &ToolCtx<'_>,
    c: &Call<'_>,
    clock: &ToolClock,
    asking: Option<Style>,
) -> Element {
    let theme = ctx.theme;
    let status = tools::status(c);
    let ended = c.ended;
    let mark = match status {
        "asking" => sp("●", asking.unwrap_or(Style::fg(theme.code))),
        // 排队的调用不是此刻在做的事：line 色的 `○`，不呼吸（§4.2「工具调用」的「行」）。
        "queued" => sp("○", ring(&theme, clock.arrived, ctx.now, theme.line)),
        // 运行中的 `○` 不呼吸：手停住时动的是点阵（§4.2「笔」的「调用」）。
        _ if !ended => sp("○", ring(&theme, clock.arrived, ctx.now, theme.fg3)),
        s => settled_mark(&theme, s, clock.ended, ctx.now),
    };
    let name_style = if status == "cancelled" || status == "queued" {
        theme.fg3
    } else {
        theme.fg2
    }
    .with_attr(attr::BOLD);
    let end: Vec<Span> = if status == "queued" {
        vec![sp("queued", theme.fg3)]
    } else if !ended {
        tools::live(c, ctx.now_ms)
            .iter()
            .map(|p| span(&theme, p))
            .collect()
    } else {
        fade::parts(
            &theme,
            tools::outcome(c, ctx.todos)
                .into_iter()
                .map(|p| (p.text, ink(&theme, p.ink)))
                .collect(),
            age(clock.ended, ctx.now),
            None,
        )
    };
    let end = with_machine(&theme, (ctx.machine)(c), end);
    let end = with_pointer(&theme, ctx.pointed == Some(c.id), end);
    // `Todos` 的对象里动词 fg2（§4.2「工具调用」的「对象」）；其余工具的对象 fg3。
    let target: Vec<Span> = if tools::tool(c) == "TodoWrite" {
        let before = (ctx.todos)(c.id);
        tools::todo_words(&tools::todo_change(c, before.as_ref()))
            .iter()
            .map(|p| span(&theme, p))
            .collect()
    } else {
        vec![sp(
            tools::target(c, ctx.where_, ctx.tasks, &tools::no_todos),
            theme.fg3,
        )]
    };
    let name = (tools::name(c), name_style);
    // 输入还在写出：只有名称，还没有对象（§4.2「笔」的「调用」）。
    if tools::writing(c) {
        single_line(vec![gap(1), mark, gap(1), sp(name.0, name.1)], false)
    } else if tools::subagent(c) {
        let cells = (ctx.cells)(c, clock.ended);
        let arrived = clock.arrived.map_or(SETTLED, |t| ctx.now - t);
        agent_row(&theme, cells, arrived, mark, name, target, end)
    } else {
        row(
            &theme,
            mark,
            name,
            target,
            end,
            clock.arrived.map(|t| ctx.now - t),
        )
    }
}

/// 远端的调用在行尾接 `@机器`（`@` fg3、名字 fg2），在等那台重连时接 ` · waits for <机器> to come back`，然后是原来的末尾
/// （spec §11.1）。
fn with_machine(theme: &Theme, at: Option<(String, bool)>, end: Vec<Span>) -> Vec<Span> {
    let Some((name, waits)) = at else {
        return end;
    };
    let mut out = vec![sp("@", theme.fg3), sp(name.clone(), theme.fg2)];
    if waits {
        out.push(sp(" · ", theme.line));
        out.push(sp(format!("waits for {name} to come back"), theme.fg3));
    }
    join(theme, &mut out, end);
    out
}

/// 在 `out` 后面接上原来的末尾，中间一个 line 色的 ` · `。末尾自己以 `·` 开头时（命令的用时 `· 9s`）只空一格：以前写成
/// `@dev-a · · 9s`，两个点。
fn join(theme: &Theme, out: &mut Vec<Span>, end: Vec<Span>) {
    let Some(first) = end.first() else {
        return;
    };
    let sep = if first.text.trim_start().starts_with('·') {
        " "
    } else {
        " · "
    };
    out.push(sp(sep, theme.line));
    out.extend(end);
}

/// 指向 `/permissions` 的那几个字（spec §11.9）。
pub fn pointer_words(theme: &Theme) -> Vec<Span> {
    vec![
        sp("always allowed now, see ", theme.fg3),
        sp("/permissions", theme.fg2),
    ]
}

/// 第一次 Always allow 的调用在行尾接 `always allowed now, see /permissions`，然后是原来的末尾（spec §11.9）。
fn with_pointer(theme: &Theme, pointed: bool, end: Vec<Span>) -> Vec<Span> {
    if !pointed {
        return end;
    }
    let mut out = pointer_words(theme);
    join(theme, &mut out, end);
    out
}

/// 对象放进 `room` 格：放得下整个，至少 4 格时截断，否则不要。
fn fit_target(target: &[Span], room: i64) -> Vec<Span> {
    let whole = fit::width(target) as i64;
    if whole == 0 {
        Vec::new()
    } else if whole <= room {
        target.to_vec()
    } else if room >= 4 {
        fit::cut(target, room)
    } else {
        Vec::new()
    }
}

/// 名称与（放得下的）对象，作为淡入的部分。
fn name_parts(name: &(String, Style), shown: Vec<Span>) -> Vec<(String, Style)> {
    let mut parts = vec![name.clone()];
    if !shown.is_empty() {
        parts.push(("  ".to_owned(), Style::DEFAULT));
        parts.extend(shown.into_iter().map(|s| (s.text.into_owned(), s.style)));
    }
    parts
}

/// 子代理的行：`✓ Agent  说明  5 tools`，然后它的数字分列在行尾，在跑时也是（§4.2「子代理的数字」的「展开」）。放得下的
/// 列留下；先截对象，再去掉它。`arrived` 是行出现了多久。
pub fn agent_row(
    theme: &Theme,
    cells: Cells,
    arrived: i64,
    mark: Span,
    name: (String, Style),
    target: Vec<Span>,
    end: Vec<Span>,
) -> Element {
    let theme = *theme;
    let lead = vec![gap(1), mark, gap(1)];
    let tail: Vec<Span> = if end.is_empty() {
        Vec::new()
    } else {
        std::iter::once(gap(2)).chain(end).collect()
    };
    measured(by_width(move |w| {
        let lead_w = fit::width(&lead) as i64;
        let name_w = string_width(&name.0) as i64;
        let keep = subagent_cols::keep_for(
            w as i64 - lead_w - name_w - 2 - fit::width(&target) as i64 - 2,
        );
        let cols = subagent_cols::columns(&cells, keep);
        let fixed = lead_w
            + name_w
            + fit::width(&tail) as i64
            + if keep > 0 {
                (subagent_cols::cols_width(keep) + subagent_cols::GAP) as i64
            } else {
                0
            };
        let shown = fit_target(&target, w as i64 - fixed - 2);
        let mut left = lead.clone();
        left.extend(fade::parts(&theme, name_parts(&name, shown), arrived, None));
        left.extend(tail.iter().cloned());
        single_line(subagent_cols::right_at(&left, &cols, w), false)
    }))
}

/// `mark Name  target  end`，对象一种墨色（fg3）。
pub fn row_text(
    theme: &Theme,
    mark: Span,
    name: (String, Style),
    target: &str,
    end: Vec<Span>,
    arrived: Option<i64>,
) -> Element {
    let target = vec![sp(target, theme.fg3)];
    row(theme, mark, name, target, end, arrived)
}

/// `mark Name  target  end`：先截对象，再去掉它；末尾保持完整。`arrived` 是行出现了多久：名称与对象随圈一起淡入。
pub fn row(
    theme: &Theme,
    mark: Span,
    name: (String, Style),
    target: Vec<Span>,
    end: Vec<Span>,
    arrived: Option<i64>,
) -> Element {
    let theme = *theme;
    let lead = vec![gap(1), mark, gap(1)];
    let tail: Vec<Span> = if end.is_empty() {
        Vec::new()
    } else {
        std::iter::once(gap(2)).chain(end).collect()
    };
    let fixed = (fit::width(&lead) + fit::width(&tail) + string_width(&name.0)) as i64;
    let age = arrived.unwrap_or(SETTLED);
    measured(by_width(move |w| {
        let shown = fit_target(&target, w as i64 - fixed - 2);
        let mut spans = lead.clone();
        spans.extend(fade::parts(&theme, name_parts(&name, shown), age, None));
        spans.extend(tail.iter().cloned());
        single_line(spans, false)
    }))
}

// ---- 挂在调用下面的 ----

/// 轨道上的一行。
pub fn railed(theme: &Theme, spans: Vec<Span>) -> Element {
    let mut r = rail(theme);
    r.extend(spans);
    measured(single_line(r, false))
}

/// 挂在调用下面的：它的证据（§4.2「工具调用」的「证据」）。行下面的东西不是 agent 写的字：作为一块直接出现，与标记的渐变
/// 同时（§4.2「笔」的「不是笔的东西」）。
pub fn under(ctx: &ToolCtx<'_>, c: &Call<'_>, clock: &ToolClock) -> Vec<Element> {
    let theme = ctx.theme;
    match tools::under(c, ctx.details, ctx.where_, ctx.todos) {
        Under::Nothing => ctx
            .details
            .then(|| (ctx.steps)(c))
            .flatten()
            .map_or_else(Vec::new, |steps| todo_rows(&theme, &steps)),
        Under::Diff(rows) => rows.iter().map(|r| diff_row(&theme, r)).collect(),
        Under::Files(files) => files_rows(&theme, &files),
        Under::Tail(lines) => tail(ctx, c, clock, &lines),
        Under::Shell {
            command,
            why,
            lines,
        } => {
            let mut out = vec![railed(
                &theme,
                vec![sp("$ ", theme.fg3), sp(command, theme.fg2)],
            )];
            if !why.is_empty() {
                out.push(railed(
                    &theme,
                    vec![sp(why, theme.fg3.with_attr(attr::ITALIC))],
                ));
            }
            out.extend(tail(ctx, c, clock, &lines));
            out
        }
        Under::Reason(lines) => lines
            .into_iter()
            .map(|l| railed(&theme, vec![sp(l, theme.fg2)]))
            .collect(),
        Under::Report(lines) => {
            let ink = if tools::failed(c) {
                theme.fg2
            } else {
                theme.fg3
            };
            lines
                .into_iter()
                .map(|l| railed(&theme, vec![sp(l, ink)]))
                .collect()
        }
        Under::Ticker(label) => {
            // 子代理的调用是机器的输出：新的直接换上，不淡入（§4.2「笔」的「不是笔的东西」）。
            let fg3 = theme.fg3;
            vec![measured(beside(
                single_line(rail(&theme), false),
                5,
                by_width(move |w| single_line(vec![sp(fit::clip(&label, w), fg3)], false)),
            ))]
        }
        Under::Todos(steps) => todo_rows(&theme, &steps),
    }
}

/// 清单的每一步一行：标记与墨色同「待办」的坞（§4.2「工具调用」的「证据」）。
fn todo_rows(theme: &Theme, steps: &Steps) -> Vec<Element> {
    steps
        .iter()
        .map(|(st, text)| {
            railed(
                theme,
                todo_step::step(theme, todo_step::status(st), text, false),
            )
        })
        .collect()
}

/// 补丁的文件：每个文件一行，文件名补齐到最长的（至多 34 格），然后加减的行数。
pub fn files_rows(theme: &Theme, files: &[(String, i64, i64)]) -> Vec<Element> {
    let pw = files
        .iter()
        .map(|f| string_width(&f.0))
        .max()
        .unwrap_or(0)
        .min(34);
    files
        .iter()
        .map(|(f, added, removed)| {
            let mut spans = vec![sp(fit::pad(&fit::clip(f, pw), pw), theme.fg2), gap(2)];
            spans.extend(counts(theme, *added, *removed));
            railed(theme, spans)
        })
        .collect()
}

/// 命令的输出行：新的照原样以 fg3 出现，是机器的输出（§4.2「笔」）；调用结束后渐变到 quiet（成功）或 fg2（失败）。
pub fn tail(ctx: &ToolCtx<'_>, c: &Call<'_>, clock: &ToolClock, lines: &[String]) -> Vec<Element> {
    let theme = ctx.theme;
    let settled_ink = if tools::failed(c) {
        theme.fg2
    } else {
        theme.quiet
    };
    let st = if c.ended {
        fade::ink(
            &theme,
            theme.fg3.fg_color(),
            settled_ink,
            age(clock.ended, ctx.now),
            fade::TICKS,
        )
    } else {
        theme.fg3
    };
    lines
        .iter()
        .map(|l| railed(&theme, vec![sp(l.clone(), st)]))
        .collect()
}

/// 一行 diff：行号、符号与文字，改动的行的底色涂到行尾。
pub fn diff_row(theme: &Theme, r: &DiffRow) -> Element {
    let theme = *theme;
    let (sign, sign_ink, bg) = match r.sign {
        '+' => ("+ ", Style::fg(theme.ok), theme.add_bg),
        '-' => ("− ", Style::fg(theme.err), theme.del_bg),
        _ => ("  ", theme.fg3, None),
    };
    let text_ink = if r.sign == ' ' { theme.fg3 } else { theme.fg2 };
    let on = move |st: Style| bg.map_or(st, |b| st.with_bg(b));
    let n = r.n.to_string();
    let body = r.text.clone();
    measured(by_width(move |w| {
        let mut head = rail(&theme);
        head.push(sp(fit::pad_left(&n, 4), theme.fg3));
        head.push(gap(1));
        let room = (w as i64 - 5 - 5 - 2).max(1) as usize;
        let text = fit::clip(&body, room);
        let pad = if bg.is_some() {
            " ".repeat(room.saturating_sub(string_width(&text)))
        } else {
            String::new()
        };
        head.push(sp(sign, on(sign_ink)));
        head.push(sp(text + &pad, on(text_ink)));
        single_line(head, false)
    }))
}

// ---- 动效与墨色 ----

/// 与它的行一起从背景淡入的圈。
pub fn ring(theme: &Theme, arrived: Option<i64>, now: i64, ink: Style) -> Style {
    fade::fade_in(
        theme,
        vec![sp("○", ink)],
        age(arrived, now),
        fade::TICKS,
        None,
    )
    .first()
    .map_or(ink, |s| s.style)
}

/// 调用结束后 `✓` 与 `✗` 从 fg3 渐变到它们的墨色；`–` 立刻是 fg3。
pub fn settled_mark(theme: &Theme, status: &str, ended: Option<i64>, now: i64) -> Span {
    let from = theme.fg3.fg_color();
    match status {
        "error" => sp(
            "✗",
            fade::ink(
                theme,
                from,
                Style::fg(theme.err),
                age(ended, now),
                fade::TICKS,
            ),
        ),
        "cancelled" => sp("–", theme.fg3),
        _ => sp(
            "✓",
            fade::ink(
                theme,
                from,
                Style::fg(theme.ok),
                age(ended, now),
                fade::TICKS,
            ),
        ),
    }
}

/// `+6 −3`：加的 ok 色，减的 err 色，之间一格。
pub fn counts(theme: &Theme, added: i64, removed: i64) -> Vec<Span> {
    let mut out = Vec::new();
    if added > 0 {
        out.push(sp(format!("+{added}"), Style::fg(theme.ok)));
    }
    if added > 0 && removed > 0 {
        out.push(gap(1));
    }
    if removed > 0 {
        out.push(sp(format!("−{removed}"), Style::fg(theme.err)));
    }
    out
}

/// 一种墨色的样式。
pub fn ink(theme: &Theme, i: Ink) -> Style {
    match i {
        Ink::Fg2 => theme.fg2,
        Ink::Fg3 => theme.fg3,
        Ink::Ok => Style::fg(theme.ok),
        Ink::Err => Style::fg(theme.err),
        Ink::Line => theme.line,
    }
}

/// 一段带墨色的文字。
pub fn span(theme: &Theme, p: &Part) -> Span {
    sp(p.text.clone(), ink(theme, p.ink))
}

/// `   │ `：行下面的竖线。
pub fn rail(theme: &Theme) -> Vec<Span> {
    vec![
        Span::new(INDENT, Style::DEFAULT),
        Span::new("│", theme.line),
        gap(1),
    ]
}
