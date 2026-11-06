//! 管理命令的界面部分（docs/specs/rust-cli/tui-display.md §2.3、§3、§4）：打开页面与列表、生成提示文字。
//! 数据来自内核（runner 取回后交给这里）；这里不保存业务状态，只有页面与列表的界面状态。
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};

use super::i18n::Lang;

/// 需要 runner 去问内核的管理命令。
#[derive(Debug, Clone, PartialEq)]
pub enum Manage {
    /// `/tasks`：打开任务卡，顺便重取后台任务的快照（tui-tasks.md §3.1）。
    Tasks,
    /// 任务卡里按 `^X`：停止选中的子代理或命令（`Command::StopTask`，tui-tasks.md §3.3）。
    StopTask { id: String, label: String },
    /// `/clear 名字`：先给旧会话改名，再离开。
    ClearNamed(String),
    /// `/plan 描述`：切到 plan 后发出这条输入。
    PlanPrompt(String),
    /// `/usage all` 与计划的页之后的 ⇥：取会话的 API 时长与全时段的统计后打开「This computer」页（usage_local.rs，
    /// docs/specs/rust-cli/usage.md §4、§5）。
    Usage,
    /// 「This computer」页按 ←→ 换到这个范围：`seq` 是页的请求序号，回执按它丢旧。
    UsageRange {
        range: rust_cli_kernel_api::metering::ActivityRange,
        seq: u64,
    },
    /// `/theme` 换了调色板：清掉排版缓存并重画。
    Repaint,
    /// `/plan open`（tui-manage.md §8）。
    PlanOpen,
    /// `/cd <path>`（tui-manage.md §7）：参数原文。
    ChangeDirectory(String),
    /// `/mcp reconnect|enable|disable …`（tui-manage.md §6）：参数原文。
    McpControl(String),
    /// `/diff`：页面已打开，现取未提交的改动（tui-manage.md §4）。
    Diff,
    /// `/export <文件名>`：渲染会话文字后经内核写文件（tui-manage.md §3）。
    Export(String),
    /// `/export` 选了复制到剪贴板。
    ExportCopy,
    /// `/autocompact`：None 说现在在哪里压缩，`Some(at)` 写设置（`at` 为 None 去掉设置，ui/auto_compact.rs）。
    AutoCompact(Option<Option<u64>>),
    /// `/export` 选了写文件：入口按注入的时钟拼缺省文件名，再打开文件名输入框（F7）。
    ExportName,
    /// `/theme` 选中（tui-kit.md §3.4）：墨色已经换上，runner 写 `theme.json`（与 Puffin 同一个文件）并重画。
    SetTheme(rust_cli_tui_kit::theme::scheme::ThemeChoice),
}

/// 在跑的命令的计数文字：shell 与 monitor 分开数——`N shell(s)`、`N monitor(s)`，两种都有时 shell 在前、用 `, ` 连接
/// （中文 `N shell、N monitor`）；没有在跑的命令时没有。任务行的命令一段（tui-tasks.md §2.2）与回合结束那一行的
/// `still running`（修复线 F118，tui-display.md §2.7）都用它，写法只有这一份。
///
/// 修复线 F117（用户报告 2026-10-30）：以前快照里没有 monitor 这一种，monitor 也数成 shell，看不出有几个 monitor；现在按
/// 条目的种类分开数。修复线 F118（用户决定 2026-10-30：“中文不用加‘个’这个字”）：中文不带“个”。子代理不在这里数
/// （修复线 F105 与 F106）：任务行另有子代理的一段。2026-10-31 起 footer 没有 pill（tui-tasks.md §1），这段文字搬进任务行。
pub fn pill(entries: &[BackgroundEntry], lang: Lang) -> Option<String> {
    let running = |kind| {
        entries
            .iter()
            .filter(|entry| entry.status == BackgroundStatus::Running && entry.kind == kind)
            .count()
    };
    command_text(
        running(BackgroundKind::Shell),
        running(BackgroundKind::Monitor),
        lang,
    )
}

/// [`pill`] 的写法（任务行按已数好的 shell 与 monitor 调它）。
pub fn command_text(shells: usize, monitors: usize, lang: Lang) -> Option<String> {
    let parts: Vec<String> = [(shells, "shell"), (monitors, "monitor")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, name)| match lang {
            Lang::En if n == 1 => format!("1 {name}"),
            Lang::En => format!("{n} {name}s"),
            Lang::Zh => format!("{n} {name}"),
        })
        .collect();
    (!parts.is_empty()).then(|| {
        parts.join(match lang {
            Lang::En => ", ",
            Lang::Zh => "、",
        })
    })
}

/// `/plan open` 没有计划时：已经在 plan 是 “还没写计划”；刚切到 plan 时不说（None）。
///
/// 修复原因：刚切到 plan 时以前说 `Enabled plan mode`。模式变了时输入框上方另有一句（status_motion.rs，照 Puffin
/// `mode_line`），两句一次只留一句：有会话时计划文件的回答与内核的模式事件谁先到不定，留下哪一句随之不定。模式的一句已经
/// 说了计划模式开了，这一句去掉。
pub(crate) fn no_plan(lang: Lang, entering: bool) -> Option<&'static str> {
    match (lang, entering) {
        (_, true) => None,
        (Lang::En, false) => Some("Already in plan mode. No plan written yet."),
        (Lang::Zh, false) => Some("已经在计划模式，还没有写计划。"),
    }
}

#[cfg(test)]
#[path = "manage_tests.rs"]
mod tests;
