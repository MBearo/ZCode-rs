//! 渲染器的测试。移植自 packages/tui/tui/test/src/zcode/tui/render/RendererSuite.scala 与 TerminalSuite.scala，以及
//! packages/tui/bench/test/src/zcode/tui/bench/VtSuite.scala 里用渲染器的那几个（feat/tui a2bf878b34）。
//!
//! 类宽度的采纳（`width::adopt`）是进程全局的：采纳的那个测试经 [`Adopted`] 拿 crate 共用的宽度锁
//! （`testing/width_lock.rs`）的写锁、drop 时还原；本文件别的测试经 [`Rig`] 或 [`widths`] 拿读锁，不会在它采纳期间铺含
//! emoji 的行。
mod emu;
mod graphics;
mod ground;
mod vt_frames;

use std::sync::RwLockReadGuard;

use super::renderer::{Backdrop, Graphic, Renderer};
use super::{Span, Style, StyledLine, ansi, width};
use crate::testing::width_lock::{Adopted, widths};
use emu::TermEmu;

fn sl(s: &str) -> StyledLine {
    StyledLine::text(s.to_string(), Style::DEFAULT)
}

fn lines(ss: &[&str]) -> Vec<StyledLine> {
    ss.iter().map(|s| sl(s)).collect()
}

/// 一个渲染器接到一个新的模拟器上；数写了几次、留着最后一次写的。（Scala 的 `setup` / `setupKitty`。）
struct Rig {
    r: Renderer,
    emu: TermEmu,
    writes: usize,
    last: String,
    _widths: RwLockReadGuard<'static, ()>,
}

impl Rig {
    fn new(cols: usize, rows: usize) -> Rig {
        Rig::on(
            TermEmu::new(cols, rows),
            Renderer::new(String::new(), String::new()),
        )
    }

    /// 能用 kitty 键盘协议的终端的备用屏设置与收尾（键盘增强的 push / pop），进出时看得见模拟器每个缓冲的增强栈。
    fn kitty(cols: usize, rows: usize) -> Rig {
        Rig::on(
            TermEmu::new(cols, rows),
            Renderer::new(
                ansi::PUSH_KEYBOARD_ENHANCEMENT.to_string(),
                ansi::POP_KEYBOARD_ENHANCEMENT.to_string(),
            ),
        )
    }

    fn on(emu: TermEmu, r: Renderer) -> Rig {
        Rig {
            r,
            emu,
            writes: 0,
            last: String::new(),
            _widths: widths(),
        }
    }

    fn take(&mut self, frame: Option<String>) {
        if let Some(f) = frame {
            self.writes += 1;
            self.emu.feed(&f);
            self.last = f;
        }
    }

    fn render(&mut self, width: usize, rows: usize, lines: &[StyledLine]) {
        self.paint(width, rows, lines, &[], None);
    }

    fn paint(
        &mut self,
        width: usize,
        rows: usize,
        lines: &[StyledLine],
        graphics: &[Graphic],
        backdrop: Option<&Backdrop>,
    ) {
        let frame = self.r.render(width, rows, lines, graphics, backdrop);
        self.take(frame);
    }

    fn exit(&mut self) {
        let frame = self.r.exit();
        self.take(frame);
    }
}

/// 把 ✅ 画成一列的终端（旧的宽度表），我们的表说两列。
fn narrow_check(cp: char) -> usize {
    if cp == '\u{2705}' {
        1
    } else {
        width::display_width(cp)
    }
}

/// 把 🖥 画成两列的终端，我们的表说一列。
fn wide_desktop(cp: char) -> usize {
    if cp == '\u{1f5a5}' {
        2
    } else {
        width::display_width(cp)
    }
}

#[test]
fn the_first_frame_enters_the_alt_buffer_and_paints_every_row() {
    let mut t = Rig::new(20, 4);
    t.render(20, 4, &lines(&["HEADER", "body", "more", "FOOTER"]));
    assert!(
        t.last.contains(ansi::ALT_SCREEN_ENTER),
        "expected alt-screen enter: {:?}",
        t.last
    );
    assert_eq!(t.emu.line(0), "HEADER");
    assert_eq!(t.emu.line(1), "body");
    assert_eq!(t.emu.line(3), "FOOTER");
}

#[test]
fn too_few_lines_are_padded_with_blank_rows_too_many_are_clipped() {
    let mut t = Rig::new(20, 3);
    t.render(20, 3, &lines(&["a"]));
    assert_eq!(t.emu.viewport(), vec!["a", "", ""]);
    t.render(20, 3, &lines(&["1", "2", "3", "4"]));
    assert_eq!(t.emu.viewport(), vec!["1", "2", "3"]);
}

#[test]
fn invalidate_forces_the_next_same_frame_to_clear_and_repaint_fully() {
    let mut t = Rig::new(20, 4);
    let frame = lines(&["HEADER", "body", "more", "FOOTER"]);
    t.render(20, 4, &frame);
    // 相同的帧通常什么都不写……
    let before = t.writes;
    t.render(20, 4, &frame);
    assert_eq!(t.writes, before, "identical frame must be a no-op");
    // ……但 invalidate 之后清掉缓冲重画每一行，不再进一次备用屏。
    t.r.invalidate();
    t.render(20, 4, &frame);
    assert!(t.writes > before, "invalidation must force a repaint");
    assert!(
        !t.last.contains(ansi::ALT_SCREEN_ENTER),
        "already in the alt buffer: {:?}",
        t.last
    );
    assert!(
        t.last.contains(ansi::CLEAR_SCREEN),
        "expected a clear + full repaint: {:?}",
        t.last
    );
    assert_eq!(t.emu.line(0), "HEADER");
    assert_eq!(t.emu.line(3), "FOOTER");
}

#[test]
fn a_same_size_second_frame_patches_only_the_changed_cell() {
    let mut t = Rig::new(20, 4);
    t.render(20, 4, &lines(&["HEADER", "body", "more", "FOOTER"]));
    t.render(20, 4, &lines(&["HEADER", "bodX", "more", "FOOTER"]));
    assert_eq!(t.emu.line(1), "bodX");
    assert!(
        !t.last.contains(ansi::ALT_SCREEN_ENTER),
        "already in the alt buffer: {:?}",
        t.last
    );
    assert!(
        !t.last.contains(ansi::CLEAR_SCREEN),
        "an incremental frame must not clear: {:?}",
        t.last
    );
    // 没变的行不重画，补丁里没有它们的文字。
    assert!(
        !t.last.contains("HEADER"),
        "unchanged rows should not be repainted: {:?}",
        t.last
    );
}

#[test]
fn a_resize_clears_and_repaints_at_the_new_size() {
    let mut t = Rig::new(24, 4);
    t.render(24, 4, &lines(&["hello world", "x", "y", "---"]));
    t.render(10, 3, &lines(&["hi", "z", "==="]));
    assert!(t.last.contains(ansi::CLEAR_SCREEN));
    assert_eq!(t.emu.line(0), "hi");
    assert_eq!(t.emu.line(2), "===");
}

#[test]
fn frames_end_with_a_style_reset_so_panel_backgrounds_cannot_leak() {
    let mut t = Rig::new(20, 2);
    let panel = Style::new(super::Color::WHITE, super::Color::Indexed(236), 0);
    t.render(20, 2, &[StyledLine::new(vec![Span::new("panel", panel)])]);
    let tail = format!("{}{}", ansi::RESET, ansi::SYNC_END);
    assert!(
        t.last.ends_with(&tail),
        "frame should reset style before leaving synchronized output"
    );
}

#[test]
fn a_links_cells_carry_its_target_the_cells_after_it_do_not_and_no_frame_ends_inside_it() {
    let mut t = Rig::new(20, 2);
    let link = super::Decor::link("https://example.com");
    t.render(
        20,
        2,
        &[StyledLine::new(vec![
            Span::new("see ", Style::DEFAULT),
            Span::with_decor("docs", Style::UNDERLINE, link),
            Span::new(" now", Style::DEFAULT),
        ])],
    );
    assert_eq!(t.emu.line(0), "see docs now");
    let targets: Vec<&str> = (0..12).map(|c| t.emu.link_at(0, c)).collect();
    let mut want = vec![""; 4];
    want.extend(vec!["https://example.com"; 4]);
    want.extend(vec![""; 4]);
    assert_eq!(targets, want);
    assert!(!t.emu.link_open(), "{:?}", t.last);
}

#[test]
fn a_patch_inside_a_link_reopens_it_and_a_link_at_the_end_of_a_frame_is_closed() {
    let mut t = Rig::new(20, 2);
    let link = super::Decor::link("https://example.com");
    t.render(
        20,
        2,
        &[StyledLine::new(vec![Span::with_decor(
            "docs",
            Style::UNDERLINE,
            link.clone(),
        )])],
    );
    t.render(
        20,
        2,
        &[StyledLine::new(vec![Span::with_decor(
            "doCs",
            Style::UNDERLINE,
            link,
        )])],
    );
    assert_eq!(t.emu.line(0), "doCs");
    assert_eq!(t.emu.link_at(0, 2), "https://example.com");
    assert!(!t.emu.link_open(), "{:?}", t.last);
}

#[test]
fn exit_is_idempotent_and_a_no_op_before_the_first_frame() {
    let mut t = Rig::new(24, 8);
    let before = t.writes;
    t.exit();
    assert_eq!(
        t.writes, before,
        "exit must not write when not in the alt buffer"
    );
    t.render(24, 4, &lines(&["a", "b", "c", "d"]));
    t.exit();
    assert_eq!(
        t.last,
        format!("{}{}", ansi::WRAP_ON, ansi::ALT_SCREEN_EXIT),
        "expected a bare wrap-restore + alt-screen exit"
    );
    let after = t.writes;
    t.exit();
    assert_eq!(t.writes, after, "a second exit is a no-op");
}

/* ---- 每个缓冲的 kitty 键盘增强（有状态的模拟） ---- */

#[test]
fn entering_the_alt_buffer_pushes_the_kitty_enhancement_onto_that_buffers_stack() {
    let mut t = Rig::kitty(20, 4);
    assert!(
        !t.emu.enhancement_active(),
        "the main buffer starts with no keyboard enhancement"
    );
    t.render(20, 4, &lines(&["A", "B", "C", "D"]));
    assert!(
        t.emu.enhancement_active(),
        "the alt buffer must have the keyboard enhancement active"
    );
    assert_eq!(
        t.emu.enhancement_depth(),
        1,
        "exactly one enhancement is pushed on entry"
    );
}

#[test]
fn same_size_frames_dont_re_push_so_the_alt_stack_stays_at_depth_one() {
    let mut t = Rig::kitty(20, 4);
    t.render(20, 4, &lines(&["A", "B", "C", "D"]));
    t.render(20, 4, &lines(&["A", "X", "C", "D"])); // 增量补丁，不再进入
    assert_eq!(
        t.emu.enhancement_depth(),
        1,
        "an incremental frame must not push again"
    );
}

#[test]
fn exit_pops_the_alt_buffers_enhancement_leaving_both_buffers_balanced() {
    let mut t = Rig::kitty(20, 4);
    t.render(20, 4, &lines(&["A", "B", "C", "D"]));
    t.exit();
    assert!(
        !t.emu.enhancement_active(),
        "the main buffer has no enhancement after leaving"
    );
    assert_eq!(
        t.emu.enhancement_depth(),
        0,
        "the (now active) main buffer's stack is empty"
    );
}

/* ---- 宽度有争议的字形 ---- */

#[test]
fn a_width_disputed_row_is_rewritten_whole_so_a_narrow_emoji_terminal_shows_no_residue() {
    // 一个（旧表的）终端把 ✅ 画成一列，Width 说两列。第 2 帧把 emoji 之后的内容挪了位置；逐格 diff 只写变化的格子、
    // 靠相对移动，而这个终端少落一列，留下第 1 帧的字形（"✅b✅c"）。整行重写加擦到行尾必须正好得出压紧的新内容。
    let mut t = Rig::on(
        TermEmu::with_widths(24, 8, narrow_check),
        Renderer::new(String::new(), String::new()),
    );
    t.render(24, 3, &lines(&["ab✅c", "plain", ""]));
    assert_eq!(t.emu.line(0), "ab✅c");
    t.render(24, 3, &lines(&["✅✅c", "plain", ""]));
    assert_eq!(
        t.emu.line(0),
        "✅✅c",
        "stale cells must not survive a shifted emoji row"
    );
    assert_eq!(
        t.emu.line(1),
        "plain",
        "containment must stay inside the risky row"
    );
    // 同一帧再来一次仍是干净的 no-op（稳定，不来回摆）。
    t.render(24, 3, &lines(&["✅✅c", "plain", ""]));
    assert_eq!(t.emu.line(0), "✅✅c");
}

#[test]
fn a_full_width_disputed_row_keeps_its_last_cell_on_agreeing_and_on_narrower_terminals() {
    // 修复原因：整行写满时光标停在最后一列，擦到行尾把最后一格擦掉了（屏幕右边缘缺一格）。
    let full = format!("✅{}", "x".repeat(22)); // 按 Width 正好 24 列
    let mut agreeing = Rig::new(24, 3);
    agreeing.render(24, 3, &lines(&["a", "", ""]));
    agreeing.render(24, 3, &lines(&[&full, "", ""]));
    assert_eq!(
        agreeing.emu.line(0),
        full,
        "the last cell survives the rewrite"
    );
    let ys = format!("✅{}", "y".repeat(22));
    agreeing.render(24, 3, &lines(&[&ys, "", ""]));
    assert_eq!(agreeing.emu.line(0), ys);
    drop(agreeing);
    // 把 ✅ 画成一列的终端写这一行少一格：最后一格不能留着上一帧的字形。
    let mut narrow = Rig::on(
        TermEmu::with_widths(24, 3, narrow_check),
        Renderer::new(String::new(), String::new()),
    );
    narrow.render(24, 3, &lines(&[&full, "", ""]));
    narrow.render(24, 3, &lines(&[&ys, "", ""]));
    // 它的 23 个字形落在第 0–22 列；重新对齐后的最后一个字形落在 23：压紧了，没有旧格子。
    assert_eq!(
        narrow.emu.line(0),
        format!("✅{}", "y".repeat(23)),
        "no stale cell at the margin; the shift stays inside the row"
    );
}

#[test]
fn a_row_going_emoji_to_plain_erases_the_terminals_wider_stale_tail() {
    // 终端把 🖥（Width：一列）画成两列，屏幕上的行比模型以为的宽。这一行换成纯文字时，重写必须擦过模型自己的范围（EL），
    // 不然旧的尾巴留着。
    let mut t = Rig::on(
        TermEmu::with_widths(24, 8, wide_desktop),
        Renderer::new(String::new(), String::new()),
    );
    t.render(24, 3, &lines(&["🖥xyz", "plain", ""]));
    t.render(24, 3, &lines(&["ok", "plain", ""]));
    assert_eq!(
        t.emu.line(0),
        "ok",
        "the wider-than-model stale tail must be erased"
    );
}

#[test]
fn autowrap_is_off_so_an_under_counted_full_row_clips_instead_of_shifting_the_frame() {
    // 顶行的模型宽度正好是 `cols`；终端把 🖥 多算一列，没有 ?7l 时写这一行会折行、把后面每一行都推下去（paint_fresh 的
    // \r\n 就落低一行）。
    let mut t = Rig::on(
        TermEmu::with_widths(10, 4, wide_desktop),
        Renderer::new(String::new(), String::new()),
    );
    let top = format!("🖥{}", "a".repeat(9));
    t.render(10, 4, &lines(&[&top, "next", "", ""]));
    assert!(
        !t.emu.autowrap_enabled(),
        "the alt screen must run with DECAWM off"
    );
    assert_eq!(
        t.emu.line(1),
        "next",
        "the overflow must clip in-row, not wrap into the next row"
    );
}

#[test]
fn adopted_probe_widths_bring_the_model_into_lockstep_with_a_narrow_emoji_terminal() {
    // 启动探测量出 ✅ / 😀 是一列、Width 采纳之后，布局、diff 与终端都一致——即使在旧终端上，emoji 之后的内容也在它
    // 该在的列。进程全局的采纳：独占宽度锁，drop 时还原（不经 Rig，它拿读锁）。
    let _adopted = Adopted::new(1, 1, 1);
    let mut emu = TermEmu::with_widths(24, 8, |cp| {
        if cp == '\u{2705}' || cp == '\u{1f600}' {
            1
        } else {
            width::display_width(cp)
        }
    });
    let mut r = Renderer::new(String::new(), String::new());
    if let Some(f) = r.render(24, 3, &lines(&["✅ok😀!", "plain", ""]), &[], None) {
        emu.feed(&f);
    }
    assert_eq!(emu.line(0), "✅ok😀!");
    if let Some(f) = r.render(24, 3, &lines(&["✅ok😀?", "plain", ""]), &[], None) {
        emu.feed(&f);
    }
    assert_eq!(emu.line(0), "✅ok😀?");
}

#[test]
fn autowrap_is_restored_on_exit() {
    let mut t = Rig::new(20, 4);
    t.render(20, 4, &lines(&["A", "B", "C", "D"]));
    assert!(!t.emu.autowrap_enabled());
    t.exit();
    assert!(t.emu.autowrap_enabled(), "exit must restore DECAWM");
}

/* ---- TerminalSuite：终端模式的序列 ---- */

#[test]
fn keyboard_enhancement_uses_kitty_push_pop_sequences() {
    assert_eq!(
        ansi::PUSH_KEYBOARD_ENHANCEMENT,
        format!("{}>27u", ansi::CSI)
    );
    assert_eq!(ansi::POP_KEYBOARD_ENHANCEMENT, format!("{}<u", ansi::CSI));
    assert_eq!(
        ansi::PUSH_KEYBOARD_DISAMBIGUATE,
        format!("{}>1u", ansi::CSI)
    );
}

#[test]
fn mouse_enable_disable_use_sgr_1006_button_motion_reporting_disable_order_reversed() {
    let csi = ansi::CSI;
    assert_eq!(
        ansi::MOUSE_ENABLE,
        format!("{csi}?1000h{csi}?1002h{csi}?1006h")
    );
    assert_eq!(
        ansi::MOUSE_DISABLE,
        format!("{csi}?1006l{csi}?1002l{csi}?1000l")
    );
}

#[test]
fn osc52_copy_wraps_a_base64_payload_in_an_osc_52_clipboard_write_terminated_with_bel() {
    assert_eq!(
        ansi::osc52_copy("aGk="),
        format!("{}52;c;aGk={}", ansi::OSC, ansi::BELL)
    );
}
