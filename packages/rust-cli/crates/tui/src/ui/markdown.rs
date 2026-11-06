//! markdown → 终端行，经 kit（docs/specs/rust-cli/tui-kit.md §3.5，工作线 TK 的 C1-A；样子 §4.2，S1）：Puffin 的
//! `markdown::Document` → `render::answer` → `Element` → `Layout::lay`。回答的样子（标题、列表的圆点、表格、代码面板、行内
//! 代码、链接）是 Puffin 的（docs/puffin-tui.md §4.2「回答的 Markdown」）。
//!
//! - 回答（[`answer`]）照 Puffin 的 `shell/view/entries.rs` 的 `answer`：kit 的回答从第 3 列起，量宽 `min(宽 − 2, 99)`。
//!   以前首行接旧界面的 `⏺ `、续行缩进 2 列（S1 改）。
//! - 流式的回答由 transcript 的播放头按词显示（`transcript_lines.rs`，§4.2「流式节奏」「笔」）：排版用的文字是 [`laid_text`]
//!   （播放头所在的词排版但不画），文档用 [`Memo`] 增量解析（kit 的 `Document::relaid`，加 `AnswerRenderCache`），笔尖
//!   与落定经 kit 的 `Tail` 与 `pen::settled`。
use std::cell::RefCell;

use rust_cli_tui_kit::app::element::{Element, measure};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::lattice;
use rust_cli_tui_kit::app::widgets::pen::{self, Pen};
use rust_cli_tui_kit::app::widgets::playhead::{self, Playhead};
use rust_cli_tui_kit::markdown::document::Document;
use rust_cli_tui_kit::markdown::render::{self as kit, AnswerRenderCache, HIDDEN, Tail};
use rust_cli_tui_kit::render::StyledLine;
use rust_cli_tui_kit::theme::ease::Ink as Nib;

use super::cells::look::{MEASURE_MARGIN, MEASURE_MAX};
use super::ink::Ink;

/// 量过宽的回答元素（Puffin `measure(…, MEASURE_MAX, MEASURE_MARGIN)`）。
pub fn measured(element: Element) -> Element {
    measure(element, MEASURE_MAX, MEASURE_MARGIN)
}

/// 完整的回答（Puffin `entries::answer`）：kit 的回答从第 3 列起，量宽 `min(宽 − 2, 99)`，按 `width` 铺。
pub fn answer(text: &str, width: usize, ink: &Ink) -> Vec<StyledLine> {
    let element = measured(kit::answer(&Document::parse(text), None, &ink.theme));
    Layout::lay(&element, width)
}

/// 一篇流式中的回复的增量文档：放在每帧共用的排版缓存里，经 `Ctx` 交给回复的排版（tui.md §4.1，修复线 F6）。
///
/// 文档用 kit 的 `relaid`：文字只在末尾追加时增量解析；播放头的排版文字带 [`HIDDEN`]（不是严格的追加）、换了回复或重试时
/// 按最终化的语义重建，下一帧回到增量。最终化的块的 `Element` 由 `AnswerRenderCache` 记着（换主题时它自己作废），布局
/// 按宽度记忆，所以每帧只重排开放的末尾。结果与整篇重排相同（测试逐个前缀核对）。
#[derive(Default)]
pub struct Memo(RefCell<Stream>);

#[derive(Default)]
struct Stream {
    doc: Document,
    cache: AnswerRenderCache,
}

impl Memo {
    /// 文字是 `text` 时的文档与渲染缓存交给 `f`。
    pub fn with<R>(&self, text: &str, f: impl FnOnce(&Document, &mut AnswerRenderCache) -> R) -> R {
        let mut stream = self.0.borrow_mut();
        stream.doc.relaid(text);
        let Stream { doc, cache } = &mut *stream;
        f(doc, cache)
    }

    /// 回答（同 [`answer`]）带上 `tail`：最终化的块不再解析、不再渲染。
    pub fn answer(
        &self,
        text: &str,
        tail: Option<&Tail>,
        width: usize,
        ink: &Ink,
    ) -> Vec<StyledLine> {
        let element = self.with(text, |doc, cache| {
            measured(kit::answer_cached(doc, tail, cache, &ink.theme))
        });
        Layout::lay(&element, width)
    }

    /// 已经最终化（不再解析）的顶层块数。
    #[cfg(test)]
    fn finalized(&self) -> usize {
        self.0.borrow().doc.finalized_count()
    }
}

/// 打出中的回答（Puffin `view/tail.rs` 的 `streaming`）：排版用的文字是 [`laid_text`]，词整个落下、最新的在落定；笔骑着时
/// 笔尖在字后面（字落下时是实的，播放头停了一会儿后在 brand 里呼吸，手停住满 0.5 s 换成点阵；末尾正在到达的表格不画、
/// 停住的笔尖或点阵在它的位置）。播放头
/// 走过表格时它整张落定一次。`lifts`（它是会话最后一个条目、回合结束了）时，显示完以后笔尖在 4 个 tick 内抬起（§4.2「笔」
/// 的「抬笔」）。`stopping`（按了 esc、回合还没结束）时落定中的词立刻是实的。第二个值说笔骑了没有：没骑时光标单独一行（`transcript_tail.rs`）。以前回复 final 后不走播放头、整篇直接
/// 显示，没有抬笔。
#[allow(clippy::too_many_arguments)]
pub fn streaming(
    memo: &Memo,
    text: &str,
    p: &Playhead,
    now: i64,
    pen: Option<&Pen>,
    lifts: bool,
    stopping: bool,
    width: usize,
    ink: &Ink,
) -> (Vec<StyledLine>, bool) {
    let (laid, _) = laid_text(text, p.shown.min(text.len()));
    let theme = ink.theme;
    let mut rode = false;
    let element = memo.with(&laid, |doc, cache| {
        let rides = kit::rides_tail(doc);
        let lift = p
            .ended_for(now)
            .filter(|_| lifts && rides)
            .and_then(|age| pen::lift(&theme, age));
        let waits = kit::waits(doc);
        let nib = pen.filter(|_| rides).map(|pen| {
            // 修复原因：以前回答末尾永远是笔尖。回答的行要到整条消息结束才不再流式，下一个调用要等输入写完才有行，所以 agent
            // 说完一句之后回答一直是最新的一块，笔骑在它上面，单独一行的等待行也不画：屏幕上没有点阵（docs/puffin-tui.md
            // §4.2「手停住」）。手的时钟在排完之后才更新，所以词落下的这一帧看播放头（`idle` 是 0），点阵在这一帧离开。
            match (pen.sign_age().filter(|_| p.idle(now) > 0), waits) {
                (Some(age), true) => lattice::sign(&theme, age),
                (Some(age), false) => {
                    pen::sign_after(&theme, laid.split(HIDDEN).next().unwrap_or_default(), age)
                }
                (None, true) => vec![pen.rest(Nib::Answer)],
                (None, false) => vec![pen.nib(Nib::Answer, p, now)],
            }
        });
        rode = nib.is_some();
        let mut tail = Tail::marked(nib.or_else(|| lift.map(|l| vec![l])).unwrap_or_default())
            .with_ended(p.ended.is_some());
        // 表格完整时整张落定一次：后面的块的第一个字显示时，或它是最后一块而回答显示完时。
        if doc
            .blocks()
            .any(|b| matches!(b, rust_cli_tui_kit::markdown::ast::Block::Table { .. }))
        {
            let ends = doc.block_ends();
            let laid_c = laid.clone();
            let pc = p.clone();
            tail = tail.with_landed(move |i| {
                let at = if i + 1 >= ends.len() {
                    pc.ended_for(now)
                } else {
                    let mut j = ends[i].min(laid_c.len());
                    while let Some(ch) = laid_c[j..]
                        .chars()
                        .next()
                        .filter(|c| rust_cli_tui_kit::theme::ease::java_space(*c))
                    {
                        j += ch.len_utf8();
                    }
                    pc.age_of(j, now)
                };
                at.unwrap_or(kit::SETTLED)
            });
        }
        let body = kit::answer_cached(doc, Some(&tail), cache, &theme);
        if stopping {
            measured(body)
        } else {
            measured(pen::settled(&theme, body, text, p, now))
        }
    });
    (Layout::lay(&element, width), rode)
}

/// 播放头在 `shown`（字节偏移）时回答按什么文字排版，与它在 `text` 里止于哪里（docs/puffin-tui.md §4.2「流式节奏」的
/// 「词不移动」）。排到播放头所在的词的末尾；播放头之后的字母跟在 [`HIDDEN`] 后面，排版但不画，一个词从第一个字母起就在它
/// 最终的那一行上。开启一行的块标记（`-`、`1.`、`#`、`>`、围栏）整个显示，因为标记里的 HIDDEN 会让解析器认不出它；表格行
/// 等它完整。照抄 Puffin 的壳（`shell/consts.rs` 的 `laid_text`；两套界面不能互相依赖，tui-kit.md §4.4）。
pub fn laid_text(text: &str, shown: usize) -> (String, usize) {
    if shown == 0 {
        return (String::new(), 0);
    }
    let line_start = text[..shown].rfind('\n').map_or(0, |i| i + 1);
    let mut j = line_start;
    while j < text.len() && text.as_bytes()[j] == b' ' {
        j += 1;
    }
    if j < text.len() && text.as_bytes()[j] == b'|' {
        return (text[..line_start].to_owned(), line_start);
    }
    let word_end = playhead::word_end(text, j);
    if shown <= word_end && is_marker(&text[j..word_end]) {
        return (text[..word_end].to_owned(), word_end);
    }
    let end = playhead::word_end(text, shown);
    if end == shown {
        (text[..shown].to_owned(), shown)
    } else {
        (
            format!("{}{HIDDEN}{}", &text[..shown], &text[shown..end]),
            end,
        )
    }
}

/// 在行首开启一个 Markdown 块的词：`#`…`######`、`-` `*` `+`、`1.` `1)`（至多九位）、`>`、围栏（Puffin
/// `consts::is_marker`）。
fn is_marker(word: &str) -> bool {
    let hashes = word.bytes().all(|b| b == b'#') && (1..=6).contains(&word.len());
    let bullet = matches!(word, "-" | "*" | "+" | ">");
    // 修 panic：原来用 `word.split_at(word.len() - 1)` 按字节切下最后一个字符，词的末字是多字节字符（如 `±×÷` 的 `÷`，
    // 两个字节）时切点落在字符中间，`split_at` panic（孪生门禁 glyphs-ambiguous 流式时撞到）。改成剥掉 ASCII 的 `.` / `)`
    // 后缀：剥不掉就不是序号，剥得掉时切点在 ASCII 字节之后，必在字符边界上；判断与原来相同。
    let ordered = word.strip_suffix(['.', ')']).is_some_and(|digits| {
        (1..=9).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    });
    let fence = (word.starts_with("```") || word.starts_with("~~~")) && !word.contains('\n');
    hashes || bullet || ordered || fence
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
