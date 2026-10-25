// 改写自 apps/zcode-host/crates/tui/src/ui/cells/mod.rs（feat/rs 79b6691c55）：输入从 V4 行换成
// `view::Row`（docs/specs/rust-cli/tui.md §3）。
//! transcript 的块：视图里的行归类成块，每个块按当前宽度与紧凑 / 详细模式排版。块不保存状态；
//! 缓存键由所含行的 id 与版本组成。
mod agent;
mod diff;
mod group;
mod messages;
pub(crate) mod patch;
mod shell;
mod tools;
mod words;

use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::i18n::{Lang, Strings};
use super::palette::Palette;
use super::text::{plain, wrap_text};
use crate::view::{Body, Row, ToolRow, ToolStatus, TurnState};

/// 排版所需的上下文；只读。
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub palette: &'a Palette,
    pub lang: Lang,
    pub strings: &'a Strings,
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
    /// 流式回复的 markdown 排版缓存（只在 transcript 排流式中的块时给，tui.md §4.1，修复线 F6）。
    pub markdown: Option<&'a crate::ui::markdown::Memo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    User,
    Agent,
    Reasoning,
    Tool,
    /// 工作段：连续的推理与读取类工具（rust-tui.md §8.2.1）。
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
}

pub struct Cell<'a> {
    pub kind: Kind,
    pub rows: Vec<&'a Row>,
    /// 工作段在回合运行中、又是回合的最后一个块：成员都结束了也算进行中。
    pub active: bool,
    /// 工作段里已结束的推理用时合计（毫秒）。
    pub thought_ms: u64,
}

impl<'a> Cell<'a> {
    fn new(kind: Kind, rows: Vec<&'a Row>) -> Self {
        Self {
            kind,
            rows,
            active: false,
            thought_ms: 0,
        }
    }

    /// 块的行 id（第一个是块的缓存键）。
    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.rows.iter().map(|row| row.id)
    }

    /// 是否还在变化：这类块每帧都要重新排版（动画）。
    pub fn live(&self) -> bool {
        self.active || self.rows.iter().any(|row| row.live())
    }
}

/// 行 → 块。每个工具调用单独成块；中断的回合在末尾加提示。`group` 为真时（普通视图）连续的推理与
/// 读取类工具并成工作段，详细视图不并；`mcp`（绑定到远端机器的 MCP 服务器）的调用不并（R5，remote-exec.md §22.8）。
pub fn build<'a>(
    rows: impl IntoIterator<Item = &'a Row>,
    group: bool,
    mcp: &[(String, String)],
) -> Vec<Cell<'a>> {
    /// 回合结束：运行中的回合以工作段结尾时，工作段算进行中；中断的回合加提示。
    fn close<'a>(cells: &mut Vec<Cell<'a>>, turn: Option<&'a Row>, open: &mut Option<usize>) {
        let state = turn.and_then(|turn| match turn.body {
            Body::Turn(state) => Some(state),
            _ => None,
        });
        if let Some(at) = open.take()
            && state == Some(TurnState::Running)
        {
            cells[at].active = true;
        }
        if let (Some(turn), Some(TurnState::Interrupted)) = (turn, state) {
            cells.push(Cell::new(Kind::Interrupted, vec![turn]));
        }
    }
    let rows: Vec<&'a Row> = rows.into_iter().collect();
    let mut cells: Vec<Cell<'a>> = Vec::new();
    let mut turn: Option<&'a Row> = None;
    // 正在累积的工作段（cells 里的下标）；别的块出现时结束。
    let mut open: Option<usize> = None;
    for (index, &row) in rows.iter().enumerate() {
        let kind = match &row.body {
            Body::Turn(_) => {
                close(&mut cells, turn, &mut open);
                turn = Some(row);
                continue;
            }
            // 空的推理不显示，也不打断工作段。
            Body::Reasoning { text, .. } if group && text.trim().is_empty() => continue,
            _ if group && group::joins(row, mcp) => {
                let at = *open.get_or_insert_with(|| {
                    cells.push(Cell::new(Kind::Group, Vec::new()));
                    cells.len() - 1
                });
                if matches!(row.body, Body::Reasoning { .. }) {
                    let next = rows.get(index + 1).map(|later| later.created_ms);
                    cells[at].thought_ms += group::thought_ms(row, next);
                }
                cells[at].rows.push(row);
                continue;
            }
            Body::User { .. } | Body::Shell { .. } => Kind::User,
            Body::Agent { .. } => Kind::Agent,
            Body::Reasoning { .. } => Kind::Reasoning,
            Body::Tool(_) => Kind::Tool,
            Body::Marker(_) | Body::Fire(_) => Kind::Marker,
            Body::Error(_) => Kind::Error,
            Body::Output(_) | Body::ShellOutput { .. } => Kind::Output,
            Body::Notice { .. } | Body::Goal(_) => Kind::Notice,
            Body::Peer(_) => Kind::Peer,
        };
        open = None;
        cells.push(Cell::new(kind, vec![row]));
    }
    close(&mut cells, turn, &mut open);
    cells
}

/// 一个块的行。
pub fn render(cell: &Cell<'_>, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let row = cell.rows[0];
    match (cell.kind, &row.body) {
        (Kind::User, Body::User { text }) => messages::user(text, ctx),
        (Kind::User, Body::Shell { command }) => shell::input(command, ctx),
        (Kind::Output, Body::ShellOutput { stdout, stderr }) => shell::output(stdout, stderr, ctx),
        (Kind::Agent, Body::Agent { text, streaming }) => messages::agent(text, *streaming, ctx),
        (Kind::Reasoning, Body::Reasoning { text, .. }) => messages::reasoning(text, ctx),
        (Kind::Group, _) if ctx.expanded => group::expanded(cell, ctx),
        (Kind::Group, _) => group::summary(cell, ctx),
        (Kind::Marker, Body::Marker(marker)) => messages::marker(*marker, ctx),
        (Kind::Marker, Body::Fire(fire)) => messages::fire(fire, ctx),
        (Kind::Error, Body::Error(message)) => error_lines(message, ctx),
        (Kind::Output, Body::Output(text)) => detail_text(text, dim(), ctx),
        (Kind::Notice, Body::Notice { level, text }) => messages::notice(*level, text, ctx),
        (Kind::Notice, Body::Goal(status)) => crate::ui::goal::status_lines(status, ctx),
        (Kind::Interrupted, _) => vec![detail_line(words::interrupted(ctx.lang), dim())],
        (Kind::Tool, Body::Tool(tool)) => tools::tool(tool, ctx),
        (Kind::Peer, Body::Peer(peer)) => messages::peer(peer, ctx),
        _ => Vec::new(),
    }
}

/// 普通视图里一个块的行与它能不能点击展开：紧凑形式与详细形式不同的块可以展开；`opened` 时返回
/// 详细形式。
pub fn lay_out(cell: &Cell<'_>, ctx: &Ctx<'_>, opened: bool) -> (Vec<Line<'static>>, bool) {
    let form = |expanded| render(cell, &Ctx { expanded, ..*ctx });
    let compact = form(false);
    // 回复的两种形式相同（`messages::agent` 不看 `expanded`），不能展开：不再排第二遍、逐行比较（修复线 F6，
    // 流式中每个换行都要排一遍整篇回复）。
    if cell.kind == Kind::Agent {
        return (compact, false);
    }
    // 工作段只要有摘要就能展开，不展开时不必排一遍详细形式。
    let group = cell.kind == Kind::Group;
    let full = (opened || !group).then(|| form(true));
    // 比显示的文字，不比片段：两种形式的同一行可能拆成不同的片段。
    let shown = |lines: &[Line<'_>]| lines.iter().map(plain).collect::<Vec<_>>();
    let expandable = match &full {
        _ if group => !compact.is_empty(),
        Some(full) => !full.is_empty() && shown(full) != shown(&compact),
        None => false,
    };
    match full {
        Some(full) if opened && expandable => (full, true),
        _ => (compact, expandable),
    }
}

pub(crate) use agent::duration;
pub use messages::{error_lines, header};

/// 流式中的回复块显示的文字（只有这一行的回复块；别的块为 None）。排版只取决于它与排版参数，transcript 的缓存据此
/// 判断能不能用上一帧排好的行（修复线 F6）。
pub(crate) fn streaming_reply<'a>(cell: &Cell<'a>) -> Option<&'a str> {
    match (cell.kind, cell.rows.as_slice()) {
        (
            Kind::Agent,
            [
                Row {
                    body:
                        Body::Agent {
                            text,
                            streaming: true,
                        },
                    ..
                },
            ],
        ) => Some(messages::visible(text, true)),
        _ => None,
    }
}
pub(crate) use patch::{diff_lines, patch_rows};

// ── 共用 ──

/// 块的前导圆点。
pub const BULLET: &str = if cfg!(target_os = "macos") {
    "⏺"
} else {
    "●"
};
/// 细节行的前缀与续行缩进（5 列）。
pub(crate) const DETAIL: &str = "  ⎿  ";
pub(crate) const INDENT: &str = "     ";
/// 运行中圆点的闪烁周期。
const BLINK_MS: u128 = 600;

pub(crate) fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub(crate) fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub(crate) fn red() -> Style {
    Style::default().fg(Color::Red)
}

/// 行首圆点的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dot {
    Live,
    Success,
    Failed,
    Idle,
}

impl From<ToolStatus> for Dot {
    fn from(status: ToolStatus) -> Self {
        match status {
            ToolStatus::Pending | ToolStatus::Approval | ToolStatus::Running => Self::Live,
            ToolStatus::Success => Self::Success,
            ToolStatus::Error => Self::Failed,
            ToolStatus::Cancelled => Self::Idle,
        }
    }
}

/// 行首圆点：运行中与等待审批时 dim 并闪烁，成功绿色，失败红色，其余 dim。
pub(crate) fn dot(status: Dot, ctx: &Ctx<'_>) -> Span<'static> {
    let style = match status {
        Dot::Live => {
            if !(ctx.elapsed.as_millis() / BLINK_MS).is_multiple_of(2) {
                return Span::raw(" ");
            }
            dim()
        }
        Dot::Success => Style::default().fg(Color::Green),
        Dot::Failed => red(),
        Dot::Idle => dim(),
    };
    Span::styled(BULLET, style)
}

/// 一条细节行：`  ⎿  文字`。
pub(crate) fn detail_line(text: String, style: Style) -> Line<'static> {
    Line::from(vec![Span::styled(DETAIL, dim()), Span::styled(text, style)])
}

/// 多行细节：第一行 `  ⎿  `，其余缩进 5 列；按宽度换行。
pub(crate) fn detail_text(text: &str, style: Style, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    wrap_text(
        text,
        style,
        ctx.width,
        &[Span::styled(DETAIL, dim())],
        &[Span::raw(INDENT)],
    )
}

/// 藏起来的行数：`     … +N lines`，dim。
pub(crate) fn more_lines(hidden: usize, ctx: &Ctx<'_>) -> Line<'static> {
    Line::from(Span::styled(
        format!("{INDENT}{}", words::more_lines(ctx.lang, hidden)),
        dim(),
    ))
}

/// 工具行显示的路径：本机的工作区内写相对路径；远端的调用原样（F98，remote-exec.md §25.4.6：远端的路径与本机的工作区
/// 没有关系。缺陷修复，Docker 试用 L12：以前按本机工作区缩写，远端恰好同名的路径被写成相对的）。
pub(crate) fn shown_path(path: &str, tool: &ToolRow, ctx: &Ctx<'_>) -> String {
    if crate::ui::machines::machine_of(&tool.input).is_some() {
        path.to_owned()
    } else {
        relative(path, ctx.workspace)
    }
}

/// 工作区内的路径显示为相对路径。
pub(crate) fn relative(path: &str, workspace: &str) -> String {
    let base = workspace.trim_end_matches(['/', '\\']);
    path.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix(['/', '\\']))
        .filter(|rest| !rest.is_empty())
        .map_or_else(|| path.to_owned(), str::to_owned)
}

/// 输出文字的行：运行中取输出预览，结束后取完整输出。
pub(crate) fn output_lines(tool: &ToolRow) -> Vec<String> {
    let text = if tool.status.is_live() {
        tool.preview.as_str()
    } else {
        tool.output.as_str()
    };
    let mut lines: Vec<String> = text.trim_end().lines().map(str::to_owned).collect();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    lines
}

#[cfg(test)]
#[path = "cells_tests.rs"]
pub(crate) mod tests;
