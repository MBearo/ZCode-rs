//! docs/puffin-tui.md §4.2 的 token。期望值是设计稿对同样的终端颜色算出的值，所以 TUI 画的正是设计稿的样子；
//! 另有与 Scala.js 产物逐位比较的参照测试（画面门禁的前提）。
//! 移植自 packages/tui/tui/test/src/zcode/tui/ThemeSuite.scala（feat/tui a2bf878b34）。
use std::fmt::Write as _;

use super::rgb::Rgb;
use super::scheme::{Ground, Scheme, ThemeChoice, ThemeMode};
use super::schemes;
use super::theme::Theme;
use crate::render::{Color, Style};

fn hex(s: &str) -> Rgb {
    Rgb::parse_hex(s).unwrap()
}

fn rgb_of(c: Color) -> Rgb {
    match c {
        Color::TrueColor(r, g, b) => Rgb::new(r, g, b),
        other => panic!("not truecolour: {other:?}"),
    }
}

fn fg_of(st: Style) -> Color {
    st.fg_color()
}

#[test]
fn a_dark_terminal_gets_the_mockups_dark_tokens() {
    let t = Theme::from(hex("#d6d8dc"), hex("#15161a"));
    assert!(t.rich);
    assert_eq!(fg_of(t.fg2), hex("#989a9e").color());
    assert_eq!(fg_of(t.fg3), hex("#727377").color());
    assert_eq!(fg_of(t.line), hex("#3c3d41").color());
    assert_eq!(t.panel, Some(hex("#212226").color()));
    assert_eq!(t.brand, hex("#52bdf9").color());
    assert_eq!(t.code, hex("#ec9d53").color());
    assert_eq!(t.ok, hex("#6fc884").color());
    assert_eq!(t.err, hex("#f98f87").color());
}

#[test]
fn a_light_terminal_gets_the_darker_accent_lightness() {
    let t = Theme::from(hex("#22262c"), hex("#fbfbf9"));
    assert_eq!(fg_of(t.fg2), hex("#676a6e").color());
    assert_eq!(fg_of(t.fg3), hex("#939597").color());
    assert_eq!(fg_of(t.line), hex("#d0d0d0").color());
    assert_eq!(t.panel, Some(hex("#eeeeed").color()));
    assert_eq!(t.brand, hex("#007ebb").color());
    assert_eq!(t.code, hex("#ad5d00").color());
    assert_eq!(t.ok, hex("#218a45").color());
    assert_eq!(t.err, hex("#b84d49").color());
}

#[test]
fn inline_code_is_a_cool_grey_and_its_backticks_are_halfway_to_the_background() {
    let dark = Theme::from(hex("#d6d8dc"), hex("#15161a"));
    assert_eq!(fg_of(dark.code_ink), hex("#c3dced").color());
    assert_eq!(fg_of(dark.tick), hex("#6c7984").color());
    // 浅色背景上彩度高一档（0.055），否则看不出冷色。
    let light = Theme::from(hex("#22262c"), hex("#fbfbf9"));
    assert_eq!(fg_of(light.code_ink), hex("#03293d").color());
    assert_eq!(fg_of(light.tick), hex("#7f929b").color());
}

#[test]
fn plain_inline_code_has_no_colour_and_dim_backticks() {
    assert_eq!(Theme::PLAIN.code_ink, Style::DEFAULT);
    assert_eq!(Theme::PLAIN.tick, Style::DIM);
}

#[test]
fn rich_needs_both_colours_and_truecolour_otherwise_plain() {
    let fg = Some(hex("#d6d8dc"));
    let bg = Some(hex("#15161a"));
    assert!(Theme::choose(fg, bg, true).rich);
    assert_eq!(Theme::choose(fg, bg, false), Theme::PLAIN);
    assert_eq!(Theme::choose(None, bg, true), Theme::PLAIN);
    assert_eq!(Theme::PLAIN.panel, None);
    assert_eq!(Theme::PLAIN.fg2, Style::DIM);
}

#[test]
fn new_text_rises_from_the_background_into_its_own_colour_never_through_brand() {
    let t = Theme::from(hex("#d6d8dc"), hex("#15161a"));
    let fg = hex("#d6d8dc").color();
    let start = hex("#d6d8dc").mix(hex("#15161a"), Theme::STREAM_DEPTH);
    assert_eq!(t.tint(fg, 0.0), Some(start.color()));
    assert_eq!(t.tint(fg, 1.0), Some(fg));
    // ease-out：一半的时间走了四分之三。
    assert_eq!(
        t.tint(fg, 0.5),
        Some(start.mix(hex("#d6d8dc"), 0.75).color())
    );
    assert_eq!(Theme::PLAIN.tint(fg, 0.0), None);
}

#[test]
fn a_fade_rises_from_the_background_or_the_ground_it_is_drawn_on() {
    let t = Theme::from(hex("#d6d8dc"), hex("#15161a"));
    let ok = t.ok;
    assert_eq!(t.rise(ok, 0.0, None), Some(hex("#15161a").color()));
    assert_eq!(
        t.rise(ok, 0.5, None),
        Some(hex("#15161a").mix(rgb_of(ok), 0.75).color())
    );
    assert_eq!(t.rise(ok, 1.0, None), Some(ok));
    // 板上的淡入从 panel 开始。
    assert_eq!(t.rise(ok, 0.0, t.panel), t.panel);
    assert_eq!(Theme::PLAIN.rise(Color::GREEN, 0.0, None), None);
}

#[test]
fn the_carets_breath_only_dims() {
    let t = Theme::from(hex("#d6d8dc"), hex("#15161a"));
    assert_eq!(t.breath(1.0), Some(t.brand));
    assert_eq!(
        t.breath(0.0),
        Some(rgb_of(t.brand).mix(hex("#15161a"), 0.45).color())
    );
}

// ---- 具名主题（docs/puffin-tui.md §4.2「主题」） ----

fn gruvbox() -> &'static Scheme {
    schemes::by_id("gruvbox").unwrap()
}

fn dark_fg() -> Rgb {
    hex("#d6d8dc")
}
fn dark_bg() -> Rgb {
    hex("#15161a")
}
fn light_fg() -> Rgb {
    hex("#22262c")
}
fn light_bg() -> Rgb {
    hex("#fbfbf9")
}

fn choice(theme: &'static str, mode: ThemeMode, ground: Ground) -> ThemeChoice {
    ThemeChoice {
        theme,
        mode,
        ground,
    }
}

#[test]
fn the_terminal_theme_is_what_it_always_was_whatever_the_mode() {
    for mode in ThemeMode::ALL {
        let c = choice("terminal", mode, Ground::Theme);
        assert_eq!(
            Theme::resolve(Some(dark_fg()), Some(dark_bg()), true, &c, None),
            Theme::from(dark_fg(), dark_bg())
        );
    }
    assert!(!Theme::from(dark_fg(), dark_bg()).paints);
}

#[test]
fn terminals_syntax_inks_are_its_accents_at_the_specs_hues() {
    let t = Theme::from(dark_fg(), dark_bg());
    assert_eq!(t.syntax.keyword, t.skill);
    assert_eq!(t.syntax.string, t.ok);
    assert_eq!(t.syntax.number, t.code);
    assert_eq!(t.syntax.function, t.brand);
    assert_eq!(t.syntax.ty, t.work);
    assert_eq!(t.syntax.constant, t.err);
    assert_eq!(t.syntax.key, t.agent);
}

#[test]
fn a_named_theme_on_its_own_ground_takes_its_comment_colour_as_fg3() {
    // Gruvbox Dark：灰阶都够清楚，不用抬。
    let p = gruvbox().dark.palette;
    let t = Theme::scheme(gruvbox(), true, None);
    assert!(t.rich && t.paints);
    assert_eq!((t.fg, t.bg), (Some(p.fg), Some(p.bg)));
    assert_eq!(fg_of(t.fg3), p.muted.color());
    assert_eq!(fg_of(t.fg2), p.fg.mix(p.muted, 0.45).color());
    assert_eq!(fg_of(t.line), p.muted.mix(p.bg, 0.58).color());
    assert_eq!(fg_of(t.quiet), p.muted.mix(p.bg, 0.25).color());
    assert_eq!(t.ok, p.ink("green").unwrap().color());
    assert_eq!(t.syntax.keyword, p.ink("red").unwrap().color());
}

#[test]
fn the_slab_keeps_its_lightness_band_from_bg() {
    let bg = hex("#202020");
    let fg = hex("#e0e0e0");
    // 太近：看不出板，用 fg 向 bg 混合 94%。
    assert_eq!(Theme::band(hex("#212121"), fg, bg), fg.mix(bg, 0.94));
    // 正好：配色自己的面。
    let ok = hex("#2c2c2c");
    assert_eq!(Theme::band(ok, fg, bg), ok);
    // 太远：向 bg 一步 5% 地退，退到 0.06 以内为止。
    let far = Theme::band(hex("#505050"), fg, bg);
    assert!(
        (far.lightness() - bg.lightness()).abs() <= 0.06,
        "{}",
        far.hex()
    );
    // 纯黑上没有落在带里的颜色：停在最后一个还看得出的一步，不退成 bg。
    let black = hex("#000000");
    let near_black = Theme::band(hex("#1e1e1e"), hex("#ffffff"), black);
    assert_ne!(near_black, black);
    assert!(
        near_black.lightness() >= 0.025 && near_black.lightness() < hex("#1e1e1e").lightness(),
        "{}",
        near_black.hex()
    );
    for s in schemes::all() {
        for dark in [false, true] {
            let t = Theme::scheme(s, dark, None);
            let panel = rgb_of(t.panel.unwrap());
            let tbg = t.bg.unwrap();
            let d = (panel.lightness() - tbg.lightness()).abs();
            assert!(
                d >= 0.025 - 1e-9 && (d <= 0.06 + 1e-9 || tbg == black),
                "{} {dark} panel {}: {d}",
                s.id,
                panel.hex()
            );
        }
    }
}

#[test]
fn the_floor_moves_an_inks_lightness_only() {
    // Solarized Light 的黄在 #fdf6e3 上不到 3:1，要变暗。
    let sol = schemes::by_id("solarized").unwrap();
    let raw = sol.light.palette.ink("yellow").unwrap();
    let t = Theme::scheme(sol, false, None);
    let lifted = rgb_of(t.code);
    assert!(raw.contrast(sol.light.palette.bg) < 3.0);
    assert!(lifted.contrast(sol.light.palette.bg) >= 3.0);
    assert!(lifted.lightness() < raw.lightness());
    let (_, c0, h0) = raw.to_oklch();
    let (_, c1, h1) = lifted.to_oklch();
    assert!((h1 - h0).abs() < 3.0, "hue {h0} → {h1}");
    assert!((c1 - c0).abs() < 0.02, "chroma {c0} → {c1}");
    // 已经够清楚的墨原样不动。
    assert_eq!(
        Theme::lift(hex("#000000"), &[hex("#ffffff")], 3.0, false),
        hex("#000000")
    );
}

#[test]
fn on_the_terminals_ground_only_the_inks_come_from_the_theme() {
    let c = choice("gruvbox", ThemeMode::Auto, Ground::Terminal);
    let t = Theme::resolve(Some(dark_fg()), Some(dark_bg()), true, &c, None);
    assert!(!t.paints);
    assert_eq!((t.fg, t.bg), (Some(dark_fg()), Some(dark_bg())));
    assert_eq!(t.panel, Some(dark_fg().mix(dark_bg(), 0.94).color()));
    assert_eq!(fg_of(t.fg3), dark_fg().mix(dark_bg(), 0.52).color());
    let grounds = [dark_bg(), dark_fg().mix(dark_bg(), 0.94)];
    let green = gruvbox().dark.palette.ink("green").unwrap();
    assert_eq!(t.ok, Theme::lift(green, &grounds, 3.0, true).color());
}

#[test]
fn auto_picks_the_side_by_the_terminals_background_light_and_dark_do_not_look() {
    let side = |fg: Rgb, bg: Rgb, mode: ThemeMode| {
        let c = choice("gruvbox", mode, Ground::Theme);
        Theme::resolve(Some(fg), Some(bg), true, &c, None)
            .bg
            .unwrap()
    };
    let g = gruvbox();
    assert_eq!(
        side(dark_fg(), dark_bg(), ThemeMode::Auto),
        g.dark.palette.bg
    );
    assert_eq!(
        side(light_fg(), light_bg(), ThemeMode::Auto),
        g.light.palette.bg
    );
    assert_eq!(
        side(dark_fg(), dark_bg(), ThemeMode::Light),
        g.light.palette.bg
    );
    assert_eq!(
        side(light_fg(), light_bg(), ThemeMode::Dark),
        g.dark.palette.bg
    );
}

#[test]
fn a_dec_2031_report_decides_auto_for_a_theme_that_paints() {
    let bg_of = |c: ThemeChoice, appearance: Option<bool>| {
        Theme::resolve(Some(dark_fg()), Some(dark_bg()), true, &c, appearance)
            .bg
            .unwrap()
    };
    let auto = choice("gruvbox", ThemeMode::Auto, Ground::Theme);
    assert_eq!(bg_of(auto, Some(false)), gruvbox().light.palette.bg);
    assert_eq!(bg_of(auto, Some(true)), gruvbox().dark.palette.bg);
    let dark = choice("gruvbox", ThemeMode::Dark, Ground::Theme);
    assert_eq!(bg_of(dark, Some(false)), gruvbox().dark.palette.bg);
    // 地是终端的：等终端的新颜色，报告本身不换侧。
    let on_terminal = choice("gruvbox", ThemeMode::Auto, Ground::Terminal);
    let reported = Theme::resolve(
        Some(dark_fg()),
        Some(dark_bg()),
        true,
        &on_terminal,
        Some(false),
    );
    let unreported = Theme::resolve(Some(dark_fg()), Some(dark_bg()), true, &on_terminal, None);
    assert_eq!(reported.ok, unreported.ok);
}

#[test]
fn plain_is_plain_whatever_theme_was_chosen() {
    let c = choice("gruvbox", ThemeMode::Auto, Ground::Theme);
    assert_eq!(
        Theme::resolve(Some(dark_fg()), Some(dark_bg()), false, &c, None),
        Theme::PLAIN
    );
    let d = choice("gruvbox", ThemeMode::Dark, Ground::Theme);
    assert_eq!(Theme::resolve(None, None, true, &d, None), Theme::PLAIN);
}

#[test]
fn theme_json_reads_back_what_was_written_and_anything_unknown_is_the_default() {
    let c = choice("tokyonight", ThemeMode::Light, Ground::Terminal);
    assert_eq!(
        c.encode(),
        "{\"theme\":\"tokyonight\",\"mode\":\"light\",\"ground\":\"terminal\"}\n"
    );
    assert_eq!(ThemeChoice::decode(&c.encode()), c);
    assert_eq!(ThemeChoice::decode(""), ThemeChoice::default());
    assert_eq!(ThemeChoice::decode("[1]"), ThemeChoice::default());
    assert_eq!(
        ThemeChoice::decode(r#"{"theme":"nope","mode":"dusk","ground":7}"#),
        ThemeChoice::default()
    );
    assert_eq!(
        ThemeChoice::decode(r#"{"theme":"nord"}"#),
        ThemeChoice::of("nord")
    );
}

// ---- 与 Scala.js 逐位相同（画面门禁的前提） ----
//
// 期望值来自 feat/tui a2bf878b34 的 Scala.js 链接产物（tui 的 fastLinkJSTest）：在 node 里直接调用其中的
// `Theme.from` 与 `Theme.scheme`，把每个结果的 `toString` 写成一行 `键\t值`，再对各组的行求 FNV-1a 64。Rust 这边按
// Scala case class 的 `toString` 格式化同样的输入；`Style` 打包成同一个 Long（§4.1 的位布局），按十进制写。

/// 基准场景都用的主题：Scala 的 `Theme.from(Rgb(214,216,220), Rgb(21,22,26)).toString`，逐字。
const BENCH_THEME: &str = "Theme(true,Some(Rgb(214,216,220)),Some(Rgb(21,22,26)),10241080064,7680679680,\
4042589952,Some(TrueColor(33,34,38)),TrueColor(82,189,249),TrueColor(236,157,83),TrueColor(111,200,132),\
TrueColor(249,143,135),TrueColor(190,157,247),TrueColor(82,189,249),TrueColor(13,203,195),TrueColor(179,185,79),\
6064014080,Some(TrueColor(34,47,41)),Some(TrueColor(53,39,41)),13144143616,7279612672,Syntax(TrueColor(190,157,247),\
TrueColor(111,200,132),TrueColor(236,157,83),TrueColor(82,189,249),TrueColor(13,203,195),TrueColor(249,143,135),\
TrueColor(179,185,79)),false)";

/// Scala 产物里 `from` 一组（4 个固定的终端 + 300 个随机的）、具名主题在自己地上一组（17 × 2）、在终端地上一组
/// （17 × 2 × 前 24 个终端）的 FNV-1a 64。
const SCALA_FROM: u64 = 0x2ba3_2ae3_a1d9_d1bf;
const SCALA_OWN_GROUND: u64 = 0x7763_b24c_6197_ccff;
const SCALA_TERMINAL_GROUND: u64 = 0x7137_956d_d379_2d6d;

fn color_string(c: Color) -> String {
    match c {
        Color::Default => "Default".to_owned(),
        Color::TrueColor(r, g, b) => format!("TrueColor({r},{g},{b})"),
        other => format!("{other:?}"),
    }
}

fn option_string<T>(o: Option<T>, f: impl Fn(T) -> String) -> String {
    o.map_or_else(|| "None".to_owned(), |v| format!("Some({})", f(v)))
}

fn rgb_string(c: Rgb) -> String {
    format!("Rgb({},{},{})", c.r, c.g, c.b)
}

/// Scala `Theme` 的 `toString`。
fn scala_string(t: &Theme) -> String {
    let y = t.syntax;
    let syntax = [
        y.keyword, y.string, y.number, y.function, y.ty, y.constant, y.key,
    ]
    .map(color_string)
    .join(",");
    let fields = [
        t.rich.to_string(),
        option_string(t.fg, rgb_string),
        option_string(t.bg, rgb_string),
        t.fg2.raw().to_string(),
        t.fg3.raw().to_string(),
        t.line.raw().to_string(),
        option_string(t.panel, color_string),
        color_string(t.brand),
        color_string(t.code),
        color_string(t.ok),
        color_string(t.err),
        color_string(t.skill),
        color_string(t.file),
        color_string(t.work),
        color_string(t.agent),
        t.quiet.raw().to_string(),
        option_string(t.add_bg, color_string),
        option_string(t.del_bg, color_string),
        t.code_ink.raw().to_string(),
        t.tick.raw().to_string(),
        format!("Syntax({syntax})"),
        t.paints.to_string(),
    ];
    format!("Theme({})", fields.join(","))
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// 与 Scala.js 参照测试相同的终端颜色：4 个固定的，再 300 个由同一个 LCG 生成的。
fn terminal_pairs() -> Vec<(Rgb, Rgb)> {
    let mut pairs = vec![
        (hex("#d6d8dc"), hex("#15161a")),
        (hex("#22262c"), hex("#fbfbf9")),
        (hex("#d6d5d0"), hex("#1b1c1f")),
        (hex("#2a2c30"), hex("#fbfaf7")),
    ];
    let mut seed: u32 = 0x2545_f491;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 24) as u8
    };
    for _ in 0..300 {
        let f = Rgb::new(next(), next(), next());
        let b = Rgb::new(next(), next(), next());
        pairs.push((f, b));
    }
    pairs
}

fn key_rgb(c: Rgb) -> String {
    format!("{},{},{}", c.r, c.g, c.b)
}

#[test]
fn the_benchmark_theme_is_scalas_to_the_bit() {
    assert_eq!(
        scala_string(&Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))),
        BENCH_THEME
    );
}

#[test]
fn every_derivation_is_scalas_to_the_bit() {
    let pairs = terminal_pairs();
    let mut from = String::new();
    for &(f, b) in &pairs {
        let t = Theme::from(f, b);
        let _ = writeln!(
            from,
            "from {} {}\t{}",
            key_rgb(f),
            key_rgb(b),
            scala_string(&t)
        );
    }
    let (mut own, mut on_terminal) = (String::new(), String::new());
    for s in schemes::all() {
        for dark in [false, true] {
            let t = Theme::scheme(s, dark, None);
            let _ = writeln!(own, "scheme {} {dark}\t{}", s.id, scala_string(&t));
            for &(f, b) in &pairs[..24] {
                let t = Theme::scheme(s, dark, Some((f, b)));
                let key = format!("scheme {} {dark} on {} {}", s.id, key_rgb(f), key_rgb(b));
                let _ = writeln!(on_terminal, "{key}\t{}", scala_string(&t));
            }
        }
    }
    assert_eq!(fnv1a(&from), SCALA_FROM, "Theme::from 与 Scala 不同");
    assert_eq!(
        fnv1a(&own),
        SCALA_OWN_GROUND,
        "具名主题（自己的地）与 Scala 不同"
    );
    assert_eq!(
        fnv1a(&on_terminal),
        SCALA_TERMINAL_GROUND,
        "具名主题（终端的地）与 Scala 不同"
    );
}
