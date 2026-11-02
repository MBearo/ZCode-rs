//! 工作区信任对话框（docs/specs/rust-cli/workspace-trust.md §2.4、§2.5）。
//!
//! 入口在启动扩展与建会话之前调用 [`ask`]：独立进入、离开一次 alternate screen，阻塞到用户回答（§1 的启动时序）。
//! 判定是否要问、写信任记录都在入口；这里只有内容与回答，循环与终端在 `ui::standalone_dialog`（与更新屏共用，F138）。
use std::collections::BTreeMap;

use anyhow::Result;
use crossterm::event::Event;
use futures_util::Stream;
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::services::{UiLocale, UiTheme};
use crate::ui::i18n::Lang;
use crate::ui::palette::Palette;
use crate::ui::standalone_dialog;
use crate::ui::trust_view::Dialog;

/// 项目级配置文件里会自动生效的授权（`permission.allowedTools`，workspace-trust.md §2.4）。规则已排好、
/// 写成规则的形式并去重；`count` 是原始条数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grants {
    pub count: usize,
    pub rules: Vec<String>,
    /// 写了这些规则的文件，相对工作区根。
    pub sources: Vec<String>,
}

/// 对话框要显示的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustPrompt {
    /// 工作区根的绝对路径。
    pub path: String,
    /// 只经父目录信任、项目有授权时的后备询问（§2.5）：取消项是“不带这些权限继续”。
    pub backstop: bool,
    pub grants: Option<Grants>,
}

/// 用户的回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustAnswer {
    /// 选了 `Yes, I trust this folder`。
    Trusted,
    /// 后备询问里选了“不带这些权限继续”或按了 esc：继续，但去掉项目级的 allow 规则。
    WithoutGrants,
    /// 退出进程，不打印文字：`No, exit` 与 ctrl+c / ctrl+d 两次是 1，esc 是 0。
    Exit(u8),
}

/// 画对话框、收按键，直到给出回答。给出回答后不再重画；输入结束（终端关掉）当作 `Exit(1)`。
pub async fn drive<B: Backend>(
    prompt: &TrustPrompt,
    palette: Palette,
    lang: Lang,
    terminal: &mut Terminal<B>,
    input: impl Stream<Item = Event> + Unpin,
) -> Result<TrustAnswer> {
    standalone_dialog::drive(Dialog::new(prompt.clone()), palette, lang, terminal, input).await
}

/// 真实终端：进入 alternate screen 与 raw mode，问完恢复。stdin 或 stdout 不是终端时与 TUI 相同地失败。
pub async fn ask(
    prompt: TrustPrompt,
    vars: &BTreeMap<String, String>,
    locale: Option<UiLocale>,
    theme: Option<UiTheme>,
    os: crate::os::Os,
) -> Result<TrustAnswer> {
    standalone_dialog::ask(Dialog::new(prompt), vars, locale, theme, os).await
}
