// 复制自 apps/zcode-host/crates/tui/src/ui/logo/logo_tests.rs（2f982570bb）。
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::shape::{FIELD, sample};
use super::*;
use crate::ui::palette::{ColorLevel, Palette};

const DARK_BG: Rgb = (0x1e, 0x1e, 0x2e);
const PAPER: Rgb = (0xfa, 0xfa, 0xf7);

fn dots(cells: &[Cell]) -> u32 {
    cells.iter().map(|cell| cell.dots.count_ones()).sum()
}

fn luma((r, g, b): Rgb) -> u32 {
    u32::from(r) + u32::from(g) + u32::from(b)
}

/// 最亮的那些格子（亮度前 5%）的平均列：灯条在徽章上的位置。
fn glint_column(cells: &[Cell], columns: usize) -> f64 {
    let mut lit: Vec<(u32, usize)> = cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.dots != 0)
        .map(|(i, c)| (luma(c.rgb), i % columns))
        .collect();
    lit.sort_by_key(|(l, _)| std::cmp::Reverse(*l));
    let top = &lit[..lit.len() / 20];
    top.iter().map(|(_, column)| *column as f64).sum::<f64>() / top.len() as f64
}

#[test]
fn the_field_is_the_badge_with_the_z_cut_out() {
    // Z 的斜杠经过中心（镂空）；方块里 Z 外面是实心；方块外面是空的。
    assert!(sample(&FIELD, [0.0, 0.0]) < 0.0);
    assert!(sample(&FIELD, [-0.7, 0.0]) > 0.0);
    assert!(sample(&FIELD, [0.97, 0.97]) < 0.0);
    // 上横的左端与下横的右端也是镂空的。
    assert!(sample(&FIELD, [-0.45, -0.38]) < 0.0);
    assert!(sample(&FIELD, [0.45, 0.38]) < 0.0);
    assert!(sample(&FIELD, [-0.6, 0.0]) > sample(&FIELD, [-0.8, 0.0]));
}

#[test]
fn the_metal_badge_reflects_a_strip_that_slides_with_the_tilt() {
    let metal = Metal::new(false);
    let rest = frame(60, 21, REST, &metal);
    assert!(dots(&rest) > 3000, "{}", dots(&rest));
    assert_eq!(frame(60, 21, REST, &metal), rest, "deterministic");
    assert!(
        rest.iter()
            .filter(|c| c.dots != 0)
            .all(|c| luma(c.rgb) > luma(DARK_BG))
    );
    // 不动时灯条在左半边；向两边倾斜时灯条往相反的方向滑动一段。
    let at_rest = glint_column(&rest, 60);
    assert!(at_rest < 30.0, "{at_rest}");
    let left = glint_column(&frame(60, 21, Pose { yaw: -0.35, ..REST }, &metal), 60);
    let right = glint_column(&frame(60, 21, Pose { yaw: 0.35, ..REST }, &metal), 60);
    assert!((left - right).abs() > 6.0, "left {left} right {right}");
    // 浅色背景：金属整体比纸暗。
    let light = frame(60, 21, REST, &Metal::new(true));
    let lit: Vec<u32> = light
        .iter()
        .filter(|c| c.dots != 0)
        .map(|c| luma(c.rgb))
        .collect();
    assert!(lit.iter().sum::<u32>() / lit.len() as u32 + 60 < luma(PAPER));
}

#[test]
fn a_press_springs_in_and_settles() {
    assert_eq!(depth(0.0), 0.0);
    let samples: Vec<(f64, f64)> = (0..=200)
        .map(|i| (i as f64 / 100.0, depth(i as f64 / 100.0)))
        .collect();
    let (peak_at, peak) = samples
        .iter()
        .copied()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!(
        (0.1..0.2).contains(&peak_at) && (0.9..1.1).contains(&peak),
        "{peak_at} {peak}"
    );
    let overshoot = samples.iter().map(|s| s.1).fold(f64::INFINITY, f64::min);
    assert!((-0.6..-0.05).contains(&overshoot), "{overshoot}");
    assert!(
        samples
            .iter()
            .filter(|s| s.0 >= 1.2)
            .all(|s| s.1.abs() < 0.05)
    );
}

#[test]
fn the_press_tilts_toward_the_point_and_lights_the_badge() {
    let peak = 0.14;
    let right = pose_at([1.0, 0.0], peak);
    assert!(right.yaw > 0.3 && right.pitch.abs() < 1e-9 && right.push < 0.0);
    let bottom = pose_at([0.0, 1.0], peak);
    assert!(bottom.pitch < -0.3 && bottom.yaw.abs() < 1e-9);
    let center = pose_at([0.0, 0.0], peak);
    assert!(center.yaw == 0.0 && center.pitch == 0.0 && center.push < right.push);
    assert!(pose_at([1.0, 1.0], 5.0).yaw.abs() < 1e-6);
    // 不透明度：0.1 秒内亮起，停住后淡回。
    assert_eq!(opacity_at(0.0), STATIC_OPACITY);
    assert_eq!(opacity_at(0.1), 1.0);
    assert_eq!(opacity_at(1.0), 1.0);
    assert!((STATIC_OPACITY..1.0).contains(&opacity_at(1.7)));
    assert_eq!(opacity_at(DURATION + 1.0), STATIC_OPACITY);
}

#[test]
fn the_stage_is_centered_and_needs_room() {
    assert_eq!(
        stage_in(Rect::new(0, 3, 100, 30)),
        Some(Rect::new(20, 7, 60, 21))
    );
    assert_eq!(
        stage_in(Rect::new(0, 0, 100, 15)).map(|s| s.height),
        Some(14)
    );
    assert_eq!(stage_in(Rect::new(0, 0, 100, 13)), None);
    assert_eq!(stage_in(Rect::new(0, 0, 40, 30)), None);
}

#[test]
fn the_badge_is_still_until_clicked() {
    let palette = Palette::new(ColorLevel::TrueColor, Some(DARK_BG));
    let area = Rect::new(0, 0, 100, 30);
    let mut logo = Logo::default();
    let now = Instant::now();
    // 没画之前的点击不算。
    logo.press(50, 15, now);
    let mut buffer = Buffer::empty(area);
    logo.draw(&mut buffer, area, &palette, now);
    assert!(!logo.animating(now), "no animation by default");
    let braille = buffer
        .content()
        .iter()
        .filter(|c| {
            c.symbol()
                .chars()
                .next()
                .is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
        })
        .count();
    assert!(braille > 300, "{braille}");
    // 点中间：换算到徽章上接近原点；点右边缘接近 1。
    let stage = logo.stage.unwrap();
    let center = to_badge(stage, stage.x + stage.width / 2, stage.y + stage.height / 2);
    assert!(center[0].abs() < 0.1 && center[1].abs() < 0.2, "{center:?}");
    assert!(to_badge(stage, stage.right() - 1, stage.y + stage.height / 2)[0] > 0.9);
    logo.press(stage.right() - 1, stage.y + 2, now);
    assert!(logo.animating(now));
    let later = now + Duration::from_secs_f64(DURATION + 0.1);
    logo.draw(&mut buffer, area, &palette, later);
    assert!(!logo.animating(later));
    // 放不下：不画，点击也不算。
    let small = Rect::new(0, 0, 30, 8);
    logo.draw(&mut Buffer::empty(small), small, &palette, later);
    logo.press(5, 5, later);
    assert!(logo.stage.is_none() && !logo.animating(later));
}
