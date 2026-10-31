//! `/resume` 页的排版（修复线 F29，docs/specs/rust-cli/tui-manage.md §10.2、§10.3；照 Codex `resume_picker.rs` 与
//! `resume_picker/layout.rs` 的规则，代码自己写）：标题、工具栏、搜索行、列表（Dense / Comfortable、展开的预览）、
//! footer（进度与两行按键提示），以及删除确认、目录选择与全屏对话。颜色只经 `Palette` 的记号，选中行用选择列表组件。
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::app::App;
use super::i18n::Lang;
use super::i18n_projects::{self as words, PreviewLabel as Label};
use super::layout;
use super::mouse::Target;
use super::palette::Palette;
use super::resume::{Preview, ResumePage, TranscriptState};
use super::resume_footer::{footer_hints, hint_row, progress};
use super::session_rows::{SessionRow, center_truncate, relative, relative_long, timestamp};
use super::text::{truncate, truncate_line, wrap_text};

/// 日期列的宽度（Codex `SESSION_META_DATE_WIDTH`）。
const DATE: usize = 12;
/// 预览里每条对话最多几行（tui-manage.md D-R6）。
const ITEM_LINES: usize = 3;
/// 标题、工具栏与搜索行左右各留一列；列表左右各留两列。
const CHROME_INSET: usize = 1;
const LIST_INSET: usize = 2;
/// 标题、工具栏、搜索行与列表之间的三个间隙（高度不够时依次收起）。
const GAPS: u16 = 3;
/// footer 从倒数第 4 行起：`─` 线、两行按键提示、最底下一行。
const FOOTER_HEIGHT: u16 = 4;
/// 行尾的路径：最多占宽度的三分之一，至少 16 列；与标题之间空两列。
const PATH_SHARE: usize = 3;
const MIN_PATH: usize = 16;
const PATH_GAP: usize = 2;
const PATH_ICON: &str = "⌁ ";
/// 预览左边的竖线。
const PREVIEW_BAR: &str = "  │ ";
/// 滚动的百分比：列表放得下时是 100。
const SCROLLED_ALL: u8 = 100;
/// ⌃t 的加载浮层：比文字宽 10 列，3 行高。
const LOADING_PADDING: u16 = 10;
const LOADING_ROWS: u16 = 3;

/// 纵向的区域（Codex `layout::areas`）：三个间隙在高度不够时依次收起。
struct Areas {
    header: u16,
    toolbar: u16,
    search: u16,
    list: (u16, u16),
    footer: u16,
}

fn areas(height: u16) -> Areas {
    const FIXED: u16 = 7;
    const MIN_LIST: u16 = 6;
    let gaps = height.saturating_sub(FIXED + MIN_LIST).min(GAPS);
    let header = 0;
    let toolbar = header + 1 + u16::from(gaps >= GAPS);
    let search = toolbar + 1 + u16::from(gaps >= GAPS - 1);
    let list_top = search + 1 + u16::from(gaps >= 1);
    let footer = height.saturating_sub(FOOTER_HEIGHT);
    Areas {
        header,
        toolbar,
        search,
        list: (list_top, footer.saturating_sub(list_top)),
        footer,
    }
}

fn italic(style: Style) -> Style {
    style.add_modifier(Modifier::ITALIC)
}

/// 工具栏的一个值（Codex `toolbar_value`）：焦点控件的选中值反色，选中值粗体，其余次要色。
fn value(label: &str, active: bool, focused: bool, palette: &Palette) -> Span<'static> {
    let text = format!(" {label} ");
    match (active, focused) {
        (true, true) => Span::styled(
            text,
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        ),
        (true, false) => Span::styled(text, Style::default().add_modifier(Modifier::BOLD)),
        _ => Span::styled(text, palette.secondary()),
    }
}

fn control(
    lang: Lang,
    palette: &Palette,
    sort: bool,
    page: &ResumePage,
    compact: bool,
) -> Vec<Span<'static>> {
    let focused = page.focus_sort == sort;
    let (label, chosen) = if sort {
        (words::sort_label(lang), page.created)
    } else {
        (words::filter_label(lang), page.all)
    };
    let name = |value: bool| {
        if sort {
            words::sort_value(lang, value)
        } else {
            words::filter_value(lang, value)
        }
    };
    if compact {
        return vec![
            Span::styled(label.to_owned(), palette.secondary()),
            value(name(chosen), true, focused, palette),
        ];
    }
    vec![
        Span::styled(format!("{label} "), palette.secondary()),
        value(name(false), !chosen, focused, palette),
        value(name(true), chosen, focused, palette),
    ]
}

/// 工具栏（Codex `toolbar_for_width`）：放不下时每个控件只写选中值，再放不下只画焦点所在的控件。
fn toolbar(lang: Lang, palette: &Palette, page: &ResumePage, width: usize) -> Line<'static> {
    for compact in [false, true] {
        let mut spans = control(lang, palette, false, page, compact);
        spans.push(Span::styled(
            if compact { " " } else { "   " },
            palette.secondary(),
        ));
        spans.extend(control(lang, palette, true, page, compact));
        let line = Line::from(spans);
        if line.width() <= width {
            return line;
        }
    }
    Line::from(control(lang, palette, page.focus_sort, page, true))
}

impl App {
    fn session_title(&self, row: &SessionRow) -> String {
        let label = row.label();
        if label.is_empty() {
            words::no_message(self.lang).to_owned()
        } else {
            label
        }
    }

    /// 一个会话占的行（不含展开的预览）。
    fn resume_row(
        &self,
        page: &ResumePage,
        row: &SessionRow,
        chosen: bool,
        width: usize,
        now_ms: u64,
    ) -> Vec<Line<'static>> {
        let lang = self.lang;
        let palette = &self.palette;
        let expanded = chosen && page.expanded.as_deref() == Some(row.id.as_str());
        let marker = match (chosen, expanded) {
            (true, true) => Span::raw("⌄ "),
            (true, false) => super::pick::pointer(true),
            _ => super::pick::pointer(false),
        };
        let at = if page.created {
            row.created
        } else {
            row.updated
        };
        let date = truncate(&relative(lang, now_ms, at), DATE - 1);
        let date = format!("{date}{}", " ".repeat(DATE.saturating_sub(date.width())));
        let path = super::app::label(&row.root, self.home.as_deref());
        if !page.comfortable {
            let mut spans = vec![marker, Span::styled(date, palette.secondary())];
            let mut room = width.saturating_sub(super::pick::POINTER.width() + DATE);
            let mut suffix = None;
            // Filter=All 时行尾写路径（用户决定，tui-manage.md D-R8）。
            if page.all {
                let shown = center_truncate(&path, (width / PATH_SHARE).max(MIN_PATH));
                room = room.saturating_sub(PATH_GAP + PATH_ICON.width() + shown.width());
                suffix = Some(shown);
            }
            let title = truncate(&self.session_title(row), room);
            let pad = room.saturating_sub(title.width());
            spans.push(Span::raw(title));
            if let Some(suffix) = suffix {
                spans.push(Span::raw(" ".repeat(pad + PATH_GAP)));
                spans.push(Span::styled(
                    format!("{PATH_ICON}{suffix}"),
                    palette.secondary(),
                ));
            }
            return vec![super::pick::paint(
                Line::from(spans),
                chosen,
                palette,
                width,
            )];
        }
        let title = truncate(
            &self.session_title(row),
            width.saturating_sub(super::pick::POINTER.width()),
        );
        let first = Line::from(vec![marker, Span::raw(title)]);
        let mut meta = vec![
            Span::raw("  "),
            Span::styled(date, palette.secondary()),
            Span::raw("  "),
        ];
        if page.all {
            let shown = center_truncate(&path, (width / PATH_SHARE).max(MIN_PATH));
            meta.push(Span::styled(
                format!("{PATH_ICON}{shown}  "),
                palette.secondary(),
            ));
        }
        match &row.branch {
            Some(branch) => meta.push(Span::styled(format!("⎇ {branch}"), palette.secondary())),
            None => meta.push(Span::styled(
                format!("⎇ {}", words::no_branch(lang)),
                italic(palette.secondary()),
            )),
        }
        vec![
            super::pick::paint(first, chosen, palette, width),
            super::pick::paint(
                truncate_line(Line::from(meta), width),
                chosen,
                palette,
                width,
            ),
        ]
    }

    /// 展开的预览（Codex `render_transcript_preview_lines`）。
    fn resume_preview(
        &self,
        page: &ResumePage,
        row: &SessionRow,
        width: usize,
        now_ms: u64,
    ) -> Vec<Line<'static>> {
        let lang = self.lang;
        let secondary = self.palette.secondary();
        let bar = || Span::styled(PREVIEW_BAR, secondary);
        let detail = |label: &str, value: String| {
            let label = format!("{label:<10}  ");
            let room = width
                .saturating_sub(PREVIEW_BAR.width() + label.width())
                .max(1);
            Line::from(vec![
                bar(),
                Span::styled(label, secondary),
                Span::raw(truncate(&value, room)),
            ])
        };
        let session = match row.title.is_empty() {
            true => row.id.clone(),
            false => format!("{} ({})", row.title, row.id),
        };
        let time = |at: u64| {
            format!(
                "{} · {}",
                relative_long(lang, now_ms, at),
                timestamp(at, page.offset_minutes)
            )
        };
        let branch = format!(
            "⎇ {}",
            row.branch.as_deref().unwrap_or(words::no_branch(lang))
        );
        let mut lines = vec![
            detail(words::preview_label(lang, Label::Session), session),
            detail(
                words::preview_label(lang, Label::Created),
                time(row.created),
            ),
            detail(
                words::preview_label(lang, Label::Updated),
                time(row.updated),
            ),
            detail(
                words::preview_label(lang, Label::Directory),
                super::app::label(&row.root, self.home.as_deref()),
            ),
            detail(words::preview_label(lang, Label::Branch), branch),
            Line::from(Span::styled("  │", secondary)),
            Line::from(vec![
                bar(),
                Span::styled(words::preview_label(lang, Label::Conversation), secondary),
            ]),
        ];
        match page.previews.get(&row.id) {
            Some(Preview::Loading) | None => lines.push(Line::from(vec![
                bar(),
                Span::styled(words::preview_loading(lang), italic(secondary)),
            ])),
            Some(Preview::Failed) => lines.push(Line::from(vec![
                bar(),
                Span::styled(
                    words::preview_failed(lang),
                    italic(Style::default().fg(Color::Red)),
                ),
            ])),
            Some(Preview::Loaded(items)) if items.is_empty() => lines.push(Line::from(vec![
                Span::styled("  └ ", secondary),
                Span::styled(words::preview_empty(lang), italic(secondary)),
            ])),
            Some(Preview::Loaded(items)) => {
                let mut content: Vec<Line<'static>> = Vec::new();
                for (user, text) in items {
                    let style = if *user {
                        italic(Style::default())
                    } else {
                        secondary
                    };
                    let room = width.saturating_sub(PREVIEW_BAR.width()).max(1);
                    let mut wrapped = wrap_text(text.trim(), style, room, &[], &[]);
                    if wrapped.len() > ITEM_LINES {
                        wrapped.truncate(ITEM_LINES);
                        wrapped[ITEM_LINES - 1]
                            .spans
                            .push(Span::styled("…", secondary));
                    }
                    content.extend(wrapped);
                }
                let last = content.len().saturating_sub(1);
                for (index, line) in content.into_iter().enumerate() {
                    let prefix = if index == last { "  └ " } else { PREVIEW_BAR };
                    let mut spans = vec![Span::styled(prefix, secondary)];
                    spans.extend(line.spans);
                    lines.push(Line::from(spans));
                }
            }
        }
        lines
    }
}

/// 画 `/resume` 页。`page` 是从 `app.overlay` 里取出来的（画完由调用方放回），滚动位置与列表高度写回它。
pub fn draw(frame: &mut Frame<'_>, app: &mut App, page: &mut ResumePage, now_ms: u64) {
    let area = frame.area();
    app.hits = super::mouse::Hits::default();
    if let Some(TranscriptState::Shown(shown)) = page.transcript.as_mut() {
        super::resume_transcript::draw(frame, app, shown, now_ms);
        return;
    }
    if page.cwd.is_some() {
        draw_cwd(frame, app, page);
        return;
    }
    let lang = app.lang;
    let width = area.width as usize;
    let layout = areas(area.height);
    let inset = CHROME_INSET as u16;
    let chrome = |y: u16| {
        Rect::new(
            area.x + inset,
            area.y + y,
            area.width.saturating_sub(2 * inset),
            1,
        )
    };
    let bold = Style::default().add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(words::resume_title(lang), bold))),
        chrome(layout.header),
    );
    frame.render_widget(
        Paragraph::new(toolbar(
            lang,
            &app.palette,
            page,
            width.saturating_sub(2 * CHROME_INSET),
        )),
        chrome(layout.toolbar),
    );
    let search = if let Some(error) = &page.error {
        Line::from(Span::styled(error.clone(), Style::default().fg(Color::Red)))
    } else if let Some((_, input)) = &page.rename {
        Line::from(vec![
            Span::styled(
                words::rename_prefix(lang),
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(input.clone()),
        ])
    } else if page.query.is_empty() {
        Line::from(Span::styled(
            words::type_to_search(lang),
            app.palette.secondary(),
        ))
    } else {
        Line::from(words::search_query(lang, &page.query))
    };
    frame.render_widget(
        Paragraph::new(truncate_line(
            search,
            width.saturating_sub(2 * CHROME_INSET),
        )),
        chrome(layout.search),
    );
    let list_inset = LIST_INSET as u16;
    let list_area = Rect::new(
        area.x + list_inset,
        area.y + layout.list.0,
        area.width.saturating_sub(2 * list_inset),
        layout.list.1,
    );
    let percent = draw_list(frame, app, page, list_area, now_ms);
    // footer：`─` 线与右侧的进度，下面两行按键提示。
    let footer = area.y + layout.footer;
    let line = "─".repeat(width);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            line,
            Style::default().add_modifier(Modifier::DIM),
        ))),
        Rect::new(area.x, footer, area.width, 1),
    );
    let label = progress(page, percent, width);
    let label_width = label.width() as u16;
    if label_width > 0 && label_width < area.width {
        frame.render_widget(
            Paragraph::new(Span::styled(label, app.palette.secondary())),
            Rect::new(
                area.x + area.width - label_width - 1,
                footer,
                label_width,
                1,
            ),
        );
    }
    let rows: Vec<Line<'static>> = if page.confirm.is_some() {
        vec![
            super::key_hint::line(1, words::select_hint(lang), &app.palette),
            Line::default(),
        ]
    } else {
        footer_hints(lang, page)
            .iter()
            .map(|hints| hint_row(hints, width, &app.palette))
            .collect()
    };
    for (index, line) in rows.into_iter().enumerate() {
        let y = footer + 1 + index as u16;
        if y < area.bottom() {
            frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        }
    }
    if page.confirm.is_some() {
        draw_confirm(frame, app, page, list_area);
    }
    if matches!(page.transcript, Some(TranscriptState::Loading(_))) {
        draw_loading(frame, app, list_area);
    }
}

/// 列表；返回滚动的百分比（Codex `picker_footer_scroll_percent`）。
fn draw_list(
    frame: &mut Frame<'_>,
    app: &mut App,
    page: &mut ResumePage,
    area: Rect,
    now_ms: u64,
) -> u8 {
    let lang = app.lang;
    let width = area.width as usize;
    let height = area.height as usize;
    let secondary = app.palette.secondary();
    let filtered: Vec<SessionRow> = page.filtered().into_iter().cloned().collect();
    if filtered.is_empty() {
        let text = if !page.query.is_empty() {
            words::no_results(lang)
        } else if page.rows.is_none() {
            words::loading_sessions(lang)
        } else {
            words::no_sessions_yet(lang)
        };
        frame.render_widget(Paragraph::new(Span::styled(text, italic(secondary))), area);
        return SCROLLED_ALL;
    }
    // 每个会话的行（含展开的预览），Comfortable 时会话之间空一行。
    let blocks: Vec<Vec<Line<'static>>> = filtered
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let chosen = index == page.selected;
            let mut lines = app.resume_row(page, row, chosen, width, now_ms);
            if chosen && page.expanded.as_deref() == Some(row.id.as_str()) {
                lines.extend(app.resume_preview(page, row, width, now_ms));
            }
            if page.comfortable && index + 1 < filtered.len() {
                lines.push(Line::default());
            }
            lines
        })
        .collect();
    let heights: Vec<usize> = blocks.iter().map(Vec::len).collect();
    let sum = |range: std::ops::Range<usize>| heights[range].iter().sum::<usize>();
    let total = sum(0..heights.len());
    // 焦点保持可见：上面露出 `↑ more` 时少 1 行，下面还有时再留 1 行给 `↓ more`。
    let room = |scroll: usize| {
        let above = usize::from(scroll > 0);
        let below = usize::from(sum(scroll..heights.len()) > height.saturating_sub(above));
        height.saturating_sub(above + below).max(1)
    };
    page.selected = page.selected.min(filtered.len() - 1);
    if page.selected < page.scroll {
        page.scroll = page.selected;
    }
    while page.scroll < page.selected && sum(page.scroll..page.selected + 1) > room(page.scroll) {
        page.scroll += 1;
    }
    let scroll = page.scroll;
    let content = room(scroll);
    page.height = content;
    let mut y = area.y;
    if scroll > 0 {
        let more = Style::default().fg(app.palette.accent);
        frame.render_widget(
            Paragraph::new(Span::styled(words::more_above(lang), more)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }
    let bottom = area.y + content as u16 + u16::from(scroll > 0);
    let mut shown_last = scroll;
    'blocks: for (index, block) in blocks.iter().enumerate().skip(scroll) {
        for (offset, line) in block.iter().enumerate() {
            if y >= bottom {
                break 'blocks;
            }
            if offset == 0 {
                app.hits
                    .regions
                    .push((Rect::new(area.x, y, area.width, 1), Target::Session(index)));
            }
            frame.render_widget(
                Paragraph::new(line.clone()),
                Rect::new(area.x, y, area.width, 1),
            );
            y += 1;
        }
        shown_last = index;
    }
    if shown_last + 1 < blocks.len() || sum(scroll..heights.len()) > content {
        let more = Style::default().fg(app.palette.accent);
        frame.render_widget(
            Paragraph::new(Span::styled(words::more_below(lang), more)),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    } else if page.more && y < area.bottom() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("  {}", words::resume_more(lang, super::projects::LIMIT)),
                Style::default().add_modifier(Modifier::DIM),
            )),
            Rect::new(area.x, y, area.width, 1),
        );
    }
    let max_scroll = total.saturating_sub(height);
    if max_scroll == 0 || sum(scroll..heights.len()) <= height {
        return SCROLLED_ALL;
    }
    let skipped = sum(0..scroll).min(max_scroll);
    ((skipped as f64 / max_scroll as f64) * super::quantities::PERCENT_F64).round() as u8
}

/// 删除确认（列表区域的下方，文字照 Codex 命令中心）。
fn draw_confirm(frame: &mut Frame<'_>, app: &App, page: &ResumePage, list: Rect) {
    let Some(confirm) = &page.confirm else {
        return;
    };
    let lang = app.lang;
    let width = list.width as usize;
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut lines = vec![
        truncate_line(
            Line::from(Span::styled(words::delete_title(lang, &confirm.name), bold)),
            width,
        ),
        Line::from(Span::styled(
            words::delete_note(lang),
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    for (index, delete) in [false, true].into_iter().enumerate() {
        let chosen = confirm.focus == index;
        let line = Line::from(vec![
            super::pick::pointer(chosen),
            Span::raw(words::delete_choice(lang, delete)),
        ]);
        lines.push(super::pick::paint(line, chosen, &app.palette, width));
    }
    let height = (lines.len() as u16).min(list.height);
    let box_area = Rect::new(
        list.x,
        list.bottom().saturating_sub(height),
        list.width,
        height,
    );
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(lines), box_area);
}

/// ⌃t 的加载浮层（Codex `render_transcript_loading_overlay`）：列表中间 3 行，底色用用户消息块的底色。
fn draw_loading(frame: &mut Frame<'_>, app: &App, list: Rect) {
    let message = words::loading_transcript(app.lang);
    let message_width = message.width() as u16;
    let width = (message_width + LOADING_PADDING).min(list.width);
    let height = LOADING_ROWS.min(list.height);
    let overlay = Rect::new(
        list.x + list.width.saturating_sub(width) / 2,
        list.y + list.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let fill = app.palette.user_block();
    let blank = " ".repeat(width as usize);
    let mut lines: Vec<Line<'static>> = (0..height)
        .map(|_| Line::from(Span::styled(blank.clone(), fill)))
        .collect();
    let pad = (width.saturating_sub(message_width) / 2) as usize;
    let middle = (height / 2) as usize;
    lines[middle] = Line::from(vec![
        Span::styled(" ".repeat(pad), fill),
        Span::styled(message.to_owned(), fill.add_modifier(Modifier::BOLD)),
        Span::styled(
            " ".repeat((width as usize).saturating_sub(pad + message_width as usize)),
            fill,
        ),
    ]);
    frame.render_widget(Clear, overlay);
    frame.render_widget(Paragraph::new(lines), overlay);
}

/// 目录选择（Codex `cwd_prompt.rs`）：标题、两行说明、两个选项、按键提示，纵向居中。
fn draw_cwd(frame: &mut Frame<'_>, app: &App, page: &ResumePage) {
    let Some(prompt) = &page.cwd else {
        return;
    };
    let area = frame.area();
    let lang = app.lang;
    let width = area.width as usize;
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let session = super::app::label(&prompt.row.root, app.home.as_deref());
    let current = super::app::label(app.view.workspace().root.as_str(), app.home.as_deref());
    let mut lines = vec![Line::from(Span::styled(
        format!("  {}", words::cwd_title(lang)),
        bold,
    ))];
    for note in words::cwd_note(lang) {
        lines.push(Line::from(Span::styled(format!("  {note}"), dim)));
    }
    lines.push(Line::default());
    for (index, (current_dir, path)) in [(false, &session), (true, &current)]
        .into_iter()
        .enumerate()
    {
        let chosen = prompt.focus == index;
        let line = Line::from(vec![
            Span::raw("  "),
            super::pick::pointer(chosen),
            Span::raw(format!(
                "{}. {}",
                index + 1,
                words::cwd_choice(lang, current_dir, path)
            )),
        ]);
        lines.push(super::pick::paint(
            truncate_line(line, width),
            chosen,
            &app.palette,
            width,
        ));
    }
    lines.push(Line::default());
    lines.push(super::key_hint::line(
        layout::INDENT,
        words::cwd_hint(lang),
        &app.palette,
    ));
    let top = (area.height as usize).saturating_sub(lines.len()) / 2;
    let mut out = vec![Line::default(); top];
    out.extend(lines);
    frame.render_widget(Paragraph::new(out), area);
}

#[cfg(test)]
#[path = "resume_draw_tests.rs"]
mod tests;
