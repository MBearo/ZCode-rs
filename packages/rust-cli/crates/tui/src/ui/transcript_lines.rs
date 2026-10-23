// 改写自 apps/zcode-host/crates/tui/src/ui/transcript.rs（feat/rs 79b6691c55）的组装部分：行来自
// `view::Transcript`，去掉子代理视图与 V4 的 `lastError`（失败原因已是视图里的错误行）。
//! transcript 的全部行：会话头、cell、本地 cell；排版结果按块缓存。
use std::collections::HashMap;
use std::rc::Rc;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{HEADER_KEY, Layout, Prompt, Rendered, local_key};
use crate::ui::app::{App, Local, LocalCell};
use crate::ui::cells::{self, Ctx, Kind};
use crate::ui::text::truncate_line;
use crate::view::Body;

/// 缓存里的一个块：命中条件（行版本、宽度、单独展开、明暗、类型、工作段的推理用时）与排好的行。
struct Entry {
    versions: Vec<(u64, u64)>,
    /// 流式中的回复：排版时显示的文字（它没变就能用，行版本每个增量都变，不看）。
    shown: Option<String>,
    width: usize,
    expanded: bool,
    light: bool,
    kind: Kind,
    thought_ms: u64,
    rendered: Rc<Rendered>,
    /// 最后一次用到它的帧；每帧结束时清掉这一帧没用到的块。
    frame: u64,
}

/// 排版缓存，按块（第一个行 id）查找。普通视图与详细视图各一份，切换不互相淘汰。
#[derive(Default)]
pub struct Cache {
    normal: HashMap<u64, Entry>,
    detail: HashMap<u64, Entry>,
    frame: u64,
    /// 流式中的回复按块缓存的 markdown 排版（每帧都要重排的块用它，修复线 F6）。
    markdown: crate::ui::markdown::Memo,
}

impl Cache {
    /// 视图换了（`/clear`、重新取快照）：行 id 从头分配，旧的块全部作废。
    pub fn clear(&mut self) {
        self.normal.clear();
        self.detail.clear();
        self.markdown = crate::ui::markdown::Memo::default();
    }
}

/// 本地 cell 的行（transcript 与独立页面共用）。
pub fn local_lines(local: &Local, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let dim = cells::dim();
    match local {
        Local::Info(text) => cells::detail_text(text, dim, ctx),
        Local::Error(text) => cells::error_lines(text, ctx),
        Local::Context { usage, all } => crate::ui::context_page::page_lines(usage, *all, ctx),
        Local::Goal(panel) => crate::ui::goal::panel_lines(panel, ctx),
        Local::Update(info) => {
            crate::ui::update_notice::lines(info, ctx.palette, ctx.lang, ctx.width)
        }
        Local::Status(rows) => {
            let label_width = rows
                .iter()
                .map(|(k, _)| unicode_width::UnicodeWidthStr::width(k.as_str()))
                .max()
                .unwrap_or(0);
            let mut lines = vec![Line::from(vec![
                Span::styled("ZCode", cells::bold()),
                Span::styled(format!(" v{}", ctx.version), dim),
            ])];
            lines.push(Line::default());
            for (key, value) in rows {
                let pad =
                    label_width.saturating_sub(unicode_width::UnicodeWidthStr::width(key.as_str()));
                // 缺陷修复（F25）：值原来按一行截断，长目录与会话 ID 被截掉、复制不到。按 Status 页的写法折行，续行缩进
                // 到值所在的列（tui-select.md §1）。
                let label = format!("  {key}:{} ", " ".repeat(pad));
                let indent = " ".repeat(unicode_width::UnicodeWidthStr::width(label.as_str()));
                lines.extend(crate::ui::text::wrap_text(
                    value,
                    Style::default(),
                    ctx.width,
                    &[Span::styled(label, dim)],
                    &[Span::raw(indent)],
                ));
            }
            lines
        }
        Local::Text { title, body } => {
            let mut lines = vec![Line::from(Span::styled(title.clone(), cells::bold()))];
            for line in body.lines() {
                lines.push(truncate_line(
                    Line::from(Span::raw(format!("  {line}"))),
                    ctx.width,
                ));
            }
            lines
        }
        Local::Table { title, rows, empty } => {
            let mut lines = vec![Line::from(Span::styled(title.clone(), cells::bold()))];
            if rows.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(empty.clone(), dim),
                ]));
            }
            let name_width = rows
                .iter()
                .map(|(n, _, _)| unicode_width::UnicodeWidthStr::width(n.as_str()))
                .max()
                .unwrap_or(0);
            for (name, status, extra) in rows {
                let pad =
                    name_width.saturating_sub(unicode_width::UnicodeWidthStr::width(name.as_str()));
                lines.push(truncate_line(
                    Line::from(vec![
                        Span::styled("  ", dim),
                        Span::raw(format!("{name}{}  ", " ".repeat(pad))),
                        Span::styled(status.clone(), Style::default().fg(ctx.palette.accent)),
                        Span::styled(format!("  {extra}"), dim),
                    ]),
                    ctx.width,
                ));
            }
            lines
        }
    }
}

/// 一个块的行：普通视图里点击展开的块铺悬停底色并多 1 行空行；详细视图是详细形式。
fn render(cell: &cells::Cell<'_>, ctx: &Ctx<'_>, opened: bool) -> Rc<Rendered> {
    if ctx.expanded {
        return Rendered::new(cells::render(cell, ctx));
    }
    let (mut lines, expandable) = cells::lay_out(cell, ctx, opened);
    if opened && expandable {
        // 自带底色的行（diff）保留它的底色。
        lines.push(Line::default());
        if let Some(bg) = ctx.palette.hover_bg {
            for line in &mut lines {
                if line.style.bg.is_none() {
                    line.style = line.style.bg(bg);
                }
            }
        }
    }
    Rendered::cell(lines, expandable)
}

/// transcript 的全部行。
pub fn lines(app: &App, cache: &mut Cache, ctx: &Ctx<'_>) -> Layout {
    let mut out = Layout::default();
    // 只读查看子代理：顶部是 `↳ 类型 · 描述`，行来自子会话的视图，没有本地 cell（tui-panels.md §1.4）。
    if let Some(header) = app.viewing_header() {
        out.push(HEADER_KEY, Rendered::new(header));
    } else {
        let model = app.model_label().unwrap_or_default();
        out.push(
            HEADER_KEY,
            Rendered::new(cells::header(app.view.mode(), &model, ctx)),
        );
    }
    let rows = app
        .viewing
        .as_ref()
        .map_or_else(|| app.view.rows(), |viewing| viewing.view.rows());
    let no_locals: &[LocalCell] = &[];
    let locals_of = if app.viewing.is_some() {
        no_locals
    } else {
        &app.local
    };
    let local_index = |local: &LocalCell| {
        app.local
            .iter()
            .position(|l| std::ptr::eq(l, local))
            .unwrap_or(0)
    };
    let push_local = |out: &mut Layout, local: &LocalCell| {
        out.push(
            local_key(local_index(local)),
            Rendered::new(local_lines(&local.local, ctx)),
        );
    };
    let mut locals = locals_of.iter().peekable();
    // 锚点为空的本地 cell 在会话头之后。
    while let Some(local) = locals.next_if(|l| l.anchor.is_none()) {
        push_local(&mut out, local);
    }
    cache.frame += 1;
    let frame = cache.frame;
    let entries = if ctx.expanded {
        &mut cache.detail
    } else {
        &mut cache.normal
    };
    // 流式中的块每帧都重排：回复的 markdown 经按块的缓存，只重排变了的块。
    let live_ctx = Ctx {
        markdown: Some(&cache.markdown),
        ..*ctx
    };
    // 普通视图并工作段，详细视图不并。
    let built = cells::build(rows, !ctx.expanded);
    let mut pending_locals: Vec<&LocalCell> = locals.collect();
    for cell in &built {
        let versions = || cell.rows.iter().map(|row| (row.id, row.version));
        let cell_key = cell.ids().next().unwrap_or(0);
        // 点击只展开这一个块；详细视图本来就是详细形式，不看单独展开。
        let opened = !ctx.expanded && app.expanded_cells.contains(&cell_key);
        // 流式中的回复每个增量都改版本，但显示只到最后一个换行：显示的文字没变就用上一帧排好的行（修复线 F6，审查
        // R5-5；以前每个增量都整篇重排）。
        let shown = cells::streaming_reply(cell);
        let rendered = if cell.live() && shown.is_none() {
            render(cell, &live_ctx, opened)
        } else {
            let hit = entries.get_mut(&cell_key).filter(|e| {
                e.width == ctx.width
                    && e.expanded == opened
                    && e.light == ctx.palette.light
                    && e.kind == cell.kind
                    && e.thought_ms == cell.thought_ms
                    && match shown {
                        Some(shown) => e.shown.as_deref() == Some(shown),
                        None => e.shown.is_none() && versions().eq(e.versions.iter().copied()),
                    }
            });
            match hit {
                Some(entry) => {
                    entry.frame = frame;
                    entry.rendered.clone()
                }
                None => {
                    let rendered =
                        render(cell, if shown.is_some() { &live_ctx } else { ctx }, opened);
                    entries.insert(
                        cell_key,
                        Entry {
                            versions: versions().collect(),
                            shown: shown.map(str::to_owned),
                            width: ctx.width,
                            expanded: opened,
                            light: ctx.palette.light,
                            kind: cell.kind,
                            thought_ms: cell.thought_ms,
                            rendered: rendered.clone(),
                            frame,
                        },
                    );
                    rendered
                }
            }
        };
        let start = out.push(cell_key, rendered);
        if cell.kind == Kind::User
            && let Some(start) = start
            && let Body::User { text } = &cell.rows[0].body
        {
            let end = out.len();
            out.prompts.push(Prompt {
                start,
                end,
                row_id: cell_key,
                text: text.clone(),
            });
        }
        // 锚定在这个 cell 最后一行的本地 cell 紧随其后。
        let last = cell.ids().last();
        let (here, rest): (Vec<&LocalCell>, Vec<&LocalCell>) = pending_locals
            .into_iter()
            .partition(|l| l.anchor.is_some() && l.anchor == last);
        for local in here {
            push_local(&mut out, local);
        }
        pending_locals = rest;
    }
    for local in pending_locals {
        push_local(&mut out, local);
    }
    entries.retain(|_, entry| entry.frame == frame);
    out
}

/// 固定在第一行的提问：最近一条已完全滚出顶部的用户消息，折成一行（`❯ ` 加文字，用户消息底色）。
pub fn pinned(prompts: &[Prompt], top: usize, width: usize, block: Style) -> Option<Line<'static>> {
    let prompt = prompts.iter().rev().find(|p| p.start < top)?;
    if prompt.end > top {
        return None;
    }
    let text = prompt.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = Line::from(vec![Span::styled("❯ ", cells::dim()), Span::raw(text)]);
    Some(truncate_line(line, width.saturating_sub(1)).style(block))
}
