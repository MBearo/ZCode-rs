//! 自动更新的选择屏（docs/specs/rust-cli/update.md §6，工作线 F48）。
//!
//! 入口在工作区信任之前调用 [`ask`]：独立进入、离开一次 alternate screen，阻塞到用户选择。要不要弹、读写缓存、执行
//! 安装都在入口；这里只有内容与选择，循环与终端在 `ui::standalone_dialog`（与信任对话框共用，F138）。会话里的
//! “有新版本”提示用同一份 [`UpdateInfo`]（入口经 `Services.update` 给出，§7）。
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
use crate::ui::update_view::Screen;

/// 选择屏与会话提示要显示的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// 当前版本。
    pub current: String,
    /// npm 上的最新版本。
    pub latest: String,
    /// 显示用的安装命令行（例如 `npm install -g @scope/zcode`）。
    pub command: String,
    /// release notes 链接（入口包的 npm 页面）。
    pub notes_url: String,
}

/// 用户的选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChoice {
    UpdateNow,
    Skip,
    SkipUntilNext,
}

/// 画选择屏、收按键，直到选出一项。选出之后不再重画；输入结束（终端关掉）当作 Skip（照常进入）。
pub async fn drive<B: Backend>(
    info: &UpdateInfo,
    palette: Palette,
    lang: Lang,
    terminal: &mut Terminal<B>,
    input: impl Stream<Item = Event> + Unpin,
) -> Result<UpdateChoice> {
    standalone_dialog::drive(Screen::new(info.clone()), palette, lang, terminal, input).await
}

/// 真实终端：进入 alternate screen 与 raw mode，选完恢复。stdin 或 stdout 不是终端时与 TUI 相同地失败。
pub async fn ask(
    info: UpdateInfo,
    vars: &BTreeMap<String, String>,
    locale: Option<UiLocale>,
    theme: Option<UiTheme>,
    os: crate::os::Os,
) -> Result<UpdateChoice> {
    standalone_dialog::ask(Screen::new(info), vars, locale, theme, os).await
}
