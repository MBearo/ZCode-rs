//! 自动更新的选择屏（docs/specs/rust-cli/update.md §6，工作线 F48；照 Codex `update_prompt.rs`）。
//!
//! 入口在工作区信任之前调用 [`ask`]：独立进入、离开一次 alternate screen，阻塞到用户选择。要不要弹、读写缓存、执行
//! 安装都在入口；这里只显示与收按键。真实终端与测试（`TestBackend`）走同一个 [`drive`]。会话里的“有新版本”提示用
//! 同一份 [`UpdateInfo`]（入口经 `Services.update` 给出，§7）。
use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::widgets::Paragraph;

use crate::services::{UiLocale, UiTheme};
use crate::ui::i18n::{self, Lang};
use crate::ui::palette::{ColorLevel, Palette};
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

/// 用户的选择（Codex `UpdateSelection`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChoice {
    UpdateNow,
    Skip,
    SkipUntilNext,
}

/// 画选择屏、收按键，直到选出一项。选出之后不再重画；输入结束（终端关掉）当作 Skip（Codex：`None => Continue`）。
pub async fn drive<B: Backend>(
    info: &UpdateInfo,
    palette: Palette,
    lang: Lang,
    terminal: &mut Terminal<B>,
    mut input: impl Stream<Item = Event> + Unpin,
) -> Result<UpdateChoice> {
    let mut screen = Screen::default();
    loop {
        terminal
            .draw(|frame| {
                let area = frame.area();
                let lines = screen.lines(info, &palette, lang, usize::from(area.width));
                frame.render_widget(Paragraph::new(lines), area);
            })
            .map_err(|error| anyhow::anyhow!("drawing the update prompt: {error}"))?;
        let Some(event) = input.next().await else {
            return Ok(UpdateChoice::Skip);
        };
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
            && let Some(choice) = screen.key(key)
        {
            return Ok(choice);
        }
    }
}

/// 真实终端：进入 alternate screen 与 raw mode，选完恢复。stdin 或 stdout 不是终端时与 TUI 相同地失败。
pub async fn ask(
    info: UpdateInfo,
    vars: &BTreeMap<String, String>,
    locale: Option<UiLocale>,
    theme: Option<UiTheme>,
    os: crate::os::Os,
) -> Result<UpdateChoice> {
    let lang = i18n::resolve(locale.map(UiLocale::as_str), vars);
    // 与 TUI 同一个判断（含 Git Bash 的 mintty，工作线 F71）。
    if let Some(refusal) = crate::ui::console::Console::current().refusal(i18n::strings(lang)) {
        bail!("{refusal}");
    }
    let (mut terminal, guard, probed) = crate::ui::terminal::enter()?;
    let palette = Palette::new(
        ColorLevel::detect(vars, os),
        crate::ui::slash_theme::background(theme.unwrap_or(UiTheme::Auto), probed.background),
    )
    .with_foreground(probed.foreground);
    // Codex `discard_pending_input_before_interactive_screen`：选择屏出现之前敲下的键不算选择。
    discard_pending_input();
    // 读失败的事件跳过（与 TUI 的输入相同）；选择屏结束时丢掉事件流，之后的界面重建自己的。
    let input = EventStream::new().filter_map(|event| std::future::ready(event.ok()));
    let choice = drive(&info, palette, lang, &mut terminal, input).await;
    drop(terminal);
    drop(guard);
    choice
}

/// 读掉已经排队的输入事件（不等待）。
fn discard_pending_input() {
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}
