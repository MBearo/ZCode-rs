//! `$` 提及技能的面板（docs/puffin-tui.md §4.2「提及技能」；docs/specs/rust-cli/tui-kit.md §4.4）：输入框上方找到的技能的
//! 名字（选中的是技能的墨色），宽的终端在旁边预览选中的那个（名字、来处、整段说明），窄的终端在选中的行下面展开说明；目录
//! 还没到、是空的或没有相符的时一行说明；面板打开时整块、预览换了时名字与说明从板上淡入。纯的；入参是中性的 [`Skill`]
//! 与 [`Panel`]，壳持有目录、排序与面板的状态。
//! 移植自 Puffin 的 shell/mentions.rs 的「面板」一节（Mentions.scala 第 230–325 行）。
use crate::app::element::{Element, single_line};
use crate::markdown::render::INDENT;
use crate::render::width::{display_width, string_width};
use crate::render::{Color, Span, Style, attr};
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{clip, java_space, measured, pad};
use super::lists::window;

/// 终端这么宽及更宽时预览在名字旁边。
pub const WIDE: usize = 76;
pub const WIDE_ROWS: usize = 8;
pub const NARROW_ROWS: usize = 7;
pub const NAME_MAX: usize = 24;
pub const PANE_MAX: usize = 64;

/// 技能从哪里来：插件（它的名字）、这个项目、你自己的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillFrom<'a> {
    Plugin(&'a str),
    Workspace,
    User,
}

/// 面板上的一个技能：名字（不带 `$`）、说明与来处。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Skill<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub from: SkillFrom<'a>,
}

/// 面板此刻：目录里有几个技能（`None` 是目录还没到）、找到的技能（好的在前）、选中的行，与面板打开、预览换了之后各过了
/// 多少 tick。
#[derive(Clone, Copy, Debug)]
pub struct Panel<'a> {
    pub catalog: Option<usize>,
    pub matches: &'a [Skill<'a>],
    pub choice: usize,
    pub opened_age: i64,
    pub shown_age: i64,
}

/// `text` 折到 `width` 格：在空格处，宽字（CJK）之间的任何地方；比一行宽的词切开。
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let w = width.max(1);
    // Java 正则的 `\s` 只有 ASCII 的空白；`trim` 去掉不大于空格的字。
    let collapsed: String = text
        .split([' ', '\t', '\n', '\u{0b}', '\u{0c}', '\r'])
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let t = collapsed.trim_matches(|c: char| c <= ' ');
    let wide = |c: char| display_width(c) == 2;
    let mut units: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let c = t[i..].chars().next().unwrap_or(' ');
        let mut j = i + c.len_utf8();
        if c == ' ' {
            j = i + t[i..].find(|x: char| x != ' ').unwrap_or(t.len() - i);
        } else if !wide(c) {
            j += t[j..]
                .find(|x: char| x == ' ' || wide(x))
                .unwrap_or(t.len() - j);
        }
        units.push(&t[i..j]);
        i = j;
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut col = 0;
    let flush = |line: &mut String, col: &mut usize, lines: &mut Vec<String>| {
        lines.push(line.trim_end_matches(java_space).to_owned());
        line.clear();
        *col = 0;
    };
    for u in units {
        let uw = string_width(u);
        if u.starts_with(' ') {
            if col > 0 {
                line.push_str(u);
                col += uw;
            }
        } else if col + uw <= w {
            line.push_str(u);
            col += uw;
        } else if uw <= w {
            flush(&mut line, &mut col, &mut lines);
            line.push_str(u);
            col = uw;
        } else {
            for c in u.chars() {
                let cw = display_width(c);
                if col + cw > w {
                    flush(&mut line, &mut col, &mut lines);
                }
                line.push(c);
                col += cw;
            }
        }
    }
    if col > 0 {
        flush(&mut line, &mut col, &mut lines);
    }
    lines
}

/// 面板里的字（tui-kit.md §4.1 第 4 条）：英文缺省是 Puffin 的 [`SKILL_WORDS`]，旧界面的中文传译文；版面不变。
#[derive(Clone, Copy, Debug)]
pub struct SkillWords {
    /// 预览的名字旁边的来处：插件（它的名字）、这个项目、你的。
    pub plugin: fn(&str) -> String,
    pub this_project: &'static str,
    pub yours: &'static str,
    /// 窄的终端行里的来处：这个项目、你的（插件的是它的名字）。
    pub project: &'static str,
    pub user: &'static str,
    pub looking: &'static str,
    /// 目录是空的：前一句、`/settings skills`、后一句。
    pub none_on: [&'static str; 3],
    pub no_match: &'static str,
    pub above: fn(usize) -> String,
    pub below: fn(usize) -> String,
}

pub const SKILL_WORDS: SkillWords = SkillWords {
    plugin: |p| format!("{p} plugin"),
    this_project: "this project",
    yours: "yours",
    project: "project",
    user: "user",
    looking: "Looking for skills…",
    none_on: [
        "No skills are turned on. ",
        "/settings skills",
        " adds one.",
    ],
    no_match: "No skill matches. It goes out as text.",
    above: |n| format!("↑ {n} more"),
    below: |n| format!("↓ {n} more"),
};

/// 技能从哪里来，在预览的名字旁边。
fn wide_source(k: &Skill<'_>, w: &SkillWords) -> String {
    match k.from {
        SkillFrom::Plugin(p) => (w.plugin)(p),
        SkillFrom::Workspace => w.this_project.to_owned(),
        SkillFrom::User => w.yours.to_owned(),
    }
}

/// 技能从哪里来，没有预览时在它的行里。
fn narrow_source(k: &Skill<'_>, w: &SkillWords) -> String {
    match k.from {
        SkillFrom::Plugin(p) => p.to_owned(),
        SkillFrom::Workspace => w.project.to_owned(),
        SkillFrom::User => w.user.to_owned(),
    }
}

/// 选中的名字的墨色：技能的墨色，plain 模式加粗。
fn chosen_ink(theme: &Theme) -> Style {
    if theme.rich {
        Style::fg(theme.skill)
    } else {
        Style::new(theme.skill, Color::Default, attr::BOLD)
    }
}

/// 名字的墨色：选中的是技能的墨色，其余 fg2。↑ ↓ 立即移动它。
fn name_ink(theme: &Theme, chosen: bool) -> Style {
    if chosen { chosen_ink(theme) } else { theme.fg2 }
}

/// 从板上淡入了 `age` 个 tick 的段。
pub fn faded(theme: &Theme, spans: Vec<Span>, age: i64) -> Vec<Span> {
    fade::fade_in(theme, spans, age, fade::TICKS, theme.panel)
}

/// 面板打开时它的行一起淡入。
fn opening(theme: &Theme, rows: Vec<Vec<Span>>, p: &Panel<'_>) -> Vec<Vec<Span>> {
    rows.into_iter()
        .map(|r| faded(theme, r, p.opened_age))
        .collect()
}

/// 选中技能的说明的一行，随它的名字淡入。
fn described(theme: &Theme, text: String, indent: &str, p: &Panel<'_>) -> Vec<Span> {
    let ink = faded(theme, vec![Span::new(text, theme.fg2)], p.shown_age);
    if indent.is_empty() {
        ink
    } else {
        let mut out = vec![Span::new(indent.to_owned(), Style::DEFAULT)];
        out.extend(ink);
        out
    }
}

fn more(theme: &Theme, text: String) -> Vec<Span> {
    vec![
        Span::new(INDENT, Style::DEFAULT),
        Span::new(text, theme.fg3),
    ]
}

/// 没有可列的时的一行：目录还没到、是空的，或没有相符的。
fn edge_row(theme: &Theme, catalog: Option<usize>, w: &SkillWords) -> Vec<Span> {
    let mut out = vec![Span::new(INDENT, Style::DEFAULT)];
    match catalog {
        None => out.push(Span::new(w.looking, theme.fg3.with_attr(attr::ITALIC))),
        Some(0) => out.extend([
            Span::new(w.none_on[0], theme.fg3),
            Span::new(w.none_on[1], theme.fg2),
            Span::new(w.none_on[2], theme.fg3),
        ]),
        Some(_) => out.push(Span::new(w.no_match, theme.fg3)),
    }
    out
}

/// 选中的行附近一窗的名字，每个补到 `name_w`，窗藏起的部分各一行 more；`with_source` 加上各自的来处。
fn name_rows(
    theme: &Theme,
    ms: &[Skill<'_>],
    c: usize,
    max: usize,
    name_w: usize,
    with_source: bool,
    w: &SkillWords,
) -> Vec<(Vec<Span>, bool)> {
    let (from, until) = window(ms.len(), c, max);
    let mut out = Vec::new();
    if from > 0 {
        out.push((more(theme, (w.above)(from)), false));
    }
    for (i, k) in ms.iter().enumerate().take(until).skip(from) {
        let mut row = vec![
            Span::new(INDENT, Style::DEFAULT),
            Span::new(
                pad(&clip(&format!("${}", k.name), NAME_MAX), name_w),
                name_ink(theme, i == c),
            ),
        ];
        if with_source {
            row.push(Span::new(narrow_source(k, w), theme.fg3));
        }
        out.push((row, i == c));
    }
    if until < ms.len() {
        out.push((more(theme, (w.below)(ms.len() - until)), false));
    }
    out
}

/// 输入框上面面板的行，为 `q` 找到的技能、终端宽 `width`：名字与旁边选中的那个的预览，或在 [`WIDE`] 以下选中的行在它的
/// 说明上展开。
pub fn rows(theme: &Theme, p: &Panel<'_>, width: usize) -> Vec<Element> {
    rows_said(theme, p, width, &SKILL_WORDS)
}

/// [`rows`]，字是 `w`（旧界面的中文）。
pub fn rows_said(theme: &Theme, p: &Panel<'_>, width: usize, w: &SkillWords) -> Vec<Element> {
    let ms = p.matches;
    let lines = if ms.is_empty() {
        opening(theme, vec![edge_row(theme, p.catalog, w)], p)
    } else {
        let c = p.choice.min(ms.len() - 1);
        let k = &ms[c];
        let name_w = ms
            .iter()
            .map(|m| string_width(m.name) + 1)
            .max()
            .unwrap_or(0)
            .min(NAME_MAX)
            + 3;
        if width >= WIDE {
            let list = opening(
                theme,
                name_rows(theme, ms, c, WIDE_ROWS, name_w, false, w)
                    .into_iter()
                    .map(|r| r.0)
                    .collect(),
                p,
            );
            // 右栏：选中的名字（提及色）与来处、空一行、整段说明；放不下时最后一行用 … 截断。
            let pane_col = INDENT.len() + name_w + 2;
            let pane_w = width.saturating_sub(pane_col + 2).min(PANE_MAX);
            let head = faded(
                theme,
                vec![
                    Span::new(format!("${}", k.name), chosen_ink(theme)),
                    Span::new(format!("  {}", wide_source(k, w)), theme.fg3),
                ],
                p.shown_age,
            );
            let body = wrap(k.description, pane_w);
            let room = list.len().max(WIDE_ROWS.min(body.len() + 2)) - 2;
            let shown = if body.len() > room {
                let keep = room.saturating_sub(1);
                let mut s = body[..keep].to_vec();
                s.push(clip(&body[keep..].join(" "), pane_w));
                s
            } else {
                body
            };
            let mut pane = vec![head, Vec::new()];
            pane.extend(shown.into_iter().map(|t| described(theme, t, "", p)));
            (0..list.len().max(pane.len()))
                .map(|i| {
                    let mut left = list.get(i).cloned().unwrap_or_default();
                    match pane.get(i).filter(|r| !r.is_empty()) {
                        None => left,
                        Some(right) => {
                            let used: usize = left.iter().map(|sp| string_width(&sp.text)).sum();
                            left.push(Span::new(
                                " ".repeat(pane_col.saturating_sub(used)),
                                Style::DEFAULT,
                            ));
                            left.extend(right.iter().cloned());
                            left
                        }
                    }
                })
                .collect()
        } else {
            // 窄的终端没有右栏：选中的那一行在它下面展开说明，最多三行。
            let named = name_rows(theme, ms, c, NARROW_ROWS, name_w, true, w);
            let at = named.iter().position(|r| r.1).unwrap_or(0);
            let list = opening(theme, named.into_iter().map(|r| r.0).collect(), p);
            let w = width.saturating_sub(5).min(96).saturating_sub(2);
            let body = wrap(k.description, w);
            let shown = if body.len() > 3 {
                let mut s = body[..2].to_vec();
                s.push(clip(&body[2..].join(" "), w));
                s
            } else {
                body
            };
            let indent = format!("{INDENT}  ");
            let desc = shown.into_iter().map(|t| described(theme, t, &indent, p));
            let mut out: Vec<Vec<Span>> = list[..at + 1].to_vec();
            out.extend(desc);
            out.extend(list[at + 1..].iter().cloned());
            out
        }
    };
    lines
        .into_iter()
        .map(|spans| measured(single_line(spans, false)))
        .collect()
}

/// 面板的键，槽从中挑（§4.2）。
pub fn keys(catalog: Option<usize>, matched: usize) -> Vec<(String, String)> {
    let ks: &[(&str, &str)] = if matched > 0 {
        &[("↑↓", "choose"), ("⏎", "insert"), ("esc", "close")]
    } else if catalog.is_some_and(|n| n > 0) {
        &[("⏎", "send"), ("esc", "close")]
    } else {
        &[("esc", "close")]
    };
    ks.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}
