//! 笔的算术（docs/puffin-tui.md §4.2「笔」）：字从 40% 用 4 个 tick 落定成它自己的墨，只有最新的 16 个，字下面什么都
//! 不涂。
//! 移植自 packages/tui/tui/test/src/zcode/tui/EaseSuite.scala（feat/tui a2bf878b34）。
use super::ease::{self, Ink};
use super::rgb::Rgb;
use super::theme::Theme;
use crate::render::{Color, Span, Style, StyledLine, attr};

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn written() -> Style {
    Style::DEFAULT.with_attr(attr::WRITTEN)
}

#[test]
fn a_letter_rises_from_40_percent_fast_then_slow_and_is_solid_after_4_ticks() {
    assert!((ease::amount(0) - (0.4 + 0.6 * (1.0 - 0.8 * 0.8))).abs() < 1e-9);
    let steps: Vec<f64> = (0..=4).map(ease::amount).collect();
    assert!(steps.windows(2).all(|w| w[1] > w[0]), "{steps:?}");
    assert!(steps[1] - steps[0] > steps[3] - steps[2], "fast then slow");
    assert_eq!(steps[4], 1.0);
    // 不计时：实的。
    assert_eq!(ease::amount(-1), 1.0);
}

#[test]
fn it_rises_from_its_ground_the_background_or_the_code_panel_under_it() {
    let rich = rich();
    let fresh = ease::settle(&rich, written(), 0);
    assert_eq!(
        fresh.fg_color(),
        rich.bg
            .unwrap()
            .mix(rich.fg.unwrap(), ease::amount(0))
            .color()
    );
    let panel = rich.panel.unwrap();
    let on_panel = ease::settle(&rich, written().with_bg(panel), 0);
    let Color::TrueColor(r, g, b) = panel else {
        panic!("panel is not truecolour")
    };
    assert_eq!(
        on_panel.fg_color(),
        Rgb::new(r, g, b)
            .mix(rich.fg.unwrap(), ease::amount(0))
            .color()
    );
    // 字下面什么都不涂。
    assert_eq!(on_panel.bg_color(), panel);
    assert_eq!(ease::settle(&rich, written(), ease::TICKS), written());
}

/// 每个字与它的前景色。
fn fgs(l: &StyledLine) -> Vec<(char, Color)> {
    l.spans
        .iter()
        .flat_map(|sp| sp.text.chars().map(move |c| (c, sp.style.fg_color())))
        .collect()
}

#[test]
fn only_the_newest_16_written_letters_settle_counted_from_the_bottom_right() {
    let rich = rich();
    let text = "abcdefghij klmnopqrst";
    let lines = vec![
        StyledLine::new(vec![
            Span::new("   ", Style::DEFAULT),
            Span::new("abcdefghij", written()),
        ]),
        StyledLine::new(vec![
            Span::new("   ", Style::DEFAULT),
            Span::new("klmnopqrst", written()),
            Span::new("▌", Style::fg(rich.brand)),
        ]),
    ];
    let out = ease::settled(&rich, |_| 0, lines.clone());
    let light: String = fgs(&out[0])
        .into_iter()
        .chain(fgs(&out[1]))
        .filter(|&(c, fg)| c.is_alphabetic() && fg != Color::Default)
        .map(|(c, _)| c)
        .collect();
    let letters: Vec<char> = text.chars().filter(|&c| c != ' ').collect();
    let newest: String = letters[letters.len() - ease::CELLS..].iter().collect();
    assert_eq!(light, newest);
    // 笔尖没有写出标记：保持它的墨。
    assert_eq!(fgs(&out[1]).last(), Some(&('▌', rich.brand)));
    let plain = |ls: &[StyledLine]| ls.iter().map(StyledLine::plain).collect::<Vec<_>>();
    assert_eq!(plain(&out), plain(&lines));
    // k 是第几新的字：最新的落下 0 tick，第 6 个之前的早就落定。
    let older = ease::settled(&rich, |k| if k <= 5 { 0 } else { 99 }, lines);
    let settling = fgs(&older[1])
        .into_iter()
        .filter(|&(c, fg)| c.is_alphabetic() && fg != Color::Default)
        .count();
    assert_eq!(settling, 5);
}

#[test]
fn a_whole_block_lands_at_once_and_plain_mode_has_no_settling() {
    let rich = rich();
    let rows = vec![StyledLine::new(vec![Span::new(
        "| a | b |",
        Style::DEFAULT,
    )])];
    let landed = ease::landed(&rich, 0, rows.clone());
    assert!(
        landed[0]
            .spans
            .iter()
            .all(|sp| sp.style.fg_color() != Color::Default)
    );
    assert_eq!(ease::landed(&rich, ease::TICKS, rows.clone()), rows);
    assert_eq!(ease::landed(&Theme::PLAIN, 0, rows.clone()), rows);
    let written_rows: Vec<StyledLine> = rows
        .iter()
        .map(|l| {
            let spans = l
                .spans
                .iter()
                .map(|sp| Span::new(sp.text.clone(), written()))
                .collect();
            StyledLine::new(spans)
        })
        .collect();
    let plain = ease::settled(&Theme::PLAIN, |_| 0, written_rows);
    assert_eq!(plain[0].spans[0].style, written());
    assert_eq!(ease::settle(&Theme::PLAIN, written(), 0), written());
}

#[test]
fn the_lifting_pen_is_brand_25_percent_toward_the_background() {
    // Scala 的这个用例经 shell 的 `Pen.lift` 检查字形序列；这里只有笔的墨与字形表，字形序列随 shell 移植。
    let rich = rich();
    assert_eq!(ease::LIFT, ["▌", "▍", "▎", "▏"]);
    assert_eq!(ease::LIFT.len() as i64, ease::LIFT_TICKS);
    let Color::TrueColor(r, g, b) = rich.brand else {
        panic!("brand is not truecolour")
    };
    assert_eq!(
        ease::lift_ink(&rich).fg_color(),
        Rgb::new(r, g, b).mix(rich.bg.unwrap(), 0.25).color()
    );
    assert_eq!(ease::lift_ink(&Theme::PLAIN), Style::fg(Theme::PLAIN.brand));
}

#[test]
fn the_nib_is_brand_for_the_answer_and_fg3_for_the_thinking() {
    let rich = rich();
    assert_eq!(ease::nib_color(&rich, Ink::Answer), rich.brand);
    assert_eq!(ease::nib_color(&rich, Ink::Thought), rich.fg3.fg_color());
}

#[test]
fn settling_counts_cells_as_scala_does_a_no_break_space_is_a_letter() {
    // Java 的 isWhitespace 不把 U+00A0 当空白：它是一个要落定的格子，与 Scala 逐格相同。
    let rich = rich();
    let lines = vec![StyledLine::new(vec![Span::new("a\u{a0}b c", written())])];
    let out = ease::settled(&rich, |_| 0, lines);
    let settled = fgs(&out[0])
        .into_iter()
        .filter(|&(_, fg)| fg != Color::Default)
        .map(|(c, _)| c)
        .collect::<String>();
    assert_eq!(settled, "a\u{a0}bc");
}
