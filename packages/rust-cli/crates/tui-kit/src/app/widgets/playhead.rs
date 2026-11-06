//! 流式文字的播放头（docs/puffin-tui.md §4.2「流式节奏」）：文字一块块不均匀地到达；播放头按跟着到达速度、但只慢慢变的
//! 速度显示它，落后约 0.3 s。每个 33 ms 的 tick 一次 [`Playhead::step`]。
//! 移植自 packages/tui/tui/src/zcode/tui/shell/Playhead.scala（feat/tui a2bf878b34）第 1–156 行。
//!
//! 与 Scala 的不同：
//! - 显示到哪里（`shown`）与笔记（`Mark`）是字节偏移（spec §3）。速度、「窗口」与「长词」的长度仍按 Scala 的单位量：
//!   一个字的权重是它的 UTF-16 单元数（BMP 之外的字是 2）。只用作权重，不存 UTF-16 偏移；按码点量时 emoji 的流式节奏与
//!   Scala 的 golden 不同（glyphs-emoji 第 17 步）。精确位置拆成整字之后的字节偏移 `at` 与剩下的单元 `frac`，每个 tick
//!   只走过新显示的字，不从头数。
//! - 词的切分是自带的规则（`playhead/words.rs`），不是 `Intl.Segmenter`（§15.1）。
//!
//! 从 Puffin 的壳（shell/playhead.rs）搬进 kit（docs/specs/rust-cli/tui-kit.md §4.4），两套界面都调；行为不变。
use crate::render::width::display_width;
use crate::theme::ease;

mod words;

pub use words::{Segment, segments};

/// 播放头的计时器的间隔（毫秒）：壳的 tick。
pub const TICK_MS: u64 = 33;
/// 状态栏的现场速度里变了的数字淡入的 tick 数（165 ms；§4.2「状态栏」的「换字」）。
pub const FADE_TICKS: i64 = 5;
const DT: f64 = TICK_MS as f64 / 1000.0;
const LAG: f64 = 0.3;
const END_LAG: f64 = 0.15;
const MIN_RATE: f64 = 20.0;
const KEEP_TICKS: i64 = ease::TICKS + 1;
/// 这么长的没有空格的一串照到达的样子显示：反正按字折行。
const LONG_WORD: usize = 24;
/// 看一串没有空格的文字的最后一个词时看多远。
const WINDOW: usize = 64;

/// 速度向目标走的时间常数是 200 ms。
fn smoothing() -> f64 {
    1.0 - (-DT / 0.2).exp()
}

/// 在 tick `tick` 显示出的字 `[from, to)`（字节）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub tick: i64,
    pub from: usize,
    pub to: usize,
}

/// 一个流式条目的播放头。
///
/// - `shown`：显示到哪里（字节）：整词，到精确位置走过的最后一个词的末尾（§4.2「笔」的「落下」）；在码点边界上，从不减少。
/// - `at` 与 `frac`：精确位置，`at` 是它走过的整字之后的字节偏移，`frac` 是还没走满一个字的单元（BMP 之外的字有 2 个
///   单元，所以可以 ≥ 1）。
/// - `speed`：上一个 tick 每秒走过的单元数。
/// - `marks`：最近几个 tick 各自显示出的字，给它们落定（§4.2「笔」的「落定」）。
/// - `ended`：条目第一次 final 且每个字都显示了的 tick：笔从那时抬起。
/// - `moved`：精确位置上次走动的 tick：笔尖在它之后停下。
#[derive(Clone, Debug, PartialEq)]
pub struct Playhead {
    pub shown: usize,
    pub at: usize,
    pub frac: f64,
    pub speed: f64,
    pub marks: Vec<Mark>,
    pub ended: Option<i64>,
    pub moved: i64,
}

impl Playhead {
    /// 从 `at`（字节）开始的播放头。
    pub fn start(at: usize) -> Playhead {
        Playhead {
            shown: at,
            at,
            frac: 0.0,
            speed: 0.0,
            marks: Vec::new(),
            ended: None,
            moved: i64::MIN / 2,
        }
    }

    /// 一个 tick 之后、在 tick `now` 的播放头，按已到达的文字。
    pub fn step(&self, text: &str, fin: bool, now: i64) -> Playhead {
        let len = text.len();
        let cap = if fin { len } else { cap(text) };
        // 精确位置到 cap 还有多少单元（cap 在它后面时为负）。
        let backlog = if cap >= self.at {
            units_between(text, self.at, cap) as f64 - self.frac
        } else {
            -(units_between(text, cap, self.at) as f64) - self.frac
        };
        let target = if backlog <= 1e-6 {
            0.0
        } else if fin {
            // final 之后不再减速：剩下的字按当前速度或更快打完。
            (backlog / END_LAG).max(MIN_RATE).max(self.speed)
        } else {
            (backlog / LAG).max(MIN_RATE)
        };
        // final 之后直接取目标速度（它不低于当前速度），最后几个字不会从零慢慢加速。
        let v = if fin {
            target
        } else {
            self.speed + (target - self.speed) * smoothing()
        };
        let delta = (v * DT).min(backlog).max(0.0);
        let (at, frac) = walk(text, self.at, self.frac + delta);
        // Scala 的位置取整后落在一对代理项中间时挪到这对之后（`snap`）：这里是剩下的单元够半个字。
        let exact = match text.get(at..).and_then(|r| r.chars().next()) {
            Some(c) if frac >= 1.0 => at + c.len_utf8(),
            _ => at,
        };
        // 整词落下：显示到播放头已走过的最后一个词的末尾，不显示半个词（§4.2「笔」的「落下」）。
        let s = self.shown.max(landing(text, exact.min(len)));
        let mut marks: Vec<Mark> = self
            .marks
            .iter()
            .copied()
            .filter(|m| now - m.tick < KEEP_TICKS)
            .collect();
        if s > self.shown {
            marks.push(Mark {
                tick: now,
                from: self.shown,
                to: s,
            });
        }
        Playhead {
            shown: s,
            at,
            frac,
            speed: delta / DT,
            marks,
            ended: self.ended.or_else(|| (fin && s >= len).then_some(now)),
            moved: if delta > 0.0 { now } else { self.moved },
        }
    }

    /// 第 `i` 个字（字节）显示出来以来的 tick，在它还近得要紧的时候。
    pub fn age_of(&self, i: usize, now: i64) -> Option<i64> {
        self.marks
            .iter()
            .find(|m| i >= m.from && i < m.to && now - m.tick < KEEP_TICKS)
            .map(|m| now - m.tick)
    }

    /// 播放头上次走动以来的 tick；没走过时是很久以前。
    pub fn idle(&self, now: i64) -> i64 {
        now.saturating_sub(self.moved)
    }

    /// 条目 final 且每个字都显示了以来的 tick；之前没有。
    pub fn ended_for(&self, now: i64) -> Option<i64> {
        self.ended.map(|t| now - t)
    }

    /// 全部显示、最后的词落定、笔抬起了（§4.2「流式节奏」的「提交」）。
    pub fn done(&self, len: usize, fin: bool, now: i64) -> bool {
        fin && self.shown >= len
            && self.marks.iter().all(|m| now - m.tick >= ease::TICKS)
            && self.ended_for(now).is_some_and(|t| t >= ease::LIFT_TICKS)
    }
}

/// 一个字的权重：它的 UTF-16 单元数（Scala 的 `String.length` 按它数）。
fn units(c: char) -> usize {
    c.len_utf16()
}

/// `text[from..to]` 的单元数。
fn units_between(text: &str, from: usize, to: usize) -> usize {
    text.get(from..to).map_or(0, |s| s.chars().map(units).sum())
}

/// 从 `from`（字节）往后走 `total` 个单元：走过放得下的整字，交回之后的字节偏移与剩下的单元。
fn walk(text: &str, from: usize, total: f64) -> (usize, f64) {
    let mut at = from;
    let mut left = total;
    // 修复原因：快照起点（`start(at)`）可以在比它短的文字之后；Scala 的位置原样不动，这里切片会越界而 panic。
    for c in text.get(from..).unwrap_or("").chars() {
        let u = units(c) as f64;
        if u > left {
            break;
        }
        left -= u;
        at += c.len_utf8();
    }
    (at, left)
}

/// 从 `from`（字节）往后至多 `n` 个单元（不跨进一个字的中间）。
fn advance(text: &str, from: usize, n: usize) -> usize {
    walk(text, from, n as f64).0
}

/// 往回至多 `n` 个单元（至多到开头，不跨进一个字的中间）。
fn back(text: &str, from: usize, n: usize) -> usize {
    let mut at = from;
    let mut left = n;
    for c in text[..from].chars().rev() {
        let u = units(c);
        if u > left {
            break;
        }
        left -= u;
        at -= c.len_utf8();
    }
    at
}

/// 条目 final 之前播放头可以走多远：到已经到达完的最后一个词的末尾（它后面有东西）。中文与日文没有空格，一行按词显示，
/// 不等满 24 个字（§4.2「流式节奏」的「播放头」）。
pub fn cap(text: &str) -> usize {
    // 只切末尾一段：从最后一个空白之后起切，词不会被截断。
    let from = text.rfind([' ', '\n']).map_or(0, |i| i + 1);
    let start = from.max(back(text, text.len(), WINDOW));
    match last_word_start(text, start) {
        Some(w) if units_between(text, w, text.len()) <= LONG_WORD => w,
        _ => text.len(),
    }
}

/// `at` 所在的一串字母的末尾（字节）：作为一块折行的东西（`at` 在空格、宽字符或末尾时就是 `at`）。宽字符自己折行，所以它
/// 后面的不预先排版。
pub fn word_end(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    while let Some(ch) = text[i..].chars().next() {
        if ch == ' ' || ch == '\n' || display_width(ch) >= 2 {
            break;
        }
        i += ch.len_utf8();
    }
    i
}

/// 精确位置在 `at`（字节）时显示到哪里：在词之间就是 `at`，在词里是这个词的开头，于是一个词整个出现。比 [`LONG_WORD`]
/// 长的、没有词尾的一串照到达的样子显示。
pub fn landing(text: &str, at: usize) -> usize {
    if at == 0 || at >= text.len() {
        return at;
    }
    let space = text[..at].rfind([' ', '\n']).map_or(0, |i| i + 1);
    let from = space.max(back(text, at, WINDOW));
    let to = advance(text, at, WINDOW);
    if from >= at {
        return at;
    }
    let seg = segments(&text[from..to])
        .into_iter()
        .map(|s| (from + s.start, from + s.end, s.word))
        .find(|(a, b, _)| *a < at && *b > at);
    match seg {
        Some((a, _, true)) if units_between(text, a, at) > LONG_WORD => at,
        Some((a, _, true)) => a,
        _ => at,
    }
}

/// `text` 的最后一个词从哪里开始（从 `from` 看起）；`text` 以不是词的东西（空格、标点）结尾时为 `None`。
fn last_word_start(text: &str, from: usize) -> Option<usize> {
    if from >= text.len() {
        return None;
    }
    segments(&text[from..])
        .last()
        .filter(|s| s.word)
        .map(|s| from + s.start)
}
