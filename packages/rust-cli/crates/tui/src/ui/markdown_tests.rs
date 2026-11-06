//! markdown 的适配（tui-kit.md §3.5）：回答的流式增量排版（修复线 F6）、网格与播放头。块的画法本身的用例在 kit
//! `markdown/render_tests.rs`。

use super::*;
use crate::ui::probe::ColorLevel;

fn plain_ink() -> Ink {
    Ink::new(ColorLevel::Ansi16, None, None)
}

fn rich_ink() -> Ink {
    Ink::new(
        ColorLevel::TrueColor,
        Some((220, 220, 220)),
        Some((20, 20, 20)),
    )
}

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

/// 增量排版与整篇重排逐字符相同（含样式）：每篇文档按每个前缀依次排一遍（流式中只在末尾追加）。
#[test]
fn every_prefix_renders_like_a_full_render() {
    for ink in [plain_ink(), rich_ink()] {
        for doc in DOCS {
            for width in [12, 40, 120] {
                let memo = Memo::default();
                let cuts = (0..=doc.len()).filter(|at| doc.is_char_boundary(*at));
                for at in cuts {
                    let text = &doc[..at];
                    let got = memo.answer(text, None, width, &ink);
                    assert_eq!(got, answer(text, width, &ink), "{width} {text:?}");
                }
            }
        }
    }
}

/// 换了文字（不是追加）、换了宽度或主题：仍与整篇重排相同。
#[test]
fn other_texts_widths_and_themes_do_not_reuse_stale_blocks() {
    let memo = Memo::default();
    let (plain, rich) = (plain_ink(), rich_ink());
    let steps = [
        (DOCS[0], 40, &rich),
        (DOCS[1], 40, &rich),
        (DOCS[0], 40, &rich),
        (DOCS[0], 30, &rich),
        (DOCS[0], 30, &plain),
        (&DOCS[0][..DOCS[0].len() / 2], 30, &plain),
        (DOCS[2], 30, &plain),
        (DOCS[3], 30, &rich),
        (DOCS[2], 30, &rich),
    ];
    for (text, width, ink) in steps {
        let got = memo.answer(text, None, width, ink);
        assert_eq!(got, answer(text, width, ink), "{width} {text:?}");
    }
}

/// 追加文字时前面的块确实不再解析（不然上面的对照只是在比整篇重排）。
#[test]
fn appending_keeps_every_finished_block() {
    let memo = Memo::default();
    memo.answer(DOCS[0], None, 40, &plain_ink());
    // DOCS[0] 有 6 个顶层块（标题、段落、代码、列表、标题、段落）；末块后面还没有下一块，仍开着。
    assert_eq!(memo.finalized(), 5);
    let longer = format!("{}\nOne more paragraph.\n", DOCS[0]);
    memo.answer(&longer, None, 40, &plain_ink());
    assert_eq!(memo.finalized(), 6);
}

/// 回答从第 3 列起，折行宽度是 `min(宽 − 2, 99)`（Puffin `entries::answer`，tui-kit.md §4.2）。
#[test]
fn answers_start_at_column_three_within_the_measure() {
    let lines = answer("one two three four five", 16, &plain_ink());
    let shown: Vec<String> = lines
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect();
    // 宽 16：正文宽 14，去掉第 3 列前的三列剩 11 格，`one two three` 放不下。
    assert_eq!(shown, ["   one two", "   three four", "   five"]);
}

/// 播放头在词中间时，排版到词尾而词尾的字母藏起来（Puffin `consts::laid_text`）；开启一行的块标记整个显示。
#[test]
fn the_playhead_lays_out_whole_words() {
    assert_eq!(laid_text("hello world", 0), (String::new(), 0));
    assert_eq!(laid_text("hello world", 2), (format!("he{HIDDEN}llo"), 5));
    assert_eq!(laid_text("hello world", 5), ("hello".to_owned(), 5));
    assert_eq!(laid_text("- item", 1), ("-".to_owned(), 1));
}

// 回归：末字是多字节字符的词不让块标记的判断 panic（原来按字节切下最后一个字符）。
#[test]
fn a_word_ending_in_a_multibyte_character_is_not_a_marker_and_does_not_panic() {
    assert!(!is_marker("±×÷"));
    assert!(!is_marker("÷"));
    assert!(!is_marker("÷."));
    assert!(is_marker("1."));
    assert!(is_marker("123456789)"));
    assert!(!is_marker("1234567890."));
    assert!(!is_marker("x."));
    // 流式时播放头停在这个词里：整段照常排版。
    let text = "±×÷ and more";
    for shown in 1..=text.len() {
        if text.is_char_boundary(shown) {
            let _ = laid_text(text, shown);
        }
    }
    assert_eq!(laid_text(text, 2).1, "±×÷".len());
}

/// 手停住 `age` 个 tick 的笔。
fn still_pen(ink: &Ink, age: i64) -> Pen {
    Pen {
        theme: ink.theme,
        frame: 0,
        still: Some(pen::Still { age, turn_ms: 0.0 }),
        held: false,
    }
}

/// 播放头显示完 `text`、上次动在 tick `moved` 时，tick 100 上回答的最后一行与笔骑了没有。
fn tail_of(text: &str, moved: i64, pen: &Pen, ink: &Ink) -> (String, usize, bool) {
    let mut head = Playhead::start(text.len());
    head.moved = moved;
    let (lines, rode) = streaming(
        &Memo::default(),
        text,
        &head,
        100,
        Some(pen),
        false,
        false,
        60,
        ink,
    );
    let last = lines.last().map(StyledLine::plain).unwrap_or_default();
    (last.trim_end().to_owned(), lines.len(), rode)
}

/// 回答停住满 0.5 s：笔尖换成点阵，在最后一个字之后空一格，字与行数不变（docs/puffin-tui.md §4.2「手停住」）。
/// 修前笔尖一直在：说完一句、在写下一个调用的输入时屏幕上没有点阵。
#[test]
fn a_stopped_answer_ends_in_the_lattice_and_nothing_moves() {
    let ink = rich_ink();
    let text = "Now the next thing.";
    let grace = rust_cli_tui_kit::app::widgets::lattice::GRACE_TICKS;
    let (nib, nib_rows, _) = tail_of(text, 90, &still_pen(&ink, grace - 1), &ink);
    assert!(!nib.contains('⋮'), "the grace period has no lattice: {nib}");
    let (sign, sign_rows, rode) = tail_of(text, 90, &still_pen(&ink, grace), &ink);
    assert_eq!(sign, "   Now the next thing. ⋮⋮⋮");
    assert_eq!(sign_rows, nib_rows);
    assert!(
        rode,
        "the lattice takes the nib's place: no line of its own"
    );
}

/// 下一个词落下的那一帧点阵就离开：手的时钟在排完之后才更新，这一帧看播放头。
#[test]
fn the_lattice_leaves_on_the_frame_the_next_word_lands() {
    let ink = rich_ink();
    let (line, _, _) = tail_of("Now the next", 100, &still_pen(&ink, 60), &ink);
    assert!(!line.contains('⋮'), "{line}");
}

/// 字后面已经是空白时不再空一格（同思考 ticker 上的点阵）。
#[test]
fn the_lattice_needs_no_second_space_after_a_space() {
    let ink = rich_ink();
    let mut head = Playhead::start("Now the ".len());
    head.moved = 0;
    let (lines, _) = streaming(
        &Memo::default(),
        "Now the nex",
        &head,
        100,
        Some(&still_pen(&ink, 60)),
        false,
        false,
        60,
        &ink,
    );
    let last = lines.last().unwrap().plain();
    assert!(last.starts_with("   Now the ⋮⋮⋮"), "{last}");
    assert_eq!(
        rust_cli_tui_kit::render::width::string_width(&last),
        "   Now the nex".len()
    );
}

/// 还在到达的表格的位置上只有点阵，它的表头不先打出来。
#[test]
fn an_arriving_table_waits_behind_the_lattice() {
    let ink = rich_ink();
    let (line, _, rode) = tail_of("| a | b |", 90, &still_pen(&ink, 60), &ink);
    assert_eq!(line, "   ⋮⋮⋮");
    assert!(rode);
}
