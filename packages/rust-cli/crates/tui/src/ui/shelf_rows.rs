//! 命令架的行（docs/puffin-tui.md §4.3「命令」；tui-kit.md §4.2「斜杠命令弹出」）：kit 的 `widgets::shelf::rows` 排旧界面
//! 的命令表（`commands.rs`）与内核的 skill，现在不能运行的命令照 Puffin 留在原处、灰色斜体说原因（Puffin
//! `shell/input/commands.rs` 的 `unavailable`），⏎ 在它上面什么都不做。规则是 kit 的 `command_reasons`，两套界面共用；
//! 旧界面按自己的状态回答它要问的事实（[`Facts`]）。
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::command_reasons::{self, CommandFacts, Reason};
use rust_cli_tui_kit::app::widgets::shelf::{self, ShelfRow};

use super::app::App;
use super::commands::Entry;
use super::i18n::Lang;

/// 规则要问的事实，按旧界面的状态回答（规则在 kit `command_reasons`，Puffin 调同一份）。
struct Facts<'a>(&'a App);

impl CommandFacts for Facts<'_> {
    /// Puffin 在 agent 就绪时打开一个空的会话，旧界面第一次发送才建会话：门道关着时照 Puffin 的空会话算；门道开着时
    /// Puffin 还没有会话（`current.is_none()`）。缺陷修复（孪生 theme-unknown-arg 与 palette-filter，2026-10-31）：
    /// 以前没有会话的几条没有移植；只看「没有会话」又会在门道关着时灰掉 `/compact`、`/new`。
    fn session(&self) -> bool {
        self.0.session.is_some() || self.0.door.is_none()
    }
    fn busy(&self) -> bool {
        self.0.running()
    }
    /// 「消息还没发出」：在途的与排队的输入（`view.outgoing()`），与建会话期间留下的（`held`）。
    fn sending(&self) -> bool {
        !self.0.view.outgoing().is_empty() || !self.0.held.is_empty()
    }
    fn asking(&self) -> bool {
        self.0.pending().is_some()
    }
    fn plan(&self) -> bool {
        self.0.view.plan()
    }
    /// 旧界面经自己的剪贴板服务复制，不靠终端接 OSC 52：没有 `This terminal can't copy.`。
    fn clipboard(&self) -> bool {
        true
    }
    fn nothing_to_copy(&self) -> bool {
        self.0.view.reply_count() == 0
    }
    fn nothing_to_export(&self) -> bool {
        !self.0.view.has_messages()
    }
    fn nothing_to_rewind(&self) -> bool {
        !self.0.view.has_messages()
    }
    fn machines(&self) -> bool {
        self.0.machines_enabled
    }
}

/// 命令 `name` 现在不能运行的原因（kit `command_reasons::unavailable`，Puffin 的条件、次序与说法；中文是译文）。
pub fn unavailable(app: &App, name: &str) -> Option<&'static str> {
    let zh = app.lang == Lang::Zh;
    // 回合中 `/new`、`/resume` 排到回合结束（修复线 F38），照旧能运行；Puffin 这里说 `Stop the turn first (esc)`。
    if matches!(name, "new" | "resume") && app.running() {
        return None;
    }
    if let Some(reason) = command_reasons::unavailable(name, &Facts(app)) {
        return Some(if zh { reason_zh(reason) } else { reason.en() });
    }
    // 还没有实现的命令（`/btw`）：照样列出，说明为什么用不了（旧界面的，Puffin 没有）。
    super::commands::find(name)
        .is_some_and(|c| !c.implemented())
        .then_some(if zh {
            "还不能用。"
        } else {
            "Not available yet."
        })
}

/// 原因的中文。
fn reason_zh(reason: Reason) -> &'static str {
    match reason {
        Reason::NothingToCompact => "还没有可以压缩的。",
        Reason::WaitForTurn => "等这一回合结束。",
        Reason::StopTurnFirst => "先停下这一回合（esc）",
        Reason::WaitForMessage => "等你的消息发出去。",
        Reason::AlreadyNew => "这已经是新会话了。",
        Reason::AnswerFirst => "先回答请求。",
        Reason::NothingToRewind => "还没有可以回退的。",
        Reason::LeavePlanMode => "先离开计划模式（⇧⇥）",
        Reason::SendFirst => "先发一条消息。",
        Reason::CannotCopy => "这个终端不能复制。",
        Reason::NothingToCopy => "还没有可以复制的。",
        Reason::NothingToExport => "还没有可以导出的。",
        Reason::NoSessionToName => "还没有可以命名的会话。先发一条消息。",
        Reason::NoMachines => "这里用不了远端机器。",
    }
}

/// 命令架的行与每一行对应的条目下标（没有匹配的那一行、more 行为 None）。
pub fn rows(app: &App, selected: usize) -> Vec<(Element, Option<usize>)> {
    let matches = app.composer.slash_matches();
    let why: Vec<Option<&str>> = matches.iter().map(|e| unavailable(app, e.name())).collect();
    let rows: Vec<ShelfRow<'_>> = matches
        .iter()
        .zip(&why)
        .map(|(entry, why)| ShelfRow {
            name: entry.name(),
            description: description(entry, app.lang),
            unavailable: *why,
        })
        .collect();
    let elements = shelf::rows(&app.ink.theme, &rows, selected);
    if rows.is_empty() {
        return elements.into_iter().map(|e| (e, None)).collect();
    }
    // 窗口与 kit 的同一个函数：知道哪一行是哪个条目（点击用）。
    let choice = selected.min(rows.len() - 1);
    let (from, until) =
        rust_cli_tui_kit::app::widgets::lists::window(rows.len(), choice, shelf::SHELF_ROWS);
    let mut index = Vec::new();
    if from > 0 {
        index.push(None);
    }
    index.extend((from..until).map(Some));
    if until < rows.len() {
        index.push(None);
    }
    elements.into_iter().zip(index).collect()
}

/// 一行的说明。`/init` 用 Puffin 的 host 给的说明（Puffin host 的 `lists.rs` 里的 `BUILTIN_COMMANDS`），不用内核 skill 的。
fn description<'a>(entry: &Entry<'a>, lang: Lang) -> &'a str {
    match (entry.name(), lang) {
        ("init", Lang::En) => "Create or update workspace AGENTS.md instructions.",
        ("init", Lang::Zh) => "创建或更新工作区的 AGENTS.md 指令。",
        _ => entry.description(lang),
    }
}
