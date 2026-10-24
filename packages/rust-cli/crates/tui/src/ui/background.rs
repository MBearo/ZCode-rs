//! 后台任务（docs/specs/rust-cli/tui-panels.md §3；Host `ui/background.rs`）：footer 的 pill、Background 面板与
//! Shell 详情。shell 来自后台任务快照（`app.background`），
//! 代理就是 §1 子代理列表的行，两者是同一份快照（修复线 F34）；这里只有焦点与 Shell 详情的最后一次读取结果。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};
use rust_cli_kernel_api::panels::{ShellDetail, ShellOutput, SubagentEntry, SubagentStatus};
use unicode_width::UnicodeWidthStr;

use super::agents::{Focus, stoppable};
use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::manage::Manage;
use super::panels::Panel;
use super::text::{truncate, truncate_line, wrap_text};

/// 命令最多占的行数。
const COMMAND_LINES: usize = 3;

/// Background 面板：焦点在全部条目（先 shell 后代理）里的下标，与打开着的 Shell 详情。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackgroundView {
    pub selected: usize,
    pub detail: Option<ShellView>,
}

/// 打开着的 Shell 详情：打开时的命令、读取序号（runner 分配）、最后一次读到的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ShellView {
    pub id: String,
    pub command: String,
    pub seq: u64,
    pub last: Option<ShellDetail>,
    /// 内核已不认识它（结束且通知已 ack、或已停止）：不再读取。
    pub gone: bool,
    /// 所在的远端机器（远端执行 R4，remote-exec.md §21.6）；本机为 None。
    pub machine: Option<String>,
}

/// 面板里的一项。
enum Entry<'a> {
    Shell(&'a BackgroundEntry),
    Agent(&'a SubagentEntry),
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

fn agent_status(agent: &SubagentEntry, lang: Lang) -> &'static str {
    match agent.status {
        SubagentStatus::Running if agent.waiting => pick(lang, "waiting", "等待中"),
        SubagentStatus::Running => pick(lang, "running", "运行中"),
        SubagentStatus::Completed => pick(lang, "done", "已完成"),
        SubagentStatus::Failed => pick(lang, "error", "出错"),
        SubagentStatus::Killed => pick(lang, "stopped", "已停止"),
    }
}

fn shell_status(status: BackgroundStatus, lang: Lang) -> &'static str {
    match status {
        BackgroundStatus::Running => pick(lang, "running", "运行中"),
        BackgroundStatus::Completed => pick(lang, "completed", "已完成"),
        BackgroundStatus::Failed => pick(lang, "failed", "失败"),
        BackgroundStatus::Killed => pick(lang, "stopped", "已停止"),
    }
}

/// `completed` / `failed` / `killed`（通知的 `<status>`）。
fn parse_status(status: &str) -> Option<BackgroundStatus> {
    match status {
        "completed" => Some(BackgroundStatus::Completed),
        "failed" => Some(BackgroundStatus::Failed),
        "killed" => Some(BackgroundStatus::Killed),
        _ => None,
    }
}

/// 输出里的一行去掉 ANSI 转义与控制字符，tab 换成 4 个空格。
fn plain_output(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // CSI：ESC [ … 结束字节（0x40–0x7e）；其他转义只跳过下一个字符。
                if chars.next() == Some('[') {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
            }
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

impl App {
    /// 在跑的后台 shell（快照的顺序）。
    pub fn shell_rows(&self) -> Vec<&BackgroundEntry> {
        self.background
            .iter()
            .filter(|entry| {
                entry.kind == BackgroundKind::Shell && entry.status == BackgroundStatus::Running
            })
            .collect()
    }

    /// 输入框为空时按 ↓：有 pill 时先选中它（tui-panels.md §3.1）。
    pub(super) fn focus_pill(&mut self) -> bool {
        if super::manage::pill(&self.background, self.lang).is_none() {
            return false;
        }
        self.focus = Some(Focus::Pill);
        true
    }

    /// 选中 pill 时的按键：enter 打开面板，↓ 进入子代理列表，↑ / esc 取消，其他键取消后照常处理。
    pub(super) fn pill_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        self.focus = None;
        match key.code {
            KeyCode::Enter => self.open_background(),
            KeyCode::Down => {
                self.focus_agents(now);
            }
            KeyCode::Up | KeyCode::Esc => {}
            _ => return self.dispatch(key, now),
        }
        None
    }

    /// 打开 Background 面板（pill 的 enter、`/tasks`）。
    pub fn open_background(&mut self) {
        self.focus = None;
        self.overlay = Some(Overlay::Background(BackgroundView::default()));
    }

    fn background_view(&self) -> Option<&BackgroundView> {
        match &self.overlay {
            Some(Overlay::Background(view)) => Some(view),
            _ => None,
        }
    }

    fn background_view_mut(&mut self) -> Option<&mut BackgroundView> {
        match self.overlay.as_mut() {
            Some(Overlay::Background(view)) => Some(view),
            _ => None,
        }
    }

    /// 焦点下标限制在条目范围内（条目随快照变化，面板不另存它们）。
    fn background_selected(&self, len: usize) -> usize {
        self.background_view()
            .map_or(0, |view| view.selected.min(len.saturating_sub(1)))
    }

    /// runner 给这次读取分配的序号（打开详情或重开时）。
    pub fn shell_reading(&mut self, id: &str, seq: u64) {
        if let Some(detail) = self.background_view_mut().and_then(|v| v.detail.as_mut())
            && detail.id == id
        {
            detail.seq = seq;
        }
    }

    /// 读取的回复：序号与打开着的详情相同时保存；返回是否要再读一次（还在跑）。
    pub fn on_shell_detail(&mut self, id: &str, seq: u64, detail: Option<ShellDetail>) -> bool {
        let Some(open) = self.background_view_mut().and_then(|v| v.detail.as_mut()) else {
            return false;
        };
        if open.id != id || open.seq != seq {
            return false;
        }
        match detail {
            Some(detail) => {
                let running = detail.status == BackgroundStatus::Running;
                open.last = Some(detail);
                running
            }
            None => {
                open.gone = true;
                false
            }
        }
    }

    /// 详情的状态：最后一次读到的；内核已不认识它时取通知的终态（`BackgroundWork`），没有时是 stopped。
    fn shell_detail_status(&self, open: &ShellView) -> BackgroundStatus {
        match &open.last {
            Some(last) if !open.gone => last.status,
            last => self
                .view
                .ended_work(&open.id)
                .and_then(parse_status)
                .or_else(|| {
                    last.as_ref()
                        .map(|l| l.status)
                        .filter(|s| *s != BackgroundStatus::Running)
                })
                .unwrap_or(BackgroundStatus::Killed),
        }
    }

    pub(super) fn background_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        if let Some(open) = self.background_view().and_then(|v| v.detail.clone()) {
            return self.shell_detail_key(&open, key);
        }
        let shells = self.shell_rows();
        let agents = self.agent_rows(now);
        let len = shells.len() + agents.len();
        let selected = self.background_selected(len);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let entry = if selected < shells.len() {
            shells.get(selected).map(|s| Entry::Shell(s))
        } else {
            agents.get(selected - shells.len()).map(|a| Entry::Agent(a))
        };
        match key.code {
            KeyCode::Esc | KeyCode::Left => self.overlay = None,
            KeyCode::Char('c') if ctrl => self.overlay = None,
            KeyCode::Up => {
                if let Some(view) = self.background_view_mut() {
                    view.selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Some(view) = self.background_view_mut() {
                    view.selected = (selected + 1).min(len.saturating_sub(1));
                }
            }
            KeyCode::Char('x') if !ctrl => {
                let (id, label) = match entry? {
                    Entry::Shell(shell) => (shell.id.clone(), shell.description.clone()),
                    Entry::Agent(agent) if stoppable(agent) => {
                        (agent.agent.to_string(), agent.description.clone())
                    }
                    Entry::Agent(_) => return None,
                };
                return Some(Action::Manage(Manage::StopTask { id, label }));
            }
            KeyCode::Enter => match entry? {
                Entry::Shell(shell) => {
                    let (id, command) = (shell.id.clone(), shell.description.clone());
                    let machine = shell.machine.clone();
                    if let Some(view) = self.background_view_mut() {
                        view.detail = Some(ShellView {
                            id: id.clone(),
                            command,
                            seq: 0,
                            last: None,
                            gone: false,
                            machine,
                        });
                    }
                    return Some(Action::Panel(Panel::ReadShell(id)));
                }
                // 面板是平铺的（tui-panels.md §3.2），按条目查看，不按 §1 的树的下标。
                Entry::Agent(agent) => {
                    let agent = agent.agent.clone();
                    self.overlay = None;
                    return self.view_target(Some(agent));
                }
            },
            _ => {}
        }
        None
    }

    /// 详情的按键：← 回到列表，esc / enter / space 关闭面板，x 停止还在跑的 shell。
    fn shell_detail_key(&mut self, open: &ShellView, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Left => {
                if let Some(view) = self.background_view_mut() {
                    view.detail = None;
                }
            }
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ') => self.overlay = None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.overlay = None;
            }
            KeyCode::Char('x') if self.shell_detail_status(open) == BackgroundStatus::Running => {
                return Some(Action::Manage(Manage::StopTask {
                    id: open.id.clone(),
                    label: open.command.clone(),
                }));
            }
            _ => {}
        }
        None
    }

    /// 点击面板的第 `index` 项：选中再按 enter。
    pub(super) fn background_activate(&mut self, index: usize, now: Instant) -> Option<Action> {
        if let Some(view) = self.background_view_mut() {
            view.selected = index;
        }
        self.background_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now)
    }

    /// 滚轮：焦点移动，到头停住。
    pub(super) fn background_wheel(&mut self, rows: isize, now: Instant) {
        let len = self.shell_rows().len() + self.agent_rows(now).len();
        let current = self.background_selected(len) as isize;
        let last = len.saturating_sub(1) as isize;
        if let Some(view) = self.background_view_mut() {
            view.selected = (current + rows).clamp(0, last) as usize;
        }
    }

    /// 面板的行与每个条目所在的行（替换底部区域）。
    pub fn background_lines(
        &self,
        width: usize,
        height: usize,
        now: Instant,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        if let Some(open) = self.background_view().and_then(|v| v.detail.as_ref()) {
            return (self.shell_detail_lines(open, width), Vec::new());
        }
        let lang = self.lang;
        let accent = self.palette.accent;
        let shells = self.shell_rows();
        let agents = self.agent_rows(now);
        let len = shells.len() + agents.len();
        let selected = self.background_selected(len);
        let active_agents = agents
            .iter()
            .filter(|a| a.status == SubagentStatus::Running)
            .count();
        let mut lines = vec![
            Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
            Line::from(Span::styled(
                format!("  {}", pick(lang, "Background", "后台任务")),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )),
        ];
        let mut parts = Vec::new();
        if !shells.is_empty() {
            parts.push(match lang {
                Lang::En if shells.len() == 1 => "1 active shell".to_owned(),
                Lang::En => format!("{} active shells", shells.len()),
                Lang::Zh => format!("{} 个 shell 运行中", shells.len()),
            });
        }
        if active_agents > 0 {
            parts.push(match lang {
                Lang::En if active_agents == 1 => "1 active agent".to_owned(),
                Lang::En => format!("{active_agents} active agents"),
                Lang::Zh => format!("{active_agents} 个代理运行中"),
            });
        }
        let subtitle = if parts.is_empty() {
            pick(lang, "No tasks currently running", "当前没有后台任务").to_owned()
        } else {
            parts.join(" · ")
        };
        lines.push(truncate_line(
            Line::from(Span::styled(format!("  {subtitle}"), dim())),
            width,
        ));
        let entries: Vec<Entry> = shells
            .iter()
            .map(|s| Entry::Shell(s))
            .chain(agents.iter().map(|a| Entry::Agent(a)))
            .collect();
        let label_of = |entry: &Entry| match entry {
            Entry::Shell(shell) => shell
                .description
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
            Entry::Agent(agent) if agent.description.is_empty() => agent.agent_type.clone(),
            Entry::Agent(agent) => agent.description.clone(),
        };
        let status_of = |entry: &Entry| match entry {
            Entry::Shell(_) => shell_status(BackgroundStatus::Running, lang),
            Entry::Agent(agent) => agent_status(agent, lang),
        };
        // 远端的后台命令与缺省在远端的子代理：名称后面接 ` @机器`（远端执行 R4，remote-exec.md §21.6）。
        let machine_of = |entry: &Entry| match entry {
            Entry::Shell(shell) => shell.machine.clone(),
            Entry::Agent(agent) => agent.machine.clone(),
        };
        let tag_width = |entry: &Entry| machine_of(entry).map_or(0, |machine| 2 + machine.width());
        let status_width = entries
            .iter()
            .map(|e| status_of(e).width())
            .max()
            .unwrap_or(0);
        // 指针 2 列、圆点 2 列、名称与状态之间至少 3 列。
        let label_width = entries
            .iter()
            .map(|e| label_of(e).width() + tag_width(e))
            .max()
            .unwrap_or(0)
            .min(width.saturating_sub(4 + 3 + status_width));
        let headers = !shells.is_empty() && !agents.is_empty();
        let mut body: Vec<(Line<'static>, Option<usize>)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let first_agent = index == shells.len();
            if index == 0 || first_agent {
                body.push((Line::default(), None));
                if headers {
                    let (name, n) = if first_agent {
                        (pick(lang, "Local agents", "本地代理"), agents.len())
                    } else {
                        (pick(lang, "Shells", "Shell"), shells.len())
                    };
                    body.push((
                        Line::from(vec![
                            Span::styled(
                                format!("  {name}"),
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(format!(" ({n})"), dim()),
                        ]),
                        None,
                    ));
                }
            }
            let focused = index == selected;
            let tag = machine_of(entry);
            let label = truncate(
                &label_of(entry),
                label_width.saturating_sub(tag_width(entry)),
            );
            let pad = label_width.saturating_sub(label.width() + tag_width(entry)) + 3;
            let mut named = vec![Span::raw(label)];
            if let Some(machine) = &tag {
                named.push(Span::raw(" "));
                named.push(super::machines::tag(&self.palette, machine));
            }
            // 选择列表组件（`› ` 加底色条，tui.md §7.4，修复线 F26；原来是强调色的 `❯ `）。
            body.push((
                super::pick::paint(
                    truncate_line(
                        Line::from(
                            [
                                super::pick::pointer(focused),
                                Span::styled(format!("{} ", super::cells::BULLET), dim()),
                            ]
                            .into_iter()
                            .chain(named)
                            .chain([
                                Span::raw(" ".repeat(pad)),
                                Span::styled(status_of(entry), self.palette.secondary()),
                            ])
                            .collect::<Vec<_>>(),
                        ),
                        width,
                    ),
                    focused,
                    &self.palette,
                    width,
                ),
                Some(index),
            ));
        }
        // 条目放不下时只显示焦点附近的（上面固定 3 行，下面空行与提示 2 行）。
        let room = height.saturating_sub(lines.len() + 2).max(3);
        let focus_line = body
            .iter()
            .position(|(_, index)| *index == Some(selected))
            .unwrap_or(0);
        let start = focus_line
            .saturating_sub(room.saturating_sub(1))
            .min(body.len().saturating_sub(room));
        let mut rows = Vec::new();
        for (line, index) in body.into_iter().skip(start).take(room) {
            if let Some(index) = index {
                rows.push((lines.len(), index));
            }
            lines.push(truncate_line(line, width));
        }
        lines.push(Line::default());
        let focused = entries.get(selected);
        let stop = match focused {
            Some(Entry::Shell(_)) => true,
            Some(Entry::Agent(agent)) => stoppable(agent),
            None => false,
        };
        let parts: [(bool, &str, &str); 4] = [
            (len > 0, "{↑/↓} to select", "{↑/↓} 选择"),
            (focused.is_some(), "{Enter} to view", "{enter} 查看"),
            (stop, "{x} to stop", "{x} 停止"),
            (true, "{Esc} to close", "{esc} 关闭"),
        ];
        let hint: Vec<&str> = parts
            .iter()
            .filter(|(show, _, _)| *show)
            .map(|(_, en, zh)| pick(lang, en, zh))
            .collect();
        lines.push(truncate_line(
            super::key_hint::line(2, &hint.join(" · "), &self.palette),
            width,
        ));
        (lines, rows)
    }

    /// Shell 详情的行：状态、时长、命令、输出框（最后 10 行）与行数、提示。
    fn shell_detail_lines(&self, open: &ShellView, width: usize) -> Vec<Line<'static>> {
        let lang = self.lang;
        let accent = self.palette.accent;
        let status = self.shell_detail_status(open);
        let field = |name: &str, value: String| {
            truncate_line(
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        name.to_owned(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(value),
                ]),
                width,
            )
        };
        let mut lines = vec![
            Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
            Line::from(Span::styled(
                format!("  {}", pick(lang, "Shell details", "Shell 详情")),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )),
            Line::default(),
        ];
        // 远端的任务写明机器（远端执行 R4，remote-exec.md §21.6）。
        if let Some(machine) = &open.machine {
            lines.push(field(
                super::i18n_machines::machine_field(lang),
                machine.clone(),
            ));
        }
        lines.push(field(
            pick(lang, "Status: ", "状态："),
            shell_status(status, lang).to_owned(),
        ));
        if let Some(elapsed) = open.last.as_ref().and_then(|last| last.elapsed_ms) {
            lines.push(field(
                pick(lang, "Runtime: ", "运行时长："),
                super::cells::duration(elapsed),
            ));
        }
        let label = pick(lang, "Command: ", "命令：");
        let indent = " ".repeat(2 + label.width());
        lines.extend(
            wrap_text(
                &plain_output(&open.command),
                Style::default(),
                width,
                &[
                    Span::raw("  "),
                    Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
                ],
                &[Span::raw(indent)],
            )
            .into_iter()
            .take(COMMAND_LINES),
        );
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("  {}", pick(lang, "Output:", "输出：")),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        let (shown, note): (Vec<String>, Option<String>) =
            match open.last.as_ref().map(|l| &l.output) {
                None => (vec![self.strings.loading.to_owned()], None),
                Some(ShellOutput::Tail { lines, .. }) if lines.is_empty() => (
                    vec![pick(lang, "No output yet", "还没有输出").to_owned()],
                    None,
                ),
                Some(ShellOutput::Tail { lines, total }) => {
                    let n = lines.len();
                    let note = match lang {
                        Lang::En if *total as usize <= n => {
                            format!("Showing {n} line{}", if n == 1 { "" } else { "s" })
                        }
                        Lang::En => format!("Showing {n} of {total} lines"),
                        Lang::Zh if *total as usize <= n => format!("显示 {n} 行"),
                        Lang::Zh => format!("显示 {total} 行中的 {n} 行"),
                    };
                    (lines.iter().map(|l| plain_output(l)).collect(), Some(note))
                }
                Some(ShellOutput::TooLarge { .. }) => (
                    vec![pick(lang, "Output too large to preview", "输出太大，不预览").to_owned()],
                    None,
                ),
                Some(ShellOutput::Unavailable) => (
                    vec![pick(lang, "Output unavailable", "读不到输出").to_owned()],
                    None,
                ),
            };
        // 输出框：从第 2 列到倒数第 2 列。
        let box_width = width.saturating_sub(4).max(6);
        let inner = box_width - 4;
        let rule = "─".repeat(box_width - 2);
        lines.push(Line::from(Span::styled(format!("  ╭{rule}╮"), dim())));
        for text in shown {
            let text = truncate(&text, inner);
            let pad = inner.saturating_sub(text.width());
            lines.push(Line::from(vec![
                Span::styled("  │ ", dim()),
                Span::raw(text),
                Span::raw(" ".repeat(pad)),
                Span::styled(" │", dim()),
            ]));
        }
        lines.push(Line::from(Span::styled(format!("  ╰{rule}╯"), dim())));
        if let Some(note) = note {
            lines.push(Line::from(Span::styled(format!("  {note}"), dim())));
        }
        lines.push(Line::default());
        let hint = match (lang, status == BackgroundStatus::Running) {
            (Lang::En, true) => "{←} to go back · {x} to stop · {Esc} to close",
            (Lang::En, false) => "{←} to go back · {Esc} to close",
            (Lang::Zh, true) => "{←} 返回 · {x} 停止 · {esc} 关闭",
            (Lang::Zh, false) => "{←} 返回 · {esc} 关闭",
        };
        lines.push(super::key_hint::line(2, hint, &self.palette));
        lines
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
