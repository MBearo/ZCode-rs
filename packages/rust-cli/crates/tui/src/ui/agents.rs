//! 输入框下面的子代理列表与只读查看（docs/specs/rust-cli/tui-panels.md §1；Host `ui/agents.rs`）。
//! 列表只是内核 `tasks_snapshot()` 的子代理条目（与 footer 的后台任务同一份快照，修复线
//! F34；runner 按请求序号写入）；这里只有
//! 视图状态：快照到达的时刻、焦点、正在查看的子会话视图。修复线 F30 起画成聚焦的树（可见集合与
//! 父子关系在 `agents_tree`，排版在这里，§1.3）。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::panels::{SubagentActivity, SubagentEntry, SubagentStatus};
use unicode_width::UnicodeWidthStr;

use super::agents_tree::{Forest, TreeRow};
use super::app::{Action, App};
use super::i18n_panels as words;
use super::panels::Panel;
use super::text::truncate;
use crate::view::Transcript;

/// 窗口里最多的子代理行数。
const MAX_ROWS: usize = 5;
/// 名字列的最小与最大宽度。
const LABEL_MIN: usize = 4;
const LABEL_MAX: usize = 28;

/// 内核给的最后一份子代理快照与它到达的时刻（用时与消失按它往后推，不比较两边的墙上时间）。
#[derive(Debug, Clone, Default)]
pub struct AgentList {
    pub entries: Vec<SubagentEntry>,
    pub at: Option<Instant>,
}

/// footer 下面获得焦点的东西：后台任务的 pill（§3.1），或子代理列表的第 i 行（0 = main）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Pill,
    Agents(usize),
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

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn since(list: &AgentList, now: Instant) -> u64 {
    list.at
        .map_or(0, |at| now.saturating_duration_since(at).as_millis() as u64)
}

/// 一行的用时：在跑时加上快照到达后经过的时间。
pub fn elapsed_ms(list: &AgentList, entry: &SubagentEntry, now: Instant) -> u64 {
    match entry.status {
        SubagentStatus::Running => entry.elapsed_ms + since(list, now),
        _ => entry.elapsed_ms,
    }
}

/// 重试或停顿还要等多久：快照里的剩余时长减去快照到达后经过的时间。
fn left_ms(list: &AgentList, activity: &SubagentActivity, now: Instant) -> u64 {
    let left = match activity {
        SubagentActivity::Retrying { retry_in_ms, .. }
        | SubagentActivity::Stalled { retry_in_ms } => *retry_in_ms,
        SubagentActivity::Thinking | SubagentActivity::Tool { .. } => 0,
    };
    left.saturating_sub(since(list, now))
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
    fn forest(&self, now: Instant) -> Forest<'_> {
        Forest::new(
            &self.agents.entries,
            since(&self.agents, now),
            self.viewing.as_ref().map(|v| &v.agent),
        )
    }

    /// 没驱逐的全部子代理，平铺、按启动顺序：Background 面板的代理（tui-panels.md §3.2）。驱逐见 §1.2：在跑的、
    /// 停放的、正在查看的、结束不到 30 秒的都在。
    pub fn agent_rows(&self, now: Instant) -> Vec<&SubagentEntry> {
        let forest = self.forest(now);
        self.agents
            .entries
            .iter()
            .filter(|entry| !forest.evicted(entry))
            .collect()
    }

    /// 输入框下面的列表的行：按查看位置的树（tui-panels.md §1.2，修复线 F30）。
    pub fn agent_tree(&self, now: Instant) -> Vec<TreeRow<'_>> {
        self.forest(now).rows()
    }

    /// footer 要不要写 `/tasks to see subagents`（tui-panels.md §1.4）：有隐藏的已完成后台代理。
    pub fn hidden_agents(&self, now: Instant) -> bool {
        self.forest(now).has_hidden()
    }

    /// 有在跑或待驱逐的条目：每秒重画一次（用时、隐藏与驱逐后的挂靠、`/tasks` 提示）。
    pub fn agents_ticking(&self, now: Instant) -> bool {
        // 停放的与正在查看的不会被驱逐，不用为它们计时。
        let viewing = self.viewing.as_ref().map(|v| &v.agent);
        self.agent_rows(now).iter().any(|row| {
            row.status == SubagentStatus::Running || (!row.parked && viewing != Some(&row.agent))
        })
    }

    /// 被查看的子代理不在快照里了（tui-panels.md §1.6）：它的 owner 还在且没驱逐时改看 owner（`Some(owner)`），
    /// 否则回到主会话（`Some(None)`）。还在快照里、或没在查看时为 None。
    pub fn viewed_gone(&self, now: Instant) -> Option<Option<AgentId>> {
        let viewing = self.viewing.as_ref()?;
        let entries = &self.agents.entries;
        if entries.iter().any(|entry| entry.agent == viewing.agent) {
            return None;
        }
        let forest = self.forest(now);
        Some(viewing.owner.clone().filter(|owner| {
            entries
                .iter()
                .find(|entry| &entry.agent == owner)
                .is_some_and(|entry| !forest.evicted(entry))
        }))
    }

    /// footer 下面的列表与每行对应的选择下标（0 = main）；没有可见的子代理且不在查看时为空（tui-panels.md §1.3）。
    pub fn agents_lines(
        &self,
        width: usize,
        now: Instant,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let rows = self.agent_tree(now);
        if rows.is_empty() && self.viewing.is_none() {
            return (Vec::new(), Vec::new());
        }
        let viewing = self.viewing.as_ref().map(|v| &v.agent);
        let selected = match self.focus {
            Some(Focus::Agents(index)) => Some(index),
            _ => None,
        };
        // 选中的行按选择列表组件画（`› ` 加底色条，tui.md §7.4，修复线 F26；原来是 `❯ `）。
        let prefix = |index: usize| {
            if selected == Some(index) {
                super::pick::POINTER
            } else {
                super::pick::BLANK
            }
        };
        let paint = |line: Line<'static>, index: usize| {
            super::pick::paint(line, selected == Some(index), &self.palette, width)
        };
        // 窗口：焦点是选中的子代理行，否则是正在查看的那一行（看 main 时第一行）；焦点落在窗口最下面一行。
        let focus = match selected {
            Some(index) if index >= 1 => index - 1,
            _ => viewing
                .and_then(|agent| rows.iter().position(|row| &row.entry.agent == agent))
                .unwrap_or(0),
        }
        .min(rows.len().saturating_sub(1));
        let start = focus
            .saturating_sub(MAX_ROWS - 1)
            .min(rows.len().saturating_sub(MAX_ROWS));
        let end = (start + MAX_ROWS).min(rows.len());
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut lines = vec![Line::default()];
        let mut targets = vec![(lines.len(), 0)];
        let main_style = match (viewing, selected) {
            (None, _) => bold,
            (Some(_), Some(0)) => Style::default(),
            _ => dim(),
        };
        let main_dot = if viewing.is_none() {
            super::cells::BULLET
        } else {
            "◯"
        };
        let mut main = vec![
            Span::raw(prefix(0)),
            Span::styled(
                format!("{main_dot} {}", words::main_line(self.lang)),
                main_style,
            ),
        ];
        // 窗口上面还有行：main 行的行尾写 `↑ N more`。
        if start > 0 {
            let above = words::more_above(self.lang, start);
            let used: usize = main.iter().map(|span| span.content.width()).sum();
            let pad = width.saturating_sub(used + above.width()).max(1);
            main.push(Span::raw(" ".repeat(pad)));
            main.push(Span::styled(above, dim()));
        }
        lines.push(paint(Line::from(main), 0));
        // 名字列宽：所有可见行（不只窗口里的）的 2 × 深度 + 名字宽 + 后缀宽，每行的名字格 = 列宽 −
        // 连接符宽，所以不同深度的描述列对齐。
        let label_width = rows
            .iter()
            .map(|row| 2 * row.depth + kind(row.entry).width() + suffix(row).width())
            .max()
            .unwrap_or(0)
            .clamp(LABEL_MIN, LABEL_MAX);
        for (index, row) in rows.iter().enumerate().take(end).skip(start) {
            let position = index + 1;
            let entry = row.entry;
            let viewed = viewing == Some(&entry.agent);
            let base = if viewed {
                bold
            } else if selected == Some(position) {
                Style::default()
            } else {
                dim()
            };
            // 停放的不上色（带 keepalive 的不算终态）。
            let color = match entry.status {
                SubagentStatus::Completed if !entry.parked => Some(Color::Green),
                SubagentStatus::Failed | SubagentStatus::Killed => Some(Color::Red),
                SubagentStatus::Completed | SubagentStatus::Running => None,
            };
            let dot_style = color.map_or(base, |c| base.fg(c));
            // 连接符：有效父代理的行在窗口里时才画，父行滚出窗口时这一行顶格。
            let connector = match row.parent {
                Some(parent) if row.depth > 0 && parent >= start => format!(
                    "{}{} ",
                    "  ".repeat(row.depth - 1),
                    if row.more_siblings { "├" } else { "└" }
                ),
                _ => String::new(),
            };
            let quiet = if viewed || selected == Some(position) {
                Style::default()
            } else {
                dim()
            };
            let name_box = label_width.saturating_sub(connector.width());
            let (name, tail) = (kind(entry), suffix(row));
            let mut name_spans = if name.width() + tail.width() <= name_box {
                vec![
                    Span::styled(name.to_owned(), base),
                    Span::styled(tail, dim()),
                ]
            } else {
                vec![Span::styled(
                    truncate(&format!("{name}{tail}"), name_box),
                    base,
                )]
            };
            let named: usize = name_spans.iter().map(|span| span.content.width()).sum();
            name_spans.push(Span::raw(" ".repeat(name_box.saturating_sub(named))));
            let status = super::cells::duration(elapsed_ms(&self.agents, entry, now));
            // 等用户回答优先；否则接当前活动（L35，stall-visibility.md §4.3）。
            let doing = if entry.waiting {
                (words::waiting(self.lang).to_owned(), false)
            } else if let Some(activity) = entry.activity.as_ref() {
                words::activity(self.lang, activity, left_ms(&self.agents, activity, now))
            } else {
                (String::new(), false)
            };
            let doing_style = if doing.1 {
                Style::default().fg(Color::Red)
            } else {
                dim()
            };
            // 缺陷修复（修复线 F26）：原来活动接在用时后面、描述的宽度按两者合计算，活动一变（`∴ Thinking` ↔ 工具名）
            // 用时就左右跳。现在用时右对齐在行尾，活动接在描述后面；活动变长时让描述截短，用时的位置不变。
            let fixed = 2 + connector.width() + 2 + name_box + 2;
            let tail_width = 2 + status.width();
            let doing_text = truncate(&doing.0, width.saturating_sub(fixed + tail_width));
            let room = width.saturating_sub(fixed + doing_text.width() + tail_width);
            let description = truncate(&entry.description, room);
            let pad = width
                .saturating_sub(fixed + description.width() + doing_text.width() + status.width());
            targets.push((lines.len(), position));
            let mut spans = vec![
                Span::raw(prefix(position)),
                Span::styled(connector, quiet),
                Span::styled(if viewed { super::cells::BULLET } else { "◯" }, dot_style),
                Span::raw(" "),
            ];
            spans.extend(name_spans);
            spans.extend([
                Span::raw("  "),
                Span::styled(description, dim()),
                Span::styled(doing_text, doing_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(status, dim()),
            ]);
            lines.push(paint(Line::from(spans), position));
        }
        // 行数超过 5 时最后一行：下面还有时 `↓ N more`，没有时空行（写一个空格，列表的高度不跳）。
        if rows.len() > MAX_ROWS {
            let below = rows.len() - end;
            lines.push(if below > 0 {
                Line::from(Span::styled(words::more(self.lang, below), dim()))
            } else {
                Line::from(" ")
            });
        }
        (lines, targets)
    }

    /// 输入框为空时按 ↓ 进入列表（选中 main）；没有可显示的子代理时返回 false。
    pub(super) fn focus_agents(&mut self, now: Instant) -> bool {
        if self.agent_tree(now).is_empty() && self.viewing.is_none() {
            return false;
        }
        self.focus = Some(Focus::Agents(0));
        true
    }

    /// 列表获得焦点时 footer 左侧的提示。
    pub fn agents_focus_hint(&self, now: Instant) -> Option<&'static str> {
        let Some(Focus::Agents(index)) = self.focus else {
            return None;
        };
        let rows = self.agent_tree(now);
        let stop = index
            .checked_sub(1)
            .and_then(|i| rows.get(i))
            .is_some_and(|row| stoppable(row.entry));
        Some(words::agents_hint(self.lang, stop))
    }

    /// 列表获得焦点时的按键（footer 的按键上下文）：↑ / ↓ 移动，enter 查看，esc 离开，x 停止；其他键离开
    /// 列表后照常处理。
    pub(super) fn agents_key(
        &mut self,
        index: usize,
        key: KeyEvent,
        now: Instant,
    ) -> Option<Action> {
        let rows = self.agent_tree(now);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up => self.focus = index.checked_sub(1).map(Focus::Agents),
            KeyCode::Char('p') if ctrl => self.focus = index.checked_sub(1).map(Focus::Agents),
            KeyCode::Down => self.focus = Some(Focus::Agents((index + 1).min(rows.len()))),
            KeyCode::Char('n') if ctrl => {
                self.focus = Some(Focus::Agents((index + 1).min(rows.len())));
            }
            KeyCode::Enter => {
                self.focus = None;
                return self.view_agent(index, now);
            }
            KeyCode::Esc => self.focus = None,
            KeyCode::Char('x') if !ctrl => {
                let row = rows.get(index.checked_sub(1)?)?.entry;
                if stoppable(row) {
                    return Some(Action::Manage(super::manage::Manage::StopTask {
                        id: row.agent.to_string(),
                        label: row.description.clone(),
                    }));
                }
            }
            _ => {
                self.focus = None;
                return self.dispatch(key, now);
            }
        }
        None
    }

    /// 查看列表里第 `index` 项（0 = 回到主会话；其余是树的行，§1.2）。
    pub(super) fn view_agent(&mut self, index: usize, now: Instant) -> Option<Action> {
        let agent = match index.checked_sub(1) {
            None => None,
            Some(row) => Some(self.agent_tree(now).get(row)?.entry.agent.clone()),
        };
        self.view_target(agent)
    }

    /// 查看某个子代理（None = 回到主会话）；已经在看它时不动。Background 面板（平铺）也经这里。
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

    /// 查看子代理时：esc 回到主会话，↓ 进入列表，翻页滚动，ctrl+o 详细视图；其余输入不生效（只读）。
    pub(super) fn viewing_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.viewport.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Esc if self.scroll > 0 => {
                self.scroll = 0;
                self.new_activity = false;
                None
            }
            KeyCode::Esc => Some(Action::Panel(Panel::ViewAgent(None))),
            KeyCode::Down => {
                self.focus_agents(now);
                None
            }
            KeyCode::PageUp => self.scroll_by(page),
            KeyCode::PageDown => self.scroll_by(-page),
            KeyCode::Char('o') if ctrl => {
                self.open_detail();
                None
            }
            // 中断与退出照常（审批、运行状态仍属于主会话）。
            KeyCode::Char('c' | 'd') if ctrl => self.compose_key(key, now),
            _ => None,
        }
    }

    /// 查看子代理时 transcript 顶部的一块：dim 的 `↳ 类型 · 描述`。
    pub fn viewing_header(&self) -> Option<Vec<Line<'static>>> {
        let viewing = self.viewing.as_ref()?;
        let mut text = format!("  ↳ {}", viewing.type_name());
        if !viewing.description.is_empty() {
            text.push_str(&format!(" · {}", viewing.description));
        }
        Some(vec![Line::default(), Line::from(Span::styled(text, dim()))])
    }
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agents_tree_tests.rs"]
mod tree_tests;
