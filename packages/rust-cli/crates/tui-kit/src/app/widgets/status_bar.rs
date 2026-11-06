//! 状态栏（docs/puffin-tui.md §4.2「状态栏」；docs/specs/rust-cli/tui-kit.md §4.4）：部分（[`Part`]）、按名次丢掉与回来的
//! 排法（[`fit`]）、一行还是两行（[`rows`]，终端至少 [`SPLIT_ROWS`] 行高才分开）、cwd 的短写法，与左边的部分怎样接起来
//! （模式与 `⇧⇥`、目标的标记与字、模型与档位、cwd 与分支、`@machine`，[`left`]）。纯的；入参是中性的 [`Left`] 与段。
//! 移植自 Puffin 的 shell/status_line.rs（`Row`、`SPLIT_ROWS`、`rows`、`short_path`、`fit`、`cwd_part`、
//! `status_line` 里左边部分的名次与接法、`label_spans` 的部分）与 shell/stats.rs（`Part`）。
use std::collections::BTreeSet;

use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fade;
use super::modes::{self, Mode};

/// 状态栏的一个部分（Scala `StatusLine.Part`）。
///
/// - `glue`：在它那一侧把它接到前一个部分的；`None` 是 `  ·  `。
/// - `after`：只与这个部分一起显示（cwd 后的分支、模式后的 `⇧⇥`）。
/// - `extra`：它可以走开再回来，状态栏不因此变成两行。
/// - `right`：它在右端。`short`：更短的形式，状态栏变成两行之前先试它（cwd）。
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub key: String,
    pub spans: Vec<Span>,
    pub rank: i32,
    pub right: bool,
    pub glue: Option<Vec<Span>>,
    pub extra: bool,
    pub after: Option<String>,
    pub short: Option<Vec<Span>>,
}

impl Part {
    /// 只有键、样式段与名次的部分，其余为缺省。
    pub fn new(key: impl Into<String>, spans: Vec<Span>, rank: i32) -> Part {
        Part {
            key: key.into(),
            spans,
            rank,
            right: false,
            glue: None,
            extra: false,
            after: None,
            short: None,
        }
    }
}

/// 排好的一行与它留下的部分。
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub spans: Vec<Span>,
    pub gone: BTreeSet<String>,
}

/// 终端少于这么多行时状态栏总是一行。
pub const SPLIT_ROWS: usize = 20;
const LEAD: &str = "   ";

fn cells(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// 宽 `width` 时的状态栏：一行；一行会丢掉不是附加的部分、两行留得住它、终端至少 [`SPLIT_ROWS`] 行高时是两行。`group` 是
/// 多少的部分，`note` 是槽的那句话或设置的键：它们占第 1 行的右端，把 group 送到第 2 行。
pub fn rows(
    left: &[Part],
    group: &[Part],
    note: &[Part],
    width: usize,
    height: usize,
    sep: Style,
) -> Vec<Vec<Span>> {
    let right = if note.is_empty() { group } else { note };
    // 多少的那一组与左边至少隔三格；说明句与设置的键照旧隔两格。
    let gap = if note.is_empty() { 3 } else { 2 };
    let all: Vec<Part> = left.iter().chain(right).cloned().collect();
    let one = fit(&all, width, gap, sep);
    let two = || {
        let first: Vec<Part> = left.iter().chain(note).cloned().collect();
        let second: Vec<Part> = group
            .iter()
            .map(|p| Part {
                right: false,
                ..p.clone()
            })
            .collect();
        [fit(&first, width, 2, sep), fit(&second, width, 2, sep)]
    };
    // 只在两行留得住一行丢掉的部分时分开：比整行还长的 cwd 两行也放不下，分开没有用。
    let kept = || {
        let t = two();
        all.iter().any(|p| {
            one.gone.contains(&p.key) && !p.extra && !t.iter().any(|r| r.gone.contains(&p.key))
        })
    };
    if group.is_empty() || height < SPLIT_ROWS || (note.is_empty() && !kept()) {
        return vec![one.spans];
    }
    if !note.is_empty() {
        return two().into_iter().map(|r| r.spans).collect();
    }
    // 分开之前先用短的写法（cwd 的第一段与最后一个文件夹）再排一次：一行放得下除附加部分以外的全部时仍是一行。
    let shorter_parts: Vec<Part> = all
        .iter()
        .map(|p| match &p.short {
            Some(s) => Part {
                spans: s.clone(),
                ..p.clone()
            },
            None => p.clone(),
        })
        .collect();
    let shorter = fit(&shorter_parts, width, gap, sep);
    if all
        .iter()
        .any(|p| shorter.gone.contains(&p.key) && !p.extra)
    {
        two().into_iter().map(|r| r.spans).collect()
    } else {
        vec![shorter.spans]
    }
}

/// 路径写成它的第一段、`…` 与最后一个文件夹（`~/…/feat-tui`、`/…/feat-tui`）；不更短时为 `None`。
pub fn short_path(path: &str) -> Option<String> {
    let sep = if path.contains('/') { '/' } else { '\\' };
    // Java 的 split 去掉末尾的空段。
    let mut segs: Vec<&str> = path.split(sep).collect();
    while segs.len() > 1 && segs.last() == Some(&"") {
        segs.pop();
    }
    if segs.len() < 3 {
        return None;
    }
    let s = format!("{}{sep}…{sep}{}", segs[0], segs[segs.len() - 1]);
    (string_width(&s) < string_width(path)).then_some(s)
}

/// 宽 `width` 时留下的部分：名次低的先走，一次一个名次；然后走掉的在放得下时回来，名次高的先回来。左边的部分从第 3 列起；
/// 右边的部分停在边前一格，与左边至少隔 `gap` 格。
pub fn fit(parts: &[Part], width: usize, gap: usize, sep: Style) -> Row {
    let present: BTreeSet<String> = parts.iter().map(|p| p.key.clone()).collect();
    let shown = |gone: &BTreeSet<String>| -> Vec<&Part> {
        parts
            .iter()
            .filter(|p| {
                !gone.contains(&p.key)
                    && p.after
                        .as_ref()
                        .is_none_or(|a| present.contains(a) && !gone.contains(a))
            })
            .collect()
    };
    let build = |gone: &BTreeSet<String>| -> Option<Vec<Span>> {
        let kept = shown(gone);
        let side = |right: bool| -> Vec<Span> {
            let mut out = Vec::new();
            for (i, p) in kept.iter().filter(|p| p.right == right).enumerate() {
                if i > 0 {
                    match &p.glue {
                        Some(g) => out.extend(g.iter().cloned()),
                        None => out.push(Span::new("  ·  ", sep)),
                    }
                }
                out.extend(p.spans.iter().cloned());
            }
            out
        };
        let l = side(false);
        let r = side(true);
        let left = if l.is_empty() {
            Vec::new()
        } else {
            let mut v = vec![Span::new(LEAD, Style::DEFAULT)];
            v.extend(l);
            v
        };
        let (lw, rw) = (cells(&left), cells(&r));
        if r.is_empty() {
            (lw == 0 || lw + 2 <= width).then_some(left)
        } else if lw + gap + rw < width {
            let mut v = left;
            v.push(Span::new(" ".repeat(width - lw - rw - 1), Style::DEFAULT));
            v.extend(r);
            v.push(Span::new(" ", Style::DEFAULT));
            Some(v)
        } else {
            None
        }
    };
    let mut thresholds: Vec<i32> = parts.iter().map(|p| p.rank).collect();
    thresholds.sort_unstable();
    thresholds.dedup();
    // 门槛 t：名次小于 t 的都走。第一个门槛什么都不丢。
    let first = std::iter::once(None)
        .chain(thresholds.iter().map(|t| Some(t + 1)))
        .map(|t| -> BTreeSet<String> {
            match t {
                None => BTreeSet::new(),
                Some(t) => parts
                    .iter()
                    .filter(|p| p.rank < t)
                    .map(|p| p.key.clone())
                    .collect(),
            }
        })
        .find(|g| build(g).is_some())
        .unwrap_or_else(|| present.clone());
    // 早丢掉的部分在放得下时回来，次序高的先回来（§4.2「丢弃的次序」）。
    let mut back: Vec<&Part> = parts.iter().filter(|p| first.contains(&p.key)).collect();
    back.sort_by_key(|p| std::cmp::Reverse(p.rank));
    let mut gone = first;
    for p in back {
        let mut g = gone.clone();
        g.remove(&p.key);
        if build(&g).is_some() {
            gone = g;
        }
    }
    let kept: BTreeSet<String> = shown(&gone).iter().map(|p| p.key.clone()).collect();
    Row {
        spans: build(&gone).unwrap_or_default(),
        gone: present.difference(&kept).cloned().collect(),
    }
}

/// 状态栏的文件夹；状态栏分开之前可以缩短成 `~/…/folder`（§4.2「一行或两行」）。
pub fn cwd_part(theme: &Theme, cwd: &str) -> Part {
    Part {
        short: short_path(cwd).map(|s| vec![Span::new(s, theme.fg3)]),
        ..Part::new("cwd", vec![Span::new(cwd.to_owned(), theme.fg3)], 40)
    }
}

/// 设置页的键与改到状态栏的说明句：占右边，次序在目标的字之后、模式之前。为空时没有。
pub fn note_parts(keys: Vec<Span>) -> Vec<Part> {
    if keys.is_empty() {
        Vec::new()
    } else {
        vec![Part {
            right: true,
            ..Part::new("note", keys, 95)
        }]
    }
}

/// `model · effort` 的部分（没有档位时只有模型）。
pub fn label_parts(theme: &Theme, model: &str, effort: &str) -> Vec<(String, Style)> {
    let mut parts = vec![(model.to_owned(), theme.fg2)];
    if !effort.is_empty() {
        parts.push((" · ".to_owned(), theme.line));
        parts.push((effort.to_owned(), theme.fg3));
    }
    parts
}

/// 带墨色的几段字（目标的标记与字）。
pub type Inked = Vec<(String, Style)>;

/// 左边要说的，每个带它变了多少 tick（从那时淡入）。
///
/// - `mode`：在用的模式与计划模式开没开；`goal`：目标的标记与字（壳按目标写好）；`model`：模型与档位。
/// - `cwd`：写短了家目录的文件夹；`branch`：它的分支；`machines`：就绪的远端机器（`@dev-box +1`）。
#[derive(Clone, Debug, Default)]
pub struct Left<'a> {
    pub mode: Option<(Mode, bool, i64)>,
    pub goal: Option<(Inked, Inked, i64)>,
    pub model: Option<(&'a str, &'a str, i64)>,
    pub cwd: &'a str,
    pub branch: Option<(&'a str, i64)>,
    pub machines: Option<&'a str>,
}

/// 左边的部分（§4.2「状态栏」）：权限模式带 line 墨色的 `⇧⇥` 领头、最后走；目标的标记跟着模式，字先于模式丢掉；模型与
/// 档位；cwd 与分支；就绪的远端机器是附加的部分，窄时最先走。
pub fn left(theme: &Theme, l: &Left<'_>) -> Vec<Part> {
    let none = Some(Vec::new());
    let mut out = Vec::new();
    if let Some((mode, plan, age)) = l.mode {
        let ms = fade::parts(theme, modes::parts(theme, mode, plan), age, None);
        out.push(Part::new("mode", ms, 100));
        // 换模式的键挨着它换的东西，在目标的标记之前（§4.2「目标」的「状态栏」）。
        out.push(Part {
            glue: none.clone(),
            extra: true,
            after: Some("mode".to_owned()),
            ..Part::new(
                "tab",
                vec![Span::new(" ", Style::DEFAULT), Span::new("⇧⇥", theme.line)],
                10,
            )
        });
    }
    // 目标的标记跟着模式，字在等你时才有，先于模式丢掉（§4.2「目标」的「状态栏」）。
    if let Some((m, w, age)) = &l.goal {
        let (mark, words) = (
            fade::parts(theme, m.clone(), *age, None),
            fade::parts(theme, w.clone(), *age, None),
        );
        if !mark.is_empty() {
            out.push(Part {
                glue: Some(vec![Span::new(" ", Style::DEFAULT)]),
                ..Part::new("goal", mark, 100)
            });
        }
        if !words.is_empty() {
            out.push(Part {
                glue: none,
                after: Some("goal".to_owned()),
                ..Part::new("goalWords", words, 70)
            });
        }
    }
    if let Some((model, effort, age)) = l.model {
        out.push(Part::new(
            "model",
            fade::parts(theme, label_parts(theme, model, effort), age, None),
            60,
        ));
    }
    out.push(cwd_part(theme, l.cwd));
    if let Some((b, age)) = l.branch {
        out.push(Part {
            glue: Some(vec![Span::new(" · ", theme.line)]),
            extra: true,
            after: Some("cwd".to_owned()),
            ..Part::new(
                "branch",
                fade::fade_in(
                    theme,
                    vec![Span::new(b.to_owned(), theme.fg3)],
                    age,
                    fade::TICKS,
                    None,
                ),
                20,
            )
        });
    }
    // 就绪的远端机器（spec §11.1）：附加的部分，窄时最先走。
    if let Some(at) = l.machines {
        out.push(Part {
            extra: true,
            ..Part::new("machines", vec![Span::new(at.to_owned(), theme.fg3)], 5)
        });
    }
    out
}

/// shell 模式的左边（§4.2「Shell 命令（!）」）：`Shell`（琥珀色，进入 shell 模式 `age` tick 之后淡入完）与下一条命令运行
/// 的文件夹，没有 `⇧⇥`。
pub fn shell_left(theme: &Theme, age: i64, folder: &str) -> Vec<Part> {
    vec![
        Part::new(
            "mode",
            fade::parts(
                theme,
                vec![("Shell".to_owned(), Style::fg(theme.code))],
                age,
                None,
            ),
            100,
        ),
        cwd_part(theme, folder),
    ]
}
