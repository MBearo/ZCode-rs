// 复制自 apps/zcode-host/crates/tui/src/ui/transcript.rs（feat/rs 79b6691c55），按 400 行上限拆成两个文件；
// 组装部分在 `transcript_lines.rs`。
//! transcript 的排版（rust-tui.md §8.1、§8.2）：会话头、cell、本地 cell 依次排列。块之间空不空行照 Puffin 的间距（Hang，
//! docs/specs/rust-cli/tui-kit.md §4.2「间距」，`shell/view/entries.rs` 的 `gap_after` 与 `hangs`，规则在
//! `transcript_lines.rs`）；以前每两个块之间都空一行。
//! 排版结果按块缓存（行版本、宽度、展开、明暗、类型），布局共享缓存里的行，不复制；还在变化的 cell
//! 每帧重排。普通视图与详细视图各一份缓存。
//!
//! 行是 kit 的 [`StyledLine`]（tui-kit.md §3.1）：读一行是 [`Layout::styled`]，读一段是 [`Layout::styled_lines`]。
use std::cell::OnceCell;
use std::ops::Range;
use std::rc::Rc;

use rust_cli_tui_kit::render::StyledLine;

use super::find_text::Folded;

/// transcript 里一条用户消息占的行（`[start, end)`），供固定提问与回退浏览使用。
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub start: usize,
    pub end: usize,
    pub row_id: u64,
    pub text: String,
}

/// 一个块排好的行；布局与缓存共享它。搜索用的索引（每行转小写的文字与列，tui-input.md §2.3.3）在第一次搜索时算出并随块
/// 保留。
pub struct Rendered {
    pub lines: Vec<StyledLine>,
    /// 能不能点击展开（§8.2）。
    pub expandable: bool,
    folded: OnceCell<Vec<Folded>>,
}

impl Rendered {
    fn new(lines: Vec<StyledLine>) -> Rc<Self> {
        Self::cell(lines, false)
    }

    fn cell(lines: Vec<StyledLine>, expandable: bool) -> Rc<Self> {
        Rc::new(Self {
            lines,
            expandable,
            folded: OnceCell::new(),
        })
    }

    fn folded(&self) -> &[Folded] {
        self.folded.get_or_init(|| {
            self.lines
                .iter()
                .map(|line| Folded::new(&line.plain()))
                .collect()
        })
    }
}

/// 布局里的一个块：从第 `start` 行开始；`gap` 为真时后面跟一个空行（算这个块的下一行）。`full` 是折叠着的块的完整形式
/// （点击展开后的样子，只在搜索模式里排，tui-input.md §2.3.2）；为 None 时显示的就是完整形式。
pub struct Block {
    pub key: u64,
    pub start: usize,
    pub rendered: Rc<Rendered>,
    pub gap: bool,
    pub full: Option<Rc<Rendered>>,
}

impl Block {
    fn height(&self) -> usize {
        self.rendered.lines.len() + usize::from(self.gap)
    }
}

/// 搜索模式的一处匹配（按块的完整形式找）：第 `block` 个块（键 `key`）里的第 `ordinal` 处，在完整形式的第 `line` 行、
/// `[from, to)` 列。`expandable`：这一块能点击展开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub block: usize,
    pub key: u64,
    pub ordinal: usize,
    pub line: usize,
    pub from: usize,
    pub to: usize,
    pub expandable: bool,
}

/// 挑选回退的点时给切线下面的行变暗（`rewind_pick::dimmer`）。
pub type Dim = Rc<dyn Fn(StyledLine) -> StyledLine>;

/// 排版结果：按块引用缓存里的行（每帧不复制全部行）与其中的用户消息。每行的归属是（块的键，块内第几行）；
/// 块之间的空行算前一个块的下一行。鼠标选区按归属定位，新内容追加时选区不漂移。
#[derive(Default)]
pub struct Layout {
    pub blocks: Vec<Block>,
    pub prompts: Vec<Prompt>,
    /// 排头的请求的行（等你回答的工具行，或没有工具行的请求自己的行）的第一行不空的行（Puffin `transcript_frame` 的
    /// `ask_line`）：请求的行与预览比正文高时，跟随停在这一行在顶上（tui-kit.md §4.0「请求」）。
    pub ask_line: Option<usize>,
    /// 挑选回退的点时切线所在的行（Puffin `transcript_frame` 的 `cut_line`）：视图停在它上方留 `ABOVE` 行，下面的部分只截在
    /// 块的边界上（`draw.rs`）。
    pub cut_line: Option<usize>,
    /// 挑选时从这一行起变暗（点和它下面的全部，kit `rewind_rows::dimmer`）：读出可见行时才变，缓存里的行不动。
    pub dim: Option<(usize, Dim)>,
    len: usize,
    blank: StyledLine,
}

impl Layout {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 追加一个块（与前一个块之间空 1 行）；返回块的第一行行号。空块不占行。
    pub fn push(&mut self, key: u64, rendered: Rc<Rendered>) -> Option<usize> {
        self.push_full(key, rendered, None)
    }

    /// 同 [`Self::push`]，另带折叠着的块的完整形式（搜索模式，tui-input.md §2.3.2）。
    pub fn push_full(
        &mut self,
        key: u64,
        rendered: Rc<Rendered>,
        full: Option<Rc<Rendered>>,
    ) -> Option<usize> {
        self.push_spaced(key, rendered, full, true)
    }

    /// 同 [`Self::push_full`]，`gap` 说与前一个块之间空不空一行（Puffin 的间距，挂着的块不空）。
    pub fn push_spaced(
        &mut self,
        key: u64,
        rendered: Rc<Rendered>,
        full: Option<Rc<Rendered>>,
        gap: bool,
    ) -> Option<usize> {
        if rendered.lines.is_empty() {
            return None;
        }
        if gap && let Some(previous) = self.blocks.last_mut() {
            previous.gap = true;
            self.len += 1;
        }
        let start = self.len;
        self.len += rendered.lines.len();
        self.blocks.push(Block {
            key,
            start,
            rendered,
            gap: false,
            full,
        });
        Some(start)
    }

    /// 第 `index` 行所在的块能点击展开时，块的键（§8.2）。
    pub fn expandable(&self, index: usize) -> Option<u64> {
        let (block, _) = self.locate(index);
        block.rendered.expandable.then_some(block.key)
    }

    /// 第 `index` 行所在的块与块内行号。
    fn locate(&self, index: usize) -> (&Block, usize) {
        let at = self.blocks.partition_point(|b| b.start <= index) - 1;
        let block = &self.blocks[at];
        (block, index - block.start)
    }

    /// 第 `index` 行（块之间的空行是空行）。
    pub fn styled(&self, index: usize) -> &StyledLine {
        let (block, offset) = self.locate(index);
        block.rendered.lines.get(offset).unwrap_or(&self.blank)
    }

    pub fn owner(&self, index: usize) -> (u64, usize) {
        let (block, offset) = self.locate(index);
        (block.key, offset)
    }

    /// 归属为（`key`，`offset`）的行号。
    pub fn position(&self, key: u64, offset: usize) -> Option<usize> {
        self.blocks
            .iter()
            .find(|b| b.key == key && offset < b.height())
            .map(|b| b.start + offset)
    }

    /// 键为 `key` 的块里第 `offset` 行的行号；`offset` 超出块的高度时取块的最后一行（宽度变了、块变矮了）。
    pub fn at(&self, key: u64, offset: usize) -> Option<usize> {
        let block = self.blocks.iter().find(|b| b.key == key)?;
        Some(block.start + offset.min(block.height().saturating_sub(1)))
    }

    /// 键为 `key` 的块是第几个块。
    pub fn block_index(&self, key: u64) -> Option<usize> {
        self.blocks.iter().position(|b| b.key == key)
    }

    /// `[start, end)` 的行（复制；超出部分截掉）。只有绘制的可见行与选中的行需要复制。
    pub fn styled_lines(&self, start: usize, end: usize) -> Vec<StyledLine> {
        (start..end.min(self.len))
            .map(|index| {
                let line = self.styled(index).clone();
                match &self.dim {
                    Some((from, dim)) if index >= *from => dim(line),
                    _ => line,
                }
            })
            .collect()
    }

    /// 显示的 `lines` 行范围里 `needle`（转过小写的查询）的出现：（行号，起始列，结束列）。规则见 `find_text`（不区分
    /// 大小写、按字素）。
    ///
    /// 缺陷修复（修复线 F114）：以前转小写后长度变了的整行（例如含 `İ`）跳过，这样的行里什么都搜不到；现在每个字素各自
    /// 转小写，列按字素的位置换算。
    pub fn find_in(&self, needle: &str, lines: Range<usize>) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        if needle.is_empty() {
            return out;
        }
        for block in &self.blocks {
            let end = block.start + block.rendered.lines.len();
            if end <= lines.start || block.start >= lines.end {
                continue;
            }
            for (offset, folded) in block.rendered.folded().iter().enumerate() {
                let line = block.start + offset;
                if lines.contains(&line) {
                    out.extend(folded.find(needle).into_iter().map(|(a, b)| (line, a, b)));
                }
            }
        }
        out
    }

    /// 搜索模式的匹配（tui-input.md §2.3.2）：每个块按完整形式找，按块、行、列的顺序。
    pub fn targets(&self, needle: &str) -> Vec<Target> {
        let mut out = Vec::new();
        if needle.is_empty() {
            return out;
        }
        for (index, block) in self.blocks.iter().enumerate() {
            let form = block.full.as_ref().unwrap_or(&block.rendered);
            let mut ordinal = 0;
            for (line, folded) in form.folded().iter().enumerate() {
                for (from, to) in folded.find(needle) {
                    out.push(Target {
                        block: index,
                        key: block.key,
                        ordinal,
                        line,
                        from,
                        to,
                        expandable: block.rendered.expandable,
                    });
                    ordinal += 1;
                }
            }
        }
        out
    }
}

/// 块的键：cell 用它的第一个行 id；会话头、本地 cell 用保留值。
pub const HEADER_KEY: u64 = u64::MAX;

fn local_key(index: usize) -> u64 {
    u64::MAX - 1 - index as u64
}

#[path = "transcript_lines.rs"]
mod compose;
#[path = "transcript_motion.rs"]
mod motion;
#[path = "transcript_tail.rs"]
mod tail;

pub use compose::{Cache, lines, pinned};
pub use tail::outgoing;

#[cfg(test)]
#[path = "transcript_duration_tests.rs"]
mod duration_tests;
