// 改写自 apps/zcode-host/crates/tui/src/ui/transcript.rs（feat/rs 79b6691c55）的组装部分：行来自
// `view::Transcript`，去掉子代理视图与 V4 的 `lastError`（失败原因已是视图里的错误行）。
//! transcript 的全部行：会话头、cell、本地 cell；排版结果按块缓存。
//!
//! 样子（docs/specs/rust-cli/tui-kit.md §4.2，工作线 TK 的 S1）：块之间的间距照 Puffin（Hang，[`gap`]）；流式中的回复由
//! 播放头按词显示（§4.2「流式节奏」，[`advance`]）；本地命令的结果按 Puffin 的网格摆（自拟，§4.3）；置顶行照 Puffin。
use std::collections::HashMap;
use std::rc::Rc;

use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::motion::Motion;
use super::tail::{self, Last};
use super::{HEADER_KEY, Layout, Prompt, Rendered, local_key};
use crate::ui::app::{App, Local, LocalCell};
use crate::ui::ask_body;
use crate::ui::cells::look::{self, LEAD};
use crate::ui::cells::work::{self, Live, Shape};
use crate::ui::cells::{self, Ctx, Kind};
use crate::ui::lines::wrap_text;
use crate::view::{Body, Row, ToolStatus};
use rust_cli_kernel_api::ids::ToolUseId;

/// 缓存里的一个块：命中条件（行版本、宽度、单独展开、明暗、类型、一段工作关没关上）与排好的行。
struct Entry {
    versions: Vec<(u64, u64)>,
    width: usize,
    expanded: bool,
    light: bool,
    kind: Kind,
    closed: bool,
    /// 回合结束那一行接的在跑的命令（修复线 F118：每次画都现取，变了就重排）；别的块为 None。
    tail: Option<String>,
    rendered: Rc<Rendered>,
    /// 最后一次用到它的帧；每帧结束时清掉这一帧没用到的块。
    frame: u64,
}

/// 排版缓存，按（块的第一个行 id，展开没有）查找。普通视图与详细视图各一份，切换不互相淘汰；搜索模式里普通视图的
/// 折叠块同时要两种形式（显示的与完整的，tui-input.md §2.3.2），各占一项。
#[derive(Default)]
pub struct Cache {
    normal: HashMap<(u64, bool), Entry>,
    detail: HashMap<(u64, bool), Entry>,
    frame: u64,
    /// 流式中的回复的增量 markdown 排版（每帧都要重排的块用它，修复线 F6，tui-kit.md §3.5）。
    markdown: crate::ui::markdown::Memo,
    /// 清空的次数：行 id 从头分配过几次。按块的键记的界面状态（搜索的当前匹配）据此判断键还作不作数。
    epoch: u64,
    /// 动效的时钟：调用的出现与结束、一段工作的关上、回答与思考的播放头、通知的淡入（`transcript_motion.rs`）。
    motion: Motion,
    /// 动效记的是哪一份视图：主会话（false）或查看中的子代理（true）；换了就清掉。
    motion_viewing: bool,
    /// 尾巴的时钟：手停住的 tick、这一串重试开始的 tick（`transcript_tail.rs`）。
    clocks: super::tail::Clocks,
}

impl Cache {
    /// 视图换了（`/clear`、重新取快照）：行 id 从头分配，旧的块全部作废。
    pub fn clear(&mut self) {
        self.normal.clear();
        self.detail.clear();
        self.markdown = crate::ui::markdown::Memo::default();
        self.motion.clear();
        self.epoch += 1;
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// 还有动效在走：按动画节拍重画（Puffin 的计时器）。
    pub fn moving(&self) -> bool {
        self.motion.moving()
    }
}

/// 本地 cell 的行（transcript 与独立页面共用）。命令的结果按 Puffin 的网格摆（自拟，tui-kit.md §4.3）：说明是第 3 列起
/// fg3 的字（同 Puffin 输入框上方的说明行）；标题第 3 列起粗体；表与键值从第 5 列起，标签与附注 fg3、状态 brand。
pub fn local_lines(local: &Local, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let theme = &ctx.ink.theme;
    let width = look::measure(ctx.width);
    let lead = [Span::plain(LEAD)];
    match local {
        Local::Info(text) => wrap_text(text.trim_end(), theme.fg3, width, &lead, &lead),
        Local::Error(text) => cells::error_lines(text, ctx),
    }
}

/// 块在间距里的角色（Puffin `entries::gap_after` 与 `hangs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    /// 板（用户消息、`!` 命令）：rich 主题下面不空行，它的半块边就是留白。
    Slab,
    /// 开启一轮工作的行（系统提示、目标的检查）：工作行挂在它下面。
    Opener,
    /// 工作行（调用、工作段、思考）：在板、开启的行与别的工作行下面挂着。
    Work,
    /// 压缩：挂在任何东西下面。
    Compact,
    /// `!` 命令的输出：它是命令的块的一部分，紧挨着命令。
    Output,
    Other,
}

fn spot(cell: &cells::Cell<'_>) -> Spot {
    match (cell.kind, cell.rows.first().map(|row| &row.body)) {
        (Kind::User, _) => Spot::Slab,
        (Kind::Notice, _) => Spot::Opener,
        (Kind::Group, _) => Spot::Work,
        (Kind::Marker, Some(Body::Compact(_))) => Spot::Compact,
        (Kind::Output, _) => Spot::Output,
        _ => Spot::Other,
    }
}

/// 上一块是 `prev`（没有时为 None）时 `cur` 上面空不空一行（docs/puffin-tui.md §4.2「间距（Hang）」）：第一块不空；板
/// 下面 rich 不空、plain 空；挂着的块不空；其余都空。
fn gap(prev: Option<Spot>, cur: Spot, rich: bool) -> bool {
    match prev {
        None => false,
        _ if cur == Spot::Output => false,
        Some(Spot::Slab) => !rich,
        Some(p) => {
            let hangs = cur == Spot::Compact
                || (cur == Spot::Work && matches!(p, Spot::Work | Spot::Opener));
            !hangs
        }
    }
}

/// 这一块在尾巴的间距里算什么（Puffin `tail_entries` 的 `gap`）：板的下沿；工作行、开启一轮的行与还在变的块挂着；其余是
/// 说完的话。
fn last_of(spot: Spot, live: bool) -> Last {
    match spot {
        Spot::Slab | Spot::Output => Last::Slab,
        Spot::Work | Spot::Opener => Last::Hanging,
        _ if live => Last::Hanging,
        _ => Last::Said,
    }
}

/// 一个块的行：普通视图里点开的块按展开的样子排（Puffin 没有悬停与点开的底色）；详细视图是详细形式。以前点开的块铺
/// 悬停的底色并多 1 行空行。
/// 回合结束那一行接 `tail`（两种视图相同，不能展开，修复线 F118）。
fn render(cell: &cells::Cell<'_>, ctx: &Ctx<'_>, opened: bool, tail: Option<&str>) -> Rc<Rendered> {
    if cell.kind == Kind::Duration {
        return Rendered::new(cells::duration_cell(cell, tail, ctx));
    }
    if ctx.expanded {
        return Rendered::new(cells::lines(cell, ctx));
    }
    let (lines, expandable) = cells::lay_out(cell, ctx, opened);
    Rendered::cell(lines, expandable)
}

/// transcript 的全部行。
pub fn lines(app: &App, cache: &mut Cache, ctx: &Ctx<'_>) -> Layout {
    let mut out = Layout::default();
    let rich = ctx.ink.theme.rich;
    // 只读查看子代理：顶部是 `↳ 类型 · 描述`，行来自子会话的视图，没有本地 cell（tui-panels.md §1.4）。
    // 欢迎块自己以空行结尾，不算一个块：它下面的第一个块同 Puffin 的首块，上面不再空行（§4.2「间距（Hang）」）。
    let mut prev = if let Some(header) = app.viewing_header() {
        out.push(HEADER_KEY, Rendered::new(header));
        Some(Spot::Other)
    } else {
        // 门道在欢迎块里：它的头代替第 4、5 行、块接在下面；关上之后第 4 行是接上了什么（doorway.rs）。
        let below = app.door_below_welcome();
        let rows = if app.door.is_some() && app.door_in_welcome() {
            Some(app.door_head())
        } else {
            app.connected.as_ref().filter(|c| c.in_welcome).map(|c| {
                let says = c.says.clone();
                (
                    rust_cli_tui_kit::app::widgets::welcome::done_row(&ctx.ink.theme, says),
                    Vec::new(),
                )
            })
        };
        let door = cells::Door {
            rows,
            below: below.as_ref(),
        };
        out.push(
            HEADER_KEY,
            Rendered::new(cells::header(
                app.view.mode(),
                app.newer_release.as_deref(),
                app.resumed.map(|at| (app.view.title(), at)),
                door,
                ctx,
            )),
        );
        None
    };
    let rows = app
        .viewing
        .as_ref()
        .map_or_else(|| app.view.rows(), |viewing| viewing.view.rows());
    let no_locals: &[LocalCell] = &[];
    let locals_of = if app.viewing.is_some() {
        no_locals
    } else {
        &app.local
    };
    let local_index = |local: &LocalCell| {
        app.local
            .iter()
            .position(|l| std::ptr::eq(l, local))
            .unwrap_or(0)
    };
    let push_local = |out: &mut Layout, prev: &mut Option<Spot>, local: &LocalCell| {
        let spaced = gap(*prev, Spot::Other, rich);
        if out
            .push_spaced(
                local_key(local_index(local)),
                Rendered::new(local_lines(&local.local, ctx)),
                None,
                spaced,
            )
            .is_some()
        {
            *prev = Some(Spot::Other);
        }
    };
    let mut locals = locals_of.iter().peekable();
    // 锚点为空的本地 cell 在会话头之后。
    while let Some(local) = locals.next_if(|l| l.anchor.is_none()) {
        push_local(&mut out, &mut prev, local);
    }
    // 播放头与动效按动画时钟走（界面启动后的时长，同工作行），不按 kernel 的墙上时钟：测试里那个时钟可以是固定的。
    let tick = i64::try_from(ctx.elapsed.as_millis() / u128::from(TICK_MS)).unwrap_or(i64::MAX);
    let view = app
        .viewing
        .as_ref()
        .map_or(&app.view, |viewing| &viewing.view);
    let viewing = app.viewing.is_some();
    if cache.motion_viewing != viewing {
        cache.motion.clear();
        cache.motion_viewing = viewing;
    }
    let cold = view.cold_until();
    cache.motion.observe(rows, cold, tick);
    cache.frame += 1;
    let frame = cache.frame;
    // 普通视图与详细视图（^O）都按 Puffin 的一段工作并行；详细视图里每一段按展开的样子铺。
    let mut built = cells::build(rows);
    if !ctx.expanded {
        cells::fold_waits(&mut built);
    }
    // 条目保持次序：一个还没打完的回答之后，什么都还没有（Puffin `holding`）。
    // ^O 时打出中的思考也让后面的等着：它所在的一段工作只到它为止（Puffin `holding` 的 `details_open`）。
    let holding = built.iter().position(|cell| {
        matches!(cell.kind, Kind::Agent | Kind::Group)
            && cell
                .rows
                .iter()
                .any(|row| cache.motion.holding(row, ctx.expanded))
    });
    if let Some(stop) = holding {
        built.truncate(stop + 1);
        let cell = &mut built[stop];
        if let Some(at) = cell
            .rows
            .iter()
            .position(|row| cache.motion.holding(row, ctx.expanded))
        {
            cell.more_after &= at + 1 == cell.rows.len();
            cell.rows.truncate(at + 1);
        }
    }
    // 「Stopped.」等停下的回合完全落定：回答打完、调用与通知的动效走完（Puffin `settle_stop` 的 `committed_count == len`）。
    if built
        .last()
        .is_some_and(|cell| cell.kind == Kind::Interrupted)
        && cache.motion.moving()
    {
        built.pop();
    }
    // 尾巴（工作光标、重试行、发出的消息，tui-kit.md §4.0「尾巴」）：笔骑在最新画的那一块上（进行中的一段工作、打出中的
    // 回答、在打出的思考），或单独一行；回答还在打出时回合结束了笔也还在。
    let mut tail = tail::begin(app, &mut cache.clocks, tick, ctx.now_ms, holding.is_some());
    // 按了 esc：笔尖在同一帧消失，回合结束、回答打完之后也不回来（Puffin `caret_on` 的 `stop_pending`，到「Stopped.」提交
    // 为止）；下一个回合开始时它不再是最近的回合。
    let stopping = !viewing && app.stop_pending.is_some() && app.stop_pending == view.latest_turn();
    if stopping {
        tail.pen = None;
    }
    tail::breathe(&mut tail, &mut cache.clocks, tick);
    let mut rode = false;
    let mut last = Last::Nothing;
    // 一段工作关上的时刻（收起的样子）与它里面一段读关上的时刻（展开的样子），标记从那里落定（Puffin `new_clocks`）。
    for cell in built.iter().filter(|cell| cell.kind == Kind::Group) {
        observe_run(&mut cache.motion, cell, rows, cold, tick);
    }
    let motion = &cache.motion;
    let entries = if ctx.expanded {
        &mut cache.detail
    } else {
        &mut cache.normal
    };
    // 流式中的块每帧都重排：回复的 markdown 经按块的缓存，只重排变了的块。
    let live_ctx = Ctx {
        markdown: Some(&cache.markdown),
        ..*ctx
    };
    // 回合结束那一行的 ` · <命令> still running`（修复线 F118，tui-display.md §2.7）：任务行的命令一段的同一个函数、同一份
    // 快照，每次画都现取（后台命令结束后已经显示的行也去掉这一段）。查看子代理时那些行不在（视图来自快照）。
    let still = app
        .viewing
        .is_none()
        .then(|| crate::ui::manage::pill(&app.snapshots.background, app.lang))
        .flatten();
    let mut pending_locals: Vec<&LocalCell> = locals.collect();
    // 搜索模式（修复线 F114，tui-input.md §2.3.2）：当前匹配所在的块按展开的样子排（临时展开）；折叠着的块另排一份完整
    // 形式供搜索。详细视图本来就是完整形式。
    let find = app.port.find.as_ref().filter(|_| !ctx.expanded);
    let peek = find.and_then(|find| find.current).map(|hit| hit.key);
    // 笔骑在最新画的一块上（Puffin `newest`）。
    let newest = built.len().saturating_sub(1);
    let busy = cells_busy(rows);
    let last_row = rows
        .iter()
        .rev()
        .find(|row| !matches!(row.body, Body::Turn(_)));
    let where_ = work::where_of(ctx);
    let clock = |id: u64| motion.clock(id);
    let head = |id: u64| motion.head(id);
    let figures = |tool_use: &str| view.figures(tool_use);
    let now = app.started + ctx.elapsed;
    // 请求在正文里的块（`ask_body::previews`，查看子代理时没有；第一块是在回答的那个）。挂在一个等你回答的行下面的，由
    // 那一段工作接在那一行下面（Puffin `asked` 的 `stack(row, preview)`）；其余的挂在内容里有那个工具调用的块之后，或在
    // 最后一个块之后（`Anchor::End`）。
    let previews = ask_body::previews(app, ctx.width, now);
    let asking_row = |id: &ToolUseId| {
        rows.iter().any(|row| {
            row.tool()
                .is_some_and(|tool| &tool.id == id && tool.status == ToolStatus::Approval)
        })
    };
    // 按次序编号：0 是在回答的那个。
    let (inline, mut rest): (Vec<_>, Vec<_>) = previews
        .iter()
        .enumerate()
        .partition(|(_, p)| matches!(&p.anchor, ask_body::Anchor::Row(id) if asking_row(id)));
    let marks = ask_body::Marks::of(app, now);
    let mark = |tool_use: &str| marks.mark(&ToolUseId::new(tool_use));
    let preview = |tool_use: &str| {
        inline
            .iter()
            .find(
                |(_, p)| matches!(&p.anchor, ask_body::Anchor::Row(id) if id.as_str() == tool_use),
            )
            .map(|(_, p)| p.lines.clone())
    };
    let live = Live {
        all: rows,
        where_: &where_,
        tick,
        now_ms: ctx.now_ms as f64,
        clock: &clock,
        head: &head,
        busy,
        stopping,
        figures: &figures,
        mark: &mark,
        preview: &preview,
        tasks: app.view.tasks(),
    };
    // 挑选回退的点：切线插在点的你的那块之前，它和下面的全部变暗（§4.2「回退（/rewind）」，Puffin `cut_of`）。
    let cut = (!viewing)
        .then(|| crate::ui::rewind_pick::cut(app, ctx.width))
        .flatten();
    for (index, cell) in built.iter().enumerate() {
        let cell_key = cell.ids().next().unwrap_or(0);
        let cut_here = cut.as_ref().is_some_and(|(row, _)| *row == cell_key);
        if let Some((_, line)) = cut.as_ref().filter(|_| cut_here) {
            push_cut(&mut out, line.clone(), app);
        }
        // 点击只展开这一个块；详细视图本来就是详细形式，不看单独展开。
        let opened = !ctx.expanded
            && (app.port.expanded_cells.contains(&cell_key) || peek == Some(cell_key));
        let still_here = still.as_deref().filter(|_| cell.kind == Kind::Duration);
        // 笔只给最新的一块，回合在跑、没有重试行代替它时（Puffin `live_entries` 的 `caret`）。
        let tip = tail.pen.filter(|_| index == newest && tail.retry_free());
        let answer = (cell.kind == Kind::Agent)
            .then(|| cell.rows[0])
            .and_then(|row| Some((row, motion.head(row.id)?)));
        let moving = cell.ids().any(|id| motion.moving_row(id));
        // 这一块里带着预览的那一行（块里的行号，与它第一个不空的行）。
        let mut asked_at = None;
        let (rendered, full) = if let Some((row, head)) = &answer {
            // 打出中的回答：播放头按词显示（§4.2「流式节奏」），每帧都排；回合结束后最后一个回答末尾的笔抬起（§4.2「笔」）。
            let Body::Agent { text, .. } = &row.body else {
                unreachable!("回答块")
            };
            let lifts = last_row.is_some_and(|last| last.id == row.id) && !busy && !stopping;
            let (lines, on) = crate::ui::markdown::streaming(
                &cache.markdown,
                text,
                head,
                tick,
                tip.as_ref(),
                lifts,
                stopping,
                ctx.width,
                ctx.ink,
            );
            rode |= on;
            (Rendered::new(lines), None)
        } else if cell.kind == Kind::Group && (cell.live() || moving || counted(cell, view)) {
            let open = ctx.expanded || opened;
            let shape = Shape {
                open,
                closed: cell.closed,
                more_after: cell.more_after,
                pen: tip,
            };
            rode |= tip.is_some() && !cell.closed && rides(cell, open);
            let laid = work::laid(&cell.rows, &live_ctx, &live, shape);
            asked_at = laid.asked.map(|at| (at, first_text(&laid.lines, at)));
            (Rendered::cell(laid.lines, !ctx.expanded), None)
        } else if let (Kind::Error, Body::Failure(f)) = (cell.kind, &cell.rows[0].body) {
            // 失败的块从地升起（它到达以来的 tick），写这个回合改没改过文件（要整个视图）。
            let age = motion.notice_age(cell_key, tick);
            // 查看子代理时不画主会话的上限（行 id 属于主会话的视图）。
            let back = (!viewing)
                .then(|| app.usage_back_line(cell.rows[0].id, f))
                .flatten();
            let lines = cells::failure::lines(cell.rows[0], f, rows, age, back, &live_ctx);
            (Rendered::cell(lines, false), None)
        } else if let (true, Body::ShellOutput { stdout, stderr }) = (moving, &cell.rows[0].body) {
            // `!` 命令的输出在命令结束（输出到达）时从 fg3 渐变到 fg2（Puffin `shell_block::output_rows`）。
            let age = motion
                .notice_age(cell_key, tick)
                .unwrap_or_else(cells::settled);
            let lines = cells::shell_output(stdout, stderr, age, &live_ctx);
            (Rendered::cell(lines, false), None)
        } else if let Some(lines) = moving
            .then(|| {
                cells::notice_entry(&cell.rows[0].body, &live_ctx, motion.fade(cell_key, tick))
            })
            .flatten()
        {
            // 通知的记号与种类从背景淡入（Puffin `notice_fade`）。
            (Rendered::cell(lines, false), None)
        } else if moving {
            (render(cell, &live_ctx, opened, still_here), None)
        } else {
            let rendered = cached(entries, cell, (ctx, &live_ctx), opened, frame, still_here);
            let full = (find.is_some() && !opened && rendered.expandable)
                .then(|| cached(entries, cell, (ctx, &live_ctx), true, frame, still_here));
            (rendered, full)
        };
        let here = spot(cell);
        // 切线下面空一行，同块之间的空行。
        let spaced = gap(prev, here, rich) || cut_here;
        let start = out.push_spaced(cell_key, rendered, full, spaced);
        if start.is_some() {
            prev = Some(here);
            last = last_of(here, answer.is_some() || cell.live());
        }
        if cell.kind == Kind::User
            && let Some(start) = start
            && let Body::User { text } = &cell.rows[0].body
        {
            let end = out.len();
            out.prompts.push(Prompt {
                start: entry_start(start, spaced),
                end,
                row_id: cell_key,
                text: text.clone(),
            });
        }
        // 在回答的那个的预览接在这一块里它的行下面：跟随停在那一行。
        if let (Some(start), Some((_, line))) = (start, asked_at)
            && inline.first().is_some_and(|(i, _)| *i == 0)
        {
            out.ask_line.get_or_insert(start + line);
        }
        if start.is_some() {
            let (under, others): (Vec<_>, Vec<_>) =
                rest.into_iter().partition(|(_, p)| asks_under(p, cell));
            rest = others;
            for (i, preview) in under {
                push_preview(&mut out, i, preview, false);
            }
        }
        // 锚定在这个 cell 最后一行的本地 cell 紧随其后。
        let last = cell.ids().last();
        let (anchored, rest): (Vec<&LocalCell>, Vec<&LocalCell>) = pending_locals
            .into_iter()
            .partition(|l| l.anchor.is_some() && l.anchor == last);
        for local in anchored {
            push_local(&mut out, &mut prev, local);
        }
        pending_locals = rest;
    }
    let locals_after = !pending_locals.is_empty();
    for local in pending_locals {
        push_local(&mut out, &mut prev, local);
    }
    if locals_after {
        last = Last::Said;
    }
    // 没挂上的（`Anchor::End`，或找不到它的行）：最后一个块之后，各自一块，照常空行。
    for (i, preview) in rest {
        let spaced = gap(prev, Spot::Other, rich);
        if push_preview(&mut out, i, preview, spaced) {
            prev = Some(Spot::Other);
        }
    }
    // 点的块没有画出来（还在等前面的回答打完）：切线在最后。
    if let Some((_, line)) = cut.filter(|_| out.cut_line.is_none()) {
        push_cut(&mut out, line, app);
    }
    // 手停住的那一刻不早于最后一个词落下（播放头这一帧动过的 tick）。
    let landed = motion.landed();
    tail::settle(&mut tail, &mut cache.clocks, tick, landed);
    tail::push(app, &mut out, &tail, last, rode, ctx);
    // 门道在 transcript 末尾（会话有了回合之后，doorway.rs）：自带上面的空行。
    if let Some(door) = app.door_at_end() {
        out.push(
            DOOR_KEY,
            Rendered::new(rust_cli_tui_kit::app::layout::Layout::lay(&door, ctx.width)),
        );
    }
    entries.retain(|_, entry| entry.frame == frame);
    out
}

/// 末尾的门道的键。
const DOOR_KEY: u64 = u64::MAX - (1 << 34);

/// 切线的块的键（与行 id、会话头、尾巴的键都不重）。
const CUT_KEY: u64 = u64::MAX - (1 << 33);

/// 切线：上一行不是空行时上面空一行（Puffin `cut_of`）；从它下面一行起变暗。
fn push_cut(out: &mut Layout, line: StyledLine, app: &App) {
    let above_blank = !out.is_empty() && out.styled(out.len() - 1).is_blank();
    if let Some(start) = out.push_spaced(CUT_KEY, Rendered::new(vec![line]), None, !above_blank) {
        out.cut_line = Some(start);
        out.dim = Some((start + 1, Rc::new(crate::ui::rewind_pick::dimmer(app))));
    }
}

/// 回合在跑：最后一个回合头是运行中（同 [`cells::build`]）。
fn cells_busy(rows: &[Row]) -> bool {
    rows.iter()
        .rev()
        .find_map(|row| match row.body {
            Body::Turn(state) => Some(state == crate::view::TurnState::Running),
            _ => None,
        })
        .unwrap_or(false)
}

/// 笔骑在这一段工作上：收起时在收拢行（此刻的那一行）上；展开时只在最后一个还在打出的思考上（Puffin `live_entries`）。
/// 一段工作里有查来了数字的子代理：按这一帧的数字画，不用缓存（缓存的形式不带数字，`cells::lines`）。
fn counted(cell: &cells::Cell<'_>, view: &crate::view::Transcript) -> bool {
    cell.rows.iter().any(|row| {
        row.tool()
            .is_some_and(|tool| view.figures(tool.id.as_str()).is_some())
    })
}

fn rides(cell: &cells::Cell<'_>, open: bool) -> bool {
    !open
        || cell.rows.last().is_some_and(|row| {
            matches!(
                row.body,
                Body::Reasoning {
                    streaming: true,
                    ..
                }
            )
        })
}

/// 记下一段工作（与展开时它里面的一段读）这一帧关没关上（Puffin `new_clocks` 的 `gathered` / `closed`）。
fn observe_run(motion: &mut Motion, cell: &cells::Cell<'_>, rows: &[Row], cold: u64, tick: i64) {
    let Some(head) = cell.rows.first() else {
        return;
    };
    if motion.settled(head.id) {
        return;
    }
    let fresh = head.id >= cold;
    motion.closed(head.id, cell.closed, fresh, false, tick);
    let held: Vec<cells::calls::Held<'_>> = cell
        .rows
        .iter()
        .filter_map(|row| cells::calls::Held::of(row))
        .collect();
    let members: Vec<_> = held.iter().map(cells::calls::Held::member).collect();
    let roles = work::open_roles(&members, rows, cell.more_after, cells_busy(rows));
    let mut all_closed = cell.closed;
    for (i, role) in roles.iter().enumerate() {
        if let work::Role::Head { closed, .. } = role {
            let id = cell.rows[i].id;
            motion.closed(id, *closed, id >= cold, true, tick);
            all_closed &= *closed;
        }
    }
    if all_closed {
        motion.settle(head.id);
    }
}

/// 请求预览的块的键（不进缓存；cell 的键是行 id，本地 cell 从 `u64::MAX − 1` 往下数）。
const ASK_KEY: u64 = u64::MAX / 2;

/// `lines` 从第 `from` 行起第一个不空的行（Puffin `ask_line`：条目的起点加上它第一个不空的行）。
fn first_text(lines: &[StyledLine], from: usize) -> usize {
    lines[from.min(lines.len())..]
        .iter()
        .position(|line| !line.spans.iter().all(|span| span.text.trim().is_empty()))
        .map_or(from, |at| from + at)
}

/// 请求的第 `index` 块自成一块；它是在回答的那个（`index` 为 0）时跟随停在它第一个不空的行。画出了时为真。
fn push_preview(out: &mut Layout, index: usize, preview: &ask_body::Preview, spaced: bool) -> bool {
    let Some(at) = out.push_spaced(ASK_KEY, Rendered::new(preview.lines.clone()), None, spaced)
    else {
        return false;
    };
    if index == 0 {
        out.ask_line
            .get_or_insert(at + first_text(&preview.lines, 0));
    }
    true
}

/// 预览挂在这个块下面：它的行里有那个工具调用。
fn asks_under(preview: &ask_body::Preview, cell: &cells::Cell<'_>) -> bool {
    match &preview.anchor {
        ask_body::Anchor::Row(id) => cell
            .rows
            .iter()
            .any(|row| row.tool().is_some_and(|tool| &tool.id == id)),
        ask_body::Anchor::End => false,
    }
}

/// 一个块排好的行：排版参数与内容都没变时用缓存里的（`opened` 是按展开的样子排；`tail` 是回合结束那一行接的 pill）。
fn cached(
    entries: &mut HashMap<(u64, bool), Entry>,
    cell: &cells::Cell<'_>,
    (ctx, live_ctx): (&Ctx<'_>, &Ctx<'_>),
    opened: bool,
    frame: u64,
    tail: Option<&str>,
) -> Rc<Rendered> {
    let versions = || cell.rows.iter().map(|row| (row.id, row.version));
    let cell_key = cell.ids().next().unwrap_or(0);
    // 还在变化的块每帧重排（流式中的回复与一段工作不经这里，`lines`）。
    if cell.live() {
        return render(cell, live_ctx, opened, tail);
    }
    let hit = entries.get_mut(&(cell_key, opened)).filter(|e| {
        e.width == ctx.width
            && e.expanded == opened
            && e.light == ctx.ink.light
            && e.kind == cell.kind
            && e.closed == cell.closed
            && versions().eq(e.versions.iter().copied())
            && e.tail.as_deref() == tail
    });
    if let Some(entry) = hit {
        entry.frame = frame;
        return entry.rendered.clone();
    }
    let rendered = render(cell, ctx, opened, tail);
    entries.insert(
        (cell_key, opened),
        Entry {
            versions: versions().collect(),
            width: ctx.width,
            expanded: opened,
            light: ctx.ink.light,
            kind: cell.kind,
            closed: cell.closed,
            tail: tail.map(str::to_owned),
            rendered: rendered.clone(),
            frame,
        },
    );
    rendered
}

/// 置顶的消息（Puffin `TranscriptView::pinned`，docs/puffin-tui.md §4.4「置顶行」）：`top` 所在的或它之上最近的一条你的消息，
/// 开头在 `top` 之上时（只露出一部分的也算）；换行画成空格。返回它与它的第一行。画法是 kit 的 `frame::pinned_row`。
/// 缺陷修复（工作线 TK 的 F）：以前只认整条滚出视口的消息（`end <= top`），并把空白折成一个空格，与 Puffin 不同。
/// 你的回合作为一个条目从哪一行起：上面的空行算它的（Puffin 的条目自带上面的间距，`TranscriptView::pinned` 按条目的起点）。
/// 修复：以前从板的上沿算，顶行落在那一行空行上时置顶的是上一条消息，Puffin 是空的 `›`。
pub(super) fn entry_start(start: usize, spaced: bool) -> usize {
    start.saturating_sub(usize::from(spaced))
}

pub fn pinned(prompts: &[Prompt], top: usize) -> Option<(String, usize)> {
    let prompt = prompts.iter().rev().find(|p| p.start <= top)?;
    (prompt.start < top).then(|| (prompt.text.replace('\n', " "), prompt.start))
}
