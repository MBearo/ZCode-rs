//! 选区的单位与复制出的文字（docs/puffin-tui.md §4.4「选择与复制」；docs/specs/rust-cli/tui-kit.md §4.4）：双击的词、
//! 三击的逻辑行、选中的行复制成什么字。纯函数，入参是铺好的行（带软折行的 `cont`）；位置是（transcript 的行，格）。
//! Puffin 的 `TranscriptView` 与旧界面的选区都用它，两个界面选中与复制的是同一段。
//! 移植自 Puffin 的 shell/transcript.rs 的 `word_at`、`line_at`、`glyphs` 与 transcript/copy.rs
//! （TranscriptView.scala）。
use crate::app::widgets::playhead;
use crate::render::StyledLine;
use crate::render::line::Continuation;
use crate::render::width::display_width;

/// 一个铺好的行上的字：码点、transcript 的行（软折行丢掉的空格是 -1）、第一格与格数。
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub cp: char,
    pub line: i64,
    pub col: usize,
    pub width: usize,
}

impl Glyph {
    fn pos(&self) -> (usize, usize) {
        (self.line.max(0) as usize, self.col)
    }
}

/// 字母与数字之外词里有的：够写路径、`file:line`、URL（§4.4）。
const WORD_PUNCT: &str = "_-./~:@#%+=?&";
/// 一个词在句末时不以它们结尾。
const TRAILING: &str = ".:?";

fn is_mark(c: char) -> bool {
    let u = c as u32;
    (0x300..=0x36f).contains(&u)
        || (0x483..=0x489).contains(&u)
        || (0x591..=0x5bd).contains(&u)
        || (0x610..=0x61a).contains(&u)
        || (0x64b..=0x65f).contains(&u)
        || (0x900..=0x903).contains(&u)
        || (0x93a..=0x94f).contains(&u)
        || (0x1ab0..=0x1aff).contains(&u)
        || (0x1dc0..=0x1dff).contains(&u)
        || (0x20d0..=0x20ff).contains(&u)
        || (0xfe20..=0xfe2f).contains(&u)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || WORD_PUNCT.contains(c) || is_mark(c)
}

/// 假名与汉字：词之间没有空格的文字。
fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x3040..=0x30ff).contains(&u)
        || (0x31f0..=0x31ff).contains(&u)
        || (0x3400..=0x4dbf).contains(&u)
        || (0x4e00..=0x9fff).contains(&u)
        || (0xf900..=0xfaff).contains(&u)
        || (0x20000..=0x3134f).contains(&u)
}

/// 铺好的行 `row`（transcript 的第 `line` 行）上的字。
fn glyphs_of(row: &StyledLine, line: i64) -> Vec<Glyph> {
    let mut out = Vec::new();
    let mut cell = 0;
    for sp in &row.spans {
        for cp in sp.text.chars() {
            let w = display_width(cp);
            out.push(Glyph {
                cp,
                line,
                col: cell,
                width: w,
            });
            cell += w;
        }
    }
    out
}

/// `cps` 里第 `k` 个码点所在的词的第一个与最后一个码点（自带的切分）。
fn segment_at(cps: &[char], k: usize) -> Option<(usize, usize)> {
    let text: String = cps.iter().collect();
    // 每个码点的字节偏移与末尾。
    let mut offs = Vec::with_capacity(cps.len() + 1);
    let mut o = 0;
    for c in cps {
        offs.push(o);
        o += c.len_utf8();
    }
    offs.push(o);
    playhead::segments(&text)
        .into_iter()
        .find(|s| offs[k] >= s.start && offs[k] < s.end)
        .map(|s| {
            let a = offs.iter().position(|x| *x >= s.start).unwrap_or(0);
            let b = offs.iter().rposition(|x| *x < s.end).unwrap_or(a);
            (a, b)
        })
}

/// `rows[i]` 所在的逻辑行的字，按阅读次序：`rows` 是一个条目的行（软折行不越过条目），它的第一行是 transcript 的第
/// `base` 行。每一行从它的文字开始处起，不带软折行在行尾留下的空格；丢掉的空格回来成一个没有格子的字（行 -1）。
pub fn logical(rows: &[StyledLine], i: usize, base: usize) -> Vec<Glyph> {
    if i >= rows.len() {
        return Vec::new();
    }
    let mut a = i;
    while a > 0 && rows[a].cont.is_some() {
        a -= 1;
    }
    let mut b = i;
    while b + 1 < rows.len() && rows[b + 1].cont.is_some() {
        b += 1;
    }
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate().take(b + 1).skip(a) {
        if r > a && row.cont.is_some_and(|c: Continuation| c.space) {
            out.push(Glyph {
                cp: ' ',
                line: -1,
                col: 0,
                width: 0,
            });
        }
        let from = if r == a {
            0
        } else {
            row.cont.map_or(0, |c| c.at)
        };
        let mut cells: Vec<Glyph> = glyphs_of(row, (base + r) as i64)
            .into_iter()
            .filter(|g| g.col >= from)
            .collect();
        if r < b {
            while cells.last().is_some_and(|g| g.cp == ' ') {
                cells.pop();
            }
        }
        out.extend(cells);
    }
    out
}

/// 逻辑行 `gs`（[`logical`]）里 `(line, col)` 下的词，第一格与最后一格（§4.4），跨过软折行；不是词的一部分的字单独一个。
/// 那里没有字时为 `None`。
pub fn word_at(gs: &[Glyph], line: usize, col: usize) -> Option<((usize, usize), (usize, usize))> {
    let k = gs
        .iter()
        .position(|g| g.line == line as i64 && col >= g.col && col < g.col + g.width.max(1))?;
    if !is_word(gs[k].cp) {
        return Some((gs[k].pos(), gs[k].pos()));
    }
    let run = |ok: &dyn Fn(char) -> bool| {
        let mut a = k;
        while a > 0 && ok(gs[a - 1].cp) {
            a -= 1;
        }
        let mut b = k;
        while b + 1 < gs.len() && ok(gs[b + 1].cp) {
            b += 1;
        }
        (a, b)
    };
    let (a, b) = if is_cjk(gs[k].cp) {
        // 中文与日文没有空格：在这一串里找词。
        let (p, q) = run(&is_cjk);
        let cps: Vec<char> = gs[p..=q].iter().map(|g| g.cp).collect();
        segment_at(&cps, k - p).map_or((p, q), |(x, y)| (p + x, p + y))
    } else {
        let (p, q) = run(&is_word);
        // 句子的句号、冒号或问号不是词的。
        let mut e = q;
        while e > k && TRAILING.contains(gs[e].cp) {
            e -= 1;
        }
        (p, e)
    };
    Some((gs[a].pos(), gs[b].pos()))
}

/// 逻辑行 `gs`（[`logical`]），从第一个到最后一个不空的格；空的时为 `None`。
pub fn line_at(gs: &[Glyph]) -> Option<((usize, usize), (usize, usize))> {
    let ink: Vec<&Glyph> = gs
        .iter()
        .filter(|g| g.line >= 0 && g.cp != ' ' && g.cp != '\u{a0}')
        .collect();
    Some((ink.first()?.pos(), ink.last()?.pos()))
}

/// Java 正则的 `\s`。
fn re_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// Java 的 `trim`：去掉两头不大于 U+0020 的字符。
fn jtrim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// 选中的行 `ls` 复制出的文字（§4.4）：第一行从第 `c1` 格起，最后一行到第 `c2` 格（含）。软折行的行接到上一行（带着折行
/// 丢掉的空格），只由半块边（板与代码面板的边）组成的行不要，每行去掉行尾空白，所有行去掉共同的缩进。
///
/// 修复原因（Scala）：以前每个屏幕行都用换行连接，拖选一段因宽度折成几行的文字，复制出的文字在每个折行处多了换行。排版
/// 现在记下软换行（StyledLine.cont），复制时把它接回上一行。
pub fn copy_text(ls: &[StyledLine], c1: usize, c2: usize) -> String {
    let mut rows: Vec<String> = Vec::new();
    let n = ls.len();
    for (i, line) in ls.iter().enumerate() {
        let until = if i + 1 == n { c2 + 1 } else { usize::MAX };
        match line.cont {
            Some(c) if i > 0 && !rows.is_empty() => {
                let text = cell_text(line, c.at, until);
                if let Some(r) = rows.last_mut() {
                    if c.space {
                        r.push(' ');
                    }
                    r.push_str(&text);
                }
            }
            _ => rows.push(cell_text(line, if i == 0 { c1 } else { 0 }, until)),
        }
        if let Some(r) = rows.last_mut() {
            let kept = r.trim_end_matches(re_space).len();
            r.truncate(kept);
        }
    }
    let kept: Vec<String> = rows
        .into_iter()
        .filter(|r| r.is_empty() || !jtrim(r).chars().all(|ch| ch == '▄' || ch == '▀'))
        .collect();
    let indent = kept
        .iter()
        .filter(|r| !jtrim(r).is_empty())
        .map(|r| r.chars().take_while(|c| *c == ' ').count())
        .min()
        .unwrap_or(0);
    kept.iter()
        .map(|r| {
            if r.chars().count() >= indent {
                r.chars().skip(indent).collect()
            } else {
                jtrim(r).to_owned()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// 铺好的一行在格子 `[from, until)` 里的文字。
pub fn cell_text(line: &StyledLine, from: usize, until: usize) -> String {
    let mut out = String::new();
    let mut cell = 0;
    for sp in &line.spans {
        for cp in sp.text.chars() {
            if cell >= from && cell < until {
                out.push(cp);
            }
            cell += display_width(cp);
        }
    }
    out
}
