// 复制自 apps/zcode-host/crates/tui/src/ui/transcript.rs（feat/rs 79b6691c55），按 400 行上限拆成两个文件；
// 组装部分在 `transcript_lines.rs`。
//! transcript 的排版（rust-tui.md §8.1、§8.2）：会话头、cell、本地 cell 依次排列，cell 之间空一行。
//! 排版结果按块缓存（行版本、宽度、展开、明暗、类型），布局共享缓存里的行，不复制；还在变化的 cell
//! 每帧重排。普通视图与详细视图各一份缓存。
use std::cell::OnceCell;
use std::rc::Rc;

use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use super::text::plain;

/// transcript 里一条用户消息占的行（`[start, end)`），供固定提问与回退浏览使用。
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub start: usize,
    pub end: usize,
    pub row_id: u64,
    pub text: String,
}

/// 一个块排好的行；布局与缓存共享它。详细视图搜索用的（纯文字，小写）在第一次搜索时算出并随块保留。
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// 能不能点击展开（§8.2）。
    pub expandable: bool,
    texts: OnceCell<Vec<(String, String)>>,
}

impl Rendered {
    fn new(lines: Vec<Line<'static>>) -> Rc<Self> {
        Self::cell(lines, false)
    }

    fn cell(lines: Vec<Line<'static>>, expandable: bool) -> Rc<Self> {
        Rc::new(Self {
            lines,
            expandable,
            texts: OnceCell::new(),
        })
    }

    fn texts(&self) -> &[(String, String)] {
        self.texts.get_or_init(|| {
            self.lines
                .iter()
                .map(|line| {
                    let text = plain(line);
                    let lower = text.to_lowercase();
                    (text, lower)
                })
                .collect()
        })
    }
}

/// 布局里的一个块：从第 `start` 行开始；`gap` 为真时后面跟一个空行（算这个块的下一行）。
pub struct Block {
    pub key: u64,
    pub start: usize,
    pub rendered: Rc<Rendered>,
    pub gap: bool,
}

impl Block {
    fn height(&self) -> usize {
        self.rendered.lines.len() + usize::from(self.gap)
    }
}

/// 排版结果：按块引用缓存里的行（每帧不复制全部行）与其中的用户消息。每行的归属是（块的键，块内第几行）；
/// 块之间的空行算前一个块的下一行。鼠标选区按归属定位，新内容追加时选区不漂移。
#[derive(Default)]
pub struct Layout {
    pub blocks: Vec<Block>,
    pub prompts: Vec<Prompt>,
    len: usize,
    blank: Line<'static>,
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
        if rendered.lines.is_empty() {
            return None;
        }
        if let Some(previous) = self.blocks.last_mut() {
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

    pub fn line(&self, index: usize) -> &Line<'static> {
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

    /// `[start, end)` 的行（复制；超出部分截掉）。只有绘制的可见行与选中的行需要复制。
    pub fn lines(&self, start: usize, end: usize) -> Vec<Line<'static>> {
        (start..end.min(self.len))
            .map(|index| self.line(index).clone())
            .collect()
    }

    /// 不区分大小写的匹配：（行号，起始列，结束列）。小写后长度变化的行不参与（无法按字节位置换算列）。
    pub fn find(&self, query: &str) -> Vec<(usize, usize, usize)> {
        let needle = query.to_lowercase();
        let mut out = Vec::new();
        if needle.is_empty() {
            return out;
        }
        for block in &self.blocks {
            for (offset, (text, lower)) in block.rendered.texts().iter().enumerate() {
                if lower.len() != text.len() {
                    continue;
                }
                let mut from = 0;
                while let Some(at) = lower[from..].find(&needle) {
                    let start = from + at;
                    let end = start + needle.len();
                    out.push((
                        block.start + offset,
                        text[..start].width(),
                        text[..end].width(),
                    ));
                    from = end;
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

pub use compose::{Cache, lines, local_lines, pinned};
