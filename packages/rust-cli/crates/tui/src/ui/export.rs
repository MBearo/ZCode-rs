//! `/export [filename]`（docs/specs/rust-cli/tui-manage.md §3）：会话文字用界面同一套排版渲染，
//! 复制经 `Screen::copy`，写文件经内核（`Kernel::write_export`）。这里只有对话框的界面状态。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_base::units;

use super::app::{Action, App, Overlay};
use super::cells::Ctx;
use super::i18n::Lang;
use super::layout;
use super::manage::Manage;
use super::palette::Palette;
use super::text::{plain, truncate_line};
use super::transcript::{self, Cache};

/// 对话框：先选方式，选“保存到文件”后输入文件名。
#[derive(Debug, Clone, PartialEq)]
pub enum ExportView {
    Choose { selected: usize },
    Name(String),
}

/// 会话的文字：与界面同一套排版（会话头、cell、本地提示，非展开视图），去掉样式与行尾空白。宽度取上一帧
/// transcript 的宽度，没有画过时 80 列。
pub fn conversation_text(app: &App, now_ms: u64) -> (String, Option<String>) {
    let width = match app.hits.transcript.width {
        0 => layout::DEFAULT_WIDTH,
        width => usize::from(width),
    };
    let label = crate::ui::app::label(app.view.workspace().root.as_str(), app.home.as_deref());
    let ctx = Ctx {
        palette: &app.palette,
        lang: app.lang,
        strings: app.strings,
        width,
        expanded: false,
        elapsed: std::time::Duration::ZERO,
        now_ms,
        workspace: app.view.workspace().root.as_str(),
        workspace_label: &label,
        version: &app.version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        markdown: None,
    };
    let layout = transcript::lines(app, &mut Cache::default(), &ctx);
    let mut text = String::new();
    for index in 0..layout.len() {
        text.push_str(plain(layout.line(index)).trim_end());
        text.push('\n');
    }
    let first = layout.prompts.first().map(|prompt| prompt.text.clone());
    (text, first)
}

/// 本地时间的 `YYYY-MM-DD-HHMMSS`（`offset_minutes` 是那一刻的 UTC 偏移）。
///
/// 修复（R2-6）：以前按 UTC（tui-manage.md D9）；现在用本地时间。
fn timestamp(now_ms: u64, offset_minutes: i32) -> String {
    let utc = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let offset_ms = i64::from(offset_minutes) * units::MS_PER_MINUTE as i64;
    let local = u64::try_from(utc.saturating_add(offset_ms)).unwrap_or(0);
    let date = rust_cli_kernel_api::clock::civil_date(local);
    let (hours, minutes, seconds) = super::quantities::time_of_day(local);
    format!("{date}-{hours:02}{minutes:02}{seconds:02}")
}

/// 文件名里第一条输入最多取这么多个字符（超出时截到少一个字符再加 `…`）。
const SLUG_SOURCE_CHARS: usize = 50;

/// 缺省文件名：`<时间>-<第一条输入的 slug>.txt`，没有 slug 时 `conversation-<时间>.txt`。
pub fn default_name(first_prompt: Option<&str>, now_ms: u64, offset_minutes: i32) -> String {
    let mut text = first_prompt.unwrap_or_default().trim().to_owned();
    if text.chars().count() > SLUG_SOURCE_CHARS {
        // 留一个字符给 `…`。
        text = text
            .chars()
            .take(SLUG_SOURCE_CHARS - 1)
            .chain(['…'])
            .collect();
    }
    let mut slug = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_whitespace() || c == '-' {
            if !slug.ends_with('-') {
                slug.push('-');
            }
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        }
    }
    let slug = slug.trim_matches('-');
    let time = timestamp(now_ms, offset_minutes);
    if slug.is_empty() {
        format!("conversation-{time}.txt")
    } else {
        format!("{time}-{slug}.txt")
    }
}

fn texts(lang: Lang) -> [&'static str; 8] {
    match lang {
        Lang::En => [
            "Export conversation",
            "Select export method",
            "Copy to clipboard",
            "Copy the conversation to your system clipboard",
            "Save to file",
            "Save the conversation to a file in the current directory",
            "Enter filename:",
            "{Enter} to save · {Esc} to go back",
        ],
        Lang::Zh => [
            "导出会话",
            "选择导出方式",
            "复制到剪贴板",
            "把会话复制到系统剪贴板",
            "保存到文件",
            "把会话保存到当前目录下的文件",
            "输入文件名：",
            "{enter} 保存 · {esc} 返回",
        ],
    }
}

pub(crate) fn exported(lang: Lang, path: &str) -> String {
    match lang {
        Lang::En => format!("Conversation exported to: {path}"),
        Lang::Zh => format!("会话已导出到：{path}"),
    }
}

pub(crate) fn export_failed(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::En => format!("Failed to export conversation: {reason}"),
        Lang::Zh => format!("导出会话失败：{reason}"),
    }
}

pub(crate) fn copied(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Conversation copied to clipboard",
        Lang::Zh => "会话已复制到剪贴板",
    }
}

fn cancelled(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Export cancelled",
        Lang::Zh => "已取消导出",
    }
}

/// 对话框的行（替换输入区）。
pub fn lines(view: &ExportView, lang: Lang, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    let accent = palette.accent;
    let [
        title,
        subtitle,
        copy,
        copy_note,
        save,
        save_note,
        enter,
        back,
    ] = texts(lang);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut lines = vec![
        Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
        Line::from(Span::styled(
            format!("  {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(format!("  {subtitle}"), dim)),
        Line::default(),
    ];
    match view {
        ExportView::Choose { selected } => {
            for (index, (label, note)) in [(copy, copy_note), (save, save_note)].iter().enumerate()
            {
                let focused = index == *selected;
                // 选择列表组件（`› ` 加底色条，tui.md §7.4，修复线 F26）。
                lines.push(super::pick::paint(
                    truncate_line(
                        Line::from(vec![
                            Span::raw("  "),
                            super::pick::pointer(focused),
                            Span::raw(format!("{}. {label}", index + 1)),
                            Span::styled(format!("   {note}"), palette.secondary()),
                        ]),
                        width,
                    ),
                    focused,
                    palette,
                    width,
                ));
            }
            lines.push(Line::default());
            let hint = match lang {
                Lang::En => "{Enter} to select · {Esc} to cancel",
                Lang::Zh => "{enter} 选择 · {esc} 取消",
            };
            lines.push(super::key_hint::line(layout::INDENT, hint, palette));
        }
        ExportView::Name(name) => {
            lines.push(Line::from(Span::raw(format!("  {enter}"))));
            lines.push(truncate_line(
                Line::from(vec![
                    Span::styled("  > ", Style::default().fg(accent)),
                    Span::raw(format!("{name}█")),
                ]),
                width,
            ));
            lines.push(Line::default());
            lines.push(super::key_hint::line(layout::INDENT, back, palette));
        }
    }
    lines
}

impl App {
    /// `/export [filename]`：带文件名时直接写；不带时打开对话框。
    pub(super) fn export_command(&mut self, args: &str) -> Option<Action> {
        if !args.trim().is_empty() {
            return Some(Action::Manage(Manage::Export(args.trim().to_owned())));
        }
        self.overlay = Some(Overlay::Export(ExportView::Choose { selected: 0 }));
        None
    }

    pub(super) fn export_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = self.typing.ctrl(&key);
        let typed = self.typing.is_text(&key);
        let Some(Overlay::Export(view)) = self.overlay.as_mut() else {
            return None;
        };
        match view {
            ExportView::Choose { selected } => match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    self.info(cancelled(self.lang));
                }
                KeyCode::Char('c') if ctrl => {
                    self.overlay = None;
                    self.info(cancelled(self.lang));
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                    *selected = 1 - *selected;
                }
                KeyCode::Char('1') => return self.export_choose(0),
                KeyCode::Char('2') => return self.export_choose(1),
                KeyCode::Enter => {
                    let chosen = *selected;
                    return self.export_choose(chosen);
                }
                _ => {}
            },
            ExportView::Name(name) => match key.code {
                KeyCode::Esc => *view = ExportView::Choose { selected: 1 },
                KeyCode::Enter if !name.trim().is_empty() => {
                    let path = name.trim().to_owned();
                    self.overlay = None;
                    return Some(Action::Manage(Manage::Export(path)));
                }
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Char(c) if typed => name.push(c),
                _ => {}
            },
        }
        None
    }

    fn export_choose(&mut self, index: usize) -> Option<Action> {
        if index == 0 {
            self.overlay = None;
            return Some(Action::Manage(Manage::ExportCopy));
        }
        Some(Action::Manage(Manage::ExportName))
    }

    /// 文件名输入框，缺省名是会话第一条输入加本地时间（`now_ms` 与那一刻的 UTC 偏移由 runner 从注入的时钟取，
    /// time-zone.md §3）。
    pub(crate) fn open_export_name(&mut self, now_ms: u64, offset_minutes: i32) {
        let (_, first) = conversation_text(self, now_ms);
        self.overlay = Some(Overlay::Export(ExportView::Name(default_name(
            first.as_deref(),
            now_ms,
            offset_minutes,
        ))));
    }
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
