use std::time::Duration;

use super::glyph;

/// 用户决定（2026-10-22，tui-display.md §2.1.2）：加载动画是点阵 spinner，十帧顺序循环，每帧 80 毫秒。
#[test]
fn the_spinner_cycles_the_braille_frames_every_80_ms() {
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    for (index, frame) in frames.iter().enumerate() {
        let at = Duration::from_millis(80 * index as u64);
        assert_eq!(glyph(at), *frame, "frame {index}");
        // 一帧之内不变。
        assert_eq!(
            glyph(at + Duration::from_millis(79)),
            *frame,
            "frame {index}"
        );
    }
    // 一圈 800 毫秒后从头开始（顺序循环，不来回）。
    assert_eq!(glyph(Duration::from_millis(800)), "⠋");
    assert_eq!(glyph(Duration::from_millis(880)), "⠙");
}

#[test]
fn the_spinner_never_shows_the_star_glyphs() {
    for ms in (0..4000).step_by(10) {
        let shown = glyph(Duration::from_millis(ms));
        assert!(
            !["·", "✢", "✳", "✶", "✻", "✽"].contains(&shown),
            "{shown} at {ms} ms"
        );
    }
}
