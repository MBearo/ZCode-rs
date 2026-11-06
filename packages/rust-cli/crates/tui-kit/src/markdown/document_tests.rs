//! 增量文档的测试。
//! 移植自 packages/tui/tui/test/src/zcode/tui/markdown/MarkdownDocumentSuite.scala（feat/tui a2bf878b34）。
//! Scala 的偏移按 UTF-16 单元，这里按字节；用例的文字都是 ASCII 时两者相同，另加了非 ASCII 的用例。
use std::sync::Arc;

use super::ast::{Block, Inline};
use super::block_parser::{parse, parse_spans, split_lines};
use super::document::Document;
use super::inline_parser;

/// 播放头的隐藏标记（渲染里的 `Hidden`）。
const HIDDEN: char = '\u{E000}';

fn para(s: &str) -> Block {
    Block::Paragraph(inline_parser::parse(s))
}

fn blocks(d: &Document) -> Vec<Block> {
    d.blocks().cloned().collect()
}

fn fed(prefix: &str) -> Document {
    let mut d = Document::new();
    d.feed_to(prefix);
    d
}

fn relaid(prefix: &str) -> Document {
    let mut d = Document::new();
    d.relaid(prefix);
    d
}

/// 每个字符边界处的前缀，像打字机揭示那样。
fn prefixes(s: &str) -> impl Iterator<Item = &str> {
    s.char_indices().map(|(k, c)| &s[..k + c.len_utf8()])
}

/// 一个字符一个字符地喂。
fn feed_char_by_char(s: &str) -> Document {
    let mut d = Document::new();
    for p in prefixes(s) {
        d.feed_to(p);
    }
    d
}

/// 老的全量算法（原 `MarkdownDocument.blockEnds(text)` 的逐字实现），作为增量结果的独立参照。
fn full_ends(text: &str) -> Vec<usize> {
    let b = text.as_bytes();
    parse_spans(&split_lines(text))
        .iter()
        .map(|sp| {
            let (mut seen, mut k) = (0, 0);
            while k < b.len() && seen < sp.end_line {
                if b[k] == b'\n' {
                    seen += 1;
                }
                k += 1;
            }
            k.min(b.len())
        })
        .collect()
}

/// 模拟壳逐帧铺出的文字：前 `shown` 个字符，加隐藏标记，加正在打的那个词的剩余部分。
/// 帧与帧之间不是追加关系——隐藏标记在词内前移，同一个词里各帧等长但内容不同。
fn laid(text: &str, shown: usize) -> String {
    let cs: Vec<char> = text.chars().collect();
    if shown == 0 {
        return String::new();
    }
    if shown >= cs.len() {
        return text.to_owned();
    }
    let mut e = shown;
    while e < cs.len() && cs[e] != ' ' && cs[e] != '\n' && e - shown < 24 {
        e += 1;
    }
    let mut out: String = cs[..shown].iter().collect();
    if e != shown {
        out.push(HIDDEN);
        out.extend(&cs[shown..e]);
    }
    out
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

#[test]
fn an_open_document_renders_the_same_blocks_as_a_one_shot_parse() {
    for src in [
        "# H\n\nbody text",
        "a\n\nb\n\nc",
        "- one\n- two\n\nafter",
        "```scala\nval x = 1\n```\n\ndone",
        "> quote\n> more\n\ntail",
        "| a | b |\n|---|---|\n| 1 | 2 |\n\nnext",
    ] {
        assert_eq!(blocks(&fed(src)), parse(src), "{src}");
    }
}

#[test]
fn feeding_character_by_character_converges_to_the_one_shot_parse() {
    let src =
        "# Title\n\nA paragraph with **bold** and `code`.\n\n- a\n- b\n\n```\nx = 1\n```\n\nEnd.";
    let mut d = feed_char_by_char(src);
    d.close();
    assert_eq!(blocks(&d), parse(src));
}

#[test]
fn closed_and_open_renderings_agree_for_finished_text() {
    let src = "para one\n\npara two";
    let mut d = fed(src);
    d.close();
    assert_eq!(blocks(&d), parse(src));
}

#[test]
fn earlier_blocks_finalise_and_are_not_disturbed_by_later_feeds() {
    let a = fed("alpha\n\nbeta\n\n");
    assert!(
        a.finalized_count() >= 1,
        "expected a finalised block, got {}",
        a.finalized_count()
    );
    let mut b = a.clone();
    b.feed_to("alpha\n\nbeta\n\ngamma");
    // 最终化的前缀在之后的喂入里原样保留——连同 Arc 的身份（渲染缓存靠它认块）。
    let kept = &b.finalized()[..a.finalized_count()];
    assert_eq!(kept, a.finalized());
    assert!(
        kept.iter()
            .zip(a.finalized())
            .all(|(x, y)| Arc::ptr_eq(x, y))
    );
    assert_eq!(blocks(&b), vec![para("alpha"), para("beta"), para("gamma")]);
}

#[test]
fn an_unclosed_emphasis_is_literal_until_its_closer_is_revealed() {
    let mut d = fed("**wor");
    assert_eq!(
        blocks(&d),
        vec![Block::Paragraph(vec![Inline::Text("**wor".into())])]
    );
    d.feed_to("**world**");
    assert_eq!(
        blocks(&d),
        vec![Block::Paragraph(vec![Inline::Strong(vec![Inline::Text(
            "world".into()
        )])])]
    );
}

#[test]
fn a_paragraph_snaps_to_a_setext_heading_when_the_underline_arrives() {
    let mut d = fed("Title\n");
    assert_eq!(blocks(&d), vec![para("Title")]);
    d.feed_to("Title\n===");
    assert_eq!(
        blocks(&d),
        vec![Block::Heading {
            level: 1,
            content: vec![Inline::Text("Title".into())]
        }]
    );
}

#[test]
fn a_paragraph_snaps_to_a_table_when_the_delimiter_row_arrives() {
    let mut d = fed("a | b\n");
    assert_eq!(blocks(&d), vec![para("a | b")]);
    d.feed_to("a | b\n--- | ---\n");
    assert!(matches!(d.blocks().next(), Some(Block::Table { .. })));
}

#[test]
fn a_fence_stays_open_across_feeds_and_closes_only_on_its_closing_fence() {
    let code = |c: &str| Block::Code {
        info: None,
        lang: None,
        code: c.into(),
        fenced: true,
    };
    let mut d = fed("```\nco");
    assert_eq!(blocks(&d), vec![code("co")]);
    d.feed_to("```\ncode\n```");
    assert_eq!(blocks(&d), vec![code("code")]);
}

#[test]
fn the_partial_last_line_is_never_finalised_before_its_newline() {
    let d = fed("done\n\nopen line still typing");
    // 只有第一段可以最终化；末尾这行还开着。
    assert!(d.finalized_count() <= 1);
    assert_eq!(d.last_block(), Some(&para("open line still typing")));
}

#[test]
fn incremental_feeding_scales_over_many_blocks() {
    let src = (1..=400)
        .map(|i| format!("paragraph number {i}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut d = Document::new();
    let mut sb = String::new();
    for l in src.split('\n') {
        sb.push_str(l);
        sb.push('\n');
        d.feed_to(&sb);
    }
    d.close();
    assert_eq!(d.block_count(), 400);
    assert_eq!(blocks(&d), parse(&src));
}

/* —— streaming 每帧铺出的形态：隐藏标记在正在打的词里前移（relaid 路径） —— */

#[test]
fn a_laid_shaped_relaid_stays_equal_to_the_one_shot_parse_of_the_same_laid() {
    let t = "# Head\n\nfirst paragraph with **bold** and `code` spans\n\n- one\n- two\n\n```scala\nval x = 1\n```\n\n| a | b |\n| -- | -- |\n| 1 | 2 |\n\ntail paragraph still going";
    let mut d = Document::new();
    let n = char_len(t);
    for s in (1..n).step_by(5).chain([n]) {
        let l = laid(t, s);
        d.relaid(&l);
        assert_eq!(blocks(&d), parse(&l), "blocks at shown={s}");
        assert_eq!(d.block_ends(), full_ends(&l), "blockEnds at shown={s}");
    }
    assert_eq!(blocks(&d), parse(t));
}

#[test]
fn a_laid_cut_at_a_line_start_before_a_pipe_row_matches_the_one_shot_parse() {
    // 表格行正在到达时铺出的文字停在行首（`|` 开头的行整行等待），以换行结尾：增量解析不能比全量多出一条空行。
    let t = "intro\n\n| a | b |\n| -- | -- |\n| 1 | 2 |\n";
    let mut d = Document::new();
    for (k, _) in t.match_indices('\n') {
        let l = &t[..=k];
        d.relaid(l);
        assert_eq!(blocks(&d), parse(l), "blocks cut at {k}");
        assert_eq!(d.block_ends(), full_ends(l), "blockEnds cut at {k}");
    }
}

#[test]
fn an_open_tail_that_ends_on_a_newline_has_no_phantom_blank_line() {
    // 修复原因（Scala 原注）：开放尾段的切行曾保留末尾换行切出的空行，全量解析会丢掉它；
    // 未闭合围栏的增量块体比全量多一个空行（"code\n" 对 "code"）。
    let t = "```scala\ncode\n";
    assert_eq!(blocks(&fed(t)), parse(t));
    assert_eq!(relaid(t).open_blocks(), parse(t).as_slice());
}

#[test]
fn relaid_does_not_mistake_an_equal_length_laid_for_an_unchanged_one() {
    // 帧与帧等长但内容不同（隐藏标记前移）是常态；feed_to 的等长短路在这里会留下过期的文档。
    let mut d = fed("alpha\n\nbeta\n\ngood");
    d.relaid("alpha\n\nbeta\n\ng0od");
    assert_eq!(blocks(&d), parse("alpha\n\nbeta\n\ng0od"));
}

#[test]
fn relaid_of_an_unrelated_text_rebuilds_instead_of_reusing_the_finalised_state() {
    // 文本被整体换掉（回退、重试）：必须整个重建，且按最终化的语义定位（消费点从安全行边界起）。
    let mut d = relaid("alpha\n\nbeta\n\n");
    assert!(d.finalized_count() >= 1);
    d.relaid("zzz");
    assert_eq!(blocks(&d), parse("zzz"));
    assert_eq!(d.consumed(), 0);
}

#[test]
fn block_ends_of_an_incrementally_fed_table_text_match_the_full_computation() {
    let t = "before\n\n| a | b | c |\n| -- | -- | -- |\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n\nafter the table ends";
    let mut d = Document::new();
    for s in (1..char_len(t)).step_by(7) {
        d.relaid(&laid(t, s));
        assert_eq!(d.block_ends(), full_ends(d.source()), "at shown={s}");
    }
    d.relaid(t);
    assert_eq!(d.block_ends(), full_ends(t));
    assert_eq!(d.block_ends().last(), Some(&t.len()));
}

/* —— 回车归一：增量路径的切行必须与全量 split_lines 一致 —— */

#[test]
fn lone_carriage_returns_normalise_like_a_one_shot_parse_blocks_and_ends_alike() {
    // 修复原因（Scala 原注）：增量路径曾按原始 \n 切行、只剥行尾 \r，行中的单独 \r 留在行内容里；
    // 全量切行会把它归一成换行。含 \r 的模型输出在新旧路径下分块不同。
    let t = "alpha\rbeta\n\npara with \r inside\n\n| a | b |\r| - | - |\r| 1 | 2 |\n\r\nlast";
    let mut d = Document::new();
    for s in (1..char_len(t)).step_by(5) {
        let l = laid(t, s);
        d.relaid(&l);
        assert_eq!(blocks(&d), parse(&l), "blocks at shown={s}");
        assert_eq!(d.block_ends(), full_ends(&l), "blockEnds at shown={s}");
    }
    d.relaid(t);
    assert_eq!(blocks(&d), parse(t));
    assert_eq!(d.block_ends(), full_ends(t));
}

#[test]
fn crlf_input_normalises_the_same_through_relaid_as_in_one_shot() {
    let t = "alpha\r\n\r\nbeta\r\n\r\n| a | b |\r\n| - | - |\r\n| 1 | 2 |\r\n";
    let mut d = Document::new();
    for (k, _) in t.match_indices('\n') {
        let l = &t[..=k];
        d.relaid(l);
        assert_eq!(blocks(&d), parse(l), "blocks cut at {k}");
        assert_eq!(d.block_ends(), full_ends(l), "blockEnds cut at {k}");
    }
}

#[test]
fn a_relaid_that_falls_back_to_a_rebuild_rejoins_the_incremental_path() {
    // 回退 / 重试触发的重建不能把 consumed 顶到文本末尾：那样之后带隐藏标记、永不严格扩展上一帧的文字
    // 会让每帧都重建，退化成每帧全量解析。
    let t1 = "alpha\n\nbeta\n\ngamma tail";
    let t2 = "different text\n\nsecond block\n\nmore words here";
    let mut d = relaid(t1);
    assert!(d.finalized_count() >= 1);
    d.relaid(t2); // 无关文本：重建
    assert_eq!(blocks(&d), parse(t2));
    assert!(
        d.consumed() < t2.len(),
        "a rebuilt document must keep an incremental position"
    );
    // 重建后的下一帧带着隐藏标记：必须仍走增量路径（consumed 停在行边界）。
    let l = laid(t2, char_len(t2) - 3);
    d.relaid(&l);
    assert_eq!(blocks(&d), parse(&l));
    assert!(
        d.consumed() < l.len(),
        "a Hidden-bearing frame after a rebuild must stay incremental"
    );
    assert!(d.finalized_count() >= 1);
    // 之后文本继续增长，最终化继续前进。
    d.relaid(&format!("{t2}\n\na third block arrives"));
    assert!(
        d.finalized_count() >= 2,
        "expected finalisation to advance again, got {}",
        d.finalized_count()
    );
}

/* —— 最终化的判据：下一块已在完整行上开始，而不是「块尾之后有两行完整」 —— */

/// `t` 的每个前缀依次喂进 relaid，块与块尾都要和一次性解析一样。
fn same_as_one_shot(t: &str) {
    let mut d = Document::new();
    for l in prefixes(t) {
        d.relaid(l);
        assert_eq!(
            blocks(&d),
            parse(l),
            "blocks at prefix {} of {t:?}",
            l.len()
        );
        assert_eq!(
            d.block_ends(),
            full_ends(l),
            "blockEnds at prefix {} of {t:?}",
            l.len()
        );
    }
}

#[test]
fn a_block_parsed_past_a_run_of_blank_lines_finalises_only_once_the_next_block_has_begun() {
    // 修复原因（Scala 原注）：判据「块尾之后再有两行完整」假定每种块至多多看两行；缩进代码块越过任意多的空行
    // 去看下一行是否还是代码，于是 "    a\n\n\n\n    b" 流式时画成两块代码板、提交后合成一块。
    for t in [
        "    a\n\n\n\n    b\n\nend",
        "    a\n\n\n\n    b",
        "- a\n\n\n- b\n\nend",
        "- a\n\n\n  b\n\nend",
        "1. a\n\n\n2. b\n\nend",
        "> q\n\n\n> r\n\nend",
        "Title\n===\nx\n\ny",
        "a | b\n|---|---|\n| 1 | 2 |\n\nx",
        "```\ncode\n\n\n\nmore\n```\n\nx",
    ] {
        same_as_one_shot(t);
    }
}

#[test]
fn random_line_soup_fed_prefix_by_prefix_matches_the_one_shot_parse() {
    let kinds = [
        "- a",
        "- b",
        "1. x",
        "2. y",
        "    code",
        "  cont",
        "",
        "",
        "",
        "text",
        "more text",
        "> q",
        "# h",
        "===",
        "---",
        "| a | b |",
        "|---|---|",
        "| 1 | 2 |",
        "```",
        "    more code",
    ];
    // Scala 用 scala.util.Random(7)；这里用一个固定种子的线性同余，覆盖同样的组合空间。
    // 命中继承的单列表格缺陷（见下一个测试）的组合跳过：Scala 版同样不一致，只是它的种子没碰上。
    let mut state = 7u64;
    let mut next = |n: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((state >> 33) % n as u64).unwrap_or(0)
    };
    for _ in 0..300 {
        let n = 2 + next(7);
        let mut t = (0..n)
            .map(|_| kinds[next(kinds.len())])
            .collect::<Vec<_>>()
            .join("\n");
        if next(2) == 0 {
            t.push('\n');
        }
        if hits_one_column_table_defect(&t) {
            continue;
        }
        same_as_one_shot(&t);
    }
}

/// 一行两列的分隔行紧跟在一条没有竖线的非空行后面：继承的单列表格缺陷的前提。
fn hits_one_column_table_defect(t: &str) -> bool {
    let lines: Vec<&str> = t.split('\n').collect();
    lines
        .windows(2)
        .any(|w| w[1] == "|---|---|" && !w[0].contains('|') && !w[0].trim().is_empty())
}

#[test]
fn inherited_defect_a_partial_delimiter_row_finalises_the_paragraph_before_it() {
    // 继承自 Scala 的缺陷，按原样保留（画面门禁要求与 Scala 逐格相同；Scala.js 实跑结果相同）：
    // 分隔行打到 "|---|" 时它只有一列，与上一行 "  cont" 构成单列表格，于是前面的段落在 "  cont" 这条完整行上
    // 被判为最终；分隔行长成 "|---|---|" 后表格不成立，一次性解析得到一个三行的段落，增量文档却留着两段，
    // 关闭后也不再变。根因：最终化只看到「下一块开始的那一行」完整，而表格的成立还取决于它后面那条未完成的行。
    let t = "| a | b |\n  cont\n|---|---|\n";
    let mut d = Document::new();
    for l in prefixes(t) {
        d.relaid(l);
    }
    let inc = vec![para("| a | b |"), para("cont\n|---|---|")];
    assert_eq!(blocks(&d), inc);
    assert_eq!(d.finalized_count(), 1);
    d.close();
    assert_eq!(blocks(&d), inc);
    assert_eq!(parse(t), vec![para("| a | b |\ncont\n|---|---|")]);
}

#[test]
fn a_rewrite_inside_the_line_that_proved_a_block_final_rebuilds_the_document() {
    // 修复原因（Scala 原注）：relaid 曾只比对到消费点，而最终化还看过消费点之后的第一条非空行
    // （表格分隔行、setext 下划线就在那里）。比对范围要覆盖最终化看过的那一行。
    let mut d = relaid("a | b\n\n\nx\n");
    assert!(d.finalized_count() >= 1, "{d:?}");
    d.relaid("a | b\n|---|---|\nx\n");
    assert_eq!(blocks(&d), parse("a | b\n|---|---|\nx\n"));
    let mut h = relaid("Title\n\nx\n");
    assert!(h.finalized_count() >= 1, "{h:?}");
    h.relaid("Title\n===\nx\n");
    assert_eq!(blocks(&h), parse("Title\n===\nx\n"));
}

/* —— 移植时加的 —— */

#[test]
fn offsets_are_bytes_into_the_source() {
    let t = "中文段落\n\n第二段 😀\n\n";
    let d = Document::parse(t);
    assert_eq!(d.block_ends(), full_ends(t));
    assert_eq!(
        d.block_ends(),
        vec!["中文段落\n".len(), "中文段落\n\n第二段 😀\n".len()]
    );
    same_as_one_shot(
        "中文 **粗** 体\n\n- 条目 ①\n- 😀\n\n| 列 | 列 |\n|---|---|\n| 甲 | 乙 |\n\n尾",
    );
}

#[test]
fn documents_are_equal_when_they_render_the_same_blocks() {
    let src = "a\n\nb\n\nc";
    let mut streamed = feed_char_by_char(src);
    let shot = Document::parse(src);
    assert_ne!(streamed.finalized_count(), shot.finalized_count());
    assert_eq!(streamed, shot);
    streamed.close();
    assert_eq!(streamed.finalized_count(), shot.finalized_count());
    assert_ne!(Document::parse("a"), shot);
}

#[test]
fn feed_appends_a_delta() {
    let mut d = Document::new();
    for delta in ["# T", "itle\n\nbo", "dy\n", "", "\nend"] {
        d.feed(delta);
    }
    assert_eq!(d.source(), "# Title\n\nbody\n\nend");
    assert_eq!(blocks(&d), parse(d.source()));
    assert_eq!(d.block_count(), 3);
}
