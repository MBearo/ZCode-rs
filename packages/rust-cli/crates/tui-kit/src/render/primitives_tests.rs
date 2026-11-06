//! 移植自 packages/tui/tui/test/src/zcode/tui/render/PrimitivesSuite.scala 与 StyledLineSuite.scala（feat/tui a2bf878b34）。
//! Width 的采纳改为测纯值 `Widths`（docs/specs/rust-cli/tui-kit.md §3.6）：进程级的采纳不在并行的测试里碰。
use super::width::Widths;
use super::width_tables as tables;
use super::*;

const ESC: &str = "\x1b";

#[test]
fn cell_packs_and_unpacks_codepoint_style_id_width_class() {
    let c = Cell::pack('\u{1f600}', 5, WidthClass::Wide);
    assert_eq!(c.code_point(), '\u{1f600}');
    assert_eq!(c.style_id(), 5);
    assert_eq!(c.width_class(), WidthClass::Wide);
    let big = Cell::pack('x', 1_000_000, WidthClass::ZeroWidth);
    assert_eq!(big.code_point(), 'x');
    assert_eq!(big.style_id(), 1_000_000);
    assert_eq!(big.width_class(), WidthClass::ZeroWidth);
    assert_eq!(Cell::BLANK.code_point(), ' ');
    assert_eq!(Cell::BLANK.style_id(), 0);
    assert_ne!(Cell::UNKNOWN, Cell::BLANK);
}

#[test]
fn width_ascii_cjk_emoji_combining() {
    let _w = crate::testing::width_lock::widths();
    assert_eq!(width::display_width('a'), 1);
    assert_eq!(width::display_width(' '), 1);
    assert_eq!(width::display_width('\u{4e00}'), 2);
    assert_eq!(width::display_width('\u{ac00}'), 2);
    assert_eq!(width::display_width('\u{1f600}'), 2);
    assert_eq!(width::display_width('\u{301}'), 0);
    assert_eq!(width::display_width('\u{200b}'), 0);
    assert_eq!(width::display_width('\u{1b}'), 0);
}

#[test]
fn width_matches_the_terminal_for_every_emoji_presentation_class() {
    let w = Widths::DEFAULT;
    for cp in [
        '\u{2705}',
        '\u{274c}',
        '\u{26a1}',
        '\u{2b50}',
        '\u{2728}',
        '\u{23f0}',
        '\u{231a}',
        '\u{1f680}',
        '\u{1f6d1}',
        '\u{1f7e2}',
    ] {
        assert_eq!(w.display_width(cp), 2, "U+{:x}", cp as u32);
    }
    for cp in ['\u{1f321}', '\u{1f5a5}', '\u{1f441}'] {
        assert_eq!(w.display_width(cp), 1, "U+{:x}", cp as u32);
    }
    for cp in ['\u{fe0f}', '\u{200d}', '\u{1f3fb}', '\u{3099}'] {
        assert_eq!(w.display_width(cp), 0, "U+{:x}", cp as u32);
    }
    assert_eq!(w.display_width('\u{3248}'), 1);
    assert_eq!(w.display_width('\u{ad}'), 1);
    assert_eq!(w.display_width('\u{1f1e6}'), 1);
}

#[test]
fn width_range_tables_are_sorted_and_disjoint() {
    fn shape(name: &str, t: &[(u32, u32)]) {
        for (i, &(lo, hi)) in t.iter().enumerate() {
            assert!(lo <= hi, "{name} range {i} inverted");
            if let Some(&(next, _)) = t.get(i + 1) {
                assert!(hi < next, "{name} ranges {i}/{} overlap or unsorted", i + 1);
            }
        }
    }
    shape("ZERO_WIDTH", tables::ZERO_WIDTH);
    shape("WIDE", tables::WIDE);
    shape("EMOJI_BMP", tables::EMOJI_BMP);
    shape("EMOJI_ASTRAL", tables::EMOJI_ASTRAL);
    shape("AMBIGUOUS", tables::AMBIGUOUS);
    shape("RISKY", tables::RISKY);
    // 一个码点不能在两张表里：先查的表赢，重叠会悄悄遮掉一个类。
    fn sweep_wide(name: &str, t: &[(u32, u32)]) {
        for &(lo, hi) in t {
            for c in lo..=hi {
                if let Some(cp) = char::from_u32(c) {
                    assert_eq!(
                        Widths::DEFAULT.display_width(cp),
                        2,
                        "{name} U+{c:x} shadowed by an earlier table"
                    );
                }
            }
        }
    }
    sweep_wide("WIDE", tables::WIDE);
    sweep_wide("EMOJI_BMP", tables::EMOJI_BMP);
    sweep_wide("EMOJI_ASTRAL", tables::EMOJI_ASTRAL);
}

#[test]
fn adopted_class_widths_reshape_only_the_disputed_classes() {
    let w = Widths::DEFAULT.adopt(1, 1, 2);
    assert_eq!(w.display_width('\u{2705}'), 1);
    assert_eq!(w.display_width('\u{1f600}'), 1);
    assert_eq!(w.display_width('\u{2460}'), 2);
    assert_eq!(w.display_width('\u{2502}'), 2);
    assert_eq!(w.display_width('\u{4e00}'), 2);
    assert_eq!(w.display_width('a'), 1);
    assert_eq!(w.display_width('\u{fe0f}'), 0);
    assert_eq!(Widths::DEFAULT.display_width('\u{2705}'), 2);
    assert_eq!(Widths::DEFAULT.display_width('\u{2460}'), 1);
    // 不合理的值按类忽略。
    let w = Widths::DEFAULT.adopt(7, 0, 300);
    assert_eq!(w, Widths::DEFAULT);
}

#[test]
fn risky_marks_the_disputed_blocks_only() {
    assert!(width::risky('\u{2705}'));
    assert!(width::risky('\u{1f600}'));
    assert!(!width::risky('a'));
    assert!(!width::risky('\u{4e00}'));
    assert!(!width::risky('\u{3030}'));
}

#[test]
fn surface_packs_a_bmp_wide_emoji_as_lead_and_spacer() {
    let _w = crate::testing::width_lock::widths();
    let mut pool = StylePool::new();
    let s = Surface::build(
        4,
        &[StyledLine::text("\u{2705}ab", Style::DEFAULT)],
        &mut pool,
    );
    assert_eq!(s.at(0, 0).code_point(), '\u{2705}');
    assert_eq!(s.at(0, 0).width_class(), WidthClass::Wide);
    assert_eq!(s.at(0, 1).width_class(), WidthClass::Spacer);
    assert_eq!(s.at(0, 2).code_point(), 'a');
    assert_eq!(s.at(0, 3).code_point(), 'b');
}

#[test]
fn a_blank_edge_cell_on_a_background_becomes_a_block_with_no_background() {
    let _w = crate::testing::width_lock::widths();
    let panel = Color::rgb(33, 34, 38);
    let mut pool = StylePool::new();
    let edge = pool.intern(Style::fg(panel), &Decor::PLAIN);
    let fill = pool.intern(Style::bg(panel), &Decor::PLAIN);
    let mut s = Surface::build(4, &[StyledLine::text("    ", Style::bg(panel))], &mut pool);
    // 逻辑上的格子不变：只有写给终端之前才换。
    assert_eq!(s.at(0, 0), Cell::pack(' ', fill, WidthClass::Narrow));
    s.keep_margins_clean(&mut pool);
    assert_eq!(s.at(0, 0), Cell::pack('█', edge, WidthClass::Narrow));
    assert_eq!(s.at(0, 1), Cell::pack(' ', fill, WidthClass::Narrow));
    assert_eq!(s.at(0, 2), Cell::pack(' ', fill, WidthClass::Narrow));
    assert_eq!(s.at(0, 3), Cell::pack('█', edge, WidthClass::Narrow));
}

#[test]
fn edge_cells_that_are_not_plain_blanks_on_a_background_stay() {
    let _w = crate::testing::width_lock::widths();
    let panel = Color::rgb(33, 34, 38);
    let mut pool = StylePool::new();
    let lines = [
        // 文字贴边：字不能换。
        StyledLine::text("ab", Style::bg(panel)),
        // 没有底色的空白：本来就不延伸。
        StyledLine::text("  ", Style::DEFAULT),
        // 光标格：反显的一格，换了就看不见了。
        StyledLine::new(vec![Span::new(" ", Style::CARET.with_bg(panel))]),
        // 宽字形的尾格贴右边：不是空白。
        StyledLine::new(vec![Span::new("\u{4e2d}", Style::bg(panel))]),
    ];
    let mut s = Surface::build(2, &lines, &mut pool);
    let before = s.clone();
    s.keep_margins_clean(&mut pool);
    assert_eq!(s, before);
}

#[test]
fn surface_records_the_first_caret_cell_and_clips_a_wide_glyph_at_the_margin() {
    let _w = crate::testing::width_lock::widths();
    let mut pool = StylePool::new();
    let lines = [
        StyledLine::new(vec![
            Span::plain("ab"),
            Span::new(" ", Style::CARET),
            Span::new("x", Style::CARET),
        ]),
        StyledLine::text("ab\u{4e2d}\u{6587}", Style::DEFAULT),
    ];
    let s = Surface::build(5, &lines, &mut pool);
    assert_eq!(s.caret, Some(2));
    assert_eq!(s.at(1, 2).code_point(), '\u{4e2d}');
    assert_eq!(s.at(1, 4), Cell::pack(' ', 0, WidthClass::Narrow));
}

#[test]
fn string_width_sums_codepoints_including_astral_planes() {
    let _w = crate::testing::width_lock::widths();
    assert_eq!(width::string_width("hello"), 5);
    assert_eq!(width::string_width("\u{4e00}\u{4e8c}\u{4e09}"), 6);
    assert_eq!(width::string_width("a\u{1f600}b"), 4);
    assert_eq!(width::string_width("e\u{301}"), 1);
    assert_eq!(width::string_width("\u{203a}"), 1);
}

#[test]
fn style_set_sequence_is_a_self_contained_reset_and_set() {
    assert_eq!(Style::DEFAULT.set_sequence(), format!("{ESC}[0m"));
    assert_eq!(Style::BOLD.set_sequence(), format!("{ESC}[0;1m"));
    assert_eq!(Style::REVERSE.set_sequence(), format!("{ESC}[0;7m"));
    // 光标标记不是 SGR：光标格看起来就是一个反显的格子。
    assert_eq!(Style::CARET.set_sequence(), format!("{ESC}[0;7m"));
    // 整段不折行的标记也不是 SGR。
    assert_eq!(
        Style::attrs_only(attr::WHOLE).set_sequence(),
        format!("{ESC}[0m")
    );
    assert_eq!(
        Style::fg(Color::CYAN).set_sequence(),
        format!("{ESC}[0;36m")
    );
    assert_eq!(Style::fg(Color::RED).set_sequence(), format!("{ESC}[0;31m"));
    assert_eq!(
        Style::new(Color::CYAN, Color::Default, attr::BOLD).set_sequence(),
        format!("{ESC}[0;1;36m")
    );
    assert_eq!(
        Style::fg(Color::rgb(135, 206, 235)).set_sequence(),
        format!("{ESC}[0;38;2;135;206;235m")
    );
    assert_eq!(
        Style::new(Color::Named(12), Color::Indexed(17), 0).set_sequence(),
        format!("{ESC}[0;94;48;5;17m")
    );
}

#[test]
fn written_lives_in_bit_60_and_round_trips() {
    let s = Style::new(Color::RED, Color::Default, attr::WRITTEN | attr::BOLD);
    assert!(s.has_attr(attr::WRITTEN));
    assert_eq!(s.attrs(), attr::WRITTEN | attr::BOLD);
    assert_eq!(s.fg_color(), Color::RED);
    assert_eq!(s.set_sequence(), format!("{ESC}[0;1;31m"));
}

#[test]
fn strikethrough_is_sgr_9() {
    assert_eq!(
        Style::attrs_only(attr::STRIKE).set_sequence(),
        format!("{ESC}[0;9m")
    );
    assert!(
        Style::DEFAULT
            .merge(Style::attrs_only(attr::STRIKE))
            .has_attr(attr::STRIKE)
    );
}

#[test]
fn a_decor_underline_colour_follows_as_its_own_colon_form_sequence() {
    let d = Decor::underline(Color::rgb(1, 2, 3));
    assert_eq!(d.sequence(), format!("{ESC}[58:2::1:2:3m"));
    assert_eq!(Decor::PLAIN.sequence(), "");
}

#[test]
fn style_pool_a_decor_makes_its_own_id_and_links_open_and_close_only_when_they_change() {
    let mut pool = StylePool::new();
    let u = Style::UNDERLINE;
    let a = Decor {
        underline: Color::rgb(1, 2, 3),
        link: "https://a.example".into(),
    };
    let b = Decor::link("https://b.example");
    let plain = pool.intern(u, &Decor::PLAIN);
    let la = pool.intern(u, &a);
    let la2 = pool.intern(
        u,
        &Decor {
            underline: Color::rgb(9, 9, 9),
            link: "https://a.example".into(),
        },
    );
    let lb = pool.intern(u, &b);
    assert_eq!(pool.intern(u, &a), la);
    assert_ne!(plain, la);
    let open = format!("{ESC}]8;;https://a.example{ESC}\\");
    assert_eq!(
        pool.transition(plain, la),
        format!("{}{}{open}", u.set_sequence(), a.sequence())
    );
    assert_eq!(
        pool.transition(la, plain),
        format!("{}{}", u.set_sequence(), ansi::LINK_END)
    );
    // 同一个链接里换样子不重开链接。
    assert_eq!(
        pool.transition(la, la2),
        format!("{}{ESC}[58:2::9:9:9m", u.set_sequence())
    );
    assert_eq!(
        pool.transition(la, lb),
        format!("{}{ESC}]8;;https://b.example{ESC}\\", u.set_sequence())
    );
    // 从未知状态（帧首，上一帧已关上链接）设一个链接样式时打开它。
    assert_eq!(
        pool.set_sequence(la),
        format!("{}{}{open}", u.set_sequence(), a.sequence())
    );
    assert_eq!(pool.closing(la), ansi::LINK_END);
    assert_eq!(pool.closing(plain), "");
}

#[test]
fn style_merge_unions_attrs_and_overrides_colour() {
    let s = Style::fg(Color::CYAN).merge(Style::BOLD);
    assert!(s.has_attr(attr::BOLD));
    assert_eq!(s.fg_color(), Color::CYAN);
    let s2 = Style::fg(Color::CYAN).merge(Style::fg(Color::RED));
    assert_eq!(s2.fg_color(), Color::RED);
}

#[test]
fn style_pool_interns_ids_and_caches_transitions() {
    let mut pool = StylePool::new();
    assert_eq!(pool.intern(Style::DEFAULT, &Decor::PLAIN), 0);
    let a = pool.intern(Style::BOLD, &Decor::PLAIN);
    let b = pool.intern(Style::fg(Color::CYAN), &Decor::PLAIN);
    assert_eq!(pool.intern(Style::BOLD, &Decor::PLAIN), a);
    assert_ne!(a, b);
    assert_eq!(pool.transition(a, a), "");
    assert_eq!(pool.transition(0, b), Style::fg(Color::CYAN).set_sequence());
    assert_eq!(pool.transition(a, b), pool.transition(0, b));
    assert_eq!(pool.set_sequence(0), ansi::RESET);
    assert_eq!(pool.set_sequence(b), Style::fg(Color::CYAN).set_sequence());
}

#[test]
fn style_pool_reground_writes_default_colours_as_the_ground() {
    let mut pool = StylePool::new();
    let red = pool.intern(Style::fg(Color::RED), &Decor::PLAIN);
    pool.reground(Some((Color::rgb(1, 2, 3), Color::rgb(4, 5, 6))));
    assert_eq!(pool.base(), format!("{ESC}[0;38;2;1;2;3;48;2;4;5;6m"));
    assert_eq!(pool.set_sequence(red), format!("{ESC}[0;31;48;2;4;5;6m"));
    pool.reground(None);
    assert_eq!(pool.base(), ansi::RESET);
}

#[test]
fn styled_line_render_wraps_a_link_in_osc_8_and_closes_it_before_the_end() {
    let line = StyledLine::new(vec![
        Span::plain("a "),
        Span::with_decor("b", Style::UNDERLINE, Decor::link("https://x.example")),
        Span::plain(" c"),
    ]);
    assert_eq!(
        line.render(),
        format!(
            "{ESC}[0ma {ESC}[0;4m{ESC}]8;;https://x.example{ESC}\\b{ESC}[0m{ESC}]8;;{ESC}\\ c{ESC}[0m"
        )
    );
    let tail = StyledLine::new(vec![Span::with_decor(
        "b",
        Style::UNDERLINE,
        Decor::link("https://x.example"),
    )]);
    assert_eq!(
        tail.render(),
        format!("{ESC}[0;4m{ESC}]8;;https://x.example{ESC}\\b{ESC}]8;;{ESC}\\{ESC}[0m")
    );
}

#[test]
fn styled_line_render_styles_each_span_and_resets_at_the_end() {
    let line = StyledLine::new(vec![
        Span::new("You", Style::fg(Color::CYAN).merge(Style::BOLD)),
        Span::plain(" hi"),
    ]);
    assert_eq!(
        line.render(),
        format!("{ESC}[0;1;36mYou{ESC}[0m hi{ESC}[0m")
    );
    assert_eq!(line.plain(), "You hi");
    assert_eq!(line.width(), 6);
}

// ---- StyledLineSuite ----

const HL_BG: Color = Color::rgb(135, 206, 235);
const HL: Style = Style::new(Color::BLACK, HL_BG, 0);

fn mark(_: Style) -> Style {
    HL
}

fn highlighted(line: &StyledLine) -> String {
    line.spans
        .iter()
        .filter(|s| s.style.bg_color() == HL_BG)
        .map(|s| s.text.as_ref())
        .collect()
}

#[test]
fn restyle_cells_highlights_a_mid_line_range_and_leaves_the_rest() {
    let out = StyledLine::text("hello world", Style::DEFAULT).restyle_cells(6, 11, mark);
    assert_eq!(out.plain(), "hello world");
    assert_eq!(out.width(), 11);
    assert_eq!(highlighted(&out), "world");
}

#[test]
fn restyle_cells_over_a_whole_line_highlights_every_cell() {
    let line = StyledLine::text("abc", Style::DEFAULT);
    let out = line.restyle_cells(0, line.width(), mark);
    assert_eq!(highlighted(&out), "abc");
    assert_eq!(out.plain(), "abc");
}

#[test]
fn restyle_cells_includes_a_wide_glyph_whole_when_a_boundary_straddles_it() {
    let _w = crate::testing::width_lock::widths();
    let out = StyledLine::text("A\u{4f60}B\u{4f60}C", Style::DEFAULT).restyle_cells(2, 5, mark);
    assert_eq!(out.plain(), "A\u{4f60}B\u{4f60}C");
    assert_eq!(highlighted(&out), "\u{4f60}B\u{4f60}");
}

#[test]
fn restyle_cells_is_a_no_op_for_an_empty_or_inverted_range() {
    let line = StyledLine::text("abc", Style::DEFAULT);
    assert_eq!(line.restyle_cells(3, 3, mark), line);
    assert_eq!(line.restyle_cells(2, 1, mark), line);
}

/// CR-01（MR 评审）：标题与链接目标里的控制字符去掉，OSC 不会被提前结束，后面的字节不会成为
/// 独立的终端命令（这里是 OSC 52 改剪贴板）。
#[test]
fn foreign_text_in_an_osc_cannot_end_it_early() {
    let hostile = "demo\u{7}\u{1b}]52;c;aGk=\u{7}\u{9b}2J\u{7f}\ttab\nline";
    assert_eq!(
        ansi::set_title(hostile),
        "\x1b]0;demo]52;c;aGk=2Jtabline\u{7}"
    );
    assert_eq!(
        ansi::link_start(&format!("https://x.example/{hostile}")),
        format!("{ESC}]8;;https://x.example/demo]52;c;aGk=2Jtabline{ESC}\\")
    );
    // 正常的文字原样：中文、emoji、空格。
    assert_eq!(
        ansi::set_title("● 修好 🐛 bug"),
        "\x1b]0;● 修好 🐛 bug\u{7}"
    );
}
