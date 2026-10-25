//! 远端机器的界面（docs/specs/rust-cli/remote-exec.md §21.3、§21.5、§21.6）：`/machines` 的解析与列表、footer 下面的
//! 机器状态行、`@机器` 标记。数据是入口的机器服务给的快照（`App::machines`，runner 在变化时重取）；这里只有焦点。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::app::{Action, App, Overlay};
use super::i18n_machines as words;
use super::palette::Palette;
use super::text::{truncate_line, wrap_text};
use crate::machines::{MachineRow, MachineStatus};

/// 交给 runner 经机器服务做的事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRequest {
    /// 写进清单，成功后马上连（§21.3）。
    Add {
        name: String,
        ssh: String,
        working_directory: Option<String>,
    },
    Connect(String),
    Disconnect(String),
    Remove(String),
    /// 立即重试（重连中的机器，F98，remote-exec.md §25.4.1）。
    Retry(String),
}

/// `/machines` 列表的界面状态：焦点按名字保持（快照变了也跟着那台机器），找不到时按原下标。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachinesView {
    pub selected: usize,
    pub name: Option<String>,
}

/// 工具输入里的远端机器（`machine` 不是 `local` 时）。
pub fn machine_of(input: &Value) -> Option<&str> {
    input.get("machine").and_then(Value::as_str).filter(|name| {
        !name.is_empty() && *name != rust_cli_kernel_api::env_registry::LOCAL_MACHINE
    })
}

/// 一次工具调用在哪台远端机器上：输入里的 `machine`（不是 `local`）；MCP 工具看它的服务器绑定的机器（R5，§22.8）。
/// `mcp` 是（规范化的服务器名，机器名）；工具名 `mcp__<服务器>__<工具>` 按最长的服务器名前缀找。
pub fn remote_of<'a>(name: &str, input: &'a Value, mcp: &'a [(String, String)]) -> Option<&'a str> {
    machine_of(input).or_else(|| {
        let rest = name.strip_prefix("mcp__")?;
        mcp.iter()
            .filter(|(server, _)| {
                rest.strip_prefix(server.as_str())
                    .is_some_and(|tail| tail.starts_with("__"))
            })
            .max_by_key(|(server, _)| server.len())
            .map(|(_, machine)| machine.as_str())
    })
}

/// 这台机器在重连中（快照里的状态）：远端的调用在等它（F98，§25.4.1）。
pub fn reconnecting(machines: &[MachineRow], name: &str) -> bool {
    machines
        .iter()
        .any(|row| row.name == name && matches!(row.status, MachineStatus::Reconnecting { .. }))
}

/// `@机器名`（强调色，§21.6）。
pub fn tag(palette: &Palette, name: &str) -> Span<'static> {
    Span::styled(format!("@{name}"), Style::default().fg(palette.accent))
}

/// 状态的圆点与颜色（§21.5 的表）。
fn dot(status: &MachineStatus, palette: &Palette) -> Span<'static> {
    match status {
        MachineStatus::Ready => Span::styled("●", palette.success()),
        MachineStatus::Connecting(_) | MachineStatus::Reconnecting { .. } => {
            Span::styled("○", palette.warning())
        }
        MachineStatus::Failed(_) => Span::styled("○", palette.destructive()),
        MachineStatus::Disconnected => Span::styled("○", palette.secondary()),
    }
}

impl App {
    /// `/machines [子命令]`（§21.3）：解析后交给 runner；没有机器服务时提示不可用。
    pub(super) fn machines_command(&mut self, args: &str) -> Option<Action> {
        if !self.machines_enabled {
            self.info(words::unavailable(self.lang));
            return None;
        }
        let args = args.trim();
        if args.is_empty() {
            self.overlay = Some(Overlay::Machines(MachinesView::default()));
            return None;
        }
        let (action, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let rest = rest.trim();
        let one =
            || (!rest.is_empty() && !rest.contains(char::is_whitespace)).then(|| rest.to_owned());
        let request = match action {
            "add" => {
                let (name, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let rest = rest.trim();
                let (ssh, directory) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let directory = directory.trim();
                (!name.is_empty() && !ssh.is_empty()).then(|| MachineRequest::Add {
                    name: name.to_owned(),
                    ssh: ssh.to_owned(),
                    working_directory: (!directory.is_empty()).then(|| directory.to_owned()),
                })
            }
            // 对重连中的机器是立即重试（F98，§25.4.1；以前回“已经连着”）。
            "connect" => one().map(|name| {
                if reconnecting(&self.machines, &name) {
                    MachineRequest::Retry(name)
                } else {
                    MachineRequest::Connect(name)
                }
            }),
            "disconnect" => one().map(MachineRequest::Disconnect),
            "remove" => one().map(MachineRequest::Remove),
            _ => None,
        };
        match request {
            Some(request) => Some(Action::Machines(request)),
            None => {
                self.info(words::usage(self.lang));
                None
            }
        }
    }

    fn machines_view_mut(&mut self) -> Option<&mut MachinesView> {
        match self.overlay.as_mut() {
            Some(Overlay::Machines(view)) => Some(view),
            _ => None,
        }
    }

    /// 焦点所在的下标（按名字找，找不到时按原下标，限制在范围内）。
    fn machine_focus(&self) -> usize {
        let Some(Overlay::Machines(view)) = &self.overlay else {
            return 0;
        };
        view.name
            .as_ref()
            .and_then(|name| self.machines.iter().position(|row| &row.name == name))
            .unwrap_or_else(|| view.selected.min(self.machines.len().saturating_sub(1)))
    }

    fn focus_machine(&mut self, index: usize) {
        let name = self.machines.get(index).map(|row| row.name.clone());
        if let Some(view) = self.machines_view_mut() {
            view.selected = index;
            view.name = name;
        }
    }

    /// 列表的按键（§21.3）：↑ / ↓ 循环移动，enter 连接或断开，`c` / `d` / `x`，esc / ctrl+c 关闭。
    pub(super) fn machines_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = self.typing.ctrl(&key);
        let count = self.machines.len();
        let focus = self.machine_focus();
        let focused = self.machines.get(focus).cloned();
        let step = |down: bool| match count {
            0 => 0,
            _ if down => (focus + 1) % count,
            _ => (focus + count - 1) % count,
        };
        let request = match key.code {
            KeyCode::Esc => {
                self.overlay = None;
                return None;
            }
            KeyCode::Char('c') if ctrl => {
                self.overlay = None;
                return None;
            }
            KeyCode::Up => {
                self.focus_machine(step(false));
                return None;
            }
            KeyCode::Char('p') if ctrl => {
                self.focus_machine(step(false));
                return None;
            }
            KeyCode::Down => {
                self.focus_machine(step(true));
                return None;
            }
            KeyCode::Char('n') if ctrl => {
                self.focus_machine(step(true));
                return None;
            }
            KeyCode::Enter => focused.map(|row| {
                if row.status.is_live() {
                    MachineRequest::Disconnect(row.name)
                } else {
                    MachineRequest::Connect(row.name)
                }
            }),
            KeyCode::Char('c') => focused.map(|row| MachineRequest::Connect(row.name)),
            KeyCode::Char('d') => focused.map(|row| MachineRequest::Disconnect(row.name)),
            KeyCode::Char('x') => focused.map(|row| MachineRequest::Remove(row.name)),
            // 立即重试（F98）：只对重连中的机器。
            KeyCode::Char('r') => focused
                .filter(|row| matches!(row.status, MachineStatus::Reconnecting { .. }))
                .map(|row| MachineRequest::Retry(row.name)),
            _ => None,
        };
        // 先记下焦点的名字：删掉之后按原下标落到相邻的一台。
        self.focus_machine(focus);
        request.map(Action::Machines)
    }

    /// 状态词与重连的进度（F96：重连中的机器接倒计时与第几次）；就绪时写延迟（列表里另写 `connected`）。
    fn status_note(
        &self,
        status: &MachineStatus,
        latency_ms: Option<u64>,
        now_ms: u64,
    ) -> Option<String> {
        match (status, latency_ms) {
            (MachineStatus::Ready, Some(ms)) => Some(format!("{ms}ms")),
            // 第一次连接的哪一步（F98，§25.4.2）。
            (MachineStatus::Connecting(step), _) => Some(format!(
                "{}{}",
                words::short_word(self.lang, status).unwrap_or_default(),
                words::connect_step(self.lang, *step).unwrap_or_default()
            )),
            (
                MachineStatus::Reconnecting {
                    attempt,
                    retry_at_ms,
                    ..
                },
                _,
            ) => {
                let word = words::short_word(self.lang, status).unwrap_or_default();
                let left = retry_at_ms.map(|at| at.saturating_sub(now_ms));
                Some(format!(
                    "{word}{}",
                    words::reconnect_progress(self.lang, *attempt, left)
                ))
            }
            (status, _) => words::short_word(self.lang, status).map(str::to_owned),
        }
    }

    /// 有机器在等下一次重连（状态行写倒计时）：没有动画时每秒重画一次（只是显示，F96）。
    pub fn machines_counting(&self) -> bool {
        self.machines.iter().any(|row| {
            matches!(
                row.status,
                MachineStatus::Reconnecting {
                    retry_at_ms: Some(_),
                    ..
                }
            )
        })
    }

    /// 列表的行（替换底部区域，照 Background 面板的样式，§21.3）。
    pub fn machines_lines(&self, width: usize, height: usize, now_ms: u64) -> Vec<Line<'static>> {
        let lang = self.lang;
        let palette = &self.palette;
        let accent = Style::default().fg(palette.accent);
        let connected = self
            .machines
            .iter()
            .filter(|row| row.status == MachineStatus::Ready)
            .count();
        let mut lines = vec![
            Line::from(Span::styled("─".repeat(width), accent)),
            Line::from(Span::styled(
                format!("  {}", words::title(lang)),
                accent.add_modifier(Modifier::BOLD),
            )),
            truncate_line(
                Line::from(Span::styled(
                    format!("  {}", words::summary(lang, self.machines.len(), connected)),
                    palette.secondary(),
                )),
                width,
            ),
            Line::default(),
        ];
        let focus = self.machine_focus();
        if self.machines.is_empty() {
            lines.push(truncate_line(
                Line::from(Span::styled(
                    format!("  {}", words::empty_hint(lang)),
                    palette.secondary(),
                )),
                width,
            ));
        }
        let name_width = self
            .machines
            .iter()
            .map(|row| row.name.width())
            .max()
            .unwrap_or(0);
        let mut body: Vec<(Line<'static>, bool)> = Vec::new();
        for (index, row) in self.machines.iter().enumerate() {
            let focused = index == focus;
            let status = match (&row.status, row.latency_ms) {
                (MachineStatus::Ready, Some(ms)) => {
                    format!("{} · {ms}ms", words::status_word(lang, &row.status))
                }
                (MachineStatus::Reconnecting { .. } | MachineStatus::Connecting(_), _) => self
                    .status_note(&row.status, None, now_ms)
                    .unwrap_or_default(),
                (status, _) => words::status_word(lang, status).to_owned(),
            };
            let head = Line::from(vec![
                super::pick::pointer(focused),
                dot(&row.status, palette),
                Span::raw(" "),
                Span::raw(row.name.clone()),
                Span::raw(" ".repeat(name_width - row.name.width() + 2)),
                Span::styled(status, palette.secondary()),
            ]);
            body.push((
                super::pick::paint(truncate_line(head, width), focused, palette, width),
                focused,
            ));
            body.push((
                truncate_line(
                    Line::from(Span::styled(
                        format!("    {}", self.machine_facts(row)),
                        palette.secondary(),
                    )),
                    width,
                ),
                focused,
            ));
            let note = match &row.status {
                MachineStatus::Failed(failure) => Some((
                    words::failure_text(lang, &row.name, failure),
                    palette.destructive(),
                )),
                // 重连中（F96）：最近一次的原因。
                MachineStatus::Reconnecting { reason, .. } if !reason.is_empty() => {
                    Some((words::last_error(lang, reason), palette.secondary()))
                }
                _ => None,
            };
            if let Some((text, style)) = note {
                for line in wrap_text(
                    &text,
                    style,
                    width,
                    &[Span::raw("    ")],
                    &[Span::raw("    ")],
                ) {
                    body.push((line, focused));
                }
            }
        }
        // 放不下时只留焦点附近的（上面固定 4 行，下面空行与提示 2 行）。
        let room = height.saturating_sub(lines.len() + 2).max(2);
        let first = body.iter().position(|(_, focused)| *focused).unwrap_or(0);
        let start = first
            .saturating_sub(room.saturating_sub(1))
            .min(body.len().saturating_sub(room));
        lines.extend(
            body.into_iter()
                .skip(start)
                .take(room)
                .map(|(line, _)| line),
        );
        lines.push(Line::default());
        let live = self.machines.get(focus).map(|row| row.status.is_live());
        let retry = self
            .machines
            .get(focus)
            .is_some_and(|row| row.status.awaits_retry());
        lines.push(truncate_line(
            super::key_hint::line(2, &words::hint(lang, live, retry), palette),
            width,
        ));
        lines
    }

    /// 远端机器的工作目录：清单里写的，没写时是握手报告的 home；都不知道时 None（F98，§25.4.3）。
    pub fn remote_root(&self, name: &str) -> Option<String> {
        let row = self.machines.iter().find(|row| row.name == name)?;
        row.working_directory
            .clone()
            .or_else(|| row.executor.as_ref().and_then(|e| e.home.clone()))
    }

    /// 第二行：ssh 目标、系统与架构、执行器版本、工作目录（工作目录可能很长，放在最后，放不下时截它）。
    fn machine_facts(&self, row: &MachineRow) -> String {
        let lang = self.lang;
        let mut parts = vec![format!("ssh {}", row.ssh)];
        if let Some(executor) = &row.executor {
            parts.push(format!(
                "{} {}",
                words::os_name(&executor.os),
                executor.arch
            ));
            parts.push(words::executor(lang, &executor.version));
            if !executor.refreshes_env {
                parts.push(words::env_not_refreshed(lang).to_owned());
            }
        }
        parts.push(
            row.working_directory
                .clone()
                .unwrap_or_else(|| words::remote_home(lang).to_owned()),
        );
        parts.join(" · ")
    }

    /// footer 下面的机器状态行（§21.5）：清单里有机器时才有。
    pub fn machine_status_line(&self, width: usize, now_ms: u64) -> Option<Line<'static>> {
        if self.machines.is_empty() {
            return None;
        }
        let palette = &self.palette;
        let mut spans = vec![
            Span::raw("  "),
            Span::styled("●", palette.success()),
            Span::raw(" local"),
        ];
        for row in &self.machines {
            spans.push(Span::raw("  "));
            spans.push(dot(&row.status, palette));
            spans.push(Span::raw(format!(" {}", row.name)));
            if let Some(note) = self.status_note(&row.status, row.latency_ms, now_ms) {
                spans.push(Span::styled(format!(" {note}"), palette.secondary()));
            }
        }
        Some(truncate_line(Line::from(spans), width))
    }
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
