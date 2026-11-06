//! 颜色深度与降采样（docs/specs/rust-cli/tui-kit.md §3.4）：降采样表、`nearest_256`（旧界面 crates/tui/src/ui/palette.rs
//! 的用例搬过来）、样式池在各深度的 intern、渲染器在 256 色与 16 色上写出的字节。
use std::collections::BTreeMap;

use super::color::{ColorDepth, indexed_rgb, nearest_256};
use super::renderer::{Backdrop, Renderer};
use super::{Color, Decor, Span, Style, StylePool, StyledLine, attr};
use crate::testing::width_lock::widths;

const RED: Color = Color::TrueColor(255, 0, 0);

#[test]
fn nearest_256_prefers_grays_for_grays() {
    assert_eq!(nearest_256(31, 31, 31), 234);
    assert_eq!(nearest_256(255, 0, 0), 196);
    // 修复线 F26：偏蓝的颜色取色块，不落到灰阶上（原来是 248）。
    assert_eq!(nearest_256(99, 168, 248), 75);
    // 旧界面的强调色（深色、浅色背景）在 256 色上的取值。
    assert_eq!(nearest_256(28, 100, 200), 26);
    assert_eq!(nearest_256(0, 0, 0), 16);
    assert_eq!(nearest_256(255, 255, 255), 231);
}

#[test]
fn indexed_rgb_is_the_xterm_value_of_the_cube_and_grey_cells() {
    assert_eq!(indexed_rgb(7), None, "前 16 色由终端的调色板决定");
    assert_eq!(indexed_rgb(16), Some((0, 0, 0)));
    assert_eq!(indexed_rgb(196), Some((255, 0, 0)));
    assert_eq!(indexed_rgb(75), Some((95, 175, 255)));
    assert_eq!(indexed_rgb(231), Some((255, 255, 255)));
    assert_eq!(indexed_rgb(232), Some((8, 8, 8)));
    assert_eq!(indexed_rgb(255), Some((238, 238, 238)));
    // 每一格的 RGB 最近的格子是它自己。
    for n in 16..=255u8 {
        let (r, g, b) = indexed_rgb(n).unwrap();
        assert_eq!(nearest_256(r, g, b), n, "cell {n}");
    }
}

#[test]
fn the_reduction_table() {
    let cases = [
        // (颜色, 真彩, 256 色, 16 色)
        (
            Color::Default,
            Color::Default,
            Color::Default,
            Color::Default,
        ),
        (
            Color::Named(3),
            Color::Named(3),
            Color::Named(3),
            Color::Named(3),
        ),
        (
            Color::Named(12),
            Color::Named(12),
            Color::Named(12),
            Color::Named(12),
        ),
        (
            Color::Indexed(9),
            Color::Indexed(9),
            Color::Indexed(9),
            Color::Indexed(9),
        ),
        (
            Color::Indexed(75),
            Color::Indexed(75),
            Color::Indexed(75),
            Color::Default,
        ),
        (RED, RED, Color::Indexed(196), Color::Default),
        (
            Color::TrueColor(31, 31, 31),
            Color::TrueColor(31, 31, 31),
            Color::Indexed(234),
            Color::Default,
        ),
    ];
    for (c, tc, a256, a16) in cases {
        assert_eq!(ColorDepth::TrueColor.reduce(c), tc, "{c:?}");
        assert_eq!(ColorDepth::Ansi256.reduce(c), a256, "{c:?}");
        assert_eq!(ColorDepth::Ansi16.reduce(c), a16, "{c:?}");
    }
    assert_eq!(ColorDepth::default(), ColorDepth::TrueColor);
    assert!(ColorDepth::TrueColor.colorful() && ColorDepth::Ansi256.colorful());
    assert!(!ColorDepth::Ansi16.colorful());
}

#[test]
fn the_depth_comes_from_the_environment() {
    let vars = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    };
    let detect = |pairs: &[(&str, &str)]| ColorDepth::detect(&vars(pairs));
    assert_eq!(detect(&[("COLORTERM", "truecolor")]), ColorDepth::TrueColor);
    assert_eq!(detect(&[("COLORTERM", "24bit")]), ColorDepth::TrueColor);
    assert_eq!(detect(&[("COLORTERM", "TrueColor")]), ColorDepth::TrueColor);
    assert_eq!(
        detect(&[("COLORTERM", "truecolor"), ("TERM", "xterm")]),
        ColorDepth::TrueColor
    );
    assert_eq!(detect(&[("TERM", "xterm-256color")]), ColorDepth::Ansi256);
    assert_eq!(detect(&[("TERM", "xterm-direct")]), ColorDepth::Ansi256);
    assert_eq!(detect(&[("TERM", "screen-256color")]), ColorDepth::Ansi256);
    assert_eq!(detect(&[("TERM", "xterm")]), ColorDepth::Ansi16);
    assert_eq!(detect(&[]), ColorDepth::Ansi16);
}

#[test]
fn a_true_color_pool_keeps_the_colors() {
    let mut pool = StylePool::new();
    assert_eq!(pool.depth(), ColorDepth::TrueColor);
    let s = Style::new(RED, Color::TrueColor(1, 2, 3), attr::BOLD);
    let id = pool.intern(s, &Decor::PLAIN);
    assert_eq!(pool.style_of(id), s);
    assert_eq!(pool.set_sequence(id), "\x1b[0;1;38;2;255;0;0;48;2;1;2;3m");
}

#[test]
fn a_256_color_pool_interns_the_nearest_cells() {
    let mut pool = StylePool::with_depth(ColorDepth::Ansi256);
    let s = Style::new(
        RED,
        Color::TrueColor(31, 31, 31),
        attr::ITALIC | attr::CARET,
    );
    let id = pool.intern(s, &Decor::PLAIN);
    assert_eq!(
        pool.style_of(id),
        Style::new(
            Color::Indexed(196),
            Color::Indexed(234),
            attr::ITALIC | attr::CARET
        ),
        "属性不动"
    );
    assert_eq!(pool.set_sequence(id), "\x1b[0;3;38;5;196;48;5;234m");
    // 降成同一个样子的样式共用一个 id；再见到原来的键也是它。
    let near = Style::new(
        Color::TrueColor(250, 5, 5),
        Color::TrueColor(30, 30, 30),
        attr::ITALIC | attr::CARET,
    );
    assert_eq!(pool.intern(near, &Decor::PLAIN), id);
    assert_eq!(pool.intern(s, &Decor::PLAIN), id);
    let reduced = Style::new(
        Color::Indexed(196),
        Color::Indexed(234),
        attr::ITALIC | attr::CARET,
    );
    assert_eq!(pool.intern(reduced, &Decor::PLAIN), id);
    // 下划线色也降；链接不动。
    let d = Decor {
        underline: Color::TrueColor(99, 168, 248),
        link: "https://example.com".to_owned(),
    };
    let lid = pool.intern(Style::UNDERLINE, &d);
    assert_eq!(pool.decor_of(lid).underline, Color::Indexed(75));
    assert_eq!(pool.decor_of(lid).link, "https://example.com");
    assert!(pool.set_sequence(lid).contains("58:5:75m"));
    assert!(!pool.set_sequence(lid).contains("58:2"));
}

#[test]
fn a_16_color_pool_drops_the_true_colors_and_the_high_cells() {
    let mut pool = StylePool::with_depth(ColorDepth::Ansi16);
    let id = pool.intern(
        Style::new(RED, Color::Indexed(200), attr::BOLD),
        &Decor::PLAIN,
    );
    assert_eq!(pool.style_of(id), Style::BOLD);
    assert_eq!(pool.set_sequence(id), "\x1b[0;1m");
    assert_eq!(pool.intern(Style::BOLD, &Decor::PLAIN), id);
    // 只有真彩的样式降成没有颜色：与 id 0 同一个。
    assert_eq!(pool.intern(Style::fg(RED), &Decor::PLAIN), 0);
    let named = pool.intern(Style::fg(Color::Named(4)), &Decor::PLAIN);
    assert_eq!(pool.style_of(named), Style::fg(Color::Named(4)));
    // 只有下划线色的 decor 丢掉颜色之后是 plain：与没有 decor 的同一个 id。
    let under = pool.intern(Style::UNDERLINE, &Decor::underline(RED));
    assert_eq!(under, pool.intern(Style::UNDERLINE, &Decor::PLAIN));
    assert!(pool.decor_of(under).is_plain());
}

#[test]
fn the_painted_ground_is_reduced_too() {
    let mut pool = StylePool::with_depth(ColorDepth::Ansi256);
    pool.reground(Some((
        Color::TrueColor(214, 216, 220),
        Color::TrueColor(21, 22, 26),
    )));
    assert_eq!(pool.base(), "\x1b[0;38;5;188;48;5;233m");
    let mut sixteen = StylePool::with_depth(ColorDepth::Ansi16);
    sixteen.reground(Some((
        Color::TrueColor(214, 216, 220),
        Color::TrueColor(21, 22, 26),
    )));
    assert_eq!(sixteen.base(), "\x1b[0m");
}

fn frame(depth: ColorDepth, backdrop: Option<&Backdrop>) -> String {
    let _w = widths();
    let line = StyledLine::new(vec![
        Span::new("hi", Style::new(RED, Color::TrueColor(31, 31, 31), 0)),
        Span::new(" ok", Style::fg(Color::Named(2))),
    ]);
    Renderer::new(String::new(), String::new())
        .with_depth(depth)
        .render(10, 2, &[line], &[], backdrop)
        .unwrap()
}

#[test]
fn the_renderer_writes_256_color_cells_at_ansi256() {
    let out = frame(ColorDepth::Ansi256, None);
    assert!(out.contains("38;5;196;48;5;234"), "{out:?}");
    assert!(!out.contains("38;2;") && !out.contains("48;2;"), "{out:?}");
    assert!(out.contains(";32m"), "命名色原样：{out:?}");
    let tc = frame(ColorDepth::TrueColor, None);
    assert!(tc.contains("38;2;255;0;0;48;2;31;31;31"), "{tc:?}");
    let ansi16 = frame(ColorDepth::Ansi16, None);
    assert!(
        !ansi16.contains("38;") && !ansi16.contains("48;"),
        "{ansi16:?}"
    );
    assert!(ansi16.contains(";32m"), "{ansi16:?}");
}

#[test]
fn the_backdrop_is_reduced_when_it_is_emitted() {
    let b = Backdrop {
        fg: Color::TrueColor(214, 216, 220),
        bg: Color::TrueColor(21, 22, 26),
        restore: None,
    };
    // 真彩：原样。
    let tc = frame(ColorDepth::TrueColor, Some(&b));
    assert!(
        tc.contains("\x1b]10;rgb:d6/d8/dc\x07\x1b]11;rgb:15/16/1a\x07"),
        "{tc:?}"
    );
    // 256 色：终端的默认色设成格子的标准 RGB（188 是 d7d7d7，233 是 121212），格子写成同样的格。
    let a256 = frame(ColorDepth::Ansi256, Some(&b));
    assert!(
        a256.contains("\x1b]10;rgb:d7/d7/d7\x07\x1b]11;rgb:12/12/12\x07"),
        "{a256:?}"
    );
    assert!(a256.contains("48;5;233"), "地是同一格：{a256:?}");
    assert!(
        !a256.contains("38;2;") && !a256.contains("48;2;"),
        "{a256:?}"
    );
    // 16 色：不设默认色，格子不带地。
    let a16 = frame(ColorDepth::Ansi16, Some(&b));
    assert!(
        !a16.contains("\x1b]10;") && !a16.contains("\x1b]11;"),
        "{a16:?}"
    );
    assert!(!a16.contains("48;"), "{a16:?}");
}
