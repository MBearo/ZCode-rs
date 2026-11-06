//! 有缓存的 `answer_cached` 必须看不出来：在每一步喂入上铺出的行与无缓存的逐字节相同，同时跨调用复用最终化块的
//! Element（与它们的布局记忆）。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/AnswerRenderCacheSuite.scala（feat/tui a2bf878b34）。
use std::sync::Arc;

use crate::app::element::{Element, Memo};
use crate::app::layout::Layout;
use crate::markdown::ast::Block;
use crate::markdown::document::Document;
use crate::markdown::render::{
    AnswerRenderCache, SETTLED, Tail, answer, answer_cached, render_blocks,
};
use crate::render::{Span, Style, StyledLine};
use crate::theme::ease;
use crate::theme::theme::Theme;

use super::rich_theme;

const SOURCE: &str = concat!(
    "# Title\n\nfirst paragraph with some text\n\n- item one\n- item two\n\n",
    "```scala\nval x = 1\n```\n\nsecond paragraph, still going\n\nthird paragraph tail"
);

const PLAIN: &Theme = &Theme::PLAIN;

/// 把 `text` 按 `step` 字节一块喂进文档，每一步调 `f`（文字都是 ASCII）。
fn feed_steps(text: &str, step: usize, mut f: impl FnMut(&Document)) {
    let mut d = Document::new();
    let mut i = 0;
    while i < text.len() {
        i = text.len().min(i + step);
        d.feed_to(&text[..i]);
        f(&d);
    }
}

/// 铺好的 ANSI 行：渲染器消费的真值。
fn laid(e: &Element) -> Vec<String> {
    Layout::lay(e, 40).iter().map(StyledLine::render).collect()
}

fn children(e: &Element) -> Vec<Element> {
    match e {
        Element::Stack(cs) => cs.clone(),
        other => vec![other.clone()],
    }
}

fn memo_of(e: &Element) -> &Arc<Memo> {
    match e {
        Element::Memo(m) => m,
        other => panic!("expected a memo node, got {other:?}"),
    }
}

fn glow() -> Tail {
    Tail::new(Some(Span::new("█", Style::DEFAULT)))
}

/// 一个流式尾巴：带笔尖，块序号 1（表格）在落定中、其余块早已落定。
fn settling(age: i64, ended: bool) -> Tail {
    glow()
        .with_landed(move |i| if i == 1 { age } else { SETTLED })
        .with_ended(ended)
}

const AGES: [i64; 6] = [0, 1, 2, 3, ease::TICKS, SETTLED];

#[test]
fn cached_plain_render_matches_the_uncached_one_at_every_feed_step() {
    let mut cache = AnswerRenderCache::new();
    feed_steps(SOURCE, 7, |d| {
        assert_eq!(
            laid(&answer_cached(d, None, &mut cache, PLAIN)),
            laid(&answer(d, None, PLAIN))
        );
    });
}

#[test]
fn cached_glow_render_matches_the_uncached_one_at_every_feed_step() {
    let mut cache = AnswerRenderCache::new();
    feed_steps(SOURCE, 13, |d| {
        let t = glow();
        assert_eq!(
            laid(&answer_cached(d, Some(&t), &mut cache, PLAIN)),
            laid(&answer(d, Some(&t), PLAIN))
        );
    });
}

#[test]
fn finalised_blocks_elements_are_rendered_once_and_reused_by_reference() {
    let mut cache = AnswerRenderCache::new();
    let mut d = Document::new();
    d.feed_to("first para\n\nsecond para\n\nthird");
    assert!(
        d.finalized_count() >= 1,
        "expected a finalised block, got {}",
        d.finalized_count()
    );
    let e1 = answer_cached(&d, None, &mut cache, PLAIN);
    let grown = format!("{} grows\n\nnext para\n\nlast", d.source());
    d.feed_to(&grown);
    let e2 = answer_cached(&d, None, &mut cache, PLAIN);
    // 第一个最终化块的 Element 在堆叠的最前面。
    assert!(
        Arc::ptr_eq(memo_of(&children(&e1)[0]), memo_of(&children(&e2)[0])),
        "first finalised Element should be the same instance"
    );
}

#[test]
fn a_memoized_block_lays_once_per_width_and_recomputes_on_resize() {
    let mut cache = AnswerRenderCache::new();
    let d = Document::parse("alpha beta gamma delta epsilon\n\ntail");
    let first = children(&answer_cached(&d, None, &mut cache, PLAIN))[0].clone();
    let m = memo_of(&first);
    let l1 = Layout::lay_memo(m, 12);
    assert!(
        Arc::ptr_eq(&Layout::lay_memo(m, 12), &l1),
        "same width should replay the memoized lines"
    );
    let l2 = Layout::lay_memo(m, 20);
    assert!(!Arc::ptr_eq(&l1, &l2));
    // 重算的行与新宽度下无缓存渲染的相同。
    let uncached_first = children(&render_blocks(d.blocks(), PLAIN))[0].clone();
    let render = |ls: &[StyledLine]| ls.iter().map(StyledLine::render).collect::<Vec<_>>();
    assert_eq!(render(&l2), render(&Layout::lay(&uncached_first, 20)));
}

#[test]
fn two_documents_sharing_one_cache_stay_correct() {
    let mut cache = AnswerRenderCache::new();
    let a = Document::parse("para a1\n\npara a2\n\npara a3");
    let b = Document::parse("para b1\n\npara b2");
    for _ in 0..3 {
        assert_eq!(
            laid(&answer_cached(&a, None, &mut cache, PLAIN)),
            laid(&answer(&a, None, PLAIN))
        );
        assert_eq!(
            laid(&answer_cached(&b, None, &mut cache, PLAIN)),
            laid(&answer(&b, None, PLAIN))
        );
    }
}

#[test]
fn a_theme_change_rebuilds_the_cache() {
    // Rust 补充：缓存的键里有主题（Scala 的 `cache.theme == theme`），换主题不能重放旧主题的块。
    let mut cache = AnswerRenderCache::new();
    let d = Document::parse("# Title\n\n`x` and [l](https://example.com)\n\nlast");
    let rich = rich_theme();
    for theme in [PLAIN, &rich, PLAIN] {
        assert_eq!(
            laid(&answer_cached(&d, None, &mut cache, theme)),
            laid(&answer(&d, None, theme))
        );
    }
}

#[test]
fn glow_on_a_fully_finalised_document_glows_the_last_block_cached_prefix_intact() {
    let mut cache = AnswerRenderCache::new();
    let d = Document::parse("first para\n\nsecond para");
    let t = glow();
    assert_eq!(
        laid(&answer_cached(&d, Some(&t), &mut cache, PLAIN)),
        laid(&answer(&d, Some(&t), PLAIN))
    );
}

#[test]
fn an_empty_document_with_a_tail_renders_just_the_nib() {
    let t = glow();
    let d = Document::new();
    assert_eq!(
        laid(&answer_cached(
            &d,
            Some(&t),
            &mut AnswerRenderCache::new(),
            PLAIN
        )),
        laid(&answer(&d, Some(&t), PLAIN))
    );
}

// 修复原因：answerCached 起初漏了 answer 对块的两处 landed 处理——非末块（含已最终化与开放尾段里的）表格的
// 落定动画，和末块 Table + ended 的落定。表格「整张落定一次」是 spec 行为（§4.2「笔」的「表格」），丢掉它就是
// 屏幕变化；这些测试钉住缓存版与无缓存版在动画的每一帧逐字节一致。
#[test]
fn cached_render_matches_the_uncached_one_while_a_table_settles_at_every_feed_step_and_age() {
    let rich = rich_theme();
    let mut cache = AnswerRenderCache::new();
    let src = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter the table\n\ntail text";
    feed_steps(src, 11, |d| {
        for age in AGES {
            let t = settling(age, false);
            assert_eq!(
                laid(&answer_cached(d, Some(&t), &mut cache, &rich)),
                laid(&answer(d, Some(&t), &rich))
            );
        }
    });
}

#[test]
fn cached_matches_uncached_when_the_open_tails_last_block_is_a_table_just_ended() {
    let rich = rich_theme();
    let mut cache = AnswerRenderCache::new();
    let mut d = Document::new();
    d.feed_to("intro\n\n| a | b |\n|---|---|\n| 1 | 2 |");
    assert!(
        matches!(d.open_blocks().last(), Some(Block::Table { .. })),
        "{:?}",
        d.open_blocks()
    );
    for age in AGES {
        let t = settling(age, true);
        assert_eq!(
            laid(&answer_cached(&d, Some(&t), &mut cache, &rich)),
            laid(&answer(&d, Some(&t), &rich))
        );
    }
}

#[test]
fn cached_matches_uncached_when_a_fully_finalised_document_is_still_streaming_and_ends_in_a_table()
{
    let rich = rich_theme();
    let mut cache = AnswerRenderCache::new();
    let d = Document::parse("intro\n\n| a | b |\n|---|---|\n| 1 | 2 |");
    assert!(
        matches!(
            d.finalized().last().map(|b| &**b),
            Some(Block::Table { .. })
        ),
        "{:?}",
        d.finalized()
    );
    for age in AGES {
        let t = settling(age, true);
        assert_eq!(
            laid(&answer_cached(&d, Some(&t), &mut cache, &rich)),
            laid(&answer(&d, Some(&t), &rich))
        );
    }
}

#[test]
fn a_settling_tables_landed_wrap_is_transient_wrapped_per_frame_never_written_back() {
    let mut cache = AnswerRenderCache::new();
    let mut d = Document::new();
    d.feed_to("first para\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter\n\nmore tail");
    assert!(
        d.finalized().len() >= 2 && matches!(&*d.finalized()[1], Block::Table { .. }),
        "{:?}",
        d.finalized()
    );
    // 加空行的堆叠是 [首块, br, 表格, br, …]，表格在 children[2]。
    let e1 = answer_cached(&d, Some(&settling(0, false)), &mut cache, PLAIN);
    let e2 = answer_cached(&d, Some(&settling(1, false)), &mut cache, PLAIN);
    let (c1, c2) = (children(&e1), children(&e2));
    let inner1 = match (&c1[2], &c2[2]) {
        (Element::Map(i1, _), Element::Map(i2, _)) => {
            assert!(
                Arc::ptr_eq(memo_of(i1), memo_of(i2)),
                "动画包的是同一份缓存件，不写回缓存"
            );
            memo_of(i1).clone()
        }
        pair => panic!("a settling table should be wrapped per frame: {pair:?}"),
    };
    // 落定恒等（amount ≥ 1）时直接用缓存件，不再包。
    let e3 = answer_cached(&d, Some(&settling(ease::TICKS, false)), &mut cache, PLAIN);
    assert!(
        Arc::ptr_eq(memo_of(&children(&e3)[2]), &inner1),
        "settled: the cached Element is used as-is"
    );
    // 不落动画的块（首段）跨帧是同一实例：缓存件被复用。
    assert!(Arc::ptr_eq(memo_of(&c1[0]), memo_of(&c2[0])));
}
