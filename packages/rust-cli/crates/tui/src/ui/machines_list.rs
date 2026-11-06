//! `/machines` 的列表（tui-kit.md §4.2「`/machines`」；照 Puffin `shell/kept/machines.rs` 的 `machine_entries`、
//! `machines_key`、`machines_slot` 与 `kept/machine_view.rs`，remote-exec.md §21.3）：项是机器（没有机器时是 `~/.ssh/config`
//! 的主机），最后总是 `+ Add a machine`；⏎ 打开设置里的那一页，空格做随状态的操作，esc 关上。行是 kit 的
//! `machine_rows::rows`，与 Puffin 同一份代码；英文的字照 Puffin（`settings/machine_words.rs`），中文是 `i18n_machines` 的译文（kit `rows_said`，tui-kit.md §4.1 第 4 条）。
//!
//! 以前列表是旧界面自己的行（`2 machines · 1 connected`、没有 `+ Add a machine`）、⏎ 随状态连或断、槽是一句长的说明，
//! 草稿行没有占位字。旧界面的 `c` / `d` / `x` / `r` 键留着（不写在槽里；删除与重试另见设置的机器页）。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::machine_rows::{self, Chosen, Host, Machine, Phase};
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::i18n_machines as zh;
use super::machines::{MachineRequest, MachinesView};
use super::settings::machine_words as words;
use super::settings::machines::{MachineOp, enter_op};
use super::settings::page::Page;
use crate::machines::{ConnectStep, MachineRow, MachineStatus, SshHost};

/// 列表上的一项（Puffin `Entry`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Machine(String),
    Host(String),
    Add,
}

/// 重连还要等多久（毫秒）；正在连时没有。
fn left_ms(status: &MachineStatus, now_ms: u64) -> Option<u64> {
    match status {
        MachineStatus::Reconnecting {
            retry_at_ms: Some(at),
            ..
        } => Some(at.saturating_sub(now_ms)),
        _ => None,
    }
}

/// 这次连接的分步：探测、这一步，没到握手时握手在最后（Puffin `steps`，没有界面看见的中间步）。
fn steps(now: ConnectStep) -> Vec<(ConnectStep, Phase)> {
    let mut out = Vec::new();
    if now != ConnectStep::Checking {
        out.push((ConnectStep::Checking, Phase::Done));
    }
    out.push((now, Phase::Now));
    if now != ConnectStep::Handshaking {
        out.push((ConnectStep::Handshaking, Phase::Later));
    }
    out
}

/// 机器源的一行在 kit 里的样子（Puffin `machine_view::machine`）：`since` 是这次连接开始的毫秒。英文的字是 Puffin 的
/// （`settings/machine_words.rs`），中文是 `i18n_machines` 的译文。
pub fn machine(m: &MachineRow, since: Option<u64>, now_ms: u64, lang: Lang) -> Machine {
    let row = match &m.status {
        MachineStatus::Ready => machine_rows::MachineRow::Ready {
            latency_ms: m.latency_ms,
        },
        MachineStatus::Disconnected => machine_rows::MachineRow::Disconnected,
        MachineStatus::Connecting(step) => machine_rows::MachineRow::Connecting {
            step: zh::step_word(lang, *step).to_owned(),
            size: zh::step_size(lang, *step),
            elapsed: words::elapsed(now_ms.saturating_sub(since.unwrap_or(now_ms))),
            steps: steps(*step)
                .into_iter()
                .map(|(s, phase)| (zh::step_done(lang, s, phase == Phase::Done), phase))
                .collect(),
        },
        MachineStatus::Reconnecting {
            attempt, reason, ..
        } => {
            let left = left_ms(&m.status, now_ms);
            machine_rows::MachineRow::Reconnecting {
                countdown: left.map(super::retry_status::countdown),
                next_try: zh::next_try(lang, left),
                attempt: zh::attempt(lang, *attempt),
                last_error: (!reason.is_empty()).then(|| match lang {
                    Lang::En => words::last_error(reason),
                    Lang::Zh => zh::last_error(lang, reason),
                }),
            }
        }
        MachineStatus::Failed(failure) => machine_rows::MachineRow::Failed {
            cause: zh::cause(lang, &m.name, failure),
            advice: zh::advice(lang, failure, &m.ssh),
        },
    };
    Machine {
        name: m.name.clone(),
        ssh: m.ssh.clone(),
        os: m
            .executor
            .as_ref()
            .map(|e| format!("{} {}", words::os_name(&e.os), e.arch))
            .unwrap_or_default(),
        dir: m
            .working_directory
            .clone()
            .unwrap_or_else(|| zh::remote_home(lang).to_owned()),
        executor: m.executor.as_ref().map(|e| {
            let word = match lang {
                Lang::En => words::executor(&e.version),
                Lang::Zh => zh::executor(lang, &e.version),
            };
            (word, e.refreshes_env)
        }),
        row,
    }
}

/// 槽的词（Puffin `machines_slot` 的英文；中文是旧界面的译文）。
fn slot_word(lang: Lang, en: &str) -> String {
    if lang == Lang::En {
        return en.to_owned();
    }
    match en {
        "details" => "详情",
        "change it first" => "先改一改",
        "add and connect" => "添加并连接",
        "add" => "添加",
        "close" => "关闭",
        "disconnect" => "断开",
        "retry now" => "立即重试",
        "stop" => "停下",
        "try again" => "再试一次",
        "connect" => "连接",
        other => other,
    }
    .to_owned()
}

impl App {
    /// 机器源的每一台在 kit 里的样子。
    pub fn kit_machines(&self, now_ms: u64) -> Vec<Machine> {
        self.machines
            .iter()
            .map(|m| {
                machine(
                    m,
                    self.machines_since.get(&m.name).copied(),
                    now_ms,
                    self.lang,
                )
            })
            .collect()
    }

    fn machines_view(&self) -> Option<&MachinesView> {
        match &self.overlay {
            Some(Overlay::Machines(view)) => Some(view),
            _ => None,
        }
    }

    /// 列表上的项：有机器时是机器，没有时是 `~/.ssh/config` 的主机；最后总是 `+ Add a machine`。
    pub fn machine_entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = if self.machines.is_empty() {
            self.machines_view()
                .map_or(&[][..], |v| &v.hosts[..])
                .iter()
                .map(|h| Entry::Host(h.alias.clone()))
                .collect()
        } else {
            self.machines
                .iter()
                .map(|r| Entry::Machine(r.name.clone()))
                .collect()
        };
        out.push(Entry::Add);
        out
    }

    /// 焦点所在的下标（按项找，找不到时按原下标，限制在范围内）。
    pub(super) fn machine_focus(&self) -> usize {
        let entries = self.machine_entries();
        let Some(view) = self.machines_view() else {
            return 0;
        };
        view.focus
            .as_ref()
            .and_then(|f| entries.iter().position(|e| e == f))
            .unwrap_or_else(|| view.selected.min(entries.len().saturating_sub(1)))
    }

    pub(super) fn focus_machine(&mut self, index: usize) {
        let entry = self.machine_entries().get(index).cloned();
        if let Some(Overlay::Machines(view)) = self.overlay.as_mut() {
            view.selected = index;
            view.focus = entry;
        }
    }

    fn focused_entry(&self) -> Option<Entry> {
        self.machine_entries().get(self.machine_focus()).cloned()
    }

    /// 读到的 `~/.ssh/config` 主机（列表开着时，没有机器时列出）。
    pub fn machines_hosts(&mut self, hosts: &[SshHost]) {
        if let Some(Overlay::Machines(view)) = self.overlay.as_mut() {
            view.hosts = hosts.to_vec();
        }
    }

    /// 列表的按键（Puffin `machines_key`）：↑↓ 与 ^P ^N 循环移动；⏎ 打开设置里的那一页；空格做随状态的操作（主机：照原样
    /// 添加）；esc 与 ^C 关上。旧界面的 `c` / `d` / `x` / `r` 照旧连、断、删、立即重试焦点上的机器。
    pub(super) fn machines_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = crate::ui::keyboard::ctrl(&key);
        let count = self.machine_entries().len();
        let focus = self.machine_focus();
        let step = |down: bool| {
            if down {
                (focus + 1) % count
            } else {
                (focus + count - 1) % count
            }
        };
        let focused = self.focused_entry();
        let row = match &focused {
            Some(Entry::Machine(name)) => self.machines.iter().find(|r| &r.name == name).cloned(),
            _ => None,
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
            // ⏎ 打开，空格做随状态的操作，同设置的 Machines 段（Puffin，2026-10-29 用户决定）。
            KeyCode::Enter => {
                return match focused {
                    Some(Entry::Machine(name)) => {
                        self.settings_machines(Some(Page::Machine(name)), None)
                    }
                    Some(Entry::Host(alias)) => {
                        self.settings_machines(Some(Page::NewMachine), Some(alias))
                    }
                    Some(Entry::Add) => self.settings_machines(Some(Page::NewMachine), None),
                    None => None,
                };
            }
            KeyCode::Char(' ') => match (&focused, &row) {
                (Some(Entry::Machine(name)), Some(row)) => Some(match enter_op(&row.status) {
                    MachineOp::Connect => MachineRequest::Connect(name.clone()),
                    MachineOp::Disconnect => MachineRequest::Disconnect(name.clone()),
                    MachineOp::Retry => MachineRequest::Retry(name.clone()),
                    MachineOp::Remove => MachineRequest::Remove(name.clone()),
                }),
                // `~/.ssh/config` 的主机照原样添加：名字与 ssh 目标都是别名，工作目录不写。
                (Some(Entry::Host(alias)), _) => Some(MachineRequest::Add {
                    name: alias.clone(),
                    ssh: alias.clone(),
                    working_directory: None,
                }),
                _ => None,
            },
            KeyCode::Char('c') => row.map(|r| MachineRequest::Connect(r.name)),
            KeyCode::Char('d') => row.map(|r| MachineRequest::Disconnect(r.name)),
            KeyCode::Char('x') => row.map(|r| MachineRequest::Remove(r.name)),
            // 立即重试（F98）：只对重连中的机器。
            KeyCode::Char('r') => row
                .filter(|r| matches!(r.status, MachineStatus::Reconnecting { .. }))
                .map(|r| MachineRequest::Retry(r.name)),
            _ => None,
        };
        // 先记下焦点：之后快照变了也跟着这一项（删掉之后按原下标落到相邻的一项）。
        self.focus_machine(focus);
        request.map(Action::Machines)
    }

    /// 列表的行（kit `machine_rows::rows`）：头一行，空一行，机器（窗口围着焦点）与选中的那台的细节，空一行，
    /// `+ Add a machine`；没有机器时是一句话与 `~/.ssh/config` 的主机。
    pub fn machines_rows(&self, now_ms: u64) -> Vec<Element> {
        let entries = self.machine_entries();
        let focus = self.machine_focus();
        let hosts = self.machines_view().map_or(&[][..], |v| &v.hosts[..]);
        let chosen = entries.get(focus).and_then(|e| match e {
            Entry::Machine(name) => self
                .machines
                .iter()
                .position(|m| &m.name == name)
                .map(Chosen::Machine),
            Entry::Host(alias) => hosts
                .iter()
                .position(|h| &h.alias == alias)
                .map(Chosen::Host),
            Entry::Add => Some(Chosen::Add),
        });
        let kit_hosts: Vec<Host> = hosts
            .iter()
            .map(|h| Host {
                alias: &h.alias,
                target: &h.target,
            })
            .collect();
        machine_rows::rows_said(
            &self.ink.theme,
            &zh::list_words(self.lang),
            &self.kit_machines(now_ms),
            &kit_hosts,
            focus,
            chosen,
        )
    }

    /// 列表的一层：[`Self::machines_rows`] 放进层的板（`pick::layer`，同 Puffin 的层）。
    pub fn machines_lines(&self, width: usize, now_ms: u64) -> Vec<StyledLine> {
        super::pick::layer(self.machines_rows(now_ms), &self.ink, width)
    }

    /// 列表开着时槽说的键（Puffin `machines_slot` 经 kit `pick`：去掉明显的 `esc close`，至多两个）：⏎ 打开，机器与主机有
    /// 空格。挑的是 Puffin 的英文，中文界面挑完再换字。
    pub fn machines_slot(&self) -> Vec<Span> {
        use rust_cli_tui_kit::app::widgets::key_hints;
        let mut keys: Vec<(&'static str, &'static str)> = match self.focused_entry() {
            Some(Entry::Machine(name)) => {
                let mut keys = vec![("⏎", "details")];
                if let Some(row) = self.machines.iter().find(|r| r.name == name) {
                    keys.push(("space", words::action_word(&row.status)));
                }
                keys
            }
            Some(Entry::Host(_)) => vec![("⏎", "change it first"), ("space", "add and connect")],
            Some(Entry::Add) => vec![("⏎", "add")],
            None => Vec::new(),
        };
        keys.push(("esc", "close"));
        let keys: Vec<(String, String)> = keys
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let keys: Vec<(String, String)> = key_hints::pick(&keys)
            .into_iter()
            .map(|(k, v)| (k, slot_word(self.lang, &v)))
            .collect();
        key_hints::spans(&self.ink.theme, &keys)
    }
}
