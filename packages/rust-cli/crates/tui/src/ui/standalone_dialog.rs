//! 启动前的独立对话框（工作区信任 docs/specs/rust-cli/workspace-trust.md §2.4、自动更新的选择屏 update.md §6）：入口在
//! 建会话之前调用，独立进入、离开一次 alternate screen，阻塞到用户回答。真实终端（[`ask`]）与测试（`TestBackend`）走
//! 同一个 [`drive`]。
//!
//! 架构整理 F138：以前 `trust.rs` 与 `update.rs` 各写一份这里的循环与终端的进入（相似度约 68%）；现在两个对话框只给
//! 内容、按键与 [`Standalone`] 里不同的两项。
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossterm::event::{Event, EventStream, KeyEvent, KeyEventKind};
use futures_util::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::i18n::{self, Lang};
use super::keyboard;
use super::palette::{ColorLevel, Palette};
use super::text::truncate_line;
use crate::services::{UiLocale, UiTheme};

/// 正文缩进两格（两个对话框相同）。
pub(crate) const INDENT: &str = "  ";

/// 一个独立对话框：要显示的内容加界面状态。
pub(crate) trait Standalone {
    type Answer;
    /// 画不出来时报错里的称呼（`drawing <NAME>: <错误>`）。
    const NAME: &'static str;
    /// 输入结束（终端关掉）时的回答。
    const CLOSED: Self::Answer;

    /// 画面的行（宽度 `width`）。
    fn lines(&self, palette: &Palette, lang: Lang, width: usize) -> Vec<Line<'static>>;
    /// 一个按下的键（松开的事件已滤掉）；给出回答时对话框结束。
    fn key(&mut self, key: KeyEvent, now: Instant) -> Option<Self::Answer>;
    /// 到这个时刻要重画（连按的提示到期）；没有计时的为 None。
    fn redraw_at(&self) -> Option<Instant>;
    /// 到了 [`Self::redraw_at`]。
    fn expire(&mut self, now: Instant);
}

/// 最后一行：按键提示组件（tui.md §7.4），与正文同样缩进。
pub(crate) fn hint_line(hint: &str, palette: &Palette, width: usize) -> Line<'static> {
    truncate_line(super::key_hint::line(INDENT.len(), hint, palette), width)
}

/// 画对话框、收按键，直到给出回答。给出回答后不再重画；输入结束时是 [`Standalone::CLOSED`]。
pub(crate) async fn drive<D: Standalone, B: Backend>(
    mut dialog: D,
    palette: Palette,
    lang: Lang,
    terminal: &mut Terminal<B>,
    mut input: impl Stream<Item = Event> + Unpin,
) -> Result<D::Answer> {
    loop {
        terminal
            .draw(|frame| {
                let area = frame.area();
                let lines = dialog.lines(&palette, lang, usize::from(area.width));
                frame.render_widget(Paragraph::new(lines), area);
            })
            .map_err(|error| anyhow::anyhow!("drawing {}: {error}", D::NAME))?;
        // 连按的提示到期时重画（界面计时，不同步任何状态）。
        let event = match dialog.redraw_at() {
            Some(until) => {
                tokio::select! {
                    event = input.next() => event,
                    () = tokio::time::sleep_until(until.into()) => {
                        dialog.expire(Instant::now());
                        continue;
                    }
                }
            }
            None => input.next().await,
        };
        let Some(event) = event else {
            return Ok(D::CLOSED);
        };
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
            && let Some(answer) = dialog.key(key, Instant::now())
        {
            return Ok(answer);
        }
    }
}

/// 真实终端：进入 alternate screen 与 raw mode，回答后恢复。stdin 或 stdout 不是终端时与 TUI 相同地失败。
pub(crate) async fn ask<D: Standalone>(
    dialog: D,
    vars: &BTreeMap<String, String>,
    locale: Option<UiLocale>,
    theme: Option<UiTheme>,
    os: crate::os::Os,
) -> Result<D::Answer> {
    let lang = i18n::resolve(locale.map(UiLocale::as_str), vars);
    // 与 TUI 同一个判断（含 Git Bash 的 mintty，工作线 F71）。
    if let Some(refusal) = super::console::Console::current().refusal(i18n::strings(lang)) {
        bail!("{refusal}");
    }
    let (mut terminal, guard, probed) = super::terminal::enter()?;
    let palette = Palette::new(
        ColorLevel::detect(vars, os),
        super::slash_theme::background(theme.unwrap_or(UiTheme::Auto), probed.background),
    )
    .with_foreground(probed.foreground);
    // 丢掉对话框出现之前敲下的键：它们不算回答（update.md §6、workspace-trust.md §2.4）。
    // 缺陷修复（F138-1，主会话定）：以前只有更新屏丢，信任对话框把它们读进来——提前按下的回车选中焦点所在的
    // `No, exit`，进程以退出码 1 退出。现在两个对话框都在进入终端之后、读第一个事件之前丢掉。
    discard_pending_input();
    // 读失败的事件跳过（与 TUI 的输入相同）；对话框结束时丢掉事件流，之后的界面重建自己的。按键与主界面同一个归一
    // （Windows 的 AltGr 字符不是 ctrl+c，tui-input.md §9.1）。
    let input = EventStream::new().filter_map(move |event| {
        std::future::ready(event.ok().map(|event| keyboard::normalize_event(event, os)))
    });
    let answer = drive(dialog, palette, lang, &mut terminal, input).await;
    drop(terminal);
    drop(guard);
    answer
}

/// 读掉已经排队的输入事件（不等待）。
fn discard_pending_input() {
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}
