//! 任务卡（docs/specs/rust-cli/tui-tasks.md §3）：^T、`/tasks` 与兼容的 ↓ 打开，是输入框的板向上长出的一层（同 Puffin 工作流
//! 的卡）。第一行列出种类，←→ 换；Subagents 是子代理的树，Commands 是在跑的命令（⏎ 展开输出），Todos 是整个清单。卡只存
//! 视图状态（种类、选中的行、展开着输出的命令）；行都从快照与视图现算（`task_card_rows.rs` 画）。以前同样的东西散在子代理的
//! 层、Background 面板与 Shell 详情页里（用户决定 2026-10-31）。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::ids::AgentId;

use super::app::{Action, App, Overlay};
use super::background::ShellView;
use super::manage::Manage;
use super::panels::Panel;

/// 卡里的种类，按种类一行的次序。工作流到了这个 CLI 时是第四个（Puffin 的卡）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Subagents,
    Commands,
    Todos,
}

pub const KINDS: [TaskKind; 3] = [TaskKind::Subagents, TaskKind::Commands, TaskKind::Todos];

/// 卡的视图状态。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskCard {
    pub kind: TaskKind,
    /// Subagents 选中的代理（`None` = main）。记代理不记行号：查看换了、子代理来去时行会挪，选中的跟着代理走
    /// （tui-tasks.md §3.2）；行号由 [`App::agent_row`] 每次从树里现算。
    pub agent: Option<AgentId>,
    /// Commands 选中的行。
    pub command: usize,
    /// Todos 窗口的第一步。
    pub todo_top: usize,
    /// 展开着输出的命令。
    pub peek: Option<ShellView>,
}

impl App {
    /// 这个种类此刻有没有东西（空的种类在种类一行里是 quiet，←→ 跳过它）。
    pub fn kind_has(&self, kind: TaskKind) -> bool {
        match kind {
            TaskKind::Subagents => !self.agent_tree().is_empty() || self.viewing.is_some(),
            TaskKind::Commands => !self.shell_rows().is_empty(),
            TaskKind::Todos => !self.view.tasks().is_empty(),
        }
    }

    /// 有东西可看（兼容的 ↓ 只在这时打开卡）。
    pub fn has_tasks(&self) -> bool {
        KINDS.iter().any(|kind| self.kind_has(*kind))
    }

    pub(super) fn task_card(&self) -> Option<&TaskCard> {
        match &self.overlay {
            Some(Overlay::Tasks(card)) => Some(card),
            _ => None,
        }
    }

    /// 新开的卡（tui-tasks.md §3.2「打开时选中哪一行」）：Subagents 选中正在查看的子代理，否则第一个子代理，都没有时 main。
    fn new_card(&self, kind: TaskKind) -> TaskCard {
        let agent = match &self.viewing {
            Some(viewing) => Some(viewing.agent.clone()),
            None => self.agent_tree().first().map(|row| row.entry.agent.clone()),
        };
        TaskCard {
            kind,
            agent,
            command: 0,
            todo_top: 0,
            peek: None,
        }
    }

    /// 卡选中的代理在 Subagents 里是第几行（0 = main）；它离开了树（被驱逐）时是 main。
    pub(super) fn agent_row(&self, card: &TaskCard) -> usize {
        let Some(agent) = &card.agent else {
            return 0;
        };
        self.agent_tree()
            .iter()
            .position(|row| &row.entry.agent == agent)
            .map_or(0, |i| i + 1)
    }

    /// 卡此刻选中的代理（画出来的那一行的；离开了树的算 main）。⏎ 与槽都按它，免得画的是 main、⏎ 却去看被驱逐的那一个。
    pub(super) fn selected_agent(&self, card: &TaskCard) -> Option<AgentId> {
        self.agent_at(self.agent_row(card))
    }

    /// Subagents 第 `row` 行的代理（0 = main，`None`）。
    pub(super) fn agent_at(&self, row: usize) -> Option<AgentId> {
        let row = row.checked_sub(1)?;
        self.agent_tree()
            .get(row)
            .map(|row| row.entry.agent.clone())
    }

    pub(super) fn task_card_mut(&mut self) -> Option<&mut TaskCard> {
        match self.overlay.as_mut() {
            Some(Overlay::Tasks(card)) => Some(card),
            _ => None,
        }
    }

    /// 打开卡（tui-tasks.md §3.1）：有等你回答的子代理时落在 Subagents；否则上次看的种类（还有东西时）；否则第一个有东西的。
    /// 三个都空时卡的位置说一句，到下一个键（`no_tasks`）。
    pub fn open_tasks(&mut self) {
        let waiting = self.task_counts().waiting > 0;
        let kind = if waiting {
            Some(TaskKind::Subagents)
        } else {
            self.task_kind
                .filter(|kind| self.kind_has(*kind))
                .or_else(|| KINDS.into_iter().find(|kind| self.kind_has(*kind)))
        };
        match kind {
            Some(kind) => {
                self.task_kind = Some(kind);
                self.overlay = Some(Overlay::Tasks(self.new_card(kind)));
            }
            None => self.no_tasks = true,
        }
    }

    /// ^T 此刻能打开卡（§3.1：草稿为空、没有请求）。任务行的 `^T` 不能时是 quiet 墨，也不能点（§2.2「点击」）。
    pub(super) fn tasks_key_works(&self) -> bool {
        self.composer.is_empty() && !self.asking()
    }

    /// 点任务行的左边（§2.2「点击」）：点的是一个部分时落在它的种类（那个种类此刻空了时照 [`Self::open_tasks`]），
    /// 记号与 `^T` 同按 ^T。^T 不能打开卡时什么也不做：这一帧画出来之后草稿或请求可能已经变了。
    pub(super) fn click_tasks(&mut self, kind: Option<TaskKind>) {
        if !self.tasks_key_works() {
            return;
        }
        match kind.filter(|kind| self.kind_has(*kind)) {
            Some(kind) => {
                self.task_kind = Some(kind);
                self.overlay = Some(Overlay::Tasks(self.new_card(kind)));
            }
            None => self.open_tasks(),
        }
    }

    /// 每帧画之前（`draw::draw`）：卡的种类空了时换到下一个有东西的，都空了或请求打开了时关上（同 Puffin 的卡）；展开着
    /// 输出的命令离开了列表时收起。
    pub(super) fn settle_tasks(&mut self) {
        let Some(card) = self.task_card() else {
            return;
        };
        if self.asking() {
            self.overlay = None;
            return;
        }
        let (kind, peek) = (card.kind, card.peek.as_ref().map(|p| p.id.clone()));
        let next = if self.kind_has(kind) {
            Some(kind)
        } else {
            self.next_kind(kind, true)
        };
        let peeked = peek.filter(|id| self.shell_rows().iter().any(|s| &s.id == id));
        match next {
            None => self.overlay = None,
            Some(kind) => {
                if let Some(card) = self.task_card_mut() {
                    if card.kind != kind {
                        card.kind = kind;
                    }
                    if peeked.is_none() {
                        card.peek = None;
                    }
                }
            }
        }
    }

    /// `from` 之后（`forward`）或之前的、有东西的种类，回绕；没有别的有东西的种类时 None。
    fn next_kind(&self, from: TaskKind, forward: bool) -> Option<TaskKind> {
        let at = KINDS.iter().position(|k| *k == from).unwrap_or(0);
        let n = KINDS.len();
        (1..n)
            .map(|step| {
                if forward {
                    (at + step) % n
                } else {
                    (at + n - step) % n
                }
            })
            .map(|i| KINDS[i])
            .find(|kind| self.kind_has(*kind))
    }

    /// 卡开着时的按键（tui-tasks.md §3.3）：`Some` 到此为止；不是卡的键时关上卡、返回 `None`，键照常处理（字进草稿）。
    pub(super) fn tasks_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let card = self.task_card()?.clone();
        let ctrl = crate::ui::keyboard::ctrl(&key);
        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Char('t') if ctrl => self.overlay = None,
            KeyCode::Left | KeyCode::Right => {
                if let Some(kind) = self.next_kind(card.kind, key.code == KeyCode::Right) {
                    self.task_kind = Some(kind);
                    if let Some(card) = self.task_card_mut() {
                        card.kind = kind;
                        card.peek = None;
                    }
                }
            }
            KeyCode::Up => self.move_task(-1),
            KeyCode::Char('p') if ctrl => self.move_task(-1),
            KeyCode::Down => self.move_task(1),
            KeyCode::Char('n') if ctrl => self.move_task(1),
            KeyCode::Enter => return Some(self.open_task(&card)),
            KeyCode::Char('x') if ctrl => return Some(self.stop_task(&card)),
            _ => {
                self.overlay = None;
                return None;
            }
        }
        Some(None)
    }

    /// ↑ ↓：Subagents、Commands 移动选中的行（到头停住，换行时收起展开的输出）；Todos 滚动窗口。
    pub(super) fn move_task(&mut self, by: isize) {
        let agents = self.agent_tree().len() + 1;
        let commands = self.shell_rows().len();
        let todos = self.view.tasks().len();
        let step = |at: usize, len: usize| at.saturating_add_signed(by).min(len.saturating_sub(1));
        let agent = self
            .task_card()
            .map(|card| self.agent_at(step(self.agent_row(card), agents)));
        let Some(card) = self.task_card_mut() else {
            return;
        };
        match card.kind {
            TaskKind::Subagents => card.agent = agent.flatten(),
            TaskKind::Commands => {
                let next = step(card.command.min(commands.saturating_sub(1)), commands);
                if next != card.command {
                    card.peek = None;
                }
                card.command = next;
            }
            TaskKind::Todos => {
                let last = todos.saturating_sub(super::task_card_rows::WINDOW);
                card.todo_top = card.todo_top.saturating_add_signed(by).min(last);
            }
        }
    }

    /// ⏎：查看选中的子代理（卡留着，选中的仍是它，tui-tasks.md §3.3）；展开或收起选中的命令的输出。
    fn open_task(&mut self, card: &TaskCard) -> Option<Action> {
        match card.kind {
            TaskKind::Subagents => self.view_target(self.selected_agent(card)),
            TaskKind::Commands => {
                let id = self.shell_rows().get(card.command)?.id.clone();
                let open = card.peek.as_ref().is_some_and(|p| p.id == id);
                let card = self.task_card_mut()?;
                if open {
                    card.peek = None;
                    return None;
                }
                card.peek = Some(ShellView::new(id.clone()));
                Some(Action::Panel(Panel::ReadShell(id)))
            }
            TaskKind::Todos => None,
        }
    }

    /// ^X：停止选中的在跑的后台子代理或命令（`Command::StopTask`，与 TaskStop 同一条路径）。
    fn stop_task(&mut self, card: &TaskCard) -> Option<Action> {
        let (id, label) = match card.kind {
            TaskKind::Subagents => {
                let rows = self.agent_tree();
                let entry = rows.get(self.agent_row(card).checked_sub(1)?)?.entry;
                if !super::agents::stoppable(entry) {
                    return None;
                }
                (entry.agent.to_string(), entry.description.clone())
            }
            TaskKind::Commands => {
                let shell = self.shell_rows().get(card.command).copied()?;
                (shell.id.clone(), shell.description.clone())
            }
            TaskKind::Todos => return None,
        };
        Some(Action::Manage(Manage::StopTask { id, label }))
    }

    /// 点击卡里的一行：选中它再按 ⏎。
    pub(super) fn tasks_activate(&mut self, index: usize) -> Option<Action> {
        let agent = self.agent_at(index);
        let card = self.task_card_mut()?;
        match card.kind {
            TaskKind::Subagents => card.agent = agent,
            TaskKind::Commands => card.command = index,
            TaskKind::Todos => return None,
        }
        let card = card.clone();
        self.open_task(&card)
    }
}

#[cfg(test)]
#[path = "task_card_tests.rs"]
mod tests;
