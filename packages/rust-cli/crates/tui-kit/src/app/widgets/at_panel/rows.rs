//! `@` 面板的行与预览（docs/puffin-tui.md §4.2「提及（@）」；docs/specs/rust-cli/tui-kit.md §4.4）：组名与它的项（名字里
//! 相符的字点亮，第二栏是文件夹、碰过它的说法、会话最后的活动或子代理的来处），打出了路径时一窗的项；宽的终端在旁边预览
//! 选中的项（文件的行数与开头、文件夹的项、会话的时间、子代理的说明与工具），窄的终端在它下面展开；面板打开、文件到达、
//! 预览换了时从板上淡入。纯的；入参是 [`AtPanel`] 与中性的 [`Ctx`]。
//! 移植自 Puffin 的 shell/at/rows.rs（At.scala 第 311–657 行）；时间的说法在 `when`。
use std::collections::{BTreeMap, BTreeSet};

use crate::app::element::{Element, single_line};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, attr};
use crate::theme::theme::Theme;

use super::super::fade;
use super::super::fit::{clip, measured, pad};
use super::super::lists::window;
use super::super::skill_panel::wrap;
use super::super::when::{day_age, day_age_long};
use super::data::{PathPeek, SubagentRef, SubagentSource};
use super::words::{AT_WORDS, AtWords};
use super::{AtItem, AtPanel, Touch, dir, name_of, title_of};

/// 终端这么宽及更宽时预览在旁边。
pub const WIDE: usize = 76;
pub const NAME_W: usize = 24;
pub const DETAIL_W: usize = 17;
/// 预览开始的列：缩进、名字、两格、第二栏、三格。
pub const PANE_COL: usize = 5 + NAME_W + 2 + DETAIL_W + 3;
pub const PANE_MAX: usize = 46;
/// 打出了路径时一窗的项。
pub const PATH_ROWS: usize = 8;

/// 行与预览要的面板之外的事实：路径的预览、这个对话碰过的文件、选中的行、面板打开与文件到达与预览换了之后各过了多少
/// tick，此刻（毫秒）与时区。
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub peeks: &'a BTreeMap<String, PathPeek>,
    pub touch: &'a BTreeMap<String, Touch>,
    pub choice: usize,
    pub opened_age: i64,
    pub files_age: i64,
    pub shown_age: i64,
    pub now: f64,
    pub offset: &'a dyn Fn(f64) -> i32,
}

// ---- 行 ----

/// 在 `/` 处缩到 `width` 格的文件夹：放得下的尽量多的整段，一样多时前面的多，其余换成 `…`。
pub fn clip_path(path: &str, width: usize) -> String {
    if string_width(path) <= width {
        return path.to_owned();
    }
    let segs: Vec<&str> = path.split('/').collect();
    let mut best: Option<String> = None;
    let (mut kept, mut lead) = (None::<usize>, None::<usize>);
    for a in 0..segs.len() {
        for b in 1..segs.len().saturating_sub(a) {
            let mut parts: Vec<&str> = segs[..a].to_vec();
            parts.push("…");
            parts.extend_from_slice(&segs[segs.len() - b..]);
            let t = parts.join("/");
            let better =
                kept.is_none_or(|k| a + b > k || (a + b == k && lead.is_none_or(|l| a > l)));
            if string_width(&t) <= width && better {
                best = Some(t);
                kept = Some(a + b);
                lead = Some(a);
            }
        }
    }
    best.unwrap_or_else(|| {
        let room = width.saturating_sub(1);
        // 从尾巴取放得下的字。
        let mut tail: Vec<char> = Vec::new();
        let mut w = 0;
        for c in path.chars().rev() {
            let cw = string_width(c.encode_utf8(&mut [0; 4]));
            if w + cw > room {
                break;
            }
            w += cw;
            tail.push(c);
        }
        let mut out = String::from("…");
        out.extend(tail.into_iter().rev());
        out
    })
}

/// 一种项的墨色：文件与文件夹、过去的工作、子代理。
pub fn ink_of(theme: &Theme, it: &AtItem) -> Color {
    match it {
        AtItem::Path { .. } => theme.file,
        AtItem::Conversation(_) => theme.work,
        AtItem::Agent(_) => theme.agent,
    }
}

/// 一个引用的墨色，按它的种类。
pub fn ink_of_kind(theme: &Theme, kind: &str) -> Color {
    match kind {
        "file" | "folder" => theme.file,
        "conversation" | "workflow" => theme.work,
        "subagent" => theme.agent,
        _ => theme.skill,
    }
}

fn chosen_style(theme: &Theme, ink: Color) -> Style {
    if theme.rich {
        Style::fg(ink)
    } else {
        Style::new(ink, Color::Default, attr::BOLD)
    }
}

/// `q` 在 `text` 里相符的字（code point 的位置）：在里面时一整段，否则按次序的字母。
fn hits(text: &str, q: &str) -> BTreeSet<usize> {
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let ql: Vec<char> = q.to_lowercase().chars().collect();
    if ql.is_empty() || t.len() != text.chars().count() {
        return BTreeSet::new();
    }
    if let Some(at) = t.windows(ql.len()).position(|w| w == ql.as_slice()) {
        return (at..at + ql.len()).collect();
    }
    let mut found = BTreeSet::new();
    let mut j = 0;
    for (i, c) in t.iter().enumerate() {
        if j < ql.len() && *c == ql[j] {
            found.insert(i);
            j += 1;
        }
    }
    if j == ql.len() {
        found
    } else {
        BTreeSet::new()
    }
}

/// 一行的名字，`width` 格：选中的整个是它种类的墨色（plain 模式加粗）；其余 fg2，`q` 相符的字是 fg（plain 加粗）。
fn name_spans(
    theme: &Theme,
    text: &str,
    width: usize,
    q: &str,
    chosen: bool,
    ink: Color,
) -> Vec<Span> {
    let shown = clip(text, width);
    let padding = Span::new(
        " ".repeat(width.saturating_sub(string_width(&shown))),
        Style::DEFAULT,
    );
    if chosen {
        return vec![Span::new(shown, chosen_style(theme, ink)), padding];
    }
    let hs = hits(&shown, q);
    let matched = if theme.rich {
        Style::DEFAULT
    } else {
        Style::BOLD
    };
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut on = false;
    for (i, c) in shown.chars().enumerate() {
        let now = hs.contains(&i);
        if i > 0 && now != on {
            spans.push(Span::new(
                std::mem::take(&mut run),
                if on { matched } else { theme.fg2 },
            ));
        }
        on = now;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::new(run, if on { matched } else { theme.fg2 }));
    }
    spans.push(padding);
    spans
}

fn note_of(t: Option<Touch>, short: bool, words: &AtWords) -> &'static str {
    let notes = if short {
        words.short_notes
    } else {
        words.long_notes
    };
    match t {
        Some(Touch::Edited) => notes[0],
        Some(Touch::Read) => notes[1],
        Some(Touch::Changed) => notes[2],
        None => "",
    }
}

fn source_wide(a: &SubagentRef) -> String {
    match a.source {
        SubagentSource::Project => "this project".to_owned(),
        SubagentSource::User => "yours".to_owned(),
        SubagentSource::Plugin => format!("{} plugin", a.plugin_name),
        SubagentSource::Builtin => "built in".to_owned(),
    }
}

fn source_short(a: &SubagentRef) -> String {
    match a.source {
        SubagentSource::Plugin => a.plugin_name.clone(),
        _ => source_wide(a),
    }
}

/// 一行的第二栏，`width` 格。
#[allow(clippy::too_many_arguments)]
fn detail_spans(
    theme: &Theme,
    it: &AtItem,
    p: &AtPanel,
    touch: &BTreeMap<String, Touch>,
    width: usize,
    now: f64,
    offset: &dyn Fn(f64) -> i32,
    words: &AtWords,
) -> Vec<Span> {
    let text = |s: &str| vec![Span::new(pad(&clip(s, width), width), theme.fg3)];
    match it {
        AtItem::Path { path, .. } => {
            if p.single() && p.q.is_empty() {
                text(note_of(touch.get(path).copied(), true, words))
            } else {
                let d = dir(path);
                let shown = if p.single() {
                    let rest = d.get(p.under.len()..).unwrap_or("");
                    rest.strip_prefix('/').unwrap_or(rest)
                } else {
                    d
                };
                vec![Span::new(pad(&clip_path(shown, width), width), theme.fg3)]
            }
        }
        AtItem::Conversation(c) => text(&day_age(now, c.updated_at as f64, offset)),
        AtItem::Agent(a) => text(&source_short(a)),
    }
}

fn indent(n: usize) -> Span {
    Span::new(" ".repeat(n), Style::DEFAULT)
}

/// `text` 折到 `width`，最多 `max` 行，最后一行用 `…` 截断。
fn lines(text: &str, width: usize, max: usize) -> Vec<String> {
    let ws = wrap(text, width);
    if ws.len() <= max {
        return ws;
    }
    let keep = max.saturating_sub(1);
    let mut out = ws[..keep].to_vec();
    out.push(clip(&ws[keep..].join(" "), width));
    out
}

/// 在 `/` 处折到 `width` 的路径。
fn wrap_path(path: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for x in path
        .split('/')
        .filter(|p| !p.is_empty())
        .map(|p| format!("{p}/"))
    {
        if !line.is_empty() && string_width(&line) + string_width(&x) > width {
            out.push(std::mem::take(&mut line));
        }
        line.push_str(&x);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{} KB", (b / 1024.0).round() as u64)
    } else {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    }
}

/// 文件的事实：`361 lines · edited in this conversation`，或文件夹的项数。
fn facts(theme: &Theme, peek: Option<&PathPeek>, note: &str, words: &AtWords) -> Vec<Span> {
    let noted = |mut v: Vec<Span>| {
        if !note.is_empty() {
            v.push(Span::new(format!(" · {note}"), theme.fg3));
        }
        v
    };
    match peek {
        Some(PathPeek::File {
            binary: true,
            bytes,
            ..
        }) => noted(vec![Span::new((words.binary)(&size(*bytes)), theme.fg2)]),
        Some(PathPeek::File { lines, more, .. }) => {
            noted(vec![Span::new((words.lines)(*lines, *more), theme.fg2)])
        }
        Some(PathPeek::Folder { entries, .. }) => {
            vec![Span::new((words.entries)(*entries), theme.fg2)]
        }
        Some(PathPeek::Missing) => vec![Span::new(words.not_found, theme.fg3)],
        None => Vec::new(),
    }
}

/// `it` 的预览，`w` 格宽。
pub fn pane(theme: &Theme, it: &AtItem, ctx: &Ctx<'_>, w: usize) -> Vec<Vec<Span>> {
    pane_said(theme, it, ctx, w, &AT_WORDS)
}

/// [`pane`]，话是 `words`。
pub fn pane_said(
    theme: &Theme,
    it: &AtItem,
    ctx: &Ctx<'_>,
    w: usize,
    words: &AtWords,
) -> Vec<Vec<Span>> {
    let (now, offset) = (ctx.now, ctx.offset);
    let ink = chosen_style(theme, ink_of(theme, it));
    match it {
        AtItem::Path { path, folder } => {
            let peek = ctx.peeks.get(path);
            let mut title = vec![Span::new(name_of(it), ink)];
            if *folder {
                title.push(Span::new(words.folder, theme.fg3));
            }
            let mut where_ = wrap_path(&format!("{}/", dir(path)), w);
            if where_.is_empty() {
                where_.push("./".to_owned());
            }
            let mut head = vec![title];
            head.extend(where_.into_iter().map(|l| vec![Span::new(l, theme.fg3)]));
            let note = || note_of(ctx.touch.get(path).copied(), false, words);
            let body: Vec<Vec<Span>> = match peek {
                None => Vec::new(),
                Some(PathPeek::Folder { names, .. }) => {
                    let mut rows: Vec<String> = Vec::new();
                    let mut line = String::new();
                    for n in names {
                        if !line.is_empty() && string_width(&line) + 2 + string_width(n) > w {
                            rows.push(std::mem::take(&mut line));
                        }
                        line = if line.is_empty() {
                            n.clone()
                        } else {
                            format!("{line}  {n}")
                        };
                    }
                    if !line.is_empty() {
                        rows.push(line);
                    }
                    let mut out = vec![facts(theme, peek, "", words)];
                    out.extend(
                        rows.into_iter()
                            .take(3)
                            .map(|l| vec![Span::new(clip(&l, w), theme.fg3)]),
                    );
                    out
                }
                Some(PathPeek::File {
                    binary: false,
                    head: first,
                    ..
                }) => {
                    let mut out = vec![facts(theme, peek, note(), words)];
                    out.extend(first.iter().enumerate().map(|(i, l)| {
                        vec![
                            Span::new(format!("{:>2}  ", i + 1), theme.quiet),
                            Span::new(
                                clip(&l.replace('\t', "    "), w.saturating_sub(4).max(1)),
                                theme.fg3,
                            ),
                        ]
                    }));
                    out
                }
                Some(_) => vec![facts(theme, peek, note(), words)],
            };
            if !body.is_empty() {
                head.push(Vec::new());
                head.extend(body);
            }
            head
        }
        AtItem::Conversation(c) => vec![
            vec![
                Span::new(clip(&title_of(c), w.saturating_sub(14).max(1)), ink),
                Span::new("  conversation", theme.fg3),
            ],
            vec![Span::new(
                format!(
                    "last active {}",
                    day_age_long(now, c.updated_at as f64, offset)
                ),
                theme.fg3,
            )],
        ],
        AtItem::Agent(a) => {
            let tools = a
                .tools
                .as_ref()
                .map_or_else(|| "all".to_owned(), |t| t.join(", "));
            let mut out = vec![
                vec![
                    Span::new(a.name.clone(), ink),
                    Span::new(format!("  subagent · {}", source_wide(a)), theme.fg3),
                ],
                Vec::new(),
            ];
            out.extend(
                lines(&a.description, w, 3)
                    .into_iter()
                    .map(|l| vec![Span::new(l, theme.fg2)]),
            );
            out.push(Vec::new());
            out.push(vec![
                Span::new("Tools  ", theme.fg3),
                Span::new(clip(&tools, w.saturating_sub(7).max(1)), theme.fg3),
            ]);
            out
        }
    }
}

/// 把空白折成一个空格（Java 的 `replaceAll("\\s+", " ")`）。
fn one_line(s: &str) -> String {
    s.split([' ', '\t', '\n', '\u{0b}', '\u{0c}', '\r'])
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 在 [`WIDE`] 以下：选中的行在它的事实上展开，在第 7 列。
fn opened(theme: &Theme, it: &AtItem, ctx: &Ctx<'_>, w: usize, words: &AtWords) -> Vec<Vec<Span>> {
    let (now, offset) = (ctx.now, ctx.offset);
    let line = |spans: Vec<Span>| {
        let mut v = vec![indent(7)];
        v.extend(spans);
        v
    };
    match it {
        AtItem::Path { path, folder } => {
            let note = note_of(ctx.touch.get(path).copied(), false, words);
            let f = facts(
                theme,
                ctx.peeks.get(path),
                if *folder { "" } else { note },
                words,
            );
            let mut out = vec![line(vec![Span::new(
                clip(&format!("{}/", dir(path)), w),
                theme.fg3,
            )])];
            if !f.is_empty() {
                out.push(line(f));
            }
            out
        }
        AtItem::Conversation(c) => {
            let facts = format!(
                "last active {}",
                day_age_long(now, c.updated_at as f64, offset)
            );
            vec![line(vec![Span::new(clip(&facts, w), theme.fg3)])]
        }
        AtItem::Agent(a) => vec![line(vec![Span::new(
            clip(&one_line(&a.description), w),
            theme.fg2,
        )])],
    }
}

/// 输入框上面面板的行，终端宽 `width`。
pub fn rows(theme: &Theme, p: &AtPanel, ctx: &Ctx<'_>, width: usize) -> Vec<Element> {
    rows_said(theme, p, ctx, width, &AT_WORDS)
}

/// [`rows`]，话是 `words`。
pub fn rows_said(
    theme: &Theme,
    p: &AtPanel,
    ctx: &Ctx<'_>,
    width: usize,
    words: &AtWords,
) -> Vec<Element> {
    let (now, offset) = (ctx.now, ctx.offset);
    let es = p.entries();
    let c = (!es.is_empty()).then(|| ctx.choice.min(es.len() - 1));
    let wide = width >= WIDE;
    let touch = ctx.touch;
    let opening = ctx.opened_age;
    let files_age = ctx.files_age;
    let faded =
        |spans: Vec<Span>, age: i64| fade::fade_in(theme, spans, age, fade::TICKS, theme.panel);
    let detail_w = if wide {
        DETAIL_W
    } else {
        width.saturating_sub(5 + NAME_W + 2 + 2)
    };
    let mut left: Vec<(Vec<Span>, bool)> = Vec::new();
    if es.is_empty() && !p.loading() {
        left.push((
            vec![
                Span::new(INDENT, Style::DEFAULT),
                Span::new(words.nothing, theme.fg3),
            ],
            false,
        ));
    }
    let mut k = 0;
    for g in &p.groups {
        // 文件的组（在找的、只有路径的）在文件到达时淡入；以前按组名认（`Files…`、`Recent files`），译过的组名认不出。
        let files =
            g.loading || p.single() || g.items.iter().all(|it| matches!(it, AtItem::Path { .. }));
        let age = if files { files_age } else { opening };
        let add = |spans: Vec<Span>, left: &mut Vec<(Vec<Span>, bool)>| {
            left.push((faded(spans, age), false))
        };
        add(
            vec![
                Span::new(INDENT, Style::DEFAULT),
                Span::new(g.label.clone(), theme.fg3),
            ],
            &mut left,
        );
        if g.loading {
            add(
                vec![
                    indent(5),
                    Span::new(words.looking, theme.fg3.with_attr(attr::ITALIC)),
                ],
                &mut left,
            );
        }
        let (from, until) = if p.single() {
            window(g.items.len(), c.unwrap_or(0).saturating_sub(k), PATH_ROWS)
        } else {
            (0, g.items.len())
        };
        if from > 0 {
            add(
                vec![indent(5), Span::new((words.above)(from), theme.fg3)],
                &mut left,
            );
        }
        for (i, it) in g.items.iter().enumerate().take(until).skip(from) {
            let chosen = Some(k + i) == c;
            let mut spans = vec![indent(5)];
            spans.extend(name_spans(
                theme,
                &name_of(it),
                NAME_W,
                &p.q,
                chosen,
                ink_of(theme, it),
            ));
            if detail_w > 0 {
                spans.push(Span::new("  ", Style::DEFAULT));
                spans.extend(detail_spans(
                    theme, it, p, touch, detail_w, now, offset, words,
                ));
            }
            left.push((faded(spans, age), chosen));
        }
        if until < g.items.len() {
            add(
                vec![
                    indent(5),
                    Span::new((words.below)(g.items.len() - until), theme.fg3),
                ],
                &mut left,
            );
        }
        if g.more > 0 {
            add(
                vec![
                    indent(5),
                    Span::new((words.more_of)(g.more, &g.more_word), theme.fg3),
                ],
                &mut left,
            );
        }
        k += g.items.len();
    }
    let lines: Vec<Vec<Span>> = match c {
        None => left.into_iter().map(|l| l.0).collect(),
        Some(c) if wide => {
            let pw = width.saturating_sub(PANE_COL + 2).min(PANE_MAX);
            let side: Vec<Vec<Span>> = pane_said(theme, es[c], ctx, pw, words)
                .into_iter()
                .map(|r| faded(r, ctx.shown_age))
                .collect();
            (0..left.len().max(side.len()))
                .map(|i| {
                    let mut l = left.get(i).map(|l| l.0.clone()).unwrap_or_default();
                    match side.get(i).filter(|r| !r.is_empty()) {
                        None => l,
                        Some(r) => {
                            let used: usize = l.iter().map(|sp| string_width(&sp.text)).sum();
                            l.push(Span::new(
                                " ".repeat(PANE_COL.saturating_sub(used)),
                                Style::DEFAULT,
                            ));
                            l.extend(r.iter().cloned());
                            l
                        }
                    }
                })
                .collect()
        }
        Some(c) => {
            let at = left.iter().position(|l| l.1);
            let under: Vec<Vec<Span>> =
                opened(theme, es[c], ctx, width.saturating_sub(9).max(1), words)
                    .into_iter()
                    .map(|r| faded(r, ctx.shown_age))
                    .collect();
            // Scala 的 indexWhere 找不到时为 -1：展开的行放在最前。
            let split = at.map_or(0, |a| a + 1);
            let mut out: Vec<Vec<Span>> = left[..split].iter().map(|l| l.0.clone()).collect();
            out.extend(under);
            out.extend(left[split..].iter().map(|l| l.0.clone()));
            out
        }
    };
    lines
        .into_iter()
        .map(|spans| measured(single_line(spans, false)))
        .collect()
}

/// 面板的键，槽从中挑。
pub fn keys(p: &AtPanel, choice: usize) -> Vec<(String, String)> {
    let es = p.entries();
    let ks: Vec<(&str, &str)> = if let Some(last) = es.len().checked_sub(1) {
        let folder = matches!(es[choice.min(last)], AtItem::Path { folder: true, .. });
        vec![
            ("↑↓", "choose"),
            ("⏎", if folder { "open folder" } else { "insert" }),
            ("esc", "close"),
        ]
    } else if p.loading() {
        vec![("esc", "close")]
    } else {
        vec![("⏎", "send"), ("esc", "close")]
    };
    ks.into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}
