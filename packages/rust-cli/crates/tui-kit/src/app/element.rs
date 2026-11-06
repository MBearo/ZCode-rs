//! 视图的 DSL：文字、颜色、样式、竖向堆叠、换行、横线、转圈……由 [`Layout`](super::layout::Layout) 铺成样式行。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Element.scala（feat/tui a2bf878b34）。
//!
//! 不变量：Element 与宽度无关。节点的值里不能带终端宽度；一切按宽度的重排（横线展开、表格定宽、文字折行）都推迟到
//! [`Layout::lay`](super::layout::Layout::lay)，它在渲染时拿到宽度。两件事靠它：resize 时按新宽度重铺已有的
//! transcript Element（从不重建），壳跨帧保留已落定条目的 Element，身份里没有宽度。加一个烤进宽度的节点（预先折好的
//! 段落、按固定宽度 render 出的字符串），两者都会在 resize 时悄悄坏掉。
//!
//! 与 Scala 的不同：树拥有子树（`Box` / `Vec`），文字是 `Cow<'static, str>`；
//! `MemoNode` 与它的 `LayMemo` 合成一个 `Arc<Memo>`，相等即同一个 `Arc`（见 [`Memo`]）；函数节点的闭包放在
//! `Arc<dyn Fn>` 里，相等也按指针，与 Scala 函数值的引用相等相同。
use std::borrow::Cow;
use std::fmt;
use std::sync::{Arc, Mutex};

use rust_cli_base::sync::LockUnpoisoned;

use crate::render::{Color, Span, Style, StyledLine};

use super::layout::Layout;

/// 文字超出宽度时，[`wrap_text`] 与表格的格子怎么重排。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wrap {
    /// 在词之间折；一个词比一行还长时，最后才在词中间断开。
    Word,
    /// 在任何码点处折（原样的文字，如代码块）。
    Char,
}

/// 表格一列格子的水平对齐。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnAlign {
    Left,
    Center,
    Right,
}

/// 内部元素铺得比上限高时，[`Clip`] 留下哪些行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipFocus {
    /// 留开头的行；末尾一行 `… N more lines` 代表藏起来的尾巴。
    Head,
    /// 留光标行（第一个含 [`attr::CARET`](crate::render::attr::CARET) 样式段的行，输入框的光标格）附近的一窗：能居中就
    /// 居中，到边就贴边，被裁的每一侧一行标记。
    Cursor,
    /// 留开头 `head` 行与末尾 `tail` 行，中间一行标记代表藏起来的部分：长的一块留住怎么开始、怎么结束。
    Ends { head: usize, tail: usize },
}

/// 按宽度造 Element 的函数（[`Element::ByWidth`]）。相等按指针：同一个闭包才相等。
#[derive(Clone)]
pub struct WidthFn(pub Arc<dyn Fn(usize) -> Element + Send + Sync>);

/// 铺好之后改行的函数（[`Element::Map`]）。相等按指针。
#[derive(Clone)]
pub struct LinesFn(pub Arc<dyn Fn(Vec<StyledLine>) -> Vec<StyledLine> + Send + Sync>);

impl PartialEq for WidthFn {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl PartialEq for LinesFn {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for WidthFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WidthFn(..)")
    }
}

impl fmt::Debug for LinesFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LinesFn(..)")
    }
}

/// 一个表格：在铺的宽度上定列宽，放得下时按内容宽，放不下时分享宽度、格子折行。格子是样式段；列之间三个空格，
/// 表头下一条与表同宽、`rule` 样式的横线。
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub first_prefix: Vec<Span>,
    pub next_prefix: Vec<Span>,
    pub align: Vec<ColumnAlign>,
    pub header: Vec<Vec<Span>>,
    pub rows: Vec<Vec<Vec<Span>>>,
    pub rule: Style,
}

/// 竖向的上限（docs/puffin-tui.md §4.4）：内部铺得比 `max_rows` 高时，按 [`ClipFocus`] 选留下的行，暗色的
/// `… N more lines` 标记行代表被裁的部分。标记算在 `max_rows` 里，铺出的高度不超过它（下限 3 行：标记夹心至少要一行
/// 内容）。`hint` 非空时用括号接在标记后（比如去哪里读全文）。标记行以 `lead` 开头，用 `ink` 画。
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub inner: Element,
    pub max_rows: usize,
    pub focus: ClipFocus,
    pub hint: Cow<'static, str>,
    pub lead: Vec<Span>,
    pub ink: Style,
    /// 标记的话：藏起来的行数 → `… N more lines`（别的界面语言给译文；铺法不变）。
    pub more: More,
}

/// 剪去的标记的话。两个说法说出一样的话就相等（函数指针的地址不唯一，不能拿来比）。
#[derive(Clone, Copy, Debug)]
pub struct More(pub fn(i64) -> String);

impl PartialEq for More {
    fn eq(&self, other: &More) -> bool {
        [1, 2].iter().all(|&n| (self.0)(n) == (other.0)(n))
    }
}

/// Puffin 的标记：`… 1 more line`、`… N more lines`。
pub fn more_lines(hidden: i64) -> String {
    let noun = if hidden == 1 { "line" } else { "lines" };
    format!("… {hidden} more {noun}")
}

impl Clip {
    /// 没有提示、没有引导、暗色标记：Scala `clipRows` 的缺省参数。
    pub fn new(inner: Element, max_rows: usize, focus: ClipFocus) -> Clip {
        Clip {
            inner,
            max_rows,
            focus,
            hint: Cow::Borrowed(""),
            lead: Vec::new(),
            ink: Style::DIM,
            more: More(more_lines),
        }
    }

    pub fn with_more(mut self, more: fn(i64) -> String) -> Clip {
        self.more = More(more);
        self
    }

    pub fn with_hint(mut self, hint: impl Into<Cow<'static, str>>) -> Clip {
        self.hint = hint.into();
        self
    }

    pub fn with_lead(mut self, lead: Vec<Span>) -> Clip {
        self.lead = lead;
        self
    }

    pub fn with_ink(mut self, ink: Style) -> Clip {
        self.ink = ink;
        self
    }
}

impl From<Clip> for Element {
    fn from(c: Clip) -> Element {
        Element::Clip(Box::new(c))
    }
}

/// `prefix` 之后的代码面板：原样的 `lines`（样式段，如语法色）在 `bg` 底上按字符折行，左右各留一格，半格的上下边，
/// 放得下时 `label` 在第一行的右端。`open`（代码还在写）时不画下边。
#[derive(Clone, Debug, PartialEq)]
pub struct CodePanel {
    pub prefix: Vec<Span>,
    pub lines: Vec<Vec<Span>>,
    pub bg: Color,
    pub label: Vec<Span>,
    pub open: bool,
}

/// 一棵不再变的子树（比如落定的 Markdown 块）的布局记忆：[`Layout::lay`] 铺 `inner` 一次，宽度与
/// [`width::generation`](crate::render::width::generation) 不变时重放铺好的行。这不违反宽度无关的不变量：节点的值里
/// 没有宽度，记忆的键是 `lay` 拿到的宽度，resize 只是重算。
///
/// 身份：记忆跟着 `Arc` 走。同一个 `Arc<Memo>` 跨帧放进视图才有用，持有者要复用节点，不要重建；`Element` 的 `clone`
/// 复制 `Arc`，两份共享同一个记忆。相等是 `Arc` 的指针相等（Scala 的 `LayMemo` 是引用相等的类，`MemoNode` 的相等
/// 等于"同一个 LayMemo"）。
///
/// 只有一格，与 Scala 相同：换了宽度就覆盖。按宽度存多格的表在 resize 风暴里让每个条目攒下每个宽度的一份行。
/// 锁只在查与存时持有，铺 `inner` 时不持有，`inner` 里再遇到同一个记忆也不会死锁。
pub struct Memo {
    inner: Element,
    slot: Mutex<Option<MemoSlot>>,
}

struct MemoSlot {
    width: usize,
    generation: u32,
    lines: Arc<Vec<StyledLine>>,
}

impl Memo {
    pub fn new(inner: Element) -> Memo {
        Memo {
            inner,
            slot: Mutex::new(None),
        }
    }

    pub fn inner(&self) -> &Element {
        &self.inner
    }

    /// 记着的行，键是（宽度，代数）。
    pub(crate) fn cached(&self, width: usize, generation: u32) -> Option<Arc<Vec<StyledLine>>> {
        let slot = self.slot.lock_unpoisoned();
        slot.as_ref()
            .filter(|s| s.width == width && s.generation == generation)
            .map(|s| Arc::clone(&s.lines))
    }

    pub(crate) fn store(&self, width: usize, generation: u32, lines: Arc<Vec<StyledLine>>) {
        let mut slot = self.slot.lock_unpoisoned();
        *slot = Some(MemoSlot {
            width,
            generation,
            lines,
        });
    }
}

impl PartialEq for Memo {
    /// 身份相等：见类型的说明。
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl fmt::Debug for Memo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memo")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

/// 视图树。各变体对应 Element.scala 的同名节点（去掉 `Node` 后缀）。
#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    /// 某个样式的原文。可以含换行（布局时拆行），可以嵌我们自己的 SGR 序列（布局时重新切成段）。Scala `TextNode`。
    Text(Cow<'static, str>, Style),
    /// 竖向堆叠：子元素依次铺开、接起来。
    Stack(Vec<Element>),
    /// 一个空行。
    LineBreak,
    /// 什么都不铺。
    Blank,
    /// 渲染时展开到铺的宽度的横线。Scala `RuleNode`。
    Rule(char, Style),
    /// 全宽的横线、标签居中：`──── label ────`。Scala `LabelledRuleNode`。
    LabelledRule(Cow<'static, str>, Style),
    /// 圆角框：`inner` 按 `width - 4` 铺，框成 `│ inner │`，上下是 `╭─…─╮` 与 `╰─…─╯`；`Style` 给边框着色。
    /// Scala `BoxNode`。
    RoundBox(Box<Element>, Style),
    /// 转圈：`frame` 选字形，`label` 跟在后面。Scala `SpinnerNode`。
    Spinner {
        label: Cow<'static, str>,
        frame: usize,
        style: Style,
    },
    /// 套在内部元素上的样式（内部自己样式的底）。Scala `StyledNode`。
    Styled(Box<Element>, Style),
    /// 按铺的宽度软折行的文字，第一行与后续行前缀不同。Scala `WrappedTextNode`。
    WrappedText {
        first_prefix: Cow<'static, str>,
        next_prefix: Cow<'static, str>,
        value: Cow<'static, str>,
        style: Style,
        mode: Wrap,
    },
    /// 样式段上的 [`Element::WrappedText`]：前缀与文字都是段，留得住 SGR 串带不了的东西（段的 `Decor`）。
    /// Scala `WrappedSpansNode`。
    WrappedSpans {
        first_prefix: Vec<Span>,
        next_prefix: Vec<Span>,
        spans: Vec<Span>,
        mode: Wrap,
    },
    /// 见 [`Table`]。
    Table(Box<Table>),
    /// 见 [`Memo`]。Scala `MemoNode` 加它的 `LayMemo`。
    Memo(Arc<Memo>),
    /// 见 [`Clip`]。Scala `ClipNode`。
    Clip(Box<Clip>),
    /// 已经铺好的行，不论宽度原样返回。宽度无关的唯一例外：只给整帧用，整帧每帧都从铺好、按视口切好的行重建，
    /// 不跨宽度缓存。transcript 的内容不要用它。Scala `RawLines`。
    Raw(Vec<StyledLine>),
    /// 一块板（docs/puffin-tui.md §4.2）：`inner` 在 `bg` 底上按全宽铺，每行补到全宽，上边一行 `▄`、下边一行 `▀`，
    /// 用 `bg` 画。Scala `SlabNode`。
    Slab(Box<Element>, Color),
    /// 见 [`CodePanel`]。
    CodePanel(Box<CodePanel>),
    /// 按 `min(width - margin, max)` 铺 `inner`：阅读的行宽。Scala `MeasureNode`。
    Measure {
        inner: Box<Element>,
        max: usize,
        margin: usize,
    },
    /// 一行：`left` 在开头，`right` 在末尾，中间至少两个空格。放不下时铺得比宽度宽，[`Element::FirstFit`] 靠这个判断。
    /// Scala `SpreadNode`。
    Spread(Vec<Span>, Vec<Span>),
    /// 第一个每行都放得下的变体；都放不下时取最后一个，每行切到宽度。Scala `FirstFitNode`。
    FirstFit(Vec<Element>),
    /// 一行，到宽度处用 `…` 切开：切掉结尾，或 `keep_end` 时切掉开头（路径的结尾才要紧）。Scala `LineNode`。
    Line { spans: Vec<Span>, keep_end: bool },
    /// 两块并排：`left` 按 `left_width` 铺并补到它，`right` 用剩下的。Scala `BesideNode`。
    Beside {
        left: Box<Element>,
        left_width: usize,
        right: Box<Element>,
    },
    /// `inner` 的最后一行带上 `caret`：放在末尾一串不换行空格（还没显示的字）的第一格，否则接在行尾。光标不参与折行，
    /// 从不移动词。Scala `CaretNode`。
    Caret(Box<Element>, Vec<Span>),
    /// 按铺的宽度由函数造出的元素。Scala `ByWidthNode`。
    ByWidth(WidthFn),
    /// `inner` 铺好的行，铺完后由函数改：笔把最新的字落在铺好的位置（docs/puffin-tui.md §4.2「笔」）。函数保持每行的
    /// 宽度与软折行。Scala `MapNode`。
    Map(Box<Element>, LinesFn),
}

impl Element {
    /// 给这个元素加一个样式：文字与样式节点直接合进去，别的包一层。
    pub fn style(self, s: Style) -> Element {
        match self {
            Element::Text(v, st) => Element::Text(v, st.merge(s)),
            Element::Styled(inner, st) => Element::Styled(inner, st.merge(s)),
            other => Element::Styled(Box::new(other), s),
        }
    }

    /// 渲染成一个 ANSI 串（按 80 列铺、行之间换行）。给拼字符串的地方用。
    pub fn render(&self) -> String {
        Layout::render_inline(self)
    }

    /// Scala 的 `body eq Empty`。
    pub fn is_blank(&self) -> bool {
        matches!(self, Element::Blank)
    }
}

/* ---- 构造函数（Scala 的顶层 DSL） ---- */

/// 默认样式的文字。Scala `Text`。
pub fn text(s: impl Into<Cow<'static, str>>) -> Element {
    Element::Text(s.into(), Style::DEFAULT)
}

/// 某个样式的文字。
pub fn styled_text(s: impl Into<Cow<'static, str>>, style: Style) -> Element {
    Element::Text(s.into(), style)
}

/// 前景色的文字：Scala 的 `Color.Cyan("text")`。
pub fn colored(c: Color, s: impl Into<Cow<'static, str>>) -> Element {
    Element::Text(s.into(), Style::fg(c))
}

/// 竖向堆叠。Scala `layout(children*)`。
pub fn stack(children: impl IntoIterator<Item = Element>) -> Element {
    Element::Stack(children.into_iter().collect())
}

/// 一个空行。
pub fn br() -> Element {
    Element::LineBreak
}

/// 什么都不铺。Scala `Empty`。
pub fn empty() -> Element {
    Element::Blank
}

pub fn spinner(label: impl Into<Cow<'static, str>>, frame: usize) -> Element {
    Element::Spinner {
        label: label.into(),
        frame,
        style: Style::DEFAULT,
    }
}

/// 全宽的横线，`ch` 用 `color` 画。Scala `hr`（缺省 `'─'` 与默认色）；`Style::fg(Color::Default)` 就是默认样式。
pub fn hr(ch: char, color: Color) -> Element {
    Element::Rule(ch, Style::fg(color))
}

/// 全宽的横线，`label` 居中，如 `──── Context compacted ────`。
pub fn labelled_hr(label: impl Into<Cow<'static, str>>, style: Style) -> Element {
    Element::LabelledRule(label.into(), style)
}

pub fn round_box(inner: Element, color: Color) -> Element {
    Element::RoundBox(Box::new(inner), Style::fg(color))
}

/// 把 `inner` 限在 `max_rows` 行（标记算在内），按 `focus` 省略。提示、引导与墨色见 [`Clip`] 的 `with_*`。
pub fn clip_rows(inner: Element, max_rows: usize, focus: ClipFocus) -> Element {
    Clip::new(inner, max_rows, focus).into()
}

pub fn wrap_text(
    first_prefix: impl Into<Cow<'static, str>>,
    next_prefix: impl Into<Cow<'static, str>>,
    value: impl Into<Cow<'static, str>>,
    mode: Wrap,
) -> Element {
    Element::WrappedText {
        first_prefix: first_prefix.into(),
        next_prefix: next_prefix.into(),
        value: value.into(),
        style: Style::DEFAULT,
        mode,
    }
}

pub fn wrap_styled(
    first_prefix: Vec<Span>,
    next_prefix: Vec<Span>,
    spans: Vec<Span>,
    mode: Wrap,
) -> Element {
    Element::WrappedSpans {
        first_prefix,
        next_prefix,
        spans,
        mode,
    }
}

pub fn table(
    first_prefix: Vec<Span>,
    next_prefix: Vec<Span>,
    align: Vec<ColumnAlign>,
    header: Vec<Vec<Span>>,
    rows: Vec<Vec<Vec<Span>>>,
    rule: Style,
) -> Element {
    Element::Table(Box::new(Table {
        first_prefix,
        next_prefix,
        align,
        header,
        rows,
        rule,
    }))
}

/// 包一层布局记忆（见 [`Memo`]）。要跨帧复用返回的元素（或它的 `clone`），记忆才有用。
pub fn memo(inner: Element) -> Element {
    Element::Memo(Arc::new(Memo::new(inner)))
}

pub fn slab(inner: Element, bg: Color) -> Element {
    Element::Slab(Box::new(inner), bg)
}

pub fn code_panel(
    prefix: Vec<Span>,
    lines: Vec<Vec<Span>>,
    bg: Color,
    label: Vec<Span>,
    open: bool,
) -> Element {
    Element::CodePanel(Box::new(CodePanel {
        prefix,
        lines,
        bg,
        label,
        open,
    }))
}

pub fn measure(inner: Element, max: usize, margin: usize) -> Element {
    Element::Measure {
        inner: Box::new(inner),
        max,
        margin,
    }
}

pub fn spread(left: Vec<Span>, right: Vec<Span>) -> Element {
    Element::Spread(left, right)
}

pub fn first_fit(variants: impl IntoIterator<Item = Element>) -> Element {
    Element::FirstFit(variants.into_iter().collect())
}

pub fn single_line(spans: Vec<Span>, keep_end: bool) -> Element {
    Element::Line { spans, keep_end }
}

pub fn beside(left: Element, left_width: usize, right: Element) -> Element {
    Element::Beside {
        left: Box::new(left),
        left_width,
        right: Box::new(right),
    }
}

pub fn caret_at(inner: Element, caret: Vec<Span>) -> Element {
    Element::Caret(Box::new(inner), caret)
}

pub fn by_width(f: impl Fn(usize) -> Element + Send + Sync + 'static) -> Element {
    Element::ByWidth(WidthFn(Arc::new(f)))
}

pub fn map_lines(
    inner: Element,
    f: impl Fn(Vec<StyledLine>) -> Vec<StyledLine> + Send + Sync + 'static,
) -> Element {
    Element::Map(Box::new(inner), LinesFn(Arc::new(f)))
}
