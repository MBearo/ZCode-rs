// 复制自 apps/zcode-host/crates/tui/src/ui/detail.rs（feat/rs 79b6691c55）。
//! 详细视图（docs/specs/rust-tui.md §8.5）：ctrl+o 打开，占满整个屏幕，显示 transcript 的详细形式；
//! 可滚动与搜索。滚动按离底部的行数记，新内容追加时停在末尾。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::app::{Action, App, Overlay};
use super::cells::Ctx;
use super::i18n::Lang;
use super::transcript::{self, Cache};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detail {
    /// 离底部的行数（0 = 末尾）。
    pub scroll: usize,
    pub search: Option<Search>,
    /// 上一帧的总行数与视口高度（翻页与到顶用）。
    pub total: usize,
    pub height: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Search {
    pub query: String,
    pub typing: bool,
    /// 当前匹配的序号；回车或 n/N 之后由绘制把它滚进视口。
    pub current: Option<usize>,
    /// 上一帧找到的匹配数。
    pub count: usize,
    pub jump: bool,
}

impl Detail {
    fn max(&self) -> usize {
        self.total.saturating_sub(self.height)
    }

    fn scroll_by(&mut self, lines: isize) {
        self.scroll = (self.scroll as isize + lines).clamp(0, self.max() as isize) as usize;
    }
}

impl App {
    pub(super) fn open_detail(&mut self) {
        self.overlay = Some(Overlay::Detail(Detail::default()));
    }

    pub(super) fn detail_key(&mut self, key: KeyEvent) -> Option<Action> {
        let (ctrl, typed) = (self.typing.ctrl(&key), self.typing.is_text(&key));
        let Some(Overlay::Detail(detail)) = self.overlay.as_mut() else {
            return None;
        };
        if let Some(search) = detail.search.as_mut()
            && search.typing
        {
            match key.code {
                KeyCode::Esc => detail.search = None,
                KeyCode::Enter => {
                    search.typing = false;
                    search.current = None;
                    search.jump = true;
                }
                KeyCode::Backspace if search.query.is_empty() => detail.search = None,
                KeyCode::Backspace => {
                    search.query.pop();
                }
                KeyCode::Char(c) if typed => search.query.push(c),
                _ => {}
            }
            return None;
        }
        let half = (detail.height / 2).max(1) as isize;
        let page = detail.height.max(1) as isize;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Char('o' | 'c') if ctrl => self.overlay = None,
            KeyCode::Char('u') if ctrl => detail.scroll_by(half),
            KeyCode::Char('d') if ctrl => detail.scroll_by(-half),
            KeyCode::Up | KeyCode::Char('k') => detail.scroll_by(1),
            KeyCode::Down | KeyCode::Char('j') => detail.scroll_by(-1),
            KeyCode::PageUp | KeyCode::Char('b') => detail.scroll_by(page),
            KeyCode::PageDown | KeyCode::Char(' ') => detail.scroll_by(-page),
            KeyCode::Home | KeyCode::Char('g') => detail.scroll = detail.max(),
            KeyCode::End | KeyCode::Char('G') => detail.scroll = 0,
            KeyCode::Char('/') => {
                detail.search = Some(Search {
                    typing: true,
                    ..Search::default()
                });
            }
            KeyCode::Char(c @ ('n' | 'N')) => {
                if let Some(search) = detail.search.as_mut()
                    && search.count > 0
                {
                    let current = search.current.unwrap_or(0);
                    search.current = Some(if c == 'n' {
                        (current + 1) % search.count
                    } else {
                        (current + search.count - 1) % search.count
                    });
                    search.jump = true;
                }
            }
            _ => {}
        }
        None
    }

    pub(super) fn detail_wheel(&mut self, lines: isize) {
        if let Some(Overlay::Detail(detail)) = self.overlay.as_mut() {
            detail.scroll_by(lines);
        }
    }
}

fn hint(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Showing detailed transcript · {ctrl+o} to toggle · {/} to search",
        Lang::Zh => "详细对话 · {ctrl+o} 关闭 · {/} 搜索",
    }
}

/// 没有匹配（主界面的搜索模式也用这句，tui-input.md §2.3.5）。
pub(super) fn no_matches(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "no matches",
        Lang::Zh => "没有匹配",
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    app: &mut App,
    cache: &mut Cache,
    elapsed: std::time::Duration,
    now_ms: u64,
) {
    let Some(Overlay::Detail(mut detail)) = app.overlay.clone() else {
        return;
    };
    let area = frame.area();
    let height = area.height.saturating_sub(1) as usize;
    let palette = app.palette;
    let (workspace, label, version) = (
        app.workspace().to_owned(),
        app.workspace_label(),
        app.version.clone(),
    );
    let ctx = Ctx {
        palette: &palette,
        lang: app.lang,
        strings: app.strings,
        width: area.width as usize,
        expanded: true,
        elapsed,
        now_ms,
        workspace: &workspace,
        workspace_label: &label,
        version: &version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        markdown: None,
    };
    let layout = transcript::lines(app, cache, &ctx);
    let total = layout.len();
    detail.total = total;
    detail.height = height;
    let matches = detail
        .search
        .as_ref()
        .map(|search| layout.find(&search.query))
        .unwrap_or_default();
    let max = detail.max();
    if let Some(search) = detail.search.as_mut() {
        search.count = matches.len();
        if search.jump && !matches.is_empty() {
            // 回车之后先停在离底部最近的匹配。
            let current = *search.current.get_or_insert(matches.len() - 1);
            let line = matches[current.min(matches.len() - 1)].0;
            let top = line.saturating_sub(height / 2);
            detail.scroll = total.saturating_sub(top + height).min(max);
        }
        search.jump = false;
    }
    detail.scroll = detail.scroll.min(detail.max());
    let end = total - detail.scroll;
    let start = end.saturating_sub(height);
    // 只复制可见行，匹配处画 `find_match`（反色，与主界面的搜索模式同一个记号，tui.md §7.4）。
    let mut visible: Vec<Line<'static>> = layout.lines(start, end);
    let matched = app.palette.find_match();
    for (line, from, to) in &matches {
        if (start..end).contains(line) {
            super::selection::paint(&mut visible[line - start], *from, *to, matched);
        }
    }
    super::draw::fill_rows(&mut visible, area.width as usize);
    frame.render_widget(
        Paragraph::new(visible),
        Rect::new(area.x, area.y, area.width, height as u16),
    );
    let dim = Style::default().add_modifier(Modifier::DIM);
    let footer = match &detail.search {
        Some(search) if search.typing => Line::from(format!("/{}", search.query)),
        Some(search) if !search.query.is_empty() => {
            let status = match search.current {
                Some(current) if search.count > 0 => {
                    format!("  {}/{}", current + 1, search.count)
                }
                _ if search.count == 0 => format!("  {}", no_matches(app.lang)),
                _ => String::new(),
            };
            let mut spans = vec![
                Span::raw(format!("/{}", search.query)),
                Span::styled(status, dim),
                super::key_hint::separator(&app.palette),
            ];
            spans.extend(super::key_hint::spans(hint(app.lang), &app.palette));
            Line::from(spans)
        }
        // 按键提示组件（tui.md §7.4，修复线 F26）。
        _ => Line::from(super::key_hint::spans(hint(app.lang), &app.palette)),
    };
    let footer_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
    frame.render_widget(Paragraph::new(footer), footer_area);
    if let Some(search) = detail.search.as_ref().filter(|s| s.typing) {
        let col = 1 + search.query.width() as u16;
        frame.set_cursor_position((
            footer_area.x + col.min(area.width.saturating_sub(1)),
            footer_area.y,
        ));
    }
    app.hits = super::mouse::Hits::default();
    app.hits.soft = (start..end)
        .filter_map(|index| {
            let soft = super::text::soft_wrap(layout.line(index))?;
            Some((area.y + u16::try_from(index - start).ok()?, soft))
        })
        .collect();
    app.overlay = Some(Overlay::Detail(detail));
}
