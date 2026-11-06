//! 设置页的画法（docs/puffin-tui.md §4.2「设置」）：页的行、选中行下面展开的、`↑ n more` 的窗口与整个 body（头一行、
//! 13 列的目录与从第 16 列起的页；窄于 76 列时没有目录，页从第 3 列起）。入参是中性的 [`Row`] 与 [`PageCtx`]。
//! 移植自 Puffin 的 shell/settings/view.rs（SettingsView.scala 第 1–339 行）；壳的 `SettingsUi` 的状态由
//! 壳映射成 [`Mode`]、`sign_in`、`test` 与 `age`。
use std::collections::BTreeMap;

use crate::app::widgets::fade;
use crate::app::widgets::fit::{clip, pad};
use crate::render::width::string_width;
use crate::render::{Span, Style, StyledLine, attr};
use crate::theme::theme::Theme;

use super::{
    Applies, Ink, LABEL_MAX, NAME_MAX, Part, Row, SECTIONS, chosen_style, clip_spans, part_style,
    spans, take_right_cells, wrap,
};
use super::{INDEX_WIDTH, NARROW};

/// 动作行旁边一列开始的地方（名字的宽度至少到这里）。
const ASIDE_AT: usize = 13;

/// 页在做的，按行（在 `rows` 里的下标）：选项在它下面打开，或在问删不删。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode<'a> {
    Browse,
    /// 行 `row` 的选项打开，`at` 是选中的选项。
    Choosing {
        row: usize,
        at: usize,
    },
    /// 在行 `row` 下面问：0 是动作 `label`，1 是 `Keep it`；`note` 说它意味着什么。
    Confirming {
        row: usize,
        at: usize,
        label: &'a str,
        note: &'a str,
    },
}

/// 页画的时候要的：主题、编辑器的名字、页在做的、登录与测试，与每一行的值变了多久（按下标；-1 是没变过）。
pub struct PageCtx<'a> {
    pub theme: &'a Theme,
    pub editor: &'a str,
    pub mode: Mode<'a>,
    /// 在浏览器里等的登录的地址（可以是 ""）；没有登录时 `None`。
    pub sign_in: Option<&'a str>,
    /// 在测的行（下标）与结果：毫秒或原因；还在测时为 `None`。
    pub test: Option<(usize, Option<Result<f64, &'a str>>)>,
    pub age: &'a dyn Fn(usize) -> i64,
}

/// 一行的值。
pub fn value_parts(row: &Row<'_>) -> Vec<Part> {
    match row {
        Row::Switch { on, .. } => vec![if *on {
            Part::new("on", Ink::Fg)
        } else {
            Part::new("off", Ink::Fg3)
        }],
        Row::Choice {
            options,
            current,
            default,
            ..
        } => {
            let label = match options.iter().find(|o| o.value == *current) {
                Some(o) => o.label.clone(),
                None if current.is_empty() => {
                    options.first().map(|o| o.label.clone()).unwrap_or_default()
                }
                None => (*current).to_owned(),
            };
            let mut out = vec![Part::new(label, Ink::Fg)];
            if !default.is_empty() && current == default {
                out.push(Part::new("  default", Ink::Fg3));
            }
            out
        }
        Row::Text(t) => {
            if t.value.is_empty() {
                let mut out = vec![Part::italic(t.empty, Ink::Fg3)];
                if !t.note.is_empty() {
                    out.push(Part::new(format!("  {}", t.note), Ink::Fg3));
                }
                out
            } else if t.secret {
                let tail = take_right_cells(t.value, 4);
                vec![Part::new(format!("{}{tail}", "•".repeat(8)), Ink::Fg2)]
            } else {
                let mut out = vec![Part::new(t.value, Ink::Fg)];
                if !t.unit.is_empty() {
                    out.push(Part::new(format!(" {}", t.unit), Ink::Fg3));
                }
                if !t.note.is_empty() {
                    out.push(Part::new(format!("  {}", t.note), Ink::Fg3));
                }
                out
            }
        }
        Row::Picker { value, .. } => match value {
            None => vec![Part::italic("not set", Ink::Fg3)],
            Some((model, effort)) => {
                let mut out = vec![Part::new(*model, Ink::Fg)];
                if !effort.is_empty() {
                    out.push(Part::new(" · ", Ink::Line));
                    out.push(Part::new(*effort, Ink::Fg3));
                }
                out
            }
        },
        Row::Info { value, .. } => value.to_vec(),
        Row::Editor { path, .. } => vec![Part::new(super::file_name(path), Ink::Fg2)],
        _ => Vec::new(),
    }
}

fn label<'a>(row: &Row<'a>) -> &'a str {
    match row {
        Row::Switch { label, .. }
        | Row::Choice { label, .. }
        | Row::Picker { label, .. }
        | Row::Info { label, .. }
        | Row::Editor { label, .. } => label,
        Row::Text(t) => t.label,
        _ => "",
    }
}

fn is_setting(row: &Row<'_>) -> bool {
    matches!(
        row,
        Row::Switch { .. }
            | Row::Choice { .. }
            | Row::Text(_)
            | Row::Picker { .. }
            | Row::Info { .. }
            | Row::Editor { .. }
    )
}

fn applies(row: &Row<'_>) -> Option<Applies> {
    match row {
        Row::Switch { applies, .. } | Row::Choice { applies, .. } => Some(*applies),
        Row::Text(t) => Some(t.applies),
        _ => None,
    }
}

fn desc<'a>(row: &Row<'a>) -> &'a str {
    match row {
        Row::Switch { desc, .. }
        | Row::Choice { desc, .. }
        | Row::Picker { desc, .. }
        | Row::Editor { desc, .. } => desc,
        Row::Action(a) => a.desc,
        Row::Text(t) => t.desc,
        Row::Item(it) => it.desc,
        _ => "",
    }
}

fn line(spans: Vec<Span>) -> StyledLine {
    StyledLine::new(spans.into_iter().filter(|s| !s.text.is_empty()).collect())
}

/// 页的行在宽 `w` 下的样子，选中的可选行 `chosen`（第几个可选行）在它下面说明自己；另给出选中行的第一行与最后一行，
/// 窗口好让它都看得见。
pub fn page_lines(
    rows: &[Row<'_>],
    chosen: Option<usize>,
    w: usize,
    ctx: &PageCtx<'_>,
) -> (Vec<StyledLine>, Option<usize>, Option<usize>) {
    let theme = ctx.theme;
    let lw = rows
        .iter()
        .filter(|r| is_setting(r))
        .map(|r| string_width(label(r)))
        .max()
        .unwrap_or(0)
        .min(LABEL_MAX)
        + 3;
    let items: Vec<_> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Item(it) => Some(it),
            _ => None,
        })
        .collect();
    let nw = items
        .iter()
        .map(|it| string_width(it.name))
        .max()
        .unwrap_or(0)
        .min(NAME_MAX)
        + 3;
    let sw = if items.iter().any(|it| !it.scope.is_empty()) {
        items
            .iter()
            .map(|it| string_width(it.scope))
            .max()
            .unwrap_or(0)
            + 3
    } else {
        0
    };
    let mut out: Vec<StyledLine> = Vec::new();
    let mut sel: Option<usize> = None;
    let mut chosen_line = None;
    for (at, r) in rows.iter().enumerate() {
        match r {
            Row::Heading(text) => {
                if !out.is_empty() {
                    out.push(StyledLine::EMPTY);
                }
                out.push(line(vec![Span::new(
                    (*text).to_owned(),
                    theme.fg2.with_attr(attr::BOLD),
                )]));
            }
            Row::Note(text) => {
                for t in wrap(text, w) {
                    out.push(line(vec![Span::new(t, theme.fg3.with_attr(attr::ITALIC))]));
                }
            }
            _ => {
                if r.selectable() {
                    sel = Some(sel.map_or(0, |s| s + 1));
                }
                let is_chosen = r.selectable() && sel.is_some() && sel == chosen;
                if is_chosen {
                    chosen_line = Some(out.len());
                }
                let name_ink = if is_chosen {
                    chosen_style(theme)
                } else {
                    theme.fg2
                };
                let age = (ctx.age)(at);
                let segs: Vec<Span> = match r {
                    Row::Item(it) => {
                        let off = it.toggle == Some(false);
                        let boxed = match it.toggle {
                            None => Span::new(" ", Style::DEFAULT),
                            Some(on) => {
                                let target = if on {
                                    Style::fg(theme.brand)
                                } else {
                                    theme.fg3
                                };
                                let mark = Span::new(if on { "■" } else { "□" }, target);
                                fade::fade_in(theme, vec![mark], age, fade::TICKS, None)
                                    .into_iter()
                                    .next()
                                    .unwrap_or_else(|| Span::new(" ", Style::DEFAULT))
                            }
                        };
                        let name_style = if is_chosen {
                            chosen_style(theme)
                        } else if off {
                            theme.fg3
                        } else {
                            theme.fg2
                        };
                        let mut segs = vec![
                            boxed,
                            Span::new(" ", Style::DEFAULT),
                            Span::new(pad(&clip(it.name, NAME_MAX), nw), name_style),
                        ];
                        if sw > 0 {
                            segs.push(Span::new(pad(it.scope, sw), theme.fg3));
                        }
                        segs.extend(spans(theme, it.status));
                        // 行下面在问删不删时不画 ›（spec §11.9）。
                        let asking_here =
                            matches!(ctx.mode, Mode::Confirming { row, .. } if row == at);
                        if is_chosen && it.opens && !asking_here {
                            segs.push(Span::new("  ›", theme.line));
                        }
                        segs
                    }
                    Row::Action(a) => {
                        let lab = if a.off {
                            theme.fg3
                        } else if a.danger {
                            if is_chosen {
                                Style::fg(theme.err)
                            } else {
                                theme.fg2
                            }
                        } else {
                            name_ink
                        };
                        // 有旁边一列时名字补到同一列宽（机器的主机：别名与 ssh 目标，spec §11.1）。
                        let shown = if a.aside.is_empty() {
                            a.label.to_owned()
                        } else {
                            pad(a.label, (string_width(a.label) + 2).max(ASIDE_AT))
                        };
                        let mut segs = if a.plus {
                            let mark = if is_chosen {
                                Style::fg(theme.brand)
                            } else {
                                theme.fg3
                            };
                            vec![
                                Span::new("+", mark),
                                Span::new(" ", Style::DEFAULT),
                                Span::new(shown, lab),
                            ]
                        } else {
                            vec![Span::new(shown, lab)]
                        };
                        if !a.aside.is_empty() {
                            segs.push(Span::new(a.aside.to_owned(), theme.fg3));
                        }
                        segs
                    }
                    _ => {
                        let label_ink = if matches!(r, Row::Info { .. }) {
                            theme.fg3
                        } else {
                            name_ink
                        };
                        let parts = value_parts(r)
                            .iter()
                            .filter(|p| !p.text.is_empty())
                            .map(|p| (p.text.clone(), part_style(theme, p)))
                            .collect();
                        let mut segs =
                            vec![Span::new(pad(&clip(label(r), LABEL_MAX), lw), label_ink)];
                        segs.extend(fade::parts(theme, parts, age, None));
                        segs
                    }
                };
                out.push(line(clip_spans(segs, w)));
                if is_chosen {
                    under(at, r, w, ctx, &mut out);
                }
            }
        }
    }
    // 选中行展开的几行（缩进两列）到哪里为止：窗口要让它们都看得见。
    let end = chosen_line.map(|c| {
        let mut end = c;
        while end + 1 < out.len() && out[end + 1].spans.first().is_some_and(|s| s.text == "  ") {
            end += 1;
        }
        end
    });
    (out, chosen_line, end)
}

/// 选中的行（下标 `at`）在它下面显示的：它的选项、问题、登录、测试，或它做什么、改动什么时候生效。
pub fn under(at: usize, r: &Row<'_>, w: usize, ctx: &PageCtx<'_>, out: &mut Vec<StyledLine>) {
    let theme = ctx.theme;
    let mut sub = |spans: Vec<Span>| {
        let mut all = vec![Span::new("  ", Style::DEFAULT)];
        all.extend(spans);
        out.push(line(clip_spans(all, w)));
    };
    match (ctx.mode, r) {
        (
            Mode::Choosing { row, at: pick },
            Row::Choice {
                options, current, ..
            },
        ) if row == at => {
            let ow = options
                .iter()
                .map(|o| string_width(&o.label))
                .max()
                .unwrap_or(0)
                + 3;
            for (i, o) in options.iter().enumerate() {
                let ch = i == pick;
                let off = !o.disabled.is_empty();
                let name_style = if off {
                    theme.fg3
                } else if ch {
                    chosen_style(theme)
                } else {
                    theme.fg2
                };
                let said = if off {
                    Span::new(o.disabled.clone(), theme.fg3.with_attr(attr::ITALIC))
                } else {
                    Span::new(o.desc.clone(), if ch { theme.fg2 } else { theme.fg3 })
                };
                let mut spans = vec![Span::new(pad(&o.label, ow), name_style), said];
                if o.value == *current {
                    let gap = if !o.desc.is_empty() || off { "  " } else { "" };
                    spans.push(Span::new(format!("{gap}current"), theme.fg3));
                }
                sub(spans);
            }
        }
        (
            Mode::Confirming {
                row,
                at: pick,
                label,
                note,
            },
            _,
        ) if row == at => {
            sub(vec![Span::new(note.to_owned(), theme.fg3)]);
            for (i, x) in [label, "Keep it"].into_iter().enumerate() {
                let style = if i == pick {
                    if i == 0 {
                        Style::fg(theme.err)
                    } else {
                        chosen_style(theme)
                    }
                } else {
                    theme.fg2
                };
                sub(vec![Span::new(x.to_owned(), style)]);
            }
        }
        (_, Row::Action(a)) if a.signs_in && ctx.sign_in.is_some() => {
            let url = ctx.sign_in.unwrap_or_default();
            sub(vec![
                Span::new("│", theme.line),
                Span::new(
                    " Opened the sign-in page in your browser. Sign in there.",
                    theme.fg2,
                ),
            ]);
            if !url.is_empty() {
                sub(vec![
                    Span::new("│", theme.line),
                    Span::new(" ", Style::DEFAULT),
                    Span::new(
                        url.to_owned(),
                        Style::fg(theme.brand).with_attr(attr::UNDERLINE),
                    ),
                ]);
            }
            sub(vec![
                Span::new("│", theme.line),
                Span::new(" ", Style::DEFAULT),
                Span::new("○", theme.fg3),
                Span::new(" Waiting for you to sign in", theme.fg3),
            ]);
        }
        (_, Row::Action(_)) if ctx.test.is_some_and(|(row, _)| row == at) => {
            match ctx.test.and_then(|(_, r)| r) {
                None => sub(vec![
                    Span::new("○", theme.fg3),
                    Span::new(" Testing…", theme.fg3),
                ]),
                Some(Ok(ms)) => sub(vec![
                    Span::new("✓", Style::fg(theme.ok)),
                    Span::new(" ", Style::DEFAULT),
                    Span::new("Connected", theme.fg2),
                    Span::new(format!(" · answered in {:.1} s", ms / 1000.0), theme.fg3),
                ]),
                Some(Err(why)) => sub(vec![
                    Span::new("✗", Style::fg(theme.err)),
                    Span::new(" ", Style::DEFAULT),
                    Span::new(why.to_owned(), theme.fg2),
                ]),
            }
        }
        (_, Row::Item(it)) if !it.detail.is_empty() => sub(spans(theme, it.detail)),
        (_, Row::Editor { .. }) => {
            let text = if ctx.editor.is_empty() {
                "⏎ opens it in your editor.".to_owned()
            } else {
                format!("⏎ opens it in {}.", ctx.editor)
            };
            sub(vec![Span::new(text, theme.fg3)]);
        }
        _ => {
            let when = applies(r)
                .filter(|a| *a != Applies::Form)
                .map_or("", Applies::short);
            let text: Vec<&str> = [desc(r), when]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();
            let text = text.join(" ");
            if !text.is_empty() {
                for t in wrap(&text, w.saturating_sub(2)) {
                    sub(vec![Span::new(t, theme.fg3)]);
                }
            }
        }
    }
}

/// `lines` 上一个 `h` 行的窗口，让 `focus` … `end` 看得见，截掉的边上是 `↑ n more` / `↓ n more`。
pub fn windowed(
    theme: &Theme,
    lines: Vec<StyledLine>,
    h: usize,
    focus: i64,
    end: i64,
) -> Vec<StyledLine> {
    if lines.len() <= h || h == 0 {
        return lines.into_iter().take(h).collect();
    }
    let n = lines.len() as i64;
    let h = h as i64;
    let f = focus.max(0);
    let mut from = (f - h / 3).min(n - h).max(0);
    if end >= from + h - 1 {
        from = (end - h + 2).min(f - 1).min(n - h).max(0);
    }
    let below = n - (from + h);
    let mut shown: Vec<StyledLine> = lines[from as usize..(from + h) as usize].to_vec();
    if from > 0 {
        shown[0] = line(vec![Span::new(format!("↑ {from} more"), theme.fg3)]);
    }
    if below > 0
        && let Some(last) = shown.last_mut()
    {
        *last = line(vec![Span::new(format!("↓ {} more", below + 1), theme.fg3)]);
    }
    shown
}

/// 一页：面包屑（段名之后的各级）、说明与行。`section` 是段在 [`SECTIONS`] 里的位置。
#[derive(Clone, Copy, Debug)]
pub struct Page<'a> {
    pub section: usize,
    pub crumbs: &'a [String],
    pub desc: &'a str,
    pub rows: &'a [Row<'a>],
}

/// 在搜索：打的词与各段的命中数（按段的 id）。
#[derive(Clone, Copy, Debug)]
pub struct Search<'a> {
    pub word: &'a str,
    pub hits: &'a BTreeMap<&'static str, usize>,
}

/// body：头一行，然后是段的目录与页，一共 `h` 行。`list_focus`：焦点在目录上，于是它选中的段是唯一画成选中的；否则
/// 那一段是粗体的前景。
pub fn body(
    page: &Page<'_>,
    chosen: Option<usize>,
    search: Option<Search<'_>>,
    width: usize,
    h: usize,
    ctx: &PageCtx<'_>,
    list_focus: bool,
) -> Vec<StyledLine> {
    let theme = ctx.theme;
    let narrow = width < NARROW;
    let pane_col = if narrow { 3 } else { 3 + INDEX_WIDTH };
    let w = width.saturating_sub(pane_col + 2).clamp(10, 84);
    let (pl, chosen_line, chosen_end) = page_lines(page.rows, chosen, w, ctx);
    let mut pane: Vec<StyledLine> = Vec::new();
    if !page.desc.is_empty() {
        pane.extend(
            wrap(page.desc, w)
                .into_iter()
                .map(|t| line(vec![Span::new(t, theme.fg3)])),
        );
        pane.push(StyledLine::EMPTY);
    }
    let lead = pane.len();
    pane.extend(pl);
    let mut head = vec![
        Span::new("   ", Style::DEFAULT),
        Span::new("Settings", Style::BOLD),
    ];
    let hit_counts = search.map(|s| s.hits);
    match search {
        Some(s) => {
            let n: usize = s.hits.values().sum();
            head.push(Span::new("  ›  ", theme.line));
            head.push(Span::new(format!("“{}”", s.word.trim()), Style::DEFAULT));
            let word = if n == 1 { "match" } else { "matches" };
            head.push(Span::new(format!("  {n} {word}"), theme.fg3));
        }
        None => {
            // 子页的 crumbs 已含它上面的各级（如模型页是 provider 与模型）；与段名重复的去掉。
            let mut crumbs: Vec<String> = vec![
                SECTIONS[page.section.min(SECTIONS.len() - 1)]
                    .name
                    .to_owned(),
            ];
            for c in page.crumbs {
                if !crumbs.contains(c) {
                    crumbs.push(c.clone());
                }
            }
            let last = crumbs.len() - 1;
            for (i, c) in crumbs.into_iter().enumerate() {
                head.push(Span::new("  ›  ", theme.line));
                head.push(Span::new(
                    c,
                    if i == last { Style::DEFAULT } else { theme.fg2 },
                ));
            }
        }
    }
    let avail = h.saturating_sub(3);
    // 没有选中的行时是 −1 + lead（Scala 的写法）。
    let focus = chosen_line.map_or(-1, |c| c as i64) + lead as i64;
    let end = chosen_end.map_or(-1, |e| e as i64) + lead as i64;
    let shown = windowed(theme, pane, avail, focus, end);
    let mut index: Vec<Vec<Span>> = Vec::new();
    if !narrow {
        for (i, s) in SECTIONS.iter().enumerate() {
            if i > 0 && SECTIONS[i - 1].group != s.group {
                index.push(Vec::new());
            }
            let n = hit_counts.and_then(|c| c.get(s.id)).copied().unwrap_or(0);
            let style = match hit_counts {
                Some(_) => {
                    if n > 0 {
                        theme.fg2
                    } else {
                        theme.line
                    }
                }
                None if i != page.section => theme.fg2,
                None if list_focus => chosen_style(theme),
                None => Style::BOLD,
            };
            let count = if hit_counts.is_some() && n > 0 {
                n.to_string()
            } else {
                String::new()
            };
            index.push(vec![
                Span::new(pad(s.name, INDEX_WIDTH - 3), style),
                Span::new(count, theme.fg3),
            ]);
        }
    }
    let mut out = vec![
        StyledLine::EMPTY,
        line(clip_spans(head, width)),
        StyledLine::EMPTY,
    ];
    for i in 0..avail {
        let mut left = match index.get(i) {
            Some(segs) => {
                let mut l = vec![Span::new("   ", Style::DEFAULT)];
                l.extend(segs.iter().cloned());
                l
            }
            None => Vec::new(),
        };
        let lw: usize = left.iter().map(|s| string_width(&s.text)).sum();
        left.push(Span::new(
            " ".repeat(pane_col.saturating_sub(lw)),
            Style::DEFAULT,
        ));
        if let Some(l) = shown.get(i) {
            left.extend(l.spans.iter().cloned());
        }
        out.push(line(left));
    }
    out
}
