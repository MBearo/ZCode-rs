//! 按块缓存的排版与整篇重排逐字符相同（修复线 F6）：每篇文档按每个前缀依次排一遍（流式中只在末尾追加），
//! 每一步都与 [`super::super::render`] 比较，含样式。
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::Memo;
use crate::ui::palette::{ColorLevel, Palette};

/// 覆盖各种块：标题（含 setext）、段落、强调与行内代码、链接、围栏与缩进代码（含没闭合的）、紧凑与松散列表、
/// 嵌套列表、任务列表、引用、表格、HTML 块、分隔线、硬换行。
const DOCS: [&str; 6] = [
    "## Section 1\n\nSome **bold** text and `code` with a [link](https://example.com/1). Lorem ipsum dolor sit amet, consectetur adipiscing elit.\n\n```rust\nfn main() {\n    let x = vec![1, 2, 3];\n    for i in x { println!(\"{}\", i); }\n}\n```\n\n- item one\n- item two\n- item three\n\n## Section 2\n\nMore text.\n",
    "Title\n=====\n\nA paragraph\nthat continues\nlazily.\n\nSub\n---\n\n1. first\n2. second\n\n   still second\n3. third\n\n> quoted *text*\n> more\n>\n> - inside\n\n---\n\nend  \nhard break\n",
    "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\nafter table\n\n<div>\nraw html\n</div>\n\ntext after html\n\n    indented code\n    more\n\n- [ ] task\n  - [x] done\n  - child\n- tail\n",
    "Intro with a [ref] link.\n\nMore.\n\n[ref]: https://example.com/ref\n\nAfter the definition.\n",
    "```python\nprint('open fence never closed')\nfor i in range(3):\n    print(i)\n",
    "- a\n- b\n\n- c\n\npara\n\n* x\n+ y\n\n~~gone~~ and _em_ and ***both***\n\n<!-- comment -->\nline\n",
];

fn palettes() -> [Palette; 2] {
    [
        Palette::new(ColorLevel::Ansi16, None),
        Palette::new(ColorLevel::TrueColor, Some((20, 20, 20))),
    ]
}

fn prefix() -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    (vec![Span::raw("⏺ ")], vec![Span::raw("  ")])
}

fn full(text: &str, width: usize, palette: &Palette) -> Vec<ratatui::text::Line<'static>> {
    let (first, rest) = prefix();
    super::super::render(text, width, first, rest, Style::default(), palette)
}

#[test]
fn every_prefix_renders_like_a_full_render() {
    for palette in palettes() {
        for doc in DOCS {
            for width in [12, 40, 120] {
                let memo = Memo::default();
                let cuts = (0..=doc.len()).filter(|at| doc.is_char_boundary(*at));
                for at in cuts {
                    let text = &doc[..at];
                    let got = memo.render(text, width, prefix(), Style::default(), &palette);
                    assert_eq!(got, full(text, width, &palette), "{width} {text:?}");
                }
            }
        }
    }
}

/// 换了文字（不是追加）、换了宽度或样式：仍与整篇重排相同。
#[test]
fn other_texts_widths_and_styles_do_not_reuse_stale_blocks() {
    let palette = palettes()[1];
    let memo = Memo::default();
    let dim = Style::default().add_modifier(Modifier::DIM);
    let steps = [
        (DOCS[0], 40, Style::default()),
        (DOCS[1], 40, Style::default()),
        (DOCS[0], 40, Style::default()),
        (DOCS[0], 30, Style::default()),
        (DOCS[0], 30, dim),
        (&DOCS[0][..DOCS[0].len() / 2], 30, dim),
        (DOCS[2], 30, dim),
        (DOCS[3], 30, dim),
        (DOCS[2], 30, dim),
    ];
    for (text, width, base) in steps {
        let got = memo.render(text, width, prefix(), base, &palette);
        let (first, rest) = prefix();
        let expected = super::super::render(text, width, first, rest, base, &palette);
        assert_eq!(got, expected, "{width} {text:?}");
    }
}

/// 追加文字时前面的块确实接上了缓存（不然上面的对照只是在比整篇重排）。
#[test]
fn appending_reuses_every_finished_block() {
    let palette = palettes()[0];
    let memo = Memo::default();
    memo.render(DOCS[0], 40, prefix(), Style::default(), &palette);
    // DOCS[0] 有 6 个顶层块（标题、段落、代码、列表、标题、段落）；追加一段后它们都接上。
    let longer = format!("{}\nOne more paragraph.\n", DOCS[0]);
    memo.render(&longer, 40, prefix(), Style::default(), &palette);
    assert_eq!(memo.reused(), 6);
}
