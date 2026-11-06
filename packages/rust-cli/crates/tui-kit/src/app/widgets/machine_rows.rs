//! 远端机器的行（docs/specs/rust-cli/remote-exec.md §21.3；docs/specs/rust-cli/tui-kit.md §4.4）：`/machines` 的列表（头一行
//! 与台数，一行一台的几列——记号、名字、系统、目录，状态靠右；选中的那台下面是细节；没有机器时是一句话与 `~/.ssh/config`
//! 的主机；最后是 `+ Add a machine`）、坞行（只列在变的机器）与状态栏的 `@机器`。纯的；入参是中性的 [`Machine`]，它的
//! [`MachineRow`] 带着壳按机器源的状态写好的字。
//! 移植自 Puffin 的 shell/kept/machine_view.rs（`mark`、`state_spans`、`step_spans`、`columns`、`under`、
//! `plus_row`、`machines_rows`、`machine_details`、`machines_row`、`machines_where`）与 kept/machine_words.rs 的列表的字。
use crate::app::element::{Element, br, by_width, single_line};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fit::pad;
use super::lists::{chosen_style, window};
use super::settings::clip_spans;

pub const TITLE: &str = "Machines";
/// 头一行放得下时接在台数后面（remote-exec.md §21.3）。
pub const HEAD_NOTE: &str = "the agent can run its tools on connected ones";
pub const NONE_YET: &str = "none yet";
pub const EMPTY_LEAD: &str = "Let the agent run its tools on another computer over SSH.";
pub const HOSTS_NOTE: &str =
    "These hosts are in your ~/.ssh/config; space adds one as it is and connects it.";
pub const SSH_CONFIG: &str = "~/.ssh/config";
pub const ADD: &str = "Add a machine";
pub const REMOTE_HOME: &str = "remote home";
pub const ENV_NOT_REFRESHED: &str = "env not refreshed (older executor)";
/// 失败与没连的机器在行尾的字。
pub const COULD_NOT_CONNECT: &str = "couldn't connect";
pub const NOT_CONNECTED: &str = "not connected";

/// 列表里写死的字（tui-kit.md §4.1 第 4 条：英文缺省是 Puffin 的字，中文界面传译文，铺法不随字变）。
#[derive(Clone, Copy, Debug)]
pub struct ListWords {
    pub title: &'static str,
    pub head_note: &'static str,
    pub none_yet: &'static str,
    pub empty_lead: &'static str,
    pub hosts_note: &'static str,
    pub add: &'static str,
    pub env_not_refreshed: &'static str,
    pub could_not_connect: &'static str,
    pub not_connected: &'static str,
    pub reconnecting: &'static str,
    /// 头一行的台数（`1 of 4 connected`）：已连的、全部。
    pub connected_of: fn(usize, usize) -> String,
    /// 窗口截掉的边：`true` 是上面（`↑ n more`），`false` 是下面。
    pub more: fn(bool, usize) -> String,
}

fn more_en(above: bool, n: usize) -> String {
    format!("{} {n} more", if above { "↑" } else { "↓" })
}

/// Puffin 的字。
pub const LIST_WORDS: ListWords = ListWords {
    title: TITLE,
    head_note: HEAD_NOTE,
    none_yet: NONE_YET,
    empty_lead: EMPTY_LEAD,
    hosts_note: HOSTS_NOTE,
    add: ADD,
    env_not_refreshed: ENV_NOT_REFRESHED,
    could_not_connect: COULD_NOT_CONNECT,
    not_connected: NOT_CONNECTED,
    reconnecting: "reconnecting",
    connected_of,
    more: more_en,
};

/// 列表一次最多显示的机器数（其余说 `↑ n more` / `↓ n more`）。
const ROWS: usize = 6;
/// 名字列至少的宽度（设计稿的 12 列）。
const NAME_MIN: usize = 12;
/// 系统与架构的列宽。
const OS_COLS: usize = 16;
/// 主机的目标列宽。
const TARGET_COLS: usize = 34;

/// 列表头一行的台数：`1 of 4 connected`。
pub fn connected_of(connected: usize, total: usize) -> String {
    format!("{connected} of {total} connected")
}

/// 分步里一步的处境：走过了、这一步、还没到。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Done,
    Now,
    Later,
}

/// 一台机器此刻的样子，字由壳写好。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MachineRow {
    /// 就绪；`latency_ms` 是量到过的心跳往返。
    Ready {
        latency_ms: Option<u64>,
    },
    Disconnected,
    /// 第一次连接：这一步的字、它的大小（`" (89 MB)"`）、过了多久，与分步（每一步的字与处境）。
    Connecting {
        step: String,
        size: Option<String>,
        elapsed: String,
        steps: Vec<(String, Phase)>,
    },
    /// 重连：列表的倒计时、坞行的 `next try in …`（等着时才有）、第几次、最近一次的原因（`Last error: …`）。
    Reconnecting {
        countdown: Option<String>,
        next_try: Option<String>,
        attempt: String,
        last_error: Option<String>,
    },
    /// 失败：原因与建议（`true` 的一段是命令）。
    Failed {
        cause: String,
        advice: Vec<(String, bool)>,
    },
}

/// 列表上的一台机器：名字、ssh 目标、系统与架构（`macOS arm64`，不知道时为 ""）、工作目录、执行器的字与它刷不刷新
/// 环境（握手过才有）、此刻的样子。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    pub ssh: String,
    pub os: String,
    pub dir: String,
    pub executor: Option<(String, bool)>,
    pub row: MachineRow,
}

/// 没有机器时的一个 `~/.ssh/config` 主机：别名与目标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host<'a> {
    pub alias: &'a str,
    pub target: &'a str,
}

/// 焦点所在的项：一台机器、一个主机（各是它的下标）、`+ Add a machine`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chosen {
    Machine(usize),
    Host(usize),
    Add,
}

/// 状态的记号与颜色（spec §11.1）：就绪 `●` ok，在连与重连 `○` 琥珀，失败 `✗` err，没连 `○` fg3。
pub fn mark(theme: &Theme, row: &MachineRow) -> Span {
    match row {
        MachineRow::Ready { .. } => Span::new("●", Style::fg(theme.ok)),
        MachineRow::Connecting { .. } | MachineRow::Reconnecting { .. } => {
            Span::new("○", Style::fg(theme.code))
        }
        MachineRow::Failed { .. } => Span::new("✗", Style::fg(theme.err)),
        MachineRow::Disconnected => Span::new("○", theme.fg3),
    }
}

fn dot(theme: &Theme) -> Span {
    Span::new(" · ", theme.line)
}

/// 行尾的状态（spec §11.1）：就绪写延迟，重连写倒计时，连接中写这一步与秒数，失败 err，没连 fg3。
fn state_spans(theme: &Theme, words: &ListWords, row: &MachineRow) -> Vec<Span> {
    match row {
        MachineRow::Ready { latency_ms } => latency_ms
            .map(|ms| vec![Span::new(format!("{ms}ms"), theme.fg3)])
            .unwrap_or_default(),
        MachineRow::Reconnecting { countdown, .. } => {
            let mut out = vec![Span::new(words.reconnecting, theme.fg2)];
            if let Some(left) = countdown {
                out.push(dot(theme));
                out.push(Span::new(left.clone(), theme.fg3));
            }
            out
        }
        MachineRow::Connecting { step, elapsed, .. } => vec![
            Span::new(step.clone(), theme.fg2),
            dot(theme),
            Span::new(elapsed.clone(), theme.fg3),
        ],
        MachineRow::Failed { .. } => vec![Span::new(words.could_not_connect, Style::fg(theme.err))],
        MachineRow::Disconnected => vec![Span::new(words.not_connected, theme.fg3)],
    }
}

/// 分步的一行：`✓ checked   ○ uploading 89 MB   handshake`（走过的 ✓ ok 加 fg3，这一步 `○` 琥珀加 fg2，没到的 quiet）。
pub fn step_spans(theme: &Theme, steps: &[(String, Phase)]) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, (word, phase)) in steps.iter().enumerate() {
        if i > 0 {
            out.push(Span::plain("   "));
        }
        match phase {
            Phase::Done => {
                out.push(Span::new("✓ ", Style::fg(theme.ok)));
                out.push(Span::new(word.clone(), theme.fg3));
            }
            Phase::Now => {
                out.push(Span::new("○ ", Style::fg(theme.code)));
                out.push(Span::new(word.clone(), theme.fg2));
            }
            Phase::Later => out.push(Span::new(word.clone(), theme.quiet)),
        }
    }
    out
}

/// 左边从第 3 列起、右边停在边前一格的一行；放不下时先截左边。
fn columns(left: Vec<Span>, right: Vec<Span>) -> Element {
    by_width(move |w| {
        let mut spans = vec![Span::plain(INDENT)];
        let rw: usize = right.iter().map(|s| string_width(&s.text)).sum();
        let room = w.saturating_sub(INDENT.len() + rw + if rw > 0 { 3 } else { 0 });
        let left = clip_spans(left.clone(), room);
        let lw: usize = left.iter().map(|s| string_width(&s.text)).sum();
        spans.extend(left);
        if rw > 0 {
            spans.push(Span::plain(
                " ".repeat(w.saturating_sub(INDENT.len() + lw + rw + 1).max(2)),
            ));
            spans.extend(right.iter().cloned());
        }
        single_line(spans, false)
    })
}

/// 选中的项下面的一行（缩进到名字）。
fn under(spans: Vec<Span>) -> Element {
    let mut all = vec![Span::plain(format!("{INDENT}  "))];
    all.extend(spans);
    single_line(all, false)
}

/// `+ Add a machine` 一类的行：`+` fg3、字 fg2，选中时都是品牌色。
fn plus_row(theme: &Theme, chosen: bool, cols: Vec<(String, Style)>) -> Element {
    let ink = |base: Style| if chosen { chosen_style(theme) } else { base };
    let mut left = vec![Span::new("+ ", ink(theme.fg3))];
    for (i, (text, style)) in cols.into_iter().enumerate() {
        left.push(Span::new(text, if i == 0 { ink(style) } else { style }));
    }
    columns(left, Vec::new())
}

/// 列表的行（spec §11.1）：头一行，空一行，机器（窗口围着焦点 `focus`）与选中的那台的细节，空一行，`+ Add a machine`。
/// 没有机器时是一句话与 `hosts`（`~/.ssh/config` 的主机）。
pub fn rows(
    theme: &Theme,
    all: &[Machine],
    hosts: &[Host<'_>],
    focus: usize,
    chosen: Option<Chosen>,
) -> Vec<Element> {
    rows_said(theme, &LIST_WORDS, all, hosts, focus, chosen)
}

/// 同 [`rows`]，字由 `words` 给（旧界面的中文）。
pub fn rows_said(
    theme: &Theme,
    words: &ListWords,
    all: &[Machine],
    hosts: &[Host<'_>],
    focus: usize,
    chosen: Option<Chosen>,
) -> Vec<Element> {
    let words = *words;
    let title = Span::new(words.title, theme.fg2.with_attr(attr::BOLD));
    let mut rows = Vec::new();
    if all.is_empty() {
        rows.push(columns(
            vec![
                title,
                Span::plain("   "),
                Span::new(words.none_yet, theme.fg3),
            ],
            Vec::new(),
        ));
        rows.push(br());
        rows.push(columns(
            vec![Span::new(words.empty_lead, theme.fg2)],
            Vec::new(),
        ));
        if !hosts.is_empty() {
            rows.push(columns(
                vec![Span::new(words.hosts_note, theme.fg3)],
                Vec::new(),
            ));
        }
        rows.push(br());
        let (from, until) = window(hosts.len(), focus.min(hosts.len()), ROWS);
        for (i, h) in hosts.iter().enumerate().take(until).skip(from) {
            rows.push(plus_row(
                theme,
                chosen == Some(Chosen::Host(i)),
                vec![
                    (pad(h.alias, NAME_MIN), theme.fg2),
                    (pad(h.target, TARGET_COLS), theme.fg3),
                    (SSH_CONFIG.to_owned(), theme.fg3),
                ],
            ));
        }
        rows.push(plus_row(
            theme,
            chosen == Some(Chosen::Add),
            vec![(words.add.to_owned(), theme.fg2)],
        ));
        return rows;
    }
    let connected = all
        .iter()
        .filter(|m| matches!(m.row, MachineRow::Ready { .. }))
        .count();
    let count = (words.connected_of)(connected, all.len());
    // 头一行的说明放得下时才有（spec §11.1）。
    let head_theme = *theme;
    rows.push(by_width(move |w| {
        let short = vec![
            Span::plain(INDENT),
            Span::new(words.title, head_theme.fg2.with_attr(attr::BOLD)),
            Span::plain("   "),
            Span::new(count.clone(), head_theme.fg3),
        ];
        let mut long = short.clone();
        long.push(dot(&head_theme));
        long.push(Span::new(words.head_note, head_theme.fg3));
        let lw: usize = long.iter().map(|s| string_width(&s.text)).sum();
        single_line(if lw < w { long } else { short }, false)
    }));
    rows.push(br());
    let name_w = all
        .iter()
        .map(|m| string_width(&m.name) + 2)
        .max()
        .unwrap_or(0)
        .max(NAME_MIN);
    let (from, until) = window(all.len(), focus.min(all.len() - 1), ROWS);
    if from > 0 {
        rows.push(columns(
            vec![Span::new((words.more)(true, from), theme.fg3)],
            Vec::new(),
        ));
    }
    for (i, m) in all.iter().enumerate().take(until).skip(from) {
        let on = chosen == Some(Chosen::Machine(i));
        rows.push(columns(
            vec![
                mark(theme, &m.row),
                Span::plain(" "),
                Span::new(
                    pad(&m.name, name_w),
                    if on { chosen_style(theme) } else { theme.fg2 },
                ),
                Span::new(pad(&m.os, OS_COLS), theme.fg3),
                Span::new(m.dir.clone(), theme.fg3),
            ],
            state_spans(theme, &words, &m.row),
        ));
        if on {
            rows.extend(details(theme, &words, m));
        }
    }
    if until < all.len() {
        rows.push(columns(
            vec![Span::new((words.more)(false, all.len() - until), theme.fg3)],
            Vec::new(),
        ));
    }
    rows.push(br());
    rows.push(plus_row(
        theme,
        chosen == Some(Chosen::Add),
        vec![(words.add.to_owned(), theme.fg2)],
    ));
    rows
}

/// 选中的机器下面的细节（spec §11.1）：就绪与没连时是 ssh 目标与执行器，失败时是原因与建议，连接中是分步，重连中是
/// 最近一次的原因。
fn details(theme: &Theme, words: &ListWords, m: &Machine) -> Vec<Element> {
    match &m.row {
        MachineRow::Failed { cause, advice } => {
            let mut out = vec![under(vec![Span::new(cause.clone(), Style::fg(theme.err))])];
            if !advice.is_empty() {
                out.push(under(
                    advice
                        .iter()
                        .map(|(text, cmd)| {
                            Span::new(text.clone(), if *cmd { theme.fg2 } else { theme.fg3 })
                        })
                        .collect(),
                ));
            }
            out
        }
        MachineRow::Connecting { steps, .. } => vec![under(step_spans(theme, steps))],
        MachineRow::Reconnecting {
            last_error: Some(e),
            ..
        } => vec![under(vec![Span::new(e.clone(), theme.fg3)])],
        MachineRow::Reconnecting { .. } => Vec::new(),
        MachineRow::Ready { .. } | MachineRow::Disconnected => {
            let sep = || Span::new("  ·  ", theme.line);
            let mut spans = vec![
                Span::new("ssh ", theme.fg3),
                Span::new(m.ssh.clone(), theme.fg2),
            ];
            if let Some((word, refreshes_env)) = &m.executor {
                spans.push(sep());
                spans.push(Span::new(word.clone(), theme.fg3));
                if !refreshes_env {
                    spans.push(sep());
                    spans.push(Span::new(words.env_not_refreshed, theme.fg3));
                }
            }
            vec![under(spans)]
        }
    }
}

/// 坞行（spec §11.1）：只列此刻在变的机器——连接中、重连中、失败；就绪与没连的不列，没有 `● local`。没有时没有。
pub fn dock_row(theme: &Theme, all: &[Machine]) -> Option<Element> {
    let mut spans: Vec<Span> = Vec::new();
    for m in all {
        let words_of: Vec<Span> = match &m.row {
            MachineRow::Connecting {
                step,
                size,
                elapsed,
                ..
            } => {
                let mut w = vec![Span::new(step.clone(), theme.fg2)];
                if let Some(size) = size {
                    w.push(Span::new(size.clone(), theme.fg3));
                }
                w.push(dot(theme));
                w.push(Span::new(elapsed.clone(), theme.fg3));
                w
            }
            MachineRow::Reconnecting {
                next_try, attempt, ..
            } => {
                let mut w = vec![Span::new("reconnecting", theme.fg2)];
                if let Some(next) = next_try {
                    w.push(dot(theme));
                    w.push(Span::new(next.clone(), theme.fg3));
                }
                w.push(dot(theme));
                w.push(Span::new(attempt.clone(), theme.fg3));
                w
            }
            MachineRow::Failed { .. } => vec![
                Span::new(COULD_NOT_CONNECT, Style::fg(theme.err)),
                dot(theme),
                Span::new("/machines", theme.fg2),
                Span::new(" shows why", theme.fg3),
            ],
            MachineRow::Ready { .. } | MachineRow::Disconnected => continue,
        };
        spans.push(Span::plain(if spans.is_empty() { " " } else { "  " }));
        spans.push(mark(theme, &m.row));
        spans.push(Span::plain(" "));
        spans.push(Span::new(m.name.clone(), theme.fg2));
        spans.push(Span::plain("  "));
        spans.extend(words_of);
    }
    (!spans.is_empty()).then(|| single_line(spans, false))
}

/// 状态栏的 `@机器`（spec §11.1）：`ready`（就绪的机器，按列表的次序）的第一台，不止一台时接 ` +N`；没有就绪的时没有。
pub fn where_(ready: &[&str]) -> Option<String> {
    let (first, rest) = ready.split_first()?;
    Some(if rest.is_empty() {
        format!("@{first}")
    } else {
        format!("@{first} +{}", rest.len())
    })
}
