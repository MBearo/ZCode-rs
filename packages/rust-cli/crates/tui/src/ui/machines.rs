//! 远端机器的界面（docs/specs/rust-cli/remote-exec.md §21.3、§21.5、§21.6）：`/machines` 的解析、坞里的机器行、`@机器`
//! 标记。数据是入口的机器服务给的快照（`App::machines`，runner 在变化时重取）。列表（项、键、槽与行）在
//! `machines_list.rs`，照 Puffin 的 `/machines`（kit `machine_rows`，tui-kit.md §4.2「`/machines`」）。
//! 画法（docs/specs/rust-cli/tui-kit.md §3.1、§3.8，工作线 TK 的 C1）：只产出 kit 的行与片段，墨色经 `Ink`。
use rust_cli_kernel_api::mcp_name;
use rust_cli_tui_kit::app::element::{Element, single_line};
use rust_cli_tui_kit::render::{Span, Style};
use serde_json::Value;

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::i18n_machines as words;
use super::ink::Ink;
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
    /// 设置的机器页改了 SSH 目标或工作目录（tui-kit.md §4.2「设置」）：清单只有加与删，于是删掉再按新的值加；`reconnect`
    /// 时再连。
    Replace {
        name: String,
        ssh: String,
        working_directory: Option<String>,
        reconnect: bool,
    },
    /// 读一次 `~/.ssh/config` 的主机（设置的 Machines 段）。
    Hosts,
}

/// `/machines` 列表的界面状态（machines_list.rs）：焦点按项保持（快照变了也跟着那一项），找不到时按原下标；`hosts` 是读到的
/// `~/.ssh/config` 主机（没有机器时列出）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachinesView {
    pub selected: usize,
    pub focus: Option<super::machines_list::Entry>,
    pub hosts: Vec<crate::machines::SshHost>,
}

/// 工具输入里的额外机器（`machine` 不是主 env 时）。
pub fn machine_of(input: &Value) -> Option<&str> {
    input
        .get("machine")
        .and_then(Value::as_str)
        .filter(|name| !rust_cli_kernel_api::ids::MachineName::is_primary_name(name))
}

/// 一次工具调用在哪台远端机器上：输入里的 `machine`（不是 `local`）；MCP 工具看它的服务器绑定的机器（R5，§22.8）。
/// `mcp` 是（规范化的服务器名，机器名）；工具名 `mcp__<服务器>__<工具>` 按最长的服务器名前缀找。
pub fn remote_of<'a>(name: &str, input: &'a Value, mcp: &'a [(String, String)]) -> Option<&'a str> {
    machine_of(input).or_else(|| {
        mcp.iter()
            .filter(|(server, _)| name.starts_with(&mcp_name::server_prefix(server)))
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
pub fn tag(ink: &Ink, name: &str) -> Span {
    Span::new(format!("@{name}"), Style::fg(ink.accent()))
}

/// 状态的记号与颜色（Puffin `kept/machine_view.rs` 的 `mark`，remote-exec.md §21.3）：就绪 `●` ok，在连与重连 `○` 琥珀
/// （`code`），失败 `✗` err，没连 `○` fg3。
pub fn mark(status: &MachineStatus, ink: &Ink) -> Span {
    let theme = &ink.theme;
    match status {
        MachineStatus::Ready => Span::new("●", Style::fg(theme.ok)),
        MachineStatus::Connecting(_) | MachineStatus::Reconnecting { .. } => {
            Span::new("○", Style::fg(theme.code))
        }
        MachineStatus::Failed(_) => Span::new("✗", Style::fg(theme.err)),
        MachineStatus::Disconnected => Span::new("○", theme.fg3),
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
            // 没有机器时列出 `~/.ssh/config` 的主机：开列表时读一次（Puffin `machines_hosts`）。
            self.overlay = Some(Overlay::Machines(MachinesView::default()));
            return Some(Action::Machines(MachineRequest::Hosts));
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

    /// 远端机器的工作目录：清单里写的，没写时是握手报告的 home；都不知道时 None（F98，§25.4.3）。
    pub fn remote_root(&self, name: &str) -> Option<String> {
        let row = self.machines.iter().find(|row| row.name == name)?;
        row.working_directory
            .clone()
            .or_else(|| row.executor.as_ref().and_then(|e| e.home.clone()))
    }

    /// 坞里的机器行（docs/specs/rust-cli/tui-kit.md §4.0 第 6 条；Puffin `kept/machine_view.rs` 的 `machines_row`）：只写
    /// 在变的机器：连接中（这一步、上传的大小、连了多久）、重连中（`next try in 4s`、第几次）、连不上（`/machines` 看原因）；
    /// 就绪与没连的不写，都不在变时没有这一行。以前状态栏下面常驻一行，从 `● local` 起列出每台机器。
    /// 移植而不调 Puffin：它的函数对着 tui-ports 的类型写，旧界面不能依赖 Puffin（tui-kit.md §4.4）。中文是旧界面的词。
    pub fn machines_row(&self, now_ms: u64) -> Option<Element> {
        let ink = &self.ink;
        let theme = &ink.theme;
        let zh = self.lang == Lang::Zh;
        let dot = || Span::new(" · ", theme.line);
        let mut spans: Vec<Span> = Vec::new();
        for row in &self.machines {
            let words_of: Vec<Span> = match &row.status {
                MachineStatus::Connecting(step) => {
                    let since = self
                        .machines_since
                        .get(&row.name)
                        .copied()
                        .unwrap_or(now_ms);
                    let (word, size) = (
                        words::step_word(self.lang, *step),
                        words::step_size(self.lang, *step),
                    );
                    let mut w = vec![Span::new(word, theme.fg2)];
                    if let Some(size) = size {
                        w.push(Span::new(size, theme.fg3));
                    }
                    w.push(dot());
                    w.push(Span::new(
                        rust_cli_kernel_api::metering::format_duration(
                            now_ms.saturating_sub(since) / 1000 * 1000,
                        ),
                        theme.fg3,
                    ));
                    w
                }
                MachineStatus::Reconnecting {
                    attempt,
                    retry_at_ms,
                    ..
                } => {
                    let mut w = vec![Span::new(words::reconnecting(self.lang), theme.fg2)];
                    if let Some(at) = retry_at_ms {
                        let left = super::retry_status::countdown(at.saturating_sub(now_ms));
                        w.push(dot());
                        w.push(Span::new(
                            if zh {
                                format!("{left}后重试")
                            } else {
                                format!("next try in {left}")
                            },
                            theme.fg3,
                        ));
                    }
                    w.push(dot());
                    w.push(Span::new(
                        if zh {
                            format!("第 {attempt} 次")
                        } else {
                            format!("attempt {attempt}")
                        },
                        theme.fg3,
                    ));
                    w
                }
                MachineStatus::Failed(_) => vec![
                    Span::new(
                        if zh {
                            "连接失败"
                        } else {
                            "couldn't connect"
                        },
                        Style::fg(theme.err),
                    ),
                    dot(),
                    Span::new("/machines", theme.fg2),
                    Span::new(if zh { " 查看原因" } else { " shows why" }, theme.fg3),
                ],
                MachineStatus::Ready | MachineStatus::Disconnected => continue,
            };
            spans.push(Span::plain(if spans.is_empty() { " " } else { "  " }));
            spans.push(mark(&row.status, ink));
            spans.push(Span::plain(" "));
            spans.push(Span::new(row.name.clone(), theme.fg2));
            spans.push(Span::plain("  "));
            spans.extend(words_of);
        }
        (!spans.is_empty()).then(|| single_line(spans, false))
    }
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
