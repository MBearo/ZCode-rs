//! 启动握手的测试。
//! 移植自 packages/tui/tui/test/src/zcode/tui/app/TerminalProbeSuite.scala、WidthProbeSuite.scala，以及
//! packages/tui/tui/test/src/zcode/tui/platform/NodeTerminalSuite.scala 的 kitty 键盘级别部分（feat/tui a2bf878b34）。
//!
//! Scala 在 settle 里调用进程级的 `Width.adopt`，再查 `Width.displayWidth`。这里 settle 只交出量到的宽度
//! （`Settled::widths`），测试把它作用在缺省宽度上（`Measured::apply`）再查纯的 `Widths::display_width`：不碰进程级
//! 状态，与别的测试并行也不互相影响。
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::handshake::{
    CellPx, Handshake, KITTY_QUERY, Measured, Settled, TerminalProbe, TerminalReport, WidthProbe,
    keyboard_enhancement, parse_color,
};
use crate::render::ansi;
use crate::render::width::Widths;
use crate::theme::Rgb;

/// 喂一段字节，收集所有放出的字节与完成标志。
fn feed_all(p: &mut impl Handshake, s: &str) -> (Vec<u8>, bool) {
    let mut leaked = Vec::new();
    let mut done = false;
    for &b in s.as_bytes() {
        let step = p.feed(b);
        leaked.extend(step.passthrough);
        done |= step.done;
    }
    (leaked, done)
}

/// 落定后采纳的宽度：量到的作用在缺省宽度上，没量到就是缺省。
fn adopted(settled: &Settled) -> Widths {
    settled
        .widths
        .map_or(Widths::DEFAULT, |m| m.apply(Widths::DEFAULT))
}

fn width_of(w: Widths, cp: u32) -> usize {
    w.display_width(char::from_u32(cp).unwrap())
}

// ---- WidthProbe ----

#[test]
fn width_request_prints_each_probe_glyph_at_column_0_and_erases_the_line() {
    let req = WidthProbe::new().request();
    for glyph in ["✅", "😀", "①", "一"] {
        let probe = format!("{}{glyph}{}6n", ansi::CARRIAGE_RETURN, ansi::CSI);
        assert!(req.contains(&probe), "missing probe for {glyph}");
    }
    assert!(
        req.starts_with(ansi::SYNC_BEGIN),
        "the probe must ride in a sync wrap (no flash)"
    );
    let tail = format!(
        "{}{}{}",
        ansi::CARRIAGE_RETURN,
        ansi::ERASE_TO_EOL,
        ansi::SYNC_END
    );
    assert!(req.ends_with(&tail), "the probe line must be erased");
}

#[test]
fn a_full_reply_adopts_the_terminals_widths() {
    // ✅→1、😀→1、①→2、一→2：CJK 配置下旧 wcwidth 的终端（老的窄 emoji、宽的模糊字符）。
    let mut p = WidthProbe::new();
    let (leaked, done) = feed_all(&mut p, "\x1b[5;2R\x1b[5;2R\x1b[5;3R\x1b[5;3R");
    assert_eq!(leaked, Vec::<u8>::new());
    assert!(done, "four CPRs must complete the probe");
    let settled = p.settle();
    assert_eq!(settled.replay, Vec::<u8>::new());
    let w = adopted(&settled);
    assert_eq!(width_of(w, 0x2705), 1, "✅ must follow the terminal");
    assert_eq!(width_of(w, 0x1f600), 1, "😀 must follow the terminal");
    assert_eq!(width_of(w, 0x2460), 2, "① must follow the terminal");
    assert_eq!(width_of(w, 0x4e00), 2, "CJK is untouched");
}

#[test]
fn a_reply_whose_cjk_sanity_glyph_measures_wrong_is_distrusted_wholesale() {
    // emoji 的回答看着合理，但一量出 3——乱码或过滤的复用器；这个终端说的都不能信。
    let mut p = WidthProbe::new();
    let (_, done) = feed_all(&mut p, "\x1b[5;2R\x1b[5;2R\x1b[5;2R\x1b[5;4R");
    assert!(done);
    let settled = p.settle();
    assert_eq!(settled.widths, None);
    let w = adopted(&settled);
    assert_eq!(
        width_of(w, 0x2705),
        2,
        "a distrusted probe must keep the defaults"
    );
    assert_eq!(width_of(w, 0x2460), 1);
}

#[test]
fn keystrokes_racing_the_reply_pass_through_untouched_including_csi_shaped_ones() {
    let mut p = WidthProbe::new();
    let (leading, _) = feed_all(&mut p, "a\x1b[Ab");
    assert_eq!(
        leading,
        b"a\x1b[Ab".to_vec(),
        "an arrow key must leak intact"
    );
    let (rest, done) = feed_all(&mut p, "\x1b[1;3R\x1b[1;3R\x1b[1;2R\x1b[1;3R");
    assert_eq!(rest, Vec::<u8>::new());
    assert!(
        done,
        "the probe must still complete around interleaved input"
    );
    let w = adopted(&p.settle());
    assert_eq!(width_of(w, 0x2705), 2);
    assert_eq!(width_of(w, 0x1f600), 2);
}

#[test]
fn a_timed_out_partial_reply_keeps_the_defaults_and_releases_held_bytes() {
    let mut p = WidthProbe::new();
    let (leaked, done) = feed_all(&mut p, "\x1b[5;2R\x1b[5;2R\x1b[5;");
    assert_eq!(leaked, Vec::<u8>::new());
    assert!(!done);
    let settled = p.settle();
    assert_eq!(
        settled.replay,
        b"\x1b[5;".to_vec(),
        "held bytes must be released on timeout"
    );
    assert_eq!(
        settled.widths, None,
        "a partial probe must not adopt anything"
    );
    assert_eq!(width_of(adopted(&settled), 0x2705), 2);
}

#[test]
fn a_malformed_cpr_records_an_ignored_width_and_cannot_poison_the_classes() {
    let mut p = WidthProbe::new();
    let (_, done) = feed_all(&mut p, "\x1b[5R\x1b[5;2R\x1b[5;2R\x1b[5;3R");
    assert!(done);
    let w = adopted(&p.settle());
    // 第一个回答没有列 → 宽度不合理 → 采纳时忽略这一类。
    assert_eq!(
        width_of(w, 0x2705),
        2,
        "the malformed class must keep its default"
    );
    assert_eq!(width_of(w, 0x1f600), 1, "well-formed classes still adopt");
}

// ---- TerminalProbe ----

const CPRS: &str = "\x1b[5;3R\x1b[5;3R\x1b[5;2R\x1b[5;3R";

/// 一个把落定时的报告记下来的探测。
fn probe() -> (TerminalProbe, Arc<Mutex<Vec<TerminalReport>>>) {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    let p = TerminalProbe::new(move |r| sink.lock().unwrap().push(r));
    (p, reports)
}

#[test]
fn the_request_asks_for_widths_colours_kitty_graphics_and_cell_size_then_da1_then_erases_the_line()
{
    let req = TerminalProbe::new(|_| {}).request();
    assert!(req.starts_with(ansi::SYNC_BEGIN));
    assert!(
        req.contains(&format!("一{}6n", ansi::CSI)),
        "the width probe rides along"
    );
    let tail = "\x1b]10;?\x07\x1b]11;?\x07\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";
    assert!(req.contains(tail), "{}", req.replace('\x1b', "ESC"));
    assert!(req.contains(KITTY_QUERY));
    let end = format!(
        "{}{}{}",
        ansi::CARRIAGE_RETURN,
        ansi::ERASE_TO_EOL,
        ansi::SYNC_END
    );
    assert!(
        req.ends_with(&end),
        "unknown replies printed as text are erased"
    );
}

#[test]
fn a_full_reply_reports_colours_kitty_graphics_sixel_and_the_cell_size() {
    let (mut p, reports) = probe();
    let reply = [
        CPRS,
        "\x1b]10;rgb:d6d6/d8d8/dcdc\x07", // BEL 结尾
        "\x1b]11;rgb:15/16/1a\x1b\\",     // ST 结尾，每通道两位
        "\x1b_Gi=31;OK\x1b\\",
        "\x1b[6;20;10t",
        "\x1b[?62;4;22c",
    ]
    .concat();
    let (leaked, done) = feed_all(&mut p, &reply);
    assert_eq!(leaked, Vec::<u8>::new());
    assert!(done, "DA1 ends the probe");
    let settled = p.settle();
    assert_eq!(settled.replay, Vec::<u8>::new());
    let reports = reports.lock().unwrap();
    let r = reports[0];
    assert_eq!(r.fg, Some(Rgb::new(0xd6, 0xd8, 0xdc)));
    assert_eq!(r.bg, Some(Rgb::new(0x15, 0x16, 0x1a)));
    assert!(r.kitty_graphics);
    assert!(r.sixel);
    assert_eq!(
        r.cell_px,
        Some(CellPx {
            width: 10,
            height: 20
        }),
        "CSI 6;height;width t"
    );
    assert_eq!(
        settled.widths,
        Some(Measured {
            emoji_bmp: 2,
            emoji_astral: 2,
            ambiguous: 1
        })
    );
    assert_eq!(
        width_of(adopted(&settled), 0x2460),
        1,
        "the width replies are still adopted"
    );
}

#[test]
fn a_terminal_that_ignores_the_extra_queries_still_finishes_on_da1() {
    let (mut p, reports) = probe();
    let (_, done) = feed_all(&mut p, &format!("{CPRS}\x1b[?1;2c"));
    assert!(done);
    p.settle();
    assert_eq!(reports.lock().unwrap()[0], TerminalReport::default());
}

#[test]
fn a_kitty_error_reply_means_no_kitty_graphics() {
    let (mut p, reports) = probe();
    feed_all(
        &mut p,
        &format!("{CPRS}\x1b_Gi=31;ENOTSUPPORTED:no\x1b\\\x1b[?62c"),
    );
    p.settle();
    assert!(!reports.lock().unwrap()[0].kitty_graphics);
}

#[test]
fn keys_typed_during_the_probe_pass_through() {
    // 字母、方向键、Alt-x。
    let (mut p, _) = probe();
    let (keys, _) = feed_all(&mut p, "a\x1b[Ab\x1bx");
    assert_eq!(keys, b"a\x1b[Ab\x1bx".to_vec());
    let (rest, done) = feed_all(&mut p, &format!("{CPRS}\x1b[?62c"));
    assert_eq!(rest, Vec::<u8>::new());
    assert!(done);
}

#[test]
fn on_timeout_the_report_has_what_arrived_and_held_bytes_are_released() {
    let (mut p, reports) = probe();
    let (_, done) = feed_all(&mut p, "\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[5;");
    assert!(!done);
    assert_eq!(p.settle().replay, b"\x1b[5;".to_vec());
    let reports = reports.lock().unwrap();
    let bgs: Vec<Option<Rgb>> = reports.iter().map(|r| r.bg).collect();
    assert_eq!(bgs, vec![Some(Rgb::new(255, 255, 255))]);
    assert_eq!(reports[0].fg, None);
}

#[test]
fn settle_reports_exactly_once() {
    let (mut p, reports) = probe();
    p.settle();
    p.settle();
    assert_eq!(reports.lock().unwrap().len(), 1);
}

// ---- parse_color（Rust 移植补的：Scala 里只经 KeyParserSuite 间接覆盖） ----

#[test]
fn parse_color_scales_1_to_4_hex_digits_per_channel() {
    assert_eq!(parse_color("rgb:f/0/8"), Some(Rgb::new(255, 0, 136)));
    assert_eq!(parse_color("rgb:ff/00/80"), Some(Rgb::new(255, 0, 128)));
    assert_eq!(parse_color("rgb:fff/000/800"), Some(Rgb::new(255, 0, 128)));
    assert_eq!(
        parse_color("rgb:ffff/0000/8000"),
        Some(Rgb::new(255, 0, 128))
    );
    assert_eq!(
        parse_color("rgba:ffff/0000/8000/ffff"),
        Some(Rgb::new(255, 0, 128))
    );
    assert_eq!(parse_color("rgb:fffff/0/0"), None, "five digits");
    assert_eq!(parse_color("rgb:ff/ff"), None, "two channels");
    assert_eq!(parse_color("rgb:ff//ff"), None, "an empty channel");
    assert_eq!(parse_color("rgb:gg/00/00"), None, "not hex");
    assert_eq!(parse_color("#ffffff"), None, "not the X11 form");
}

// ---- kitty 键盘级别（docs/puffin-tui.md §4.3「按键解码」） ----

fn push(vars: &[(&str, &str)]) -> Option<&'static str> {
    let vars: BTreeMap<String, String> = vars
        .iter()
        .map(|&(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    keyboard_enhancement(&vars)
}

const FULL: Option<&str> = Some(ansi::PUSH_KEYBOARD_ENHANCEMENT);
const DISAMBIGUATE: Option<&str> = Some(ansi::PUSH_KEYBOARD_DISAMBIGUATE);

#[test]
fn kitty_ghostty_and_wezterm_get_the_full_level() {
    assert_eq!(push(&[("TERM", "xterm-kitty")]), FULL);
    assert_eq!(push(&[("KITTY_WINDOW_ID", "1")]), FULL);
    assert_eq!(push(&[("TERM", "xterm-ghostty")]), FULL);
    assert_eq!(push(&[("TERM_PROGRAM", "ghostty")]), FULL);
    assert_eq!(push(&[("TERM_PROGRAM", "WezTerm")]), FULL);
}

#[test]
fn other_terminals_get_no_push() {
    assert_eq!(push(&[]), None);
    assert_eq!(
        push(&[
            ("TERM_PROGRAM", "Apple_Terminal"),
            ("TERM", "xterm-256color")
        ]),
        None
    );
}

#[test]
fn iterm2_3_7_and_later_get_the_full_level() {
    for version in ["3.7.0", "3.7.6", "3.10.1", "4.0.0beta1", "3.7"] {
        assert_eq!(
            push(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", version)
            ]),
            FULL,
            "{version}"
        );
    }
}

#[test]
fn iterm2_before_3_7_or_of_unknown_version_gets_disambiguate_only() {
    // 开着所有键报告时它的输入法上屏会崩。
    for version in ["3.6.4", "3.6.11", "3.5.0", "3.7.0beta1", "nightly"] {
        assert_eq!(
            push(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", version)
            ]),
            DISAMBIGUATE,
            "{version}"
        );
    }
    assert_eq!(push(&[("TERM_PROGRAM", "iTerm.app")]), DISAMBIGUATE);
}
