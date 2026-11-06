//! 一段工作（docs/puffin-tui.md §4.2「工作的收拢（Gather）」「工具调用」「思考」；docs/specs/rust-cli/tui-kit.md §4.2）：
//! 连着的思考与工具调用是一段，收起时是 kit 的一行（`✓ Thought for 9s, explored 4 files, edited x +6 −3, ran 1 command`，
//! 进行中时下面是此刻的那一行、一批调用时是名册）；点开它或在详细视图里，按 Puffin 展开的样子铺：一段读折成一行
//! （`Explored` 与计数，详细时下面是每个调用），其余的调用与思考各自成行。
//!
//! 画法全在 kit（`gather::line`、`tool_row::{call, run}`、`entries::thought`），这里照 Puffin 的 `view/tail.rs` 的
//! `live_entries` 与 `view.rs` 的 `committed_run` 把旧界面的行映射过去（[`super::calls`]）。以前旧界面只并推理与读取类的
//! 工具，改动、命令、子代理各自成块，句子用旧的分段词，太长时折行。
use rust_cli_kernel_api::agents::TaskRecord;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::entries;
use rust_cli_tui_kit::app::widgets::gather::{self, Member};
use rust_cli_tui_kit::app::widgets::pen::Pen;
use rust_cli_tui_kit::app::widgets::playhead::Playhead;
use rust_cli_tui_kit::app::widgets::subagent_cols::Cells;
use rust_cli_tui_kit::app::widgets::tool_row::{self, PAST, ToolClock, ToolCtx};
use rust_cli_tui_kit::app::widgets::tools::{self, Call, Steps, Where};
use rust_cli_tui_kit::render::{Span, Style, StyledLine};

use super::calls::{self, Held, row_of};
use super::{Ctx, look};
use crate::ui::i18n::Lang;
use crate::view::{Row, ToolStatus};

/// 画一段工作这一帧要的、行里没有的：整个 transcript（后台任务的命令、之前的待办）、路径怎样写短、tick 与墙钟、每个
/// 调用的动效、思考与回答的播放头、骑在这一段上的笔、回合在不在跑、停下了没有。
pub(crate) struct Live<'a> {
    pub all: &'a [Row],
    pub where_: &'a Where,
    pub tick: i64,
    pub now_ms: f64,
    pub clock: &'a dyn Fn(u64) -> ToolClock,
    pub head: &'a dyn Fn(u64) -> Option<Playhead>,
    pub busy: bool,
    pub stopping: bool,
    /// Agent 调用（按工具调用 id）的子代理的数字。
    pub figures: &'a dyn Fn(&str) -> Option<crate::view::SubagentFigures>,
    /// 等你回答的调用（按工具调用 id）的记号的墨：从请求出现起淡入到 amber（Puffin `ask_mark`）；没有时是 amber。
    pub mark: &'a dyn Fn(&str) -> Option<Style>,
    /// 在回答的请求的预览（按它的工具调用 id，已按屏宽排好）：紧接在那一行下面（Puffin `asked` 的 `stack(row, preview)`）。
    pub preview: &'a dyn Fn(&str) -> Option<Vec<StyledLine>>,
    /// 内核此刻的任务清单（`TasksChanged` 的快照）：^O 时挂在最新的任务调用下面（[`task_list`]）。
    pub tasks: &'a [TaskRecord],
}

impl Live<'_> {
    fn figures(&self, tool_use: &str) -> Option<crate::view::SubagentFigures> {
        (self.figures)(tool_use)
    }
}

/// 一段工作这一帧的样子：展开没有、结束了没有（后面有别的，或回合不在跑）、骑着它的笔。
#[derive(Clone, Copy)]
pub(crate) struct Shape {
    pub open: bool,
    pub closed: bool,
    pub more_after: bool,
    pub pen: Option<Pen>,
}

/// 一段工作的成员里调用的角色（Puffin `tools::open_roles`，按展开铺时）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Own,
    Unseen,
    Head { end: usize, closed: bool },
    Member,
}

/// 展开的一段的角色：一段读从一个读开始，带上它后面的读与中间的管道；后面有别的、回合结束了、或这一批调用还在外面但不会
/// 再有读时，它就关上了（Puffin `open_roles`、`batch_ended`）。
pub(crate) fn open_roles(
    members: &[Member<'_>],
    all: &[Row],
    more_after: bool,
    busy: bool,
) -> Vec<Role> {
    let transparent = |m: &Member<'_>| m.call().is_some_and(tools::transparent);
    let reads = |m: &Member<'_>| m.call().is_some_and(tools::reads);
    let mut out = Vec::new();
    let mut i = 0;
    while i < members.len() {
        if transparent(&members[i]) {
            out.push(Role::Unseen);
            i += 1;
        } else if reads(&members[i]) {
            let mut j = i + 1;
            let mut last = i;
            while j < members.len() && (reads(&members[j]) || transparent(&members[j])) {
                if reads(&members[j]) {
                    last = j;
                }
                j += 1;
            }
            let closed =
                j < members.len() || more_after || !busy || batch_ended(&members[i..=last], all);
            out.push(Role::Head {
                end: last + 1,
                closed,
            });
            out.extend((i + 1..=last).map(|_| Role::Member));
            out.extend((last + 1..j).map(|_| Role::Unseen));
            i = j;
        } else {
            out.push(Role::Own);
            i += 1;
        }
    }
    out
}

/// 这一段读都结束了，它们的回答在它们之外还有调用：这一批还在外面，但不会再有读（Puffin `batch_ended`）。
fn batch_ended(run: &[Member<'_>], all: &[Row]) -> bool {
    let calls: Vec<&Call<'_>> = run
        .iter()
        .filter_map(Member::call)
        .filter(|c| !tools::transparent(c))
        .collect();
    let response = calls.first().map_or("", |c| c.response);
    let ids: Vec<&str> = calls.iter().map(|c| c.id).collect();
    !response.is_empty()
        && calls.iter().all(|c| c.ended && c.response == response)
        && all.iter().any(|row| {
            row.tool().is_some_and(|tool| {
                tool.response == response && !ids.contains(&calls::member_id(row).as_str())
            })
        })
}

/// 后台任务 `task_id` 跑的命令：把它送到后台的 Bash 调用的命令行（Puffin `tools::task_command`）。
fn task_command(all: &[Row], task_id: &str) -> Option<String> {
    if task_id.is_empty() {
        return None;
    }
    let marker = format!("ID: {task_id}");
    all.iter().rev().find_map(|row| {
        let tool = row.tool()?;
        (tool.name == "Bash" && tool.output.contains(&marker))
            .then(|| tools::text::first_line(tools::text::str_of(&tool.input, "command")))
    })
}

/// `TodoWrite` 调用 `id` 找到的清单：它之前最新的、既没失败也没停下的 `TodoWrite` 的输入（Puffin `todos_before`）。
fn todos_before(all: &[Row], id: &str) -> Option<Steps> {
    let at = all.iter().rposition(|row| calls::member_id(row) == id)?;
    all[..at].iter().rev().find_map(|row| {
        let tool = row.tool()?;
        (tool.name == "TodoWrite"
            && !matches!(tool.status, ToolStatus::Error | ToolStatus::Cancelled))
        .then(|| tools::todos(&tool.input))
    })
}

/// 旧界面的任务工具：一次只建或改一项，清单本身是内核的快照。
const TASK_TOOLS: [&str; 2] = ["TaskCreate", "TaskUpdate"];

/// ^O 时挂在调用 `id` 下面的任务清单（tui-display.md §2.2，Puffin「^O 的 `Todos` 行」）：只挂在 transcript 里最新的、
/// 成功了的 TaskCreate / TaskUpdate 下面，是内核此刻的整个清单；更早的任务调用不挂（内核只给最新的快照，旧的清单说不出）。
fn task_list(all: &[Row], id: &str, tasks: &[TaskRecord], lang: Lang) -> Option<Steps> {
    if tasks.is_empty() {
        return None;
    }
    let latest = all.iter().rev().find(|row| {
        row.tool().is_some_and(|tool| {
            TASK_TOOLS.contains(&tool.name.as_str()) && tool.status == ToolStatus::Success
        })
    })?;
    (calls::member_id(latest) == id).then(|| crate::ui::tasks::steps(tasks, lang))
}

/// `Agent` 调用的三格数字（Puffin `subagents::cells`）：在跑时是它的墙钟、它的会话的 token 与最近的速度；结束了是报告的
/// 用时与 token、平均速度（随 `✓` 淡入，`ended` 是那个 tick）。数字是 runner 为子会话查来的（[`crate::view::SubagentFigures`]）；
/// 不知道的不画。旧界面不跟随子代理的现场速度（Puffin 被窥视的子代理的 `LiveSpeed`），速度是查来的那个数。
fn subagent_cells(
    c: &Call<'_>,
    ended: Option<i64>,
    live: &Live<'_>,
    theme: &rust_cli_tui_kit::theme::theme::Theme,
) -> Cells {
    use rust_cli_tui_kit::app::widgets::{fade, fit, lattice, tool_row};
    let figures = calls::row_of(c.id)
        .and_then(|id| live.all.iter().find(|row| row.id == id))
        .and_then(|row| row.tool())
        .and_then(|tool| live.figures(tool.id.as_str()))
        .unwrap_or_default();
    let (started, finished) = tools::times(c);
    let number =
        |n: String, unit: &'static str| vec![Span::new(n, theme.fg3), Span::new(unit, theme.quiet)];
    if !c.ended {
        let time = if started > 0.0 {
            vec![Span::new(lattice::clock(live.now_ms - started), theme.fg3)]
        } else {
            Vec::new()
        };
        return Cells {
            time,
            tokens: figures
                .tokens
                .map_or_else(Vec::new, |n| number(fit::count(n as f64), " tok")),
            speed: figures
                .last
                .map_or_else(Vec::new, |v| number(fit::rate(v), " tok/s")),
        };
    }
    let took = tools::reported(c, "duration_ms")
        .or_else(|| (started > 0.0 && finished > started).then_some(finished - started));
    let tokens = tools::reported(c, "subagent_tokens").or(figures.tokens.map(|t| t as f64));
    Cells {
        time: took.map_or_else(Vec::new, |ms| vec![Span::new(fit::time(ms), theme.fg3)]),
        tokens: tokens.map_or_else(Vec::new, |n| number(fit::count(n), " tok")),
        // 平均速度与 `✓` 一起淡入：它在子代理结束时才算得出。
        speed: figures.avg.map_or_else(Vec::new, |v| {
            fade::fade_in(
                theme,
                number(fit::rate(v), " tok/s"),
                tool_row::age(ended, live.tick),
                fade::TICKS,
                None,
            )
        }),
    }
}

/// 路径怎样写短（Puffin `Where`）：工作区与家目录。家目录从工作区与它显示的写法（家目录写成 `~`）算出。
pub(crate) fn where_of(ctx: &Ctx<'_>) -> Where {
    let rest = ctx.workspace_label.strip_prefix('~');
    let home = rest
        .and_then(|rest| ctx.workspace.strip_suffix(rest))
        .map(str::to_owned);
    Where {
        cwd: ctx.workspace.to_owned(),
        home,
    }
}

/// 一段工作排好的行，与在回答的请求的那一行（它带着预览时）从第几行起。
pub(crate) struct Laid {
    pub lines: Vec<StyledLine>,
    pub asked: Option<usize>,
}

/// 一段工作的行。
pub(crate) fn lines(
    rows: &[&Row],
    ctx: &Ctx<'_>,
    live: &Live<'_>,
    shape: Shape,
) -> Vec<StyledLine> {
    laid(rows, ctx, live, shape).lines
}

/// 一段工作的行：逐块排（`stack` 就是把孩子的行接起来，Layout 的 `Stack`），记下带预览的那一行的起点。
pub(crate) fn laid(rows: &[&Row], ctx: &Ctx<'_>, live: &Live<'_>, shape: Shape) -> Laid {
    let mut out = Laid {
        lines: Vec::new(),
        asked: None,
    };
    for piece in pieces(rows, ctx, live, shape) {
        let start = out.lines.len();
        out.lines.extend(look::lay(&piece.element, ctx.width));
        if let Some(preview) = piece.preview {
            out.asked.get_or_insert(start);
            out.lines.extend(preview);
        }
    }
    out
}

/// 一段工作的一块，与紧接在它下面的请求预览。
struct Piece {
    element: Element,
    preview: Option<Vec<StyledLine>>,
}

impl Piece {
    fn new(element: Element) -> Self {
        Piece {
            element,
            preview: None,
        }
    }
}

/// 一段工作的块（Puffin `live_entries` 的 `Role::Run` 与展开的一段，`committed_run`）：等你回答的调用各自一行，接在
/// 收拢行或一段读的下面（Puffin `with_waiting`），在回答的那个下面是它的预览（`asked`）。
fn pieces(rows: &[&Row], ctx: &Ctx<'_>, live: &Live<'_>, shape: Shape) -> Vec<Piece> {
    let theme = ctx.ink.theme;
    let held: Vec<Held<'_>> = rows.iter().filter_map(|row| Held::of(row)).collect();
    let members: Vec<Member<'_>> = held.iter().map(Held::member).collect();
    // 与 `members` 一一对应的行（成员之外的行不在里面）。
    let member_rows: Vec<&Row> = rows
        .iter()
        .copied()
        .filter(|row| Held::of(row).is_some())
        .collect();
    let clocks = |id: &str| row_of(id).map_or_else(ToolClock::default, |id| (live.clock)(id));
    let heads = |id: &str| row_of(id).and_then(|id| (live.head)(id));
    let tasks = |id: &str| task_command(live.all, id);
    let todos = |id: &str| todos_before(live.all, id);
    let steps = |c: &Call<'_>| task_list(live.all, c.id, live.tasks, ctx.lang);
    let cells = |c: &Call<'_>, ended: Option<i64>| subagent_cells(c, ended, live, &theme);
    let machine = |c: &Call<'_>| {
        tools::machine(c, ctx.mcp_machines).map(|m| {
            // 远端的调用在等那台机器重连（remote-exec.md §25.4.1，Puffin spec §11.1）。
            let waits = !c.ended
                && ctx.machines.iter().any(|row| {
                    row.name == m
                        && matches!(
                            row.status,
                            crate::machines::MachineStatus::Reconnecting { .. }
                        )
                });
            (m.to_owned(), waits)
        })
    };
    let kit = |details: bool| ToolCtx {
        theme,
        where_: live.where_,
        tasks: &tasks,
        now: live.tick,
        now_ms: live.now_ms,
        details,
        todos: &todos,
        steps: &steps,
        cells: &cells,
        machine: &machine,
        pointed: None,
    };
    let head_id = rows.first().map_or(0, |row| row.id);
    if !shape.open {
        let k = kit(false);
        let gathered = shape
            .closed
            .then(|| (live.clock)(head_id).gathered.or(Some(PAST)))
            .flatten();
        let caret = shape.pen.filter(|_| !shape.closed);
        let line = gather::line_said(
            &k,
            &members,
            &clocks,
            gathered,
            shape.closed,
            caret.as_ref(),
            &heads,
            crate::ui::i18n_kit::gather(ctx.lang),
        );
        // 等你回答的调用画在这一行下面（Puffin `with_waiting`）。
        let mut out = vec![Piece::new(line)];
        out.extend(waiting(&members, &k, &clocks, &member_rows, live));
        return out;
    }
    let k = kit(true);
    let roles = open_roles(&members, live.all, shape.more_after, live.busy);
    let mut out = Vec::new();
    for (i, role) in roles.iter().enumerate() {
        let member = &members[i];
        match (*role, member) {
            (Role::Member, _) => {}
            (Role::Head { end, closed }, _) => {
                let run: Vec<Call<'_>> = members[i..end]
                    .iter()
                    .filter_map(Member::call)
                    .copied()
                    .collect();
                let id = member_rows.get(i).map_or(0, |row| row.id);
                let at = closed
                    .then(|| (live.clock)(id).closed.or(Some(PAST)))
                    .flatten();
                out.push(Piece::new(tool_row::run(&k, &run, &clocks, at)));
                out.extend(waiting(
                    &members[i..end],
                    &k,
                    &clocks,
                    &member_rows[i..end],
                    live,
                ));
            }
            (_, Member::Thought(t)) => {
                let head = heads(t.id);
                let last = i + 1 == members.len();
                let pen = shape.pen.filter(|_| last && !shape.closed);
                out.push(Piece::new(entries::thought_said(
                    &theme,
                    t.text,
                    t.ended,
                    t.duration_ms,
                    pen.as_ref(),
                    head.as_ref(),
                    live.tick,
                    live.stopping,
                    crate::ui::i18n_kit::entries(ctx.lang),
                )));
            }
            (_, Member::Call(c)) if c.status == tools::Status::Asking => {
                out.push(asked(c, &k, &clocks, member_rows.get(i).copied(), live));
            }
            (_, Member::Call(c)) => {
                out.push(Piece::new(tool_row::call(&k, c, &clocks(c.id), None)));
            }
        }
    }
    out
}

/// `members` 里等你回答的调用，各自一行（Puffin `with_waiting`）。`rows` 与 `members` 一一对应。
fn waiting(
    members: &[Member<'_>],
    k: &ToolCtx<'_>,
    clocks: &dyn Fn(&str) -> ToolClock,
    rows: &[&Row],
    live: &Live<'_>,
) -> Vec<Piece> {
    members
        .iter()
        .zip(rows)
        .filter_map(|(m, row)| m.call().map(|c| (c, *row)))
        .filter(|(c, _)| c.status == tools::Status::Asking)
        .map(|(c, row)| asked(c, k, clocks, Some(row), live))
        .collect()
}

/// 等你回答的调用（Puffin `asked`）：记号从请求出现起淡入到 amber，在回答的那个下面是它的预览。
fn asked(
    c: &Call<'_>,
    k: &ToolCtx<'_>,
    clocks: &dyn Fn(&str) -> ToolClock,
    row: Option<&Row>,
    live: &Live<'_>,
) -> Piece {
    let tool = row.and_then(|row| row.tool()).map(|tool| tool.id.as_str());
    let mark = tool
        .and_then(|id| (live.mark)(id))
        .unwrap_or(Style::fg(k.theme.code));
    Piece {
        element: tool_row::call(k, c, &clocks(c.id), Some(mark)),
        preview: tool.and_then(|id| (live.preview)(id)),
    }
}

//END
