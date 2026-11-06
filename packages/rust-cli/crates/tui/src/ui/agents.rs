//! 子代理的行与只读查看（docs/specs/rust-cli/tui-panels.md §1；Host `ui/agents.rs`）。
//! 列表只是内核 `tasks_snapshot()` 的子代理条目（与后台命令同一份快照，修复线 F34；runner 按请求序号写入）；这里只有
//! 视图状态：快照到达的时刻、正在查看的子会话视图。修复线 F30 起画成聚焦的树（可见集合与父子关系在 `agents_tree`，
//! 排版在这里，§1.3）。
//! 画法（docs/specs/rust-cli/tui-kit.md §3.1、§3.8，工作线 TK 的 C1）：只产出 kit 的行与片段，墨色经 `Ink`，宽度经 kit 的
//! `render::width`。行是任务卡的 Subagents（tui-tasks.md §3.2），同 `/resume` 的列表（kit `lists`），行尾是 Puffin 子代理的
//! 三列数字（kit `subagent_cols`：用时、token、速度）。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::panels::{SubagentActivity, SubagentEntry, SubagentStatus};
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::fit::{self, MEASURE_MARGIN, MEASURE_MAX};
use rust_cli_tui_kit::app::widgets::subagent_cols::{self, Cells};
use rust_cli_tui_kit::app::widgets::{lattice, lists};
use rust_cli_tui_kit::markdown::render::INDENT;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

use super::agents_tree::{Forest, TreeRow};
use super::app::{Action, App};
use super::i18n_panels as words;
use super::keyboard;
use super::lines::truncate;
use super::panels::Panel;
use crate::view::Transcript;

/// 名字列的最小与最大宽度。
const LABEL_MIN: usize = 4;
const LABEL_MAX: usize = 28;

/// 内核给的最后一份子代理快照、它到达的时刻与画面的时刻，都是墙上时间（runner 注入的时钟，Unix 毫秒）。用时、倒计时与
/// 驱逐按 `shown − at` 往后推（快照里的用时是内核算好的，不比较两边的墙上时间）。
/// 缺陷修复（twin-harness 报告，2026-10-31）：以前按动画的 `Instant` 推，孪生里动画按 tick 走，墙上时间走了而子代理的
/// 用时不动；秒级的数一律按墙上时间（团队的决定），动画的时钟只给动画。
#[derive(Debug, Clone, Default)]
pub struct AgentList {
    pub entries: Vec<SubagentEntry>,
    /// 快照到达的墙上时间。
    pub at: Option<u64>,
    /// 最近一帧的墙上时间（`draw::draw` 在画之前写下）：画面与按键用同一个时刻，选择下标对得上画出来的行。
    pub shown: u64,
}

/// 正在查看的子代理：它的只读视图与载入时条目的版本与状态（变了就重取）。
pub struct Viewing {
    pub agent: AgentId,
    pub kind: String,
    pub description: String,
    pub version: u64,
    pub status: SubagentStatus,
    pub view: Transcript,
    /// 已折进 `view` 的消息 id（刷新时新快照以它们为前缀就只追加，tui-panels.md §1.6）。
    pub messages: Vec<rust_cli_kernel_api::ids::MessageId>,
    /// 载入时条目的 owner（原始的父代理）：它不在快照里了时改看 owner（tui-panels.md §1.6）。
    pub owner: Option<AgentId>,
}

impl Viewing {
    /// 显示用的类型名：类型为空时 `agent`（与列表的名字列相同）。
    pub fn type_name(&self) -> &str {
        if self.kind.is_empty() {
            "agent"
        } else {
            &self.kind
        }
    }
}

/// 快照到达后经过的墙上时间（到画面的时刻）。
fn since(list: &AgentList) -> u64 {
    list.at.map_or(0, |at| list.shown.saturating_sub(at))
}

/// 一行的用时：在跑时加上快照到达后经过的时间。
pub fn elapsed_ms(list: &AgentList, entry: &SubagentEntry) -> u64 {
    match entry.status {
        SubagentStatus::Running => entry.elapsed_ms + since(list),
        _ => entry.elapsed_ms,
    }
}

/// 重试或停顿还要等多久：快照里的剩余时长减去快照到达后经过的时间。
pub fn left_ms(list: &AgentList, activity: &SubagentActivity) -> u64 {
    let left = match activity {
        SubagentActivity::Retrying { retry_in_ms, .. }
        | SubagentActivity::Stalled { retry_in_ms } => *retry_in_ms,
        SubagentActivity::Thinking | SubagentActivity::Tool { .. } => 0,
    };
    left.saturating_sub(since(list))
}

/// 能停止：在跑或停放的后台子代理（与 TaskStop 的“能停”同一个判断）。
pub fn stoppable(entry: &SubagentEntry) -> bool {
    entry.background && entry.status == SubagentStatus::Running
}

/// 名字列的名字：类型，没有时 `agent`。
fn kind(entry: &SubagentEntry) -> &str {
    if entry.agent_type.is_empty() {
        "agent"
    } else {
        &entry.agent_type
    }
}

/// 子代理的缺省机器在远端时名字后面的 ` @机器`（远端执行 R4，remote-exec.md §21.6）。
fn machine_suffix(entry: &SubagentEntry) -> String {
    entry
        .machine
        .remote()
        .map_or_else(String::new, |machine| format!(" @{machine}"))
}

/// 记号（Puffin 的记号）：等你回答 `●` code；完成 `✓` ok（停放的不算终态）；失败 `✗` err；停止 `–` fg3；其余 `○` fg3。
fn mark(theme: &Theme, entry: &SubagentEntry) -> Span {
    if entry.waiting {
        return Span::new("●", Style::fg(theme.code));
    }
    match entry.status {
        SubagentStatus::Completed if !entry.parked => Span::new("✓", Style::fg(theme.ok)),
        SubagentStatus::Failed => Span::new("✗", Style::fg(theme.err)),
        SubagentStatus::Killed => Span::new("–", theme.fg3),
        SubagentStatus::Completed | SubagentStatus::Running => Span::new("○", theme.fg3),
    }
}

/// 名字后面的 ` (+N)`。
fn suffix(row: &TreeRow<'_>) -> String {
    if row.descendants > 0 {
        format!(" (+{})", row.descendants)
    } else {
        String::new()
    }
}

impl App {
    /// 这份快照在当前查看位置、当前时刻的样子（tui-panels.md §1.2）。
    fn forest(&self) -> Forest<'_> {
        Forest::new(
            &self.snapshots.agents.entries,
            since(&self.snapshots.agents),
            self.viewing.as_ref().map(|v| &v.agent),
        )
    }

    /// 没驱逐的全部子代理，平铺、按启动顺序。驱逐见 tui-panels.md §1.2：在跑的、停放的、正在查看的、结束不到 30 秒的都在。
    pub fn agent_rows(&self) -> Vec<&SubagentEntry> {
        let forest = self.forest();
        self.snapshots
            .agents
            .entries
            .iter()
            .filter(|entry| !forest.evicted(entry))
            .collect()
    }

    /// 输入框下面的列表的行：按查看位置的树（tui-panels.md §1.2，修复线 F30）。
    pub fn agent_tree(&self) -> Vec<TreeRow<'_>> {
        self.forest().rows()
    }

    /// 有在跑或待驱逐的条目：每秒重画一次（用时、隐藏与驱逐后的挂靠）。
    pub fn agents_ticking(&self) -> bool {
        // 停放的与正在查看的不会被驱逐，不用为它们计时。
        let viewing = self.viewing.as_ref().map(|v| &v.agent);
        self.agent_rows().iter().any(|row| {
            row.status == SubagentStatus::Running || (!row.parked && viewing != Some(&row.agent))
        })
    }

    /// 正在查看的子代理在列表快照里的条目（修复线 F112：查看时的状态行与命中率的查询对象都取自它）。
    pub fn viewed_entry(&self) -> Option<&SubagentEntry> {
        let viewing = self.viewing.as_ref()?;
        self.snapshots
            .agents
            .entries
            .iter()
            .find(|entry| entry.agent == viewing.agent)
    }

    /// 被查看的子代理不在快照里了（tui-panels.md §1.6）：它的 owner 还在且没驱逐时改看 owner（`Some(owner)`），
    /// 否则回到主会话（`Some(None)`）。还在快照里、或没在查看时为 None。
    pub fn viewed_gone(&self) -> Option<Option<AgentId>> {
        let viewing = self.viewing.as_ref()?;
        let entries = &self.snapshots.agents.entries;
        if entries.iter().any(|entry| entry.agent == viewing.agent) {
            return None;
        }
        let forest = self.forest();
        Some(viewing.owner.clone().filter(|owner| {
            entries
                .iter()
                .find(|entry| &entry.agent == owner)
                .is_some_and(|entry| !forest.evicted(entry))
        }))
    }

    /// 任务卡的 Subagents（tui-tasks.md §3.2；tui-panels.md §1.3）：照 Puffin 的 picker（`/resume` 的列表）：没有头，第一项是
    /// 主会话，然后是树的行；窗口是 kit 的 `picker_window`，藏起的行各用一行 `↑ N more` / `↓ N more`；行尾是 Puffin 子代理的
    /// 三列数字（[`Self::figure_cells`]）。返回排好的行与每个子代理行的（行，选择下标；0 = main）。以前是 ^T 打开的一层，槽是 `x stop`。
    pub fn subagent_rows(
        &self,
        selected: usize,
        width: usize,
    ) -> (Vec<StyledLine>, Vec<(usize, usize)>) {
        let rows = self.agent_tree();
        let theme = &self.ink.theme;
        let count = rows.len() + 1;
        let sel = selected.min(count - 1);
        let (from, n) = lists::picker_window(sel, count);
        // 行自己的字从 INDENT 之后起，止于量度（kit `lists::row` 的 `measured`）。
        let edge = width
            .saturating_sub(MEASURE_MARGIN)
            .min(MEASURE_MAX)
            .saturating_sub(string_width(INDENT));
        let keep = subagent_cols::keep_for(edge as i64);
        let viewing = self.viewing.as_ref().map(|v| &v.agent);
        // 名字列宽：所有可见行（不只窗口里的）的 2 × 深度 + 名字宽 + 后缀宽，主会话的名字也算；每行的名字格 = 列宽 −
        // 连接符宽，所以不同深度的说明列对齐。
        let label_width = rows
            .iter()
            .map(|row| {
                2 * row.depth
                    + string_width(kind(row.entry))
                    + string_width(&suffix(row))
                    + string_width(&machine_suffix(row.entry))
            })
            .chain([string_width(words::main_line(self.lang))])
            .max()
            .unwrap_or(0)
            .clamp(LABEL_MIN, LABEL_MAX);
        let name_style = |item: usize| {
            if item == sel {
                self.ink.selection()
            } else {
                theme.fg2
            }
        };
        let current = || Span::new(format!("  {}", words::viewed(self.lang)), theme.fg3);
        // 藏起的行：名字的列（记号与一格之后），fg3。
        let more = |text: String| lists::row(vec![Span::plain("  "), Span::new(text, theme.fg3)]);
        let mut items: Vec<(Element, Option<usize>)> = Vec::new();
        if from > 0 {
            items.push((more(words::more_above(self.lang, from)), None));
        }
        for item in from..from + n {
            let Some(row) = item.checked_sub(1).map(|i| &rows[i]) else {
                let name = words::main_line(self.lang);
                let mut left = vec![Span::plain("  "), Span::new(name.to_owned(), name_style(0))];
                if viewing.is_none() {
                    left.push(Span::plain(
                        " ".repeat(label_width.saturating_sub(string_width(name))),
                    ));
                    left.push(current());
                }
                items.push((lists::row(left), Some(0)));
                continue;
            };
            let entry = row.entry;
            // 连接符：有效父代理的行在窗口里时才画，父行滚出窗口时这一行顶格。
            let connector = match row.parent {
                Some(parent) if row.depth > 0 && parent + 1 >= from => format!(
                    "{}{} ",
                    "  ".repeat(row.depth - 1),
                    if row.more_siblings { "├" } else { "└" }
                ),
                _ => String::new(),
            };
            let name_box = label_width.saturating_sub(string_width(&connector));
            let (name, tail, machine) = (kind(entry), suffix(row), machine_suffix(entry));
            let base = name_style(item);
            let mut left = vec![mark(theme, entry), Span::plain(" ")];
            left.push(Span::new(connector, theme.line));
            if string_width(name) + string_width(&tail) + string_width(&machine) <= name_box {
                left.extend([
                    Span::new(name.to_owned(), base),
                    Span::new(tail, theme.fg3),
                    Span::new(machine, Style::fg(self.ink.accent())),
                ]);
            } else {
                left.push(Span::new(
                    truncate(&format!("{name}{tail}{machine}"), name_box),
                    base,
                ));
            }
            let named: usize = left
                .iter()
                .skip(2)
                .map(|span| string_width(&span.text))
                .sum();
            left.push(Span::plain(" ".repeat(label_width.saturating_sub(named))));
            left.push(Span::plain("  "));
            left.push(Span::new(entry.description.clone(), theme.fg3));
            // 状态（Puffin 名单的状态）：等你回答优先（code 色），否则当前活动（L35，stall-visibility.md §4.3）。
            if entry.waiting {
                left.push(Span::new(
                    format!("  {}", words::waiting(self.lang)),
                    Style::fg(theme.code),
                ));
            } else if let Some(activity) = entry.activity.as_ref() {
                let (text, failing) = words::activity(
                    self.lang,
                    activity,
                    left_ms(&self.snapshots.agents, activity),
                );
                let text = text.trim_start_matches(" · ");
                if !text.is_empty() {
                    let style = if failing {
                        Style::fg(theme.err)
                    } else {
                        theme.fg3
                    };
                    left.push(Span::new(format!("  {text}"), style));
                }
            }
            if viewing == Some(&entry.agent) {
                left.push(current());
            }
            let cells = self.figure_cells(entry);
            let right = subagent_cols::columns(&cells, keep);
            let spans = subagent_cols::right_at(&left, if keep > 0 { &right } else { &[] }, edge);
            items.push((lists::row(spans), Some(item)));
        }
        if from + n < count {
            let below = count - from - n;
            items.push((
                more(words::more(self.lang, below).trim_start().to_owned()),
                None,
            ));
        }
        let mut lines = Vec::new();
        let mut targets = Vec::new();
        for (row, item) in items {
            if let Some(item) = item {
                targets.push((lines.len(), item));
            }
            lines.extend(Layout::lay(&row, width));
        }
        (lines, targets)
    }

    /// 一行的三列（Puffin「子代理的数字」，tui-panels.md §1.3），与 transcript 里它的 Agent 行同一个数（`cells::work`
    /// 的 `subagent_cells`）。缺陷修复（修复线 F26）：活动一变用时不再左右跳，它在列里右对齐，行自己的字放不下时从后面截。
    /// 2026-11-02 以前只有用时一列：查来的数字按 Agent 调用记，子代理派生的子代理与卡里的行对不上它。
    fn figure_cells(&self, entry: &SubagentEntry) -> Cells {
        let theme = &self.ink.theme;
        let running = entry.status == SubagentStatus::Running;
        let ms = elapsed_ms(&self.snapshots.agents, entry) as f64;
        let time = if running {
            lattice::clock(ms)
        } else {
            fit::time(ms)
        };
        let figures = self.view.child_figures(&entry.child).unwrap_or_default();
        // 结束后取派生它的 Agent 调用报告的 token（同 Agent 行），没有那个调用或没报告时是子会话的和。
        let reported = (!running)
            .then(|| self.view.spawned_by(&entry.child))
            .flatten()
            .and_then(|tool_use| reported_tokens(&self.view, tool_use));
        let tokens = reported.or(figures.tokens.map(|t| t as f64));
        let speed = if running { figures.last } else { figures.avg };
        let number = |n: String, unit: &'static str| {
            vec![Span::new(n, theme.fg3), Span::new(unit, theme.quiet)]
        };
        Cells {
            time: vec![Span::new(time, theme.fg3)],
            tokens: tokens.map_or_else(Vec::new, |n| number(fit::count(n), " tok")),
            speed: speed.map_or_else(Vec::new, |v| number(fit::rate(v), " tok/s")),
        }
    }

    /// 查看某个子代理（None = 回到主会话）；已经在看它时不动。
    pub(super) fn view_target(&self, agent: Option<AgentId>) -> Option<Action> {
        match agent {
            None => self
                .viewing
                .is_some()
                .then_some(Action::Panel(Panel::ViewAgent(None))),
            Some(agent) if self.viewing.as_ref().is_some_and(|v| v.agent == agent) => None,
            Some(agent) => Some(Action::Panel(Panel::ViewAgent(Some(agent)))),
        }
    }

    /// 查看子代理时：esc 回到主会话，^T 与 ↓ 打开任务卡（tui-tasks.md §3.1），翻页滚动，ctrl+o 详细视图；其余输入不生效（只读）。
    pub(super) fn viewing_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let ctrl = keyboard::ctrl(&key);
        let page = self.viewport.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Esc if self.port.scroll > 0 => {
                self.port.scroll = 0;
                self.port.new_activity = false;
                None
            }
            KeyCode::Esc => Some(Action::Panel(Panel::ViewAgent(None))),
            KeyCode::Down => {
                if self.has_tasks() {
                    self.open_tasks();
                }
                None
            }
            KeyCode::Char('t') if ctrl => {
                self.open_tasks();
                None
            }
            KeyCode::PageUp => self.scroll_by(page),
            KeyCode::PageDown => self.scroll_by(-page),
            KeyCode::Char('o') if ctrl => {
                self.open_detail();
                None
            }
            // 搜索模式（修复线 F114，tui-input.md §2.3.1）：输入框只读、没有斜杠列表，单个 `/` 就进入；紧跟的第二个 `/`
            // 丢掉，手打 `//` 与主会话的结果相同。搜的是被查看的子会话。
            KeyCode::Char('/') if !ctrl => {
                self.open_find(true);
                None
            }
            // 中断与退出照常（审批、运行状态仍属于主会话）。
            KeyCode::Char('c' | 'd') if ctrl => self.compose_key(key, now),
            _ => None,
        }
    }

    /// 查看子代理时 transcript 顶部的一块（自拟，tui-kit.md §4.3「子代理列表与只读查看」）：空一行，第 1 列是 `agent` 色的
    /// `↳`（记号的列），第 3 列起是 `agent` 色的类型，然后是 fg3 的 ` · 描述`。
    pub fn viewing_header(&self) -> Option<Vec<StyledLine>> {
        let viewing = self.viewing.as_ref()?;
        let theme = &self.ink.theme;
        let mut spans = vec![
            Span::plain(" "),
            Span::new("↳", Style::fg(theme.agent)),
            Span::plain(" "),
            Span::new(viewing.type_name().to_owned(), Style::fg(theme.agent)),
        ];
        if !viewing.description.is_empty() {
            spans.push(Span::new(format!(" · {}", viewing.description), theme.fg3));
        }
        Some(vec![StyledLine::default(), StyledLine::new(spans)])
    }
}

/// 主会话里 Agent 调用 `tool_use` 报告的 `subagent_tokens`（结束了才有）。
fn reported_tokens(view: &Transcript, tool_use: &str) -> Option<f64> {
    use rust_cli_tui_kit::app::widgets::tools;
    view.rows().iter().find_map(|row| {
        let crate::view::Body::Tool(tool) = &row.body else {
            return None;
        };
        if tool.id.as_str() != tool_use {
            return None;
        }
        let owned = super::cells::calls::owned(row, tool);
        tools::reported(&super::cells::calls::call(tool, &owned), "subagent_tokens")
    })
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agents_tree_tests.rs"]
mod tree_tests;
