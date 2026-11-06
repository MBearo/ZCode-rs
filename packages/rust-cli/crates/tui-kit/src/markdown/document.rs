//! 增量解析的 Markdown 文档：喂给它流式回答不断增长的前缀。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/MarkdownDocument.scala（feat/tui a2bf878b34）。
//!
//! 便宜的关键在一条不变式：一个顶层块只有在下一块已经在一条**完整的行**上开始后才是最终的。
//! 解析器正是看着那一行决定这块在哪结束的（段落停在空行或块起点；缩进代码与列表越过一串空行去看下一条非空行），
//! 而完整的行不会再变。喂入时把这些块最终化（从开放尾段自己的解析推导，所以一次喂入只解析一次尾段），
//! 并把 [`Document::consumed`] 推过它们。最终化的块不可变、跨喂入共享（`Arc`），从不重解析；
//! 只有还开着的尾段（以当前块为界）会重扫，所以常见的有界块总工作量是 O(N)。
//! （唯一的例外：末尾一个无界的容器——比如后面什么都没有的很长的列表——每次喂入都重解析；
//! 对模型输出没问题，这种容器后面总会有更多文字，或者回合结束。）
//!
//! [`Document::relaid`] 是同一个想法，用在视图每帧铺出的文字上；那段文字不是只追加的：播放头的隐藏标记
//! （渲染里的 `Hidden`，U+E000）在正在打的词里前移，相邻两帧可以在 `consumed` 之后的任何地方不同，等长也一样。
//!
//! 与 Scala 的不同（行为相同）：
//! - 所有偏移是 `source` 里的**字节**偏移（Scala 是 UTF-16 单元）。偏移只落在 `\n` / `\r` 之后，两种算法逐个对应。
//! - Scala 的文档是不可变值，方法返回新文档；这里方法就地修改（`&mut self`），需要旧值时先 `clone`。
//! - Scala 的开放尾段是惰性解析的；这里在每次修改时就解析好，`blocks` / `open_blocks` / `block_ends` 不再解析。
//!   最终化之后剩下的开放块直接取自同一次解析（从块边界重解析得到的是同样的块），不再解析第二遍。
//! - Scala 的 `relaid` 在 `prefix eq source`（同一个对象）时原样返回；这里按内容相等判。
use std::sync::Arc;

use super::ast::Block;
use super::block_parser::{BlockSpan, is_blank, parse_spans, split_lines};

/// 增量解析的 Markdown 文档。
///
/// 两个文档相等当且仅当渲染出同样的块，与增量最终化走到了哪里无关：逐段喂入的文档与一次解析的文档相等。
#[derive(Clone, Debug, Default)]
pub struct Document {
    /// 已证明最终的块，按顺序。渲染的缓存按 `Arc` 的指针认块（Scala 按引用 `eq`）。
    finalized: Vec<Arc<Block>>,
    /// 已经变成 `finalized` 的前缀长度（字节；总在行边界上）。
    consumed: usize,
    /// 最后喂入的前缀，留着让 `blocks` 渲染开放尾段（含未完成的最后一行）。
    source: String,
    /// 每个最终化的块的最后一行之后在 `source` 里的偏移——`block_ends` 不重解析就能重放。
    finalized_ends: Vec<usize>,
    /// `[0, consumed)` 里单独的 `\r` 个数：归一化的切行把它们算作换行，按原始 `\n` 走的计算不算，
    /// 所以开放块的结尾要往外多找这么多行——旧的全文 `blockEnds` 的算术，留着让两边的值逐位相同。
    consumed_cr: usize,
    /// 最终化看到了多远：直到最后一个最终化块之后那一块开始的那条完整行。
    /// `relaid` 只在新文字与 `source` 一直到这里都一致时才保留最终化的块，而不只是到 `consumed`。
    checked: usize,
    /// 开放尾段（`source[consumed..]`）的块。
    open: Vec<Block>,
    /// 开放块各自的结尾行号，相对开放尾段。
    open_end_lines: Vec<usize>,
}

impl Document {
    /// 空文档。
    pub fn new() -> Self {
        Self::default()
    }

    /// 一次解析完整的文字（非增量的便利写法）。全部最终化。
    pub fn parse(text: &str) -> Self {
        let spans = parse_spans(&split_lines(text));
        let ends = offsets_after_lines(text, spans.iter().map(|sp| sp.end_line));
        Document {
            finalized: spans.into_iter().map(|sp| Arc::new(sp.block)).collect(),
            consumed: text.len(),
            source: text.to_owned(),
            finalized_ends: ends,
            consumed_cr: 0,
            checked: text.len(),
            open: Vec::new(),
            open_end_lines: Vec::new(),
        }
    }

    /// 喂入新到的文字。
    pub fn feed(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.source.push_str(delta);
        self.finalize_open();
    }

    /// 喂入目前为止揭示的整个前缀（必须以上一次的前缀开头）。
    pub fn feed_to(&mut self, prefix: &str) {
        // 只追加：等长即内容不变（每帧的便宜判断）。
        if prefix.len() == self.source.len() {
            return;
        }
        self.set_source(prefix);
        self.finalize_open();
    }

    /// 修复原因（Scala 原注）：streaming 每帧把铺出的文字整个重解析，而帧与帧之间只在末尾的残行里变——
    /// 播放头的隐藏标记在正在打的词里前移，已闭合的块不变。本方法按「最终化看过的范围内仍一致」这一弱条件
    /// 复用已最终化的块：与上一次的 `source` 在 `checked` 之前一致即可，等长但内容不同也能喂；
    /// 不一致（回退、重试、换会话）则重建。重建不回退到 [`Document::parse`]：那会把 `consumed` 顶到文本末尾，
    /// 而带隐藏标记的帧永不严格扩展上一帧，之后每帧都会重建；按最终化的语义重建（消费点停在最后一个安全行边界），
    /// 下一帧即回到增量。
    ///
    /// 修复原因（Scala 原注）：这里曾只比对到消费点；最终化还看过消费点之后下一块开始的那一行
    /// （表格分隔行、setext 下划线就在那里）。只在那一行不同时，旧段落会被留下、一次性解析却得到表格。
    /// 所以比对到 `checked` 为止。
    pub fn relaid(&mut self, prefix: &str) {
        if prefix == self.source {
            return;
        }
        if !agrees_up_to(&self.source, prefix, self.checked) {
            *self = Document {
                source: prefix.to_owned(),
                ..Document::default()
            };
        } else {
            self.set_source(prefix);
        }
        self.finalize_open();
    }

    /// 流结束：把还开着的都最终化。
    pub fn close(&mut self) {
        if self.consumed >= self.source.len() {
            return;
        }
        let region = &self.source[self.consumed..];
        let ends = offsets_after_lines(
            region,
            self.open_end_lines.iter().map(|&e| e + self.consumed_cr),
        );
        self.finalized_ends
            .extend(ends.into_iter().map(|e| e + self.consumed));
        self.finalized.extend(self.open.drain(..).map(Arc::new));
        self.open_end_lines.clear();
        self.consumed = self.source.len();
        self.checked = self.source.len();
    }

    /// 把 `source` 换成 `prefix`，沿用已有的缓冲区。
    fn set_source(&mut self, prefix: &str) {
        self.source.clear();
        self.source.push_str(prefix);
    }

    /// 解析开放尾段，并把其中已证明最终的块最终化。
    ///
    /// 修复原因（Scala 原注）：最终化的判据曾是「块尾之后再有两行完整」，它假定每种块至多多看两行；
    /// 缩进代码块把尾随空行修剪出自己的范围、却越过任意多的空行去看下一行，于是 "    a\n\n\n\n    b"
    /// 流式时画成两块代码板、提交后合成一块。判据改为「下一块已在一条完整的行上开始」。
    fn finalize_open(&mut self) {
        let tail = &self.source[self.consumed..];
        // 完整行数按原始 '\n' 计：切行会把单独的 '\r' 也算作换行，变换后的完整行只多不少，
        // 判据因此偏严——罕见的 \r 文本少复用一点，不影响正确性。
        let complete = tail.bytes().filter(|&b| b == b'\n').count();
        let lines = split_lines(tail);
        let spans = parse_spans(&lines);
        let mut cut = 0;
        let mut next = 0; // 最后保留的块之后那一块开始的（完整）行
        while cut < spans.len() {
            let mut j = spans[cut].end_line;
            while j < lines.len() && is_blank(lines[j]) {
                j += 1;
            }
            if j < lines.len() && j < complete {
                cut += 1;
                next = j;
            } else {
                break;
            }
        }
        if cut == 0 {
            self.set_open(spans, 0);
            return;
        }
        let last_end = spans[cut - 1].end_line;
        // 消费点按「归一化的换行」（'\n' 或单独的 '\r'）推进：只数原始 '\n' 会在 \r 文本里越过尾段末尾，
        // 把还开着的块也吞进已消费区。checked 同理，停在下一块开始的那一行的行尾。
        let advance = break_walk(tail, last_end);
        let ends = offsets_after_lines(
            tail,
            spans[..cut].iter().map(|sp| sp.end_line + self.consumed_cr),
        );
        let checked = self.consumed + break_walk(tail, next + 1);
        let lone = lone_cr(&self.source, self.consumed, self.consumed + advance);
        let consumed = self.consumed;
        self.finalized_ends
            .extend(ends.into_iter().map(|e| e + consumed));
        let mut spans = spans.into_iter();
        self.finalized
            .extend(spans.by_ref().take(cut).map(|sp| Arc::new(sp.block)));
        self.set_open(spans.collect(), last_end);
        self.consumed += advance;
        self.consumed_cr += lone;
        self.checked = checked;
    }

    /// 记下开放尾段的块；`shift` 是新尾段在旧尾段里开始的行号。
    fn set_open(&mut self, spans: Vec<BlockSpan>, shift: usize) {
        self.open.clear();
        self.open_end_lines.clear();
        for sp in spans {
            self.open.push(sp.block);
            self.open_end_lines.push(sp.end_line - shift);
        }
    }

    /// 已证明最终的块。
    pub fn finalized(&self) -> &[Arc<Block>] {
        &self.finalized
    }

    /// 已最终化的块数——这个下标之前的块都冷却了、按普通样子渲染；只有它及之后的块可能还在流式中。
    pub fn finalized_count(&self) -> usize {
        self.finalized.len()
    }

    /// 开放尾段（含正在打的最后一行）的临时解析；全部最终化后为空。与 `finalized` 不同，下次喂入可能变。
    pub fn open_blocks(&self) -> &[Block] {
        &self.open
    }

    /// 渲染用的全部块：最终化的块加上开放的块。
    pub fn blocks(&self) -> impl DoubleEndedIterator<Item = &Block> + '_ {
        self.finalized.iter().map(|b| &**b).chain(self.open.iter())
    }

    /// 块数（`blocks` 的长度）。
    pub fn block_count(&self) -> usize {
        self.finalized.len() + self.open.len()
    }

    /// 最后一个块。
    pub fn last_block(&self) -> Option<&Block> {
        self.open
            .last()
            .or_else(|| self.finalized.last().map(|b| &**b))
    }

    /// 已经变成 `finalized` 的前缀长度（`source` 里的字节偏移，总在行边界上）。
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    /// 最后喂入的前缀。
    pub fn source(&self) -> &str {
        &self.source
    }

    /// `blocks` 的每一块在 `source` 里结束的位置（字节），与一次全量解析报告的相同：块最后一行之后的偏移，
    /// 不超过文字末尾。表格整张落下，播放头越过它的末尾时一次落定（docs/puffin-tui.md §4.2「笔」）。
    /// 最终化的块用记下的偏移，开放尾段用本次解析的行号，不再解析。
    pub fn block_ends(&self) -> Vec<usize> {
        let mut out = self.finalized_ends.clone();
        if !self.open.is_empty() {
            let region = &self.source[self.consumed..];
            let ends = offsets_after_lines(
                region,
                self.open_end_lines.iter().map(|&e| e + self.consumed_cr),
            );
            out.extend(ends.into_iter().map(|e| e + self.consumed));
        }
        out
    }
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        self.block_count() == other.block_count() && self.blocks().eq(other.blocks())
    }
}

impl Eq for Document {}

/// `region` 里恰好越过各个（不减的）行数之后的偏移，一次向前走完。数的是原始 '\n'，即使行号来自归一化的切行
/// （CRLF 与单独的 '\r' 都算换行）——与旧的全文 `blockEnds` 同样的算术，值逐位相同
/// （行号超过原始换行数时走到区域末尾，与旧的截断相同）。
///
/// 修复原因（Scala 原注）：逐个行号从 0 扫起是 O(块数 × 文本长)；行号随块单调，一次扫完即可。
fn offsets_after_lines(region: &str, lines: impl Iterator<Item = usize>) -> Vec<usize> {
    let b = region.as_bytes();
    let mut out = Vec::new();
    let mut seen = 0;
    let mut k = 0;
    for target in lines {
        while k < b.len() && seen < target {
            if b[k] == b'\n' {
                seen += 1;
            }
            k += 1;
        }
        out.push(k);
    }
    out
}

/// `region` 里恰好越过前 `lines` 个归一化换行之后的原始偏移——'\n'，或 `split_lines` 当换行的单独 '\r'
/// （"\r\n" 里的 '\r' 不算，它的 '\n' 算）。
fn break_walk(region: &str, lines: usize) -> usize {
    let b = region.as_bytes();
    let mut seen = 0;
    let mut k = 0;
    while k < b.len() && seen < lines {
        if b[k] == b'\n' || (b[k] == b'\r' && b.get(k + 1) != Some(&b'\n')) {
            seen += 1;
        }
        k += 1;
    }
    k
}

/// `s` 的 `[from, until)` 里单独的 '\r'（不属于 "\r\n"）个数；向后看可能读到 `until` 处，不越过字符串。
fn lone_cr(s: &str, from: usize, until: usize) -> usize {
    let b = s.as_bytes();
    (from..until)
        .filter(|&k| b[k] == b'\r' && b.get(k + 1) != Some(&b'\n'))
        .count()
}

/// `b` 至少 `n` 字节长，且前 `n` 字节与 `a` 相同：足以让 `b` 保留从 `a` 最终化出的每一块。
fn agrees_up_to(a: &str, b: &str, n: usize) -> bool {
    b.len() >= n && a.as_bytes().get(..n) == b.as_bytes().get(..n)
}
