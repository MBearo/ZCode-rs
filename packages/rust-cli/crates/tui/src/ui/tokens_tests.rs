//! 配色记号（tui.md §7.4，修复线 F26）的取值：深浅两种背景、知道 / 不知道前景、256 色与 16 色。
use ratatui::style::{Color, Modifier, Style};

use super::*;

fn palette(level: ColorLevel, background: Option<Rgb>, foreground: Option<Rgb>) -> Palette {
    Palette::new(level, background).with_foreground(foreground)
}

const DARK: Option<Rgb> = Some((0, 0, 0));
const LIGHT: Option<Rgb> = Some((255, 255, 255));
const WHITE: Option<Rgb> = Some((255, 255, 255));
const BLACK: Option<Rgb> = Some((0, 0, 0));

#[test]
fn secondary_text_blends_the_foreground_into_the_background() {
    // Codex：白字黑底 → (153,153,153)，黑字白底 → (102,102,102)。
    let dark = palette(ColorLevel::TrueColor, DARK, WHITE).secondary();
    assert_eq!(dark.fg, Some(Color::Rgb(153, 153, 153)));
    assert!(!dark.add_modifier.contains(Modifier::DIM));
    let light = palette(ColorLevel::TrueColor, LIGHT, BLACK).secondary();
    assert_eq!(light.fg, Some(Color::Rgb(102, 102, 102)));
    // 前景不知道、前景与背景同样明暗（换了主题）、16 色：dim。
    for p in [
        palette(ColorLevel::TrueColor, DARK, None),
        palette(ColorLevel::TrueColor, DARK, BLACK),
        palette(ColorLevel::Ansi16, DARK, WHITE),
        palette(ColorLevel::TrueColor, None, WHITE),
    ] {
        assert_eq!(p.secondary(), Style::default().add_modifier(Modifier::DIM));
    }
    assert!(matches!(
        palette(ColorLevel::Ansi256, DARK, WHITE).secondary().fg,
        Some(Color::Indexed(_))
    ));
}

#[test]
fn keys_are_bold_and_softened_on_light_backgrounds() {
    let dark = palette(ColorLevel::TrueColor, DARK, WHITE).key();
    assert_eq!(
        dark,
        Style::default()
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM)
    );
    let light = palette(ColorLevel::TrueColor, LIGHT, BLACK).key();
    assert_eq!(light.fg, Some(Color::Rgb(38, 38, 38)));
    assert!(light.add_modifier.contains(Modifier::BOLD));
    assert!(light.sub_modifier.contains(Modifier::DIM));
    // 不知道前景：只有粗体。
    assert_eq!(palette(ColorLevel::TrueColor, LIGHT, None).key().fg, None);
}

#[test]
fn the_selection_is_a_blue_bar_with_dark_bold_text() {
    let dark = palette(ColorLevel::TrueColor, DARK, None).selection();
    assert_eq!(dark.bg, Some(Color::Rgb(99, 168, 248)));
    assert_eq!(dark.fg, Some(Color::Rgb(0, 0, 46)));
    assert!(dark.add_modifier.contains(Modifier::BOLD));
    let light = palette(ColorLevel::TrueColor, LIGHT, None).selection();
    assert_eq!(light.bg, Some(Color::Rgb(164, 205, 251)));
    // 蓝色的背景：底色换成对比度大的那种。
    let blue = palette(ColorLevel::TrueColor, Some((95, 160, 240)), None).selection();
    assert_eq!(blue.bg, Some(Color::Rgb(164, 205, 251)));
    // 256 色取最近的一格；16 色与背景不知道时反色。
    assert_eq!(
        palette(ColorLevel::Ansi256, DARK, None).selection().bg,
        Some(Color::Indexed(75))
    );
    let reversed = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
    assert_eq!(
        palette(ColorLevel::Ansi16, DARK, None).selection(),
        reversed
    );
    assert_eq!(
        palette(ColorLevel::TrueColor, None, None).selection(),
        reversed
    );
}

#[test]
fn model_and_path_follow_the_codex_status_line() {
    let dark = palette(ColorLevel::TrueColor, Some((30, 30, 46)), None);
    assert_eq!(dark.model().fg, Some(Color::Rgb(246, 226, 183)));
    assert_eq!(dark.path().fg, Some(Color::Rgb(171, 223, 167)));
    let light = palette(ColorLevel::TrueColor, Some((239, 241, 245)), None);
    // 浅色背景上调到 4.5:1。
    for style in [light.model(), light.path()] {
        let Some(Color::Rgb(r, g, b)) = style.fg else {
            panic!("{style:?}");
        };
        assert!(ratio((r, g, b), (239, 241, 245)) >= 4.5, "{style:?}");
    }
    assert_eq!(
        palette(ColorLevel::Ansi16, DARK, None).model().fg,
        Some(Color::Cyan)
    );
    assert_eq!(
        palette(ColorLevel::Ansi16, DARK, None).path().fg,
        Some(Color::Green)
    );
}

#[test]
fn readable_keeps_good_colors_and_pushes_bad_ones() {
    assert_eq!(readable((255, 255, 255), (0, 0, 0)), (255, 255, 255));
    let pushed = readable((200, 200, 200), (255, 255, 255));
    assert!(ratio(pushed, (255, 255, 255)) >= 4.5, "{pushed:?}");
}

#[test]
fn the_context_notice_is_red_when_low_and_secondary_otherwise() {
    let p = palette(ColorLevel::TrueColor, DARK, WHITE);
    assert_eq!(p.context_notice(true).fg, Some(CONTEXT_LOW));
    assert_eq!(p.context_notice(false), p.secondary());
    // 同一套规则给别的状态栏颜色（F27 的 footer 统计复用）。
    assert_eq!(
        p.status_line(MODEL_DARK, MODEL_LIGHT, MODEL_NAMED),
        p.model()
    );
    assert_eq!(p.status_line(PATH_DARK, PATH_LIGHT, PATH_NAMED), p.path());
}

/// 修复线 F62（tui-panels.md §1.6、tui.md §7.4）：查看子代理时输入框的边线与标签的底色在深浅背景上都是
/// (8,145,178)，标签的字深色背景黑、浅色背景白；16 色深色背景亮青、浅色背景青。背景不知道时按深色。
#[test]
fn subagent_tokens_in_both_themes() {
    let cyan = Color::Rgb(8, 145, 178);
    for (background, text) in [
        (DARK, Color::Rgb(0, 0, 0)),
        (None, Color::Rgb(0, 0, 0)),
        (LIGHT, Color::Rgb(255, 255, 255)),
    ] {
        let p = palette(ColorLevel::TrueColor, background, None);
        assert_eq!(p.subagent_rule(), Style::default().fg(cyan));
        assert_eq!(p.subagent_label(), Style::default().fg(text).bg(cyan));
    }
    // 256 色：最近的一格（(0,135,175)、黑 16、白 231）。
    let p = palette(ColorLevel::Ansi256, DARK, None);
    assert_eq!(p.subagent_rule().fg, Some(Color::Indexed(31)));
    assert_eq!(
        p.subagent_label(),
        Style::default()
            .fg(Color::Indexed(16))
            .bg(Color::Indexed(31))
    );
    let p = palette(ColorLevel::Ansi256, LIGHT, None);
    assert_eq!(p.subagent_label().fg, Some(Color::Indexed(231)));
    // 16 色。
    let p = palette(ColorLevel::Ansi16, DARK, None);
    assert_eq!(p.subagent_rule(), Style::default().fg(Color::LightCyan));
    assert_eq!(
        p.subagent_label(),
        Style::default().fg(Color::Black).bg(Color::LightCyan)
    );
    let p = palette(ColorLevel::Ansi16, LIGHT, None);
    assert_eq!(p.subagent_rule(), Style::default().fg(Color::Cyan));
    assert_eq!(
        p.subagent_label(),
        Style::default().fg(Color::White).bg(Color::Cyan)
    );
}

/// 工作线 F74（tui.md §7.6）：成功色深浅背景各一种绿，16 色按背景取亮绿或绿，背景不知道时按深色。
#[test]
fn success_is_green_in_both_themes() {
    let p = palette(ColorLevel::TrueColor, DARK, None);
    assert_eq!(p.success(), Style::default().fg(Color::Rgb(78, 186, 101)));
    let p = palette(ColorLevel::TrueColor, LIGHT, None);
    assert_eq!(p.success(), Style::default().fg(Color::Rgb(44, 122, 57)));
    let p = Palette::new(ColorLevel::TrueColor, None);
    assert_eq!(p.success(), Style::default().fg(Color::Rgb(78, 186, 101)));
    let p = palette(ColorLevel::Ansi16, DARK, None);
    assert_eq!(p.success(), Style::default().fg(Color::LightGreen));
    let p = palette(ColorLevel::Ansi16, LIGHT, None);
    assert_eq!(p.success(), Style::default().fg(Color::Green));
}
