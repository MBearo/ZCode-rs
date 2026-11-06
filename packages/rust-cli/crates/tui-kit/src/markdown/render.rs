//! 把解析好的 Markdown 文档渲染成视图 DSL 的 [`Element`]，以及流式回答每帧的增量渲染缓存。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/render/MarkdownRender.scala（feat/tui a2bf878b34）；
//! 方向 A 的样子见 docs/puffin-tui.md §4.2「回答的 Markdown」。
//!
//! 块级构造变成 [`wrap_styled`](crate::app::element::wrap_styled) 节点，折行推迟到终端宽度；行内样式是带样式的段
//! （链接的下划线颜色与 OSC 8 目标在段的 [`Decor`](crate::render::Decor) 里）。容器的前缀（列表标记、引用的栏）
//! 作为第一行 / 接续行的段传下去，嵌套直接组合，不用铺完再改。
//!
//! 正文用终端自己的前景；h1 brand，h3 及以下 fg2；圆点（按深度 `•` `◦` `▪`）与序号 fg3；任务的 `✓` / `□` 代替圆点；
//! 引用是 line 色的栏、fg2 的文字；表格没有竖线；行内代码冷灰、夹在半色调的反引号里；链接 brand、下划线更安静；
//! 删除线 fg3。rich 主题里代码块在面板上、用安静的语法墨，plain 模式在暗色的细栏后面。
//!
//! 流式回答里代理的字标成 `Written`，调用方的笔据此落定最新的字；末尾开着的块带笔尖（见 [`answer`]）。
//!
//! 与 Scala 的不同：Scala 的 `render(doc)` 等重载缺省用 `Theme.plain`，这里主题总是显式传入（`&Theme::PLAIN`）；
//! `answerBlock` 的有缓存与无缓存两个重载分成 [`answer_cached`] 与 [`answer`]。
use std::fmt;
use std::sync::Arc;

use crate::app::element::{Element, br, empty, map_lines, memo, single_line, stack};
use crate::markdown::ast::{Block, Inline};
use crate::markdown::document::Document;
use crate::render::{Color, Span, Style, attr, width};
use crate::theme::ease;
use crate::theme::theme::Theme;

mod blocks;
mod inline;

use blocks::At;

/// 内容从第 3 列开始（第 0 列是边距，第 1 列是标记，第 2 列是空格）。
pub const INDENT: &str = "   ";

/// 标出正在打的词里显示到哪里。它之后的字参与布局（词折到它结束时会在的那一行），但画成空白。
pub const HIDDEN: char = '\u{E000}';

/// plain 模式的工作光标，也是输入框的（docs/puffin-tui.md §4.2）。
pub const CURSOR_GLYPH: &str = "█";

/// 早已落定的块的年龄（Scala 的 `Long.MaxValue / 2`）。
pub const SETTLED: i64 = i64::MAX / 2;

/// 顶层块 `i` 这张表格整张落下了多少个 tick。
pub type Landed = Arc<dyn Fn(usize) -> i64 + Send + Sync>;

/// 流式回答的开放末端（docs/puffin-tui.md §4.2「流式节奏」「笔」）：`nib` 骑在下一个词要去的地方（笔尖或手停住时的点阵；空的：什么都没有）；
/// `landed(i)` 是顶层块 `i`（一张表格）整张落下后过了几个 tick，用来落定它；`ended` 说每个字都显示了，末尾的代码块
/// 合上、末尾的表格画出来。
#[derive(Clone)]
pub struct Tail {
    pub nib: Vec<Span>,
    pub landed: Landed,
    pub ended: bool,
}

impl Default for Tail {
    fn default() -> Tail {
        Tail {
            nib: Vec::new(),
            landed: Arc::new(|_| SETTLED),
            ended: false,
        }
    }
}

impl fmt::Debug for Tail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tail")
            .field("nib", &self.nib)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl Tail {
    /// 带笔尖（或不带）、每块早已落定、还没结束的尾巴。
    pub fn new(nib: Option<Span>) -> Tail {
        Tail::marked(nib.into_iter().collect())
    }

    /// 同 [`Tail::new`]，骑着的是几个段（手停住时的点阵）。
    pub fn marked(nib: Vec<Span>) -> Tail {
        Tail {
            nib,
            ..Tail::default()
        }
    }

    pub fn with_landed(mut self, f: impl Fn(usize) -> i64 + Send + Sync + 'static) -> Tail {
        self.landed = Arc::new(f);
        self
    }

    pub fn with_ended(mut self, ended: bool) -> Tail {
        self.ended = ended;
        self
    }

    fn landed_at(&self, i: usize) -> i64 {
        (self.landed)(i)
    }
}

/// 一篇回答的增量渲染缓存，回答流式到达时每帧喂一次。
///
/// [`Document`] 保证最终化的块不可变、只追加，所以它们的 Element 只渲染一次、从这里重放；每个都包在
/// [`memo`](crate::app::element::memo) 里，占大头的折行与布局在每个宽度上也只付一次。每帧只重渲染开放的尾段。
///
/// 构造上就安全：缓存以它渲染的最后一个最终化块的身份（`Arc` 指针；缓存持有这个 `Arc`，地址不会被重用）与主题为键。
/// 交给它一篇**别的**文档（新回合复用这个位置、两篇回答共用一个缓存）或换了主题，键不符，从头重建——退化成无缓存的
/// 代价，从不退化成过时的输出。归视图所有（单线程），字段直接改。
#[derive(Debug, Default)]
pub struct AnswerRenderCache {
    elems: Vec<Element>,
    last_final: Option<Arc<Block>>,
    theme: Option<Theme>,
}

impl AnswerRenderCache {
    pub fn new() -> AnswerRenderCache {
        AnswerRenderCache::default()
    }
}

/// 渲染一篇完全落定的文档（不发光）：已提交的回答、测试。
pub fn render(doc: &Document, theme: &Theme) -> Element {
    Md::new(theme, false).render(&doc.blocks().collect::<Vec<_>>())
}

/// 同 [`render`]，直接给块。
pub fn render_blocks<'a>(blocks: impl IntoIterator<Item = &'a Block>, theme: &Theme) -> Element {
    Md::new(theme, false).render(&blocks.into_iter().collect::<Vec<_>>())
}

/// 渲染一篇流式回答：有 `tail` 时它的字标成 `Written`、开放的块带笔尖；`None` 按已提交渲染。
pub fn answer(doc: &Document, tail: Option<&Tail>, theme: &Theme) -> Element {
    Md::new(theme, tail.is_some()).answer(doc, tail)
}

/// 每帧流式路径上的 [`answer`]：输出相同，但最终化块的 Element 来自 `cache`（只渲染一次、布局记忆），
/// 只重渲染开放的尾段。
pub fn answer_cached(
    doc: &Document,
    tail: Option<&Tail>,
    cache: &mut AnswerRenderCache,
    theme: &Theme,
) -> Element {
    Md::new(theme, tail.is_some()).answer_cached(doc, tail, cache)
}

/// 流式回答的笔尖是否在文字末尾：末块是文字（段落或标题）、代码、还在到达的表格、最后一条以它们之一结尾的列表，
/// 或者还没有块。别的块不带笔尖。
pub fn rides_tail(doc: &Document) -> bool {
    doc.last_block().is_none_or(rides_block)
}

/// 回答以一个整块出现前不画的东西结尾：还在到达的表格，或第一行以 `|` 开头的段落（可能是 `|---|` 行到来前的表头）。
/// 笔尖停在它的位置（§4.2「流式节奏」的「表格」）。
pub fn waits(doc: &Document) -> bool {
    doc.last_block().is_some_and(waits_block)
}

fn waits_block(b: &Block) -> bool {
    match b {
        Block::Table { .. } => true,
        Block::Paragraph(content) => piped(content),
        Block::List { items, .. } => last_item_block(items).is_some_and(waits_block),
        Block::Quote(inner) => inner.last().is_some_and(waits_block),
        _ => false,
    }
}

fn rides_block(b: &Block) -> bool {
    match b {
        Block::Paragraph(_) | Block::Heading { .. } | Block::Code { .. } | Block::Table { .. } => {
            true
        }
        Block::List { items, .. } => last_item_block(items).is_some_and(rides_block),
        _ => false,
    }
}

fn last_item_block(items: &[crate::markdown::ast::ListItem]) -> Option<&Block> {
    items.last().and_then(|i| i.blocks.last())
}

/// 段落的第一段文字以 `|` 开头。
fn piped(content: &[Inline]) -> bool {
    matches!(content.first(), Some(Inline::Text(v)) if v.starts_with('|'))
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|sp| width::string_width(&sp.text)).sum()
}

fn is_table(b: &Block) -> bool {
    matches!(b, Block::Table { .. })
}

/// 一次渲染的上下文。`written`：把代理的字标成 `Written`（回答在流式中）。
struct Md {
    theme: Theme,
    written: bool,
    /// 链接：brand、下划线。
    link_style: Style,
    /// 链接的下划线：brand 向背景混合 45 %，长链接下的线比字安静。plain 模式用终端的。
    link_underline: Color,
}

impl Md {
    fn new(theme: &Theme, written: bool) -> Md {
        let link_underline = match (theme.brand, theme.bg) {
            (Color::TrueColor(r, g, b), Some(bg)) => {
                crate::theme::Rgb::new(r, g, b).mix(bg, 0.45).color()
            }
            _ => Color::Default,
        };
        Md {
            theme: *theme,
            written,
            link_style: Style::new(theme.brand, Color::Default, attr::UNDERLINE),
            link_underline,
        }
    }

    fn render(&self, blocks: &[&Block]) -> Element {
        // 顶层的第一块与之后的块前缀相同（`At::top().rest()` 就是 `At::top()`）。
        let parts = blocks
            .iter()
            .map(|&b| (b, self.render_block(b, &At::top())))
            .collect();
        self.join_blocks(parts, true)
    }

    /// 只有笔尖的一行（还没有块）。
    fn nib_only(&self, t: &Tail) -> Element {
        if t.nib.is_empty() {
            return empty();
        }
        let mut spans = vec![Span::plain(INDENT)];
        spans.extend_from_slice(&t.nib);
        single_line(spans, false)
    }

    /// 末块：开放地渲染；结束了的表格也要落定。
    fn open_last(&self, b: &Block, t: &Tail, index: usize) -> Element {
        let e = self.render_open(b, &At::top(), t);
        if is_table(b) && t.ended {
            self.landed(b, e, t.landed_at(index))
        } else {
            e
        }
    }

    fn answer(&self, doc: &Document, tail: Option<&Tail>) -> Element {
        let blocks: Vec<&Block> = doc.blocks().collect();
        let Some(t) = tail else {
            return self.render(&blocks);
        };
        let Some((&last, init)) = blocks.split_last() else {
            return self.nib_only(t);
        };
        let mut parts: Vec<(&Block, Element)> = init
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                (
                    b,
                    self.landed(b, self.render_block(b, &At::top()), t.landed_at(i)),
                )
            })
            .collect();
        let open = self.open_last(last, t, blocks.len() - 1);
        // 不画的块（还在到达的表格）也不留它前面的空行。
        if !open.is_blank() {
            parts.push((last, open));
        }
        self.join_blocks(parts, true)
    }

    /// 修复原因（Scala 的同名注释）：answerCached 起初相对 [`Md::answer`] 漏了两处 landed——非末块（已最终化与开放
    /// 尾段里的）表格的落定动画，和末块 Table + ended 的落定。「表格整张落定一次」是 spec 行为（§4.2「笔」的「表格」），
    /// 缓存的测试在流式喂入的每一步、动画的每个年龄上逐字节比对缓存版与无缓存版钉住了它。动画中的表格每帧把缓存件临时
    /// 包一层 `map_lines`（`Element::Map`），不写回缓存；动画结束（`ease::amount ≥ 1`）`landed` 恒等，直接用缓存件。
    /// `t.landed(i)` 的 `i` 按 finalized ++ open_blocks（== doc.blocks）的全局序。
    fn answer_cached(
        &self,
        doc: &Document,
        tail: Option<&Tail>,
        cache: &mut AnswerRenderCache,
    ) -> Element {
        let fs = doc.finalized();
        let open = doc.open_blocks();
        let fin = self.finalized_elems(fs, cache);
        let Some(t) = tail else {
            let mut parts: Vec<(&Block, Element)> = fs.iter().map(|b| &**b).zip(fin).collect();
            parts.extend(open.iter().map(|b| (b, self.render_block(b, &At::top()))));
            return self.join_blocks(parts, true);
        };
        if fs.is_empty() && open.is_empty() {
            return self.nib_only(t);
        }
        let mut parts: Vec<(&Block, Element)> = fs
            .iter()
            .zip(fin)
            .enumerate()
            .map(|(i, (b, e))| (&**b, self.landed(b, e, t.landed_at(i))))
            .collect();
        if let Some((last, init)) = open.split_last() {
            parts.extend(init.iter().enumerate().map(|(k, b)| {
                let e = self.render_block(b, &At::top());
                (b, self.landed(b, e, t.landed_at(fs.len() + k)))
            }));
            let open_elem = self.open_last(last, t, fs.len() + open.len() - 1);
            if !open_elem.is_blank() {
                parts.push((last, open_elem));
            }
        } else {
            // 全部最终化了（一篇已经关上、仍在流式的文档——少见）：开放的块是最后一个最终化的块，其余仍用缓存。
            // 与 Scala 相同，这里即使末块不画（空元素）也保留它和它前面的空行，不像上面两处那样丢掉。
            let last = &*fs[fs.len() - 1];
            parts.pop();
            parts.push((last, self.open_last(last, t, fs.len() - 1)));
        }
        self.join_blocks(parts, true)
    }

    /// 最终化块的 Element，经 `cache` 增量地长：只追加的增长只渲染新的尾巴；最后一个共有块的身份不符（别的文档）
    /// 或换了主题时重置。返回的是缓存件的克隆（`Element::Memo` 共享同一个 `Arc`，布局记忆跨帧有效）。
    fn finalized_elems(&self, fin: &[Arc<Block>], cache: &mut AnswerRenderCache) -> Vec<Element> {
        let n = fin.len();
        if n == 0 {
            return Vec::new();
        }
        let have = cache.elems.len();
        let keep = have > 0
            && have <= n
            && cache
                .last_final
                .as_ref()
                .is_some_and(|lf| Arc::ptr_eq(lf, &fin[have - 1]))
            && cache.theme == Some(self.theme);
        if !keep {
            cache.elems.clear();
        }
        if cache.elems.len() < n {
            for b in &fin[cache.elems.len()..] {
                cache.elems.push(memo(self.render_block(b, &At::top())));
            }
            cache.last_final = Some(Arc::clone(&fin[n - 1]));
            cache.theme = Some(self.theme);
        }
        cache.elems.clone()
    }

    /// 整张落下的表格在 `age` 个 tick 里一起落定一次（§4.2「笔」的「表格」）。
    fn landed(&self, b: &Block, e: Element, age: i64) -> Element {
        if is_table(b) && ease::amount(age) < 1.0 {
            let theme = self.theme;
            map_lines(e, move |lines| ease::landed(&theme, age, lines))
        } else {
            e
        }
    }

    /* ---- 块的序列 ---- */

    /// 块之间空一行，代码面板旁边除外：它半格的上下边已经给了半行的空气（docs/puffin-tui.md §4.2）。
    fn join_blocks(&self, parts: Vec<(&Block, Element)>, spaced: bool) -> Element {
        if parts.is_empty() {
            return empty();
        }
        if !spaced {
            return stack(parts.into_iter().map(|(_, e)| e));
        }
        let mut out = Vec::with_capacity(parts.len() * 2);
        let mut prev: Option<&Block> = None;
        for (b, e) in parts {
            if prev.is_some_and(|p| !self.panelled(p) && !self.panelled(b)) {
                out.push(br());
            }
            out.push(e);
            prev = Some(b);
        }
        Element::Stack(out)
    }

    fn panelled(&self, b: &Block) -> bool {
        self.theme.panel.is_some() && matches!(b, Block::Code { .. })
    }

    fn join_spaced(&self, elems: Vec<Element>, spaced: bool) -> Element {
        if elems.is_empty() {
            return empty();
        }
        if !spaced {
            return Element::Stack(elems);
        }
        let mut out = Vec::with_capacity(elems.len() * 2);
        for (i, e) in elems.into_iter().enumerate() {
            if i > 0 {
                out.push(br());
            }
            out.push(e);
        }
        Element::Stack(out)
    }
}
