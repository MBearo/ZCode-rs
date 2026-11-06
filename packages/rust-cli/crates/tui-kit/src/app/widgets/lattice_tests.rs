//! 点阵的墨与光带（docs/puffin-tui.md §4.2「笔」的「手停住」）：粗体的点在 fg2 与 7:1 的亮色之间滚动，深浅两种地相同；
//! 亮光离开第三列之后走过后面的那个词。plain 模式三点都是蓝，最亮的加粗。
use super::lattice::{self, DOTS, PEAK, bright};
use crate::render::{Color, Span, Style, attr};
use crate::theme::rgb::Rgb;
use crate::theme::schemes;
use crate::theme::theme::Theme;

const WHITE: Rgb = Rgb::new(255, 255, 255);
const BLACK: Rgb = Rgb::new(0, 0, 0);

fn rgb(c: Color) -> Rgb {
    match c {
        Color::TrueColor(r, g, b) => Rgb::new(r, g, b),
        other => panic!("not true colour: {other:?}"),
    }
}

/// 淡入结束之后 `age` 个 tick 的三列。
fn dots(theme: &Theme, age: i64) -> Vec<Span> {
    lattice::sign(theme, 4 + age)
}

/// 第 `col` 列的亮色：色相只改亮度，直到对地 7:1。
fn lit(theme: &Theme, col: usize) -> Rgb {
    let bg = theme.bg.unwrap();
    let hue = rgb(theme.brand).mix(rgb(theme.work), col as f64 / 2.0);
    Theme::lift(hue, &[bg], PEAK, bg.lightness() < 0.5)
}

/// 报告过颜色的终端：浅色的地（正文偏淡的那种）、白、黑；再加每一个配色的两侧。
fn grounds() -> Vec<Theme> {
    let mut all = vec![
        Theme::from(Rgb::new(0x40, 0x37, 0x4f), Rgb::new(0xf7, 0xf4, 0xfb)),
        Theme::from(BLACK, WHITE),
        Theme::from(WHITE, BLACK),
    ];
    for scheme in schemes::all() {
        all.push(Theme::scheme(scheme, false, None));
        all.push(Theme::scheme(scheme, true, None));
    }
    all
}

#[test]
fn the_dots_are_bold_and_roll_from_fg2_to_the_lit_hue_on_every_ground() {
    for theme in grounds() {
        let fg2 = rgb(theme.fg2.fg_color());
        for age in 0..lattice::PERIOD_TICKS {
            for (col, sp) in dots(&theme, age).iter().enumerate() {
                assert_eq!(sp.text, DOTS);
                assert!(sp.style.has_attr(attr::BOLD), "{sp:?}");
                let k = bright(col, 4 + age);
                assert_eq!(
                    rgb(sp.style.fg_color()),
                    fg2.mix(lit(&theme, col), k),
                    "{age} {col}"
                );
            }
        }
    }
}

#[test]
fn the_lit_dot_keeps_its_hue_and_reaches_seven_to_one() {
    for theme in grounds() {
        let bg = theme.bg.unwrap();
        for col in 0..3 {
            let hue = rgb(theme.brand).mix(rgb(theme.work), col as f64 / 2.0);
            let lit = lit(&theme, col);
            assert!(lit.contrast(bg) >= PEAK - 0.05, "{col} {lit:?} on {bg:?}");
            let (h0, h1) = (hue.to_oklch().2, lit.to_oklch().2);
            let turn = ((h0 - h1 + 540.0) % 360.0 - 180.0).abs();
            // 只改亮度时会走出 sRGB 色域，裁回时色相转几度（蓝在浅色的地上约 8°，个别配色到 14°），仍是同一个颜色。
            assert!(turn < 15.0, "{col}: hue {h0} became {h1}");
        }
    }
}

/// 修前：浅色的地上点最暗时是 line 色（1.4:1），一半时间低于 3:1。
#[test]
fn the_dots_never_fade_below_the_quieter_of_fg2_and_the_lit_hue() {
    for theme in grounds() {
        let bg = theme.bg.unwrap();
        let fg2 = rgb(theme.fg2.fg_color()).contrast(bg);
        for age in 0..lattice::PERIOD_TICKS {
            for (col, sp) in dots(&theme, age).iter().enumerate() {
                let floor = fg2.min(lit(&theme, col).contrast(bg));
                let c = rgb(sp.style.fg_color()).contrast(bg);
                // sRGB 里混合对比度不严格单调：两端都很亮时中间可以低 2–3%。
                assert!(c >= floor * 0.95, "{age} {col}: {c} < {floor}");
            }
        }
    }
    let soft = Theme::from(Rgb::new(0x40, 0x37, 0x4f), Rgb::new(0xf7, 0xf4, 0xfb));
    let bg = soft.bg.unwrap();
    let low = (0..lattice::PERIOD_TICKS)
        .flat_map(|age| dots(&soft, age))
        .map(|sp| rgb(sp.style.fg_color()).contrast(bg))
        .fold(f64::INFINITY, f64::min);
    assert!(low >= 4.0, "{low}");
}

#[test]
fn plain_dots_are_all_blue_and_the_brightest_is_bold() {
    let theme = Theme::PLAIN;
    for age in 0..lattice::PERIOD_TICKS {
        for (col, sp) in dots(&theme, age).iter().enumerate() {
            assert_eq!(sp.style.fg_color(), theme.brand, "{sp:?}");
            assert_eq!(
                sp.style.has_attr(attr::BOLD),
                bright(col, 4 + age) > lattice::PLAIN_BRIGHT,
                "{age} {col}"
            );
        }
    }
}

/// 光带在第 `i` 个字上的混合量：点阵的亮度，列号换成 `3 + 0.55·i`，取 6 次方。
fn band_at(i: usize, age: i64) -> f64 {
    let phase = age.rem_euclid(lattice::PERIOD_TICKS) as f64;
    let col = 3.0 + 0.55 * i as f64;
    let k = 0.5
        + 0.5
            * (2.0 * std::f64::consts::PI * phase / lattice::PERIOD_TICKS as f64
                - lattice::LAG * col)
                .cos();
    k.powi(6)
}

#[test]
fn the_band_is_the_dots_light_carried_past_the_third_column() {
    for age in 0..lattice::PERIOD_TICKS {
        for i in 0..8 {
            assert!(
                (lattice::band_k(i, age) - band_at(i, age)).abs() < 1e-12,
                "{age} {i}"
            );
        }
    }
}

#[test]
fn the_light_runs_on_through_the_word_after_the_dots() {
    for theme in grounds() {
        let fg2 = theme.fg2;
        for age in 4..4 + lattice::PERIOD_TICKS {
            let word = lattice::band(&theme, age, Span::new("Working", fg2.with_attr(attr::BOLD)));
            let text: String = word.iter().map(|sp| &*sp.text).collect();
            assert_eq!(text, "Working");
            for (i, sp) in word.iter().enumerate() {
                assert!(sp.style.has_attr(attr::BOLD), "the word keeps its weight");
                let want = rgb(fg2.fg_color()).mix(lit(&theme, 2), band_at(i, age));
                assert_eq!(rgb(sp.style.fg_color()), want, "{age} {i}");
            }
        }
    }
}

#[test]
fn the_band_is_narrow_each_letter_is_lit_briefly_and_it_leaves_the_word() {
    for i in 0..7 {
        let on = (0..lattice::PERIOD_TICKS)
            .filter(|&a| lattice::band_k(i, a) > 0.5)
            .count();
        assert!(
            on > 0 && on * 4 <= lattice::PERIOD_TICKS as usize,
            "{i}: {on}"
        );
    }
    // 词比两次光带之间的空当宽：最后一个字淡下去时下一次已经淡淡地到了第一个字。所以只要求有一刻每个字都至少九成是它自己的颜色。
    assert!(
        (0..lattice::PERIOD_TICKS).any(|a| (0..7).all(|i| lattice::band_k(i, a) < 0.1)),
        "a tick with the band nearly off the word"
    );
}

#[test]
fn in_plain_mode_the_band_lifts_the_letters_out_of_dim() {
    let theme = Theme::PLAIN;
    for age in 0..lattice::PERIOD_TICKS {
        for (i, sp) in lattice::band(&theme, age, Span::new("Working", Style::DIM))
            .iter()
            .enumerate()
        {
            assert_eq!(
                sp.style.has_attr(attr::DIM),
                lattice::band_k(i, age) <= 0.5,
                "{age} {i}"
            );
        }
    }
}

#[test]
fn the_waiting_row_carries_the_band_on_working() {
    let theme = Theme::from(BLACK, WHITE);
    let age = (0..lattice::PERIOD_TICKS)
        .map(|a| a + 4)
        .find(|&a| lattice::band_k(2, a) > 0.9)
        .unwrap();
    let row = lattice::line(&theme, age, 7_000.0);
    let text: String = row.iter().map(|sp| &*sp.text).collect();
    assert_eq!(text, "⋮⋮⋮ Working  7s");
    let r = row.iter().find(|sp| sp.text == "r").unwrap();
    assert_ne!(r.style, theme.fg2, "the band is on the word");
}

fn pen(theme: Theme, still: Option<i64>) -> crate::app::widgets::pen::Pen {
    crate::app::widgets::pen::Pen {
        theme,
        frame: 0,
        still: still.map(|age| crate::app::widgets::pen::Still { age, turn_ms: 0.0 }),
        held: false,
    }
}

/// 此刻那一行（调用的名称）与名单末行（`N running`）：点阵在时，它后面的第一段带着光带，其余不变；宽限期里不动。
#[test]
fn the_pen_puts_the_band_on_the_first_run_after_the_slot() {
    let theme = Theme::from(BLACK, WHITE);
    let runs = || {
        vec![
            Span::new("Bash", theme.fg2),
            Span::new("  cargo test", theme.fg3),
        ]
    };
    assert_eq!(pen(theme, None).band(runs()), runs());
    assert_eq!(
        pen(theme, Some(lattice::GRACE_TICKS - 1)).band(runs()),
        runs()
    );
    let age = 9;
    let out = pen(theme, Some(lattice::GRACE_TICKS + age)).band(runs());
    let mut want = lattice::band(&theme, age, runs()[0].clone());
    want.push(runs()[1].clone());
    assert_eq!(out, want);
}
