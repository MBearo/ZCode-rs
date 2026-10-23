// 复制自 apps/zcode-host/crates/tui/src/ui/shimmer.rs（feat/rs 79b6691c55）。
//! 运行中标题的扫光（docs/specs/rust-tui.md §8.3）：一条亮带每 2 秒从左扫到右，亮带半径 5 个字符，
//! 在背景色与标题颜色之间按余弦曲线混合。只有真彩色终端有扫光，其余终端用标题颜色加粗。
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::palette::{ColorLevel, Palette, blend};

const SWEEP: Duration = Duration::from_secs(2);
const BAND: f64 = 5.0;

pub fn spans(text: &str, elapsed: Duration, palette: &Palette, color: Color) -> Vec<Span<'static>> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let (ColorLevel::TrueColor, Some(background), Color::Rgb(r, g, b)) =
        (palette.level, palette.background, color)
    else {
        return vec![Span::styled(text.to_owned(), bold.fg(color))];
    };
    let chars: Vec<char> = text.chars().collect();
    let span = chars.len() as f64 + 2.0 * BAND;
    let phase = (elapsed.as_secs_f64() % SWEEP.as_secs_f64()) / SWEEP.as_secs_f64();
    let center = phase * span - BAND;
    chars
        .iter()
        .enumerate()
        .map(|(index, c)| {
            let distance = (index as f64 - center).abs();
            // 亮带外为 0.6（偏暗），亮带中心为 1（标题颜色）。
            let glow = if distance < BAND {
                0.5 * (1.0 + (std::f64::consts::PI * distance / BAND).cos())
            } else {
                0.0
            };
            let alpha = 0.6 + 0.4 * glow;
            let (r, g, b) = blend(background, (r, g, b), alpha);
            Span::styled(c.to_string(), bold.fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_moves_and_plain_terminals_fall_back_to_bold() {
        let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
        let accent = palette.accent;
        let early = spans("Working", Duration::from_millis(400), &palette, accent);
        let late = spans("Working", Duration::from_millis(1400), &palette, accent);
        assert_eq!(early.len(), 7);
        assert_ne!(early[0].style.fg, late[0].style.fg, "the highlight moved");
        let plain = Palette::new(ColorLevel::Ansi16, None);
        let spans = spans("Working", Duration::ZERO, &plain, plain.accent);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }
}
