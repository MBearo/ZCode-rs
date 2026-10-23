//! 流式回复的 markdown 排版缓存（docs/specs/rust-cli/tui.md §4.1，修复线 F6，审查 R5-5）。
//!
//! 性能修复：以前流式中的回复每一帧都对整篇 markdown 做解析、排版与折行，73 KB 的回复 CPU 13 s（平方增长）。
//! 现在按顶层块缓存：每块记下它在原文里的范围与原文、它输出的行和它之后的排版状态。下一次排同一篇（流式中只会在
//! 末尾追加）时，范围与原文都没变的块直接接上缓存的行和状态，从第一个变了的块往后重排，通常只有最后一块。
//!
//! 为什么结果与整篇重排相同：事件仍来自对全文的一次解析（解析很便宜，贵的是排版、折行与代码高亮），所以块的划分
//! 与整篇重排时完全一样；排版器是事件的确定性状态机，起点状态相同、事件相同，输出就相同。一个顶层块的事件只取决于
//! 它自己的原文，唯一的例外是链接引用定义（`[x]: url` 写在后面也会改变前面的链接），文档里有引用定义时整篇重排。
use std::cell::RefCell;
use std::ops::Range;

use pulldown_cmark::{Event, Parser};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{OPTIONS, Renderer, State};
use crate::ui::palette::Palette;

/// 一篇回复的缓存；放在每帧共用的排版缓存里，经 `Ctx` 交给回复的排版（只用于流式中的回复）。
#[derive(Default)]
pub struct Memo(RefCell<Option<Doc>>);

/// 决定排版结果的参数：任何一个变了，缓存作废。
#[derive(PartialEq)]
struct Key {
    width: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    base: Style,
    palette: Palette,
}

struct Doc {
    key: Key,
    blocks: Vec<Block>,
    /// 这一次接上的缓存块数（测试看缓存确实被用到）。
    #[cfg_attr(not(test), expect(dead_code, reason = "只在测试里读"))]
    reused: usize,
}

struct Block {
    range: Range<usize>,
    source: String,
    lines: Vec<Line<'static>>,
    after: State,
}

/// 全文的顶层块：每块的原文范围与它的事件（按全文解析的结果分组）。
fn blocks(parser: Parser<'_>) -> Vec<(Range<usize>, Vec<Event<'_>>)> {
    let mut out: Vec<(Range<usize>, Vec<Event<'_>>)> = Vec::new();
    let mut depth = 0usize;
    for (event, range) in parser.into_offset_iter() {
        let opens = depth == 0;
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        match out.last_mut() {
            Some((_, events)) if !opens => events.push(event),
            _ => out.push((range, vec![event])),
        }
    }
    out
}

impl Memo {
    /// 与 [`super::render`] 相同的结果；能接上缓存的顶层块不再排版。
    pub fn render(
        &self,
        text: &str,
        width: usize,
        (first, rest): (Vec<Span<'static>>, Vec<Span<'static>>),
        base: Style,
        palette: &Palette,
    ) -> Vec<Line<'static>> {
        let parser = Parser::new_ext(text, OPTIONS);
        // 链接引用定义会改变它前面的块：整篇重排，缓存作废。
        if parser.reference_definitions().iter().next().is_some() {
            self.0.replace(None);
            return super::render(text, width, first, rest, base, palette);
        }
        let key = Key {
            width,
            first,
            rest,
            base,
            palette: *palette,
        };
        let mut cached = self
            .0
            .take()
            .filter(|doc| doc.key == key)
            .map(|doc| doc.blocks)
            .unwrap_or_default()
            .into_iter();
        let mut renderer =
            Renderer::new(palette, width, (key.first.clone(), key.rest.clone()), base);
        let mut kept = Vec::new();
        let mut reuse = true;
        let mut reused = 0;
        for (range, events) in blocks(parser) {
            let source = &text[range.clone()];
            if reuse
                && let Some(block) = cached
                    .next()
                    .filter(|block| block.range == range && block.source == source)
            {
                renderer.lines.extend(block.lines.iter().cloned());
                renderer.restore(&block.after);
                kept.push(block);
                reused += 1;
                continue;
            }
            reuse = false;
            let start = renderer.lines.len();
            for event in events {
                renderer.event(event);
            }
            kept.push(Block {
                range,
                source: source.to_owned(),
                lines: renderer.lines[start..].to_vec(),
                after: renderer.state(),
            });
        }
        self.0.replace(Some(Doc {
            key,
            blocks: kept,
            reused,
        }));
        renderer.finish()
    }

    /// 上一次排版接上的缓存块数。
    #[cfg(test)]
    fn reused(&self) -> usize {
        self.0.borrow().as_ref().map_or(0, |doc| doc.reused)
    }
}

#[cfg(test)]
#[path = "markdown_memo_tests.rs"]
mod tests;
