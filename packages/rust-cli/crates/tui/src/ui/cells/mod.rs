// 改写自 apps/zcode-host/crates/tui/src/ui/cells/mod.rs（feat/rs 79b6691c55）：输入从 V4 行换成
// `view::Row`（docs/specs/rust-cli/tui.md §3）。
//! transcript 的块：视图里的行归类成块，每个块按当前宽度与紧凑 / 详细模式排版。块不保存状态；
//! 缓存键由所含行的 id 与版本组成。
pub(crate) mod calls;
mod duration;
pub(crate) mod failure;
pub(crate) mod look;
mod messages;
pub(crate) mod patch;
mod shell;
mod words;
pub(crate) mod work;

use std::time::Duration;

use rust_cli_tui_kit::render::{Style, StyledLine};

use super::i18n::Lang;
use super::ink::Ink;
use crate::view::{Body, Row, TurnState};

/// 排版所需的上下文；只读。
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// kit 的墨色（tui-kit.md §3.4、§3.8）：块只经它取色。
    pub ink: &'a Ink,
    pub lang: Lang,
    pub width: usize,
    pub expanded: bool,
    /// 动画时间（运行中的圆点闪烁）。
    pub elapsed: Duration,
    pub now_ms: u64,
    pub workspace: &'a str,
    /// 显示用的工作区路径（家目录写成 `~`）。
    pub workspace_label: &'a str,
    pub version: &'a str,
    /// 绑定到远端机器的 MCP 服务器（规范化的服务器名，机器名）：MCP 工具行标出机器（R5，remote-exec.md §22.8）。
    pub mcp_machines: &'a [(String, String)],
    /// 机器服务的快照（F98：远端的调用在等重连中的机器时，工具行写“等待重连”，remote-exec.md §25.4.1）。
    pub machines: &'a [crate::machines::MachineRow],
    /// 认得的技能（composer 的目录）：用户行里光秃的 `$名字` 是它们之一时画成提及（Puffin `Names.skills`）。
    pub skills: &'a [crate::ui::skill_mention::MentionSkill],
    /// 流式回复的 markdown 排版缓存（只在 transcript 排流式中的块时给，tui.md §4.1，修复线 F6）。
    pub markdown: Option<&'a crate::ui::markdown::Memo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    User,
    Agent,
    /// 一段工作：连着的思考与工具调用（Puffin `tools::roles`，docs/puffin-tui.md §4.2「工作的收拢」）。
    Group,
    Marker,
    Error,
    /// 本地命令的输出（`⎿` 行）。
    Output,
    /// 只供显示的系统提示（L18）。
    Notice,
    /// 中断的回合：最后一个块后面的提示（用回合行）。
    Interrupted,
    /// 跨会话的同伴消息与回执（cross-session.md §2.6）。
    Peer,
    /// 回合结束那一行（修复线 F118，tui-display.md §2.7）。
    Duration,
}

pub struct Cell<'a> {
    pub kind: Kind,
    pub rows: Vec<&'a Row>,
    /// 一段工作关上了：后面还有别的，或回合不在跑（Puffin `tools::run`）。
    pub closed: bool,
    /// 一段工作后面还有别的行（展开时一段读据此关上，Puffin `open_roles`）。
    pub more_after: bool,
}

impl<'a> Cell<'a> {
    fn new(kind: Kind, rows: Vec<&'a Row>) -> Self {
        Self {
            kind,
            rows,
            closed: true,
            more_after: false,
        }
    }

    /// 块的行 id（第一个是块的缓存键）。
    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.rows.iter().map(|row| row.id)
    }

    /// 是否还在变化：这类块每帧都要重新排版（动画）。
    pub fn live(&self) -> bool {
        (self.kind == Kind::Group && !self.closed) || self.rows.iter().any(|row| row.live())
    }
}

/// 一行是工作：思考或工具调用（Puffin `tools::work`）。空白的思考不成条目（Puffin 投影不收它）。
fn work(row: &Row) -> bool {
    match &row.body {
        Body::Tool(_) => true,
        Body::Reasoning { text, .. } => !text.trim().is_empty(),
        _ => false,
    }
}

/// 行 → 块（Puffin `tools::roles`）：连着的思考与工具调用是一段工作，一个块；别的行各自成块；中断的回合在末尾加提示。
/// 一段工作后面有别的行、或回合不在跑时，它关上了。回合头不显示，也不打断一段工作。以前只有推理与读取类的工具并成
/// 工作段，每个别的调用单独成块。
pub fn build<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Vec<Cell<'a>> {
    let rows: Vec<&'a Row> = rows.into_iter().collect();
    let busy = rows
        .iter()
        .rev()
        .find_map(|row| match row.body {
            Body::Turn(state) => Some(state == TurnState::Running),
            _ => None,
        })
        .unwrap_or(false);
    let mut cells: Vec<Cell<'a>> = Vec::new();
    // 正在累积的一段工作（cells 里的下标）；别的块出现时结束。
    let mut open: Option<usize> = None;
    // 停下的回合：「Stopped.」在它的最后一行之后（Puffin `settle_stop` 接在提交的条目末尾），即下一个回合头之前或末尾。
    // 以前画在回合头的位置，在你的消息上面。
    let mut stopped: Option<&'a Row> = None;
    let stop =
        |cells: &mut Vec<Cell<'a>>, open: &mut Option<usize>, stopped: &mut Option<&'a Row>| {
            if let Some(turn) = stopped.take() {
                if let Some(at) = open.take() {
                    cells[at].closed = true;
                }
                cells.push(Cell::new(Kind::Interrupted, vec![turn]));
            }
        };
    for &row in &rows {
        let kind = match &row.body {
            Body::Turn(state) => {
                stop(&mut cells, &mut open, &mut stopped);
                if *state == TurnState::Interrupted {
                    stopped = Some(row);
                }
                continue;
            }
            Body::Reasoning { text, .. } if text.trim().is_empty() => continue,
            _ if work(row) => {
                let at = *open.get_or_insert_with(|| {
                    cells.push(Cell::new(Kind::Group, Vec::new()));
                    cells.len() - 1
                });
                cells[at].rows.push(row);
                continue;
            }
            Body::User { .. } | Body::Shell { .. } => Kind::User,
            Body::Duration(_) => Kind::Duration,
            Body::Agent { .. } => Kind::Agent,
            Body::Compact(_) => Kind::Marker,
            Body::Failure(_) => Kind::Error,
            Body::Output(_) | Body::ShellOutput { .. } => Kind::Output,
            Body::Notice { .. }
            | Body::Fire(_)
            | Body::Goal(_)
            | Body::Returned { .. }
            | Body::Forked { .. } => Kind::Notice,
            Body::Peer(_) => Kind::Peer,
            Body::Reasoning { .. } | Body::Tool(_) => unreachable!("工作行在上面"),
        };
        if let Some(at) = open.take() {
            cells[at].more_after = true;
        }
        cells.push(Cell::new(kind, vec![row]));
    }
    stop(&mut cells, &mut open, &mut stopped);
    if let Some(at) = open {
        cells[at].closed = !busy;
    }
    cells
}

/// 落定的动效年龄（kit `tool_row::age` 没有时刻时）。
pub(crate) fn settled() -> i64 {
    rust_cli_tui_kit::app::widgets::tool_row::age(None, 0)
}

/// 一个块的行（kit 的类型，tui-kit.md §3.1）。一段工作要这一帧的动效（[`work::Live`]），由 transcript 画
/// （`transcript_lines.rs`）；这里按没有动效、关上的样子画（导出、搜索的完整形式）。
pub fn lines(cell: &Cell<'_>, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let row = cell.rows[0];
    match (cell.kind, &row.body) {
        (Kind::User, Body::User { text }) => messages::user(text, ctx),
        (Kind::User, Body::Shell { command }) => shell::input(command, ctx),
        (Kind::Output, Body::ShellOutput { stdout, stderr }) => {
            shell::output(stdout, stderr, settled(), ctx)
        }
        (Kind::Agent, Body::Agent { text, streaming }) => messages::agent(text, *streaming, ctx),
        (Kind::Group, _) => {
            let where_ = work::where_of(ctx);
            let still = |_: u64| rust_cli_tui_kit::app::widgets::tool_row::ToolClock::default();
            let none = |_: u64| None;
            let live = work::Live {
                all: &[],
                where_: &where_,
                tick: 0,
                now_ms: ctx.now_ms as f64,
                clock: &still,
                head: &none,
                busy: false,
                stopping: false,
                figures: &|_| None,
                mark: &|_| None,
                preview: &|_| None,
                tasks: &[],
            };
            let shape = work::Shape {
                open: ctx.expanded,
                closed: true,
                more_after: true,
                pen: None,
            };
            work::lines(&cell.rows, ctx, &live, shape)
        }
        (Kind::Marker, Body::Compact(compact)) => messages::compact(compact, ctx),
        (Kind::Error, Body::Failure(f)) => failure::lines(row, f, &[], None, None, ctx),
        (Kind::Output, Body::Output(text)) => detail_text(text, ctx.ink.dim(), ctx),
        (
            Kind::Notice | Kind::Peer,
            Body::Notice { .. } | Body::Fire(_) | Body::Peer(_) | Body::Returned { .. },
        ) => messages::notice_entry(&row.body, ctx, 1.0).unwrap_or_default(),
        (Kind::Notice, Body::Goal(status)) => crate::ui::goal::status_lines(status, ctx),
        (Kind::Notice, Body::Forked { title }) => look::lay(
            &rust_cli_tui_kit::app::widgets::rewind_rows::trace_row_said(
                &ctx.ink.theme,
                title.as_deref(),
                crate::ui::i18n_kit::rewind(ctx.lang),
            ),
            ctx.width,
        ),
        (Kind::Interrupted, _) => messages::interrupted(ctx),
        (Kind::Duration, _) => duration_cell(cell, None, ctx),
        _ => Vec::new(),
    }
}

/// 普通视图里一个块的行与它能不能点击展开：一段工作点开时按展开的样子铺（Puffin `opened`）；别的块折叠与展开的样子
/// 不同时可以展开；`opened` 时返回展开的样子。
pub fn lay_out(cell: &Cell<'_>, ctx: &Ctx<'_>, opened: bool) -> (Vec<StyledLine>, bool) {
    let form = |expanded| lines(cell, &Ctx { expanded, ..*ctx });
    let compact = form(opened && cell.kind == Kind::Group);
    if matches!(cell.kind, Kind::Agent | Kind::Group) {
        return (compact, cell.kind == Kind::Group);
    }
    let full = form(true);
    // 比显示的文字，不比片段：两种形式的同一行可能拆成不同的片段。
    let shown = |lines: &[StyledLine]| lines.iter().map(StyledLine::plain).collect::<Vec<_>>();
    let expandable = !full.is_empty() && shown(&full) != shown(&compact);
    if opened && expandable {
        (full, true)
    } else {
        (compact, expandable)
    }
}

pub use messages::{Door, error_lines, header, notice_entry, user};
/// 数字与时长的写法：唯一定义在 kernel-api `metering`（F58）。
pub(crate) use rust_cli_kernel_api::metering::format_duration as duration;
pub use shell::{input as shell_input, output as shell_output};

/// 回合结束那一行的块（修复线 F118）：`still_running` 是调用方每次画时现取的在跑的命令（`manage::pill`，transcript 给，
/// 别处为 None）。
pub(crate) fn duration_cell(
    cell: &Cell<'_>,
    still_running: Option<&str>,
    ctx: &Ctx<'_>,
) -> Vec<StyledLine> {
    match &cell.rows[0].body {
        Body::Duration(line) => duration::lines(line, still_running, ctx),
        _ => Vec::new(),
    }
}

/// 普通视图里相同的等待行只留第一行（修复线 F118，tui-display.md §2.7）：连着两行回合结束那一行都在等同样多的后台子
/// 代理、中间没有用户的输入（用户行、`!` 命令）时去掉第二行。详细视图不调它。
pub(crate) fn fold_waits(cells: &mut Vec<Cell<'_>>) {
    let mut waiting = 0;
    cells.retain(|cell| match (cell.kind, &cell.rows[0].body) {
        (Kind::User, _) => {
            waiting = 0;
            true
        }
        (Kind::Duration, Body::Duration(line)) => {
            let repeated = line.waiting_agents > 0 && line.waiting_agents == waiting;
            waiting = line.waiting_agents;
            !repeated
        }
        _ => true,
    });
}

// ── 共用 ──

/// 多行细节：在竖线后面按正文宽度折行。
pub(crate) fn detail_text(text: &str, style: Style, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    look::rail_text(text, style, ctx)
}

#[cfg(test)]
#[path = "cells_tests.rs"]
pub(crate) mod tests;
