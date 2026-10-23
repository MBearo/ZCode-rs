//! ← 项目页的排版（修复线 F29，docs/specs/rust-cli/tui-panels.md §2.2）：筛选标签、组头、会话行、右侧详情。用独立页面的
//! 框架（`page::frame`）；会话行用选择列表组件（`pick`），提示用按键提示组件。
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::app::App;
use super::i18n::Lang;
use super::i18n_panels as panel_words;
use super::i18n_projects as words;
use super::mouse::Target;
use super::projects::{Entry, FILTERS, ProjectsPage, State};
use super::session_rows::{SessionRow, center_truncate, relative};
use super::text::{truncate, truncate_line, wrap_text};

/// 右侧详情的宽度与它前面的分隔（`│ `）。
const DETAILS: usize = 38;
/// 宽度不小于这个值时画右侧详情（Codex `body.width >= 90`）。
const DETAILS_FROM: usize = 90;
/// 组头里路径最多占的列数（Codex 64）。
const GROUP_PATH: usize = 64;
/// 状态列的宽度（Codex 11）。
const STATUS: usize = 11;

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub fn dot(state: State) -> (&'static str, Style) {
    match state {
        State::NeedsYou => ("●", Style::default().fg(Color::Red)),
        State::Working => ("●", Style::default().fg(Color::Green)),
        State::Ready => ("○", Style::default().fg(Color::Cyan)),
        State::Inactive => ("○", dim()),
    }
}

pub fn state_label(lang: Lang, state: Option<State>) -> &'static str {
    let pick = |en, zh| match lang {
        Lang::En => en,
        Lang::Zh => zh,
    };
    match state {
        None => pick("All", "全部"),
        Some(State::NeedsYou) => pick("Needs you", "需要你"),
        Some(State::Working) => pick("Working", "运行中"),
        Some(State::Ready) => pick("Ready", "就绪"),
        Some(State::Inactive) => pick("Inactive", "不活跃"),
    }
}

/// 一个会话的标题（当前会话加 ` (current)`，没有标题写 `(untitled)`）。
fn title(lang: Lang, row: &SessionRow) -> String {
    let mut title = row.label();
    if title.is_empty() {
        title = panel_words::untitled(lang).to_owned();
    }
    if row.current {
        title.push_str(panel_words::current(lang));
    }
    title
}

impl App {
    /// 正文（不含标题与提示）与每个会话行的位置（正文的行号，`rows` 的下标）。
    fn projects_body(
        &self,
        page: &mut ProjectsPage,
        width: usize,
        height: usize,
        now_ms: u64,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let lang = self.lang;
        let current_root = self.view.workspace().root.as_str().to_owned();
        let all = page.rows.as_deref().unwrap_or_default();
        let mut tabs = vec![Span::raw("  ")];
        for (index, filter) in FILTERS.iter().enumerate() {
            let n = all
                .iter()
                .filter(|r| filter.is_none_or(|f| State::of(r.activity) == f))
                .count();
            let style = if index == page.filter % FILTERS.len() {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            tabs.push(Span::styled(
                format!(" {} {n} ", state_label(lang, *filter)),
                style,
            ));
            tabs.push(Span::raw(" "));
        }
        tabs.extend(super::key_hint::spans(
            panel_words::filter_hint(lang),
            &self.palette,
        ));
        let mut lines = vec![
            truncate_line(Line::from(tabs), width),
            Line::from(Span::styled("─".repeat(width), dim())),
        ];
        // 搜索行（搜索、改名或出错时才有，Codex `Search › ` / `Rename › `）。
        let accent = Style::default()
            .fg(self.palette.accent)
            .add_modifier(Modifier::BOLD);
        if let Some(error) = &page.error {
            lines.push(Line::from(Span::styled(
                format!("  {error}"),
                Style::default().fg(Color::Red),
            )));
        } else if let Some((_, input)) = &page.rename {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(words::rename_prefix(lang), accent),
                Span::raw(input.clone()),
            ]));
        } else if let Some(query) = &page.search {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(words::search_prefix(lang), accent),
                Span::raw(query.clone()),
            ]));
        }
        if page.rows.is_none() {
            lines.push(Line::from(Span::styled(
                format!("  {}", self.strings.loading),
                dim(),
            )));
            return (lines, Vec::new());
        }
        let entries = page.entries(&current_root);
        if entries.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", panel_words::no_sessions(lang)),
                dim(),
            )));
            return (lines, Vec::new());
        }
        let room = height.saturating_sub(lines.len()).max(1);
        page.height = room;
        let focused = page.focused(&current_root);
        let at = entries
            .iter()
            .position(|e| matches!(e, Entry::Session(i) if Some(*i) == focused))
            .unwrap_or(0);
        // 焦点保持可见；焦点上一行是组头时连组头一起露出来（Codex `render_center_rows`）。
        if at < page.scroll {
            page.scroll = if at > 0 && matches!(entries[at - 1], Entry::Group { .. }) {
                at - 1
            } else {
                at
            };
        }
        if at >= page.scroll + room {
            page.scroll = at + 1 - room;
        }
        page.scroll = page.scroll.min(entries.len().saturating_sub(room));
        let details = width >= DETAILS_FROM;
        let list_width = if details { width - DETAILS - 2 } else { width };
        let mut targets = Vec::new();
        let mut body: Vec<Line<'static>> = Vec::new();
        for entry in entries.iter().skip(page.scroll).take(room) {
            match *entry {
                Entry::Gap => body.push(Line::default()),
                Entry::More => body.push(Line::from(Span::styled(
                    format!("  {}", words::projects_more(lang, super::projects::LIMIT)),
                    dim(),
                ))),
                Entry::Group { row, shown, total } => {
                    let count = if shown == total {
                        total.to_string()
                    } else {
                        match lang {
                            Lang::En => format!("{shown} of {total}"),
                            Lang::Zh => format!("{shown} / {total}"),
                        }
                    };
                    let path = super::app::label(&all[row].root, self.home.as_deref());
                    let room = list_width.saturating_sub(count.width() + 4).min(GROUP_PATH);
                    body.push(Line::from(Span::styled(
                        format!("  {}  {count}", center_truncate(&path, room)),
                        dim(),
                    )));
                }
                Entry::Session(index) => {
                    let row = &all[index];
                    let chosen = Some(index) == focused;
                    let state = State::of(row.activity);
                    let (dot, dot_style) = dot(state);
                    let updated = relative(lang, now_ms, row.updated);
                    let label = state_label(lang, Some(state));
                    let status = format!(
                        "{label}{}",
                        " ".repeat(STATUS.saturating_sub(label.width()))
                    );
                    let fixed = 2 + 2 + 2 + status.width() + updated.width() + 2;
                    let title = truncate(&title(lang, row), list_width.saturating_sub(fixed));
                    let pad = list_width.saturating_sub(fixed + title.width());
                    targets.push((lines.len() + body.len(), index));
                    body.push(super::pick::paint(
                        Line::from(vec![
                            super::pick::pointer(chosen),
                            Span::styled(dot, dot_style),
                            Span::raw(" "),
                            Span::raw(title),
                            Span::raw(" ".repeat(pad + 2)),
                            Span::styled(status, self.palette.secondary()),
                            Span::styled(updated, self.palette.secondary()),
                        ]),
                        chosen,
                        &self.palette,
                        list_width,
                    ));
                }
            }
        }
        if details && let Some(row) = focused.and_then(|i| all.get(i)) {
            let side = self.project_details(row, DETAILS - 2, room);
            for (index, side_line) in side.into_iter().enumerate().take(room) {
                while body.len() <= index {
                    body.push(Line::default());
                }
                let line = &mut body[index];
                // 选中行的底色条只到列表这一栏：行本身不带底色，右侧的详情不铺。
                line.style = Style::default();
                let used = line.width();
                line.spans
                    .push(Span::raw(" ".repeat(list_width.saturating_sub(used))));
                line.spans.push(Span::styled("│ ", dim()));
                line.spans.extend(side_line.spans);
            }
        }
        lines.extend(body);
        (lines, targets)
    }

    /// 右侧详情（Codex `render_details`）：标题、状态、项目、分支、首条输入（最多 2 行）、最近一条回复。
    fn project_details(&self, row: &SessionRow, width: usize, room: usize) -> Vec<Line<'static>> {
        let lang = self.lang;
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let state = State::of(row.activity);
        let mut title = row.label();
        if title.is_empty() {
            title = panel_words::untitled(lang).to_owned();
        }
        let (dot, dot_style) = dot(state);
        let mut side = vec![
            Line::from(Span::styled(words::details(lang), bold)),
            Line::default(),
            Line::from(Span::styled(truncate(&title, width), bold)),
            Line::from(vec![
                Span::styled(dot, dot_style),
                Span::raw(" "),
                Span::raw(state_label(lang, Some(state))),
            ]),
            Line::default(),
            Line::from(Span::styled(words::project(lang), dim())),
        ];
        let path = super::app::label(&row.root, self.home.as_deref());
        side.extend(wrap_text(&path, Style::default(), width, &[], &[]));
        if let Some(branch) = &row.branch {
            side.push(Line::default());
            side.push(Line::from(Span::styled(words::branch(lang), dim())));
            side.push(Line::from(truncate(branch, width)));
        }
        if !row.prompt.trim().is_empty() {
            side.push(Line::default());
            side.push(Line::from(Span::styled(words::prompt(lang), dim())));
            let mut prompt = wrap_text(row.prompt.trim(), Style::default(), width, &[], &[]);
            if prompt.len() > 2 {
                prompt.truncate(2);
                prompt[1] = Line::from(Span::styled("…", dim()));
            }
            side.extend(prompt);
        }
        if !row.reply.is_empty() {
            side.push(Line::default());
            side.push(Line::from(Span::styled(
                panel_words::last_message(lang),
                dim(),
            )));
            let reply = wrap_text(&row.reply, Style::default(), width, &[], &[]);
            let left = room.saturating_sub(side.len());
            if reply.len() > left && left > 0 {
                side.extend(reply.into_iter().take(left - 1));
                side.push(Line::from(Span::styled("…", dim())));
            } else {
                side.extend(reply);
            }
        }
        side
    }
}

/// 画页面（占满整个屏幕，独立页面的框架），并记下每一行的点击区域。`page` 是从 `app.overlay` 里取出来的（画完由
/// 调用方放回），滚动位置与可见行数写回它。
pub fn draw(frame: &mut Frame<'_>, app: &mut App, page: &mut ProjectsPage, now_ms: u64) {
    let area = frame.area();
    let width = area.width as usize;
    let height = super::page::body_height(area.height);
    let (body, targets) = app.projects_body(page, width, height, now_ms);
    app.hits = super::mouse::Hits::default();
    for (row, index) in targets {
        let y = area.y + super::page::BODY_TOP + row as u16;
        if y < area.bottom().saturating_sub(1) {
            app.hits
                .regions
                .push((Rect::new(area.x, y, area.width, 1), Target::Session(index)));
        }
    }
    let hint = words::projects_hint(app.lang, page.search.is_some(), page.rename.is_some());
    super::page::frame(
        frame,
        &app.palette,
        words::projects_title(app.lang),
        body,
        hint,
    );
}
