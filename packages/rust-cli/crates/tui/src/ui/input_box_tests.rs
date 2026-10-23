//! 查看子代理时的输入框（docs/specs/rust-cli/tui-panels.md §1.6，修复线 F62，用户决定 2026-10-22）：上下边线是子代理色，
//! 上边线右侧用同色底色标出描述；占位文字写 `@类型`，并写明只读。回到主会话后恢复原样。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::panels::SubagentStatus;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;
use unicode_width::UnicodeWidthStr;

use super::*;
use crate::ui::agents::Viewing;
use crate::ui::palette::{ColorLevel, Palette, Rgb};
use crate::view::Transcript;

const DARK: Rgb = (0, 0, 0);
const LIGHT: Rgb = (255, 255, 255);
const CYAN: Color = Color::Rgb(8, 145, 178);
const DESCRIPTION: &str = "F58 differing duplicate values";

fn transcript() -> Transcript {
    Transcript::new(
        selection(),
        PermissionMode::Default,
        crate::view::test_workspace(),
    )
}

/// 主会话有标题 `Fix lint` 的 App。
fn app(level: ColorLevel, background: Rgb, lang: Lang) -> App {
    let mut app = App::new(
        Palette::new(level, Some(background)),
        lang,
        transcript(),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.view.apply(
        SequencedEvent {
            seq: 1,
            event: KernelEvent::TitleChanged {
                title: "Fix lint".into(),
            },
        },
        0,
    );
    app
}

fn view(app: &mut App, kind: &str, description: &str) {
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: kind.into(),
        description: description.into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view: transcript(),
    });
}

fn lines(app: &App, width: usize) -> Vec<Line<'static>> {
    input_box(app, width, 3).0
}

fn plain(line: &Line<'_>) -> String {
    crate::ui::text::plain(line)
}

/// 一行里每个 span 的（文字，样式）。
fn spans(line: &Line<'static>) -> Vec<(String, Style)> {
    line.spans
        .iter()
        .map(|span| (span.content.to_string(), span.style))
        .collect()
}

#[test]
fn viewing_paints_the_box_in_the_subagent_color() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    view(&mut app, "general-purpose", DESCRIPTION);
    let lines = lines(&app, 80);
    // 上边线：`─` 补满，右侧 ` <描述> ` 是子代理色的底色、黑字（深色背景），后面 1 个 `─`；会话标题不显示。
    let top = plain(&lines[0]);
    let label = format!(" {DESCRIPTION} ");
    assert_eq!(
        top,
        format!("{}{label}─", "─".repeat(80 - label.width() - 1))
    );
    assert!(!top.contains("Fix lint"), "{top}");
    let rule = Style::default().fg(CYAN);
    assert_eq!(
        spans(&lines[0]),
        [
            ("─".repeat(80 - label.width() - 1), rule),
            (label, Style::default().fg(Color::Rgb(0, 0, 0)).bg(CYAN)),
            ("─".to_owned(), rule),
        ]
    );
    // 占位文字写 `@类型`，并写明只读（查看时不能给子代理发消息，D17）。
    assert_eq!(
        plain(&lines[1]),
        "❯ Viewing @general-purpose (read-only) · esc to return"
    );
    // 下边线整条是子代理色。
    assert_eq!(spans(&lines[2]), [("─".repeat(80), rule)]);
}

#[test]
fn the_label_text_is_white_on_a_light_background() {
    let mut app = app(ColorLevel::TrueColor, LIGHT, Lang::En);
    view(&mut app, "general-purpose", DESCRIPTION);
    let lines = lines(&app, 80);
    let label = &lines[0].spans[1];
    assert_eq!(label.content, format!(" {DESCRIPTION} "));
    assert_eq!(
        label.style,
        Style::default().fg(Color::Rgb(255, 255, 255)).bg(CYAN)
    );
    assert_eq!(lines[0].spans[0].style, Style::default().fg(CYAN));
    assert_eq!(lines[2].spans[0].style, Style::default().fg(CYAN));
}

#[test]
fn chinese_says_the_view_is_read_only() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::Zh);
    view(&mut app, "Explore", "找调用方");
    let lines = lines(&app, 40);
    assert_eq!(plain(&lines[1]), "❯ 正在查看 @Explore（只读）· esc 返回");
    let top = plain(&lines[0]);
    assert!(top.ends_with(" 找调用方 ─"), "{top}");
    assert_eq!(top.width(), 40);
}

#[test]
fn sixteen_colors_use_the_named_cyan() {
    let mut app = app(ColorLevel::Ansi16, DARK, Lang::En);
    view(&mut app, "general-purpose", DESCRIPTION);
    let lines = lines(&app, 80);
    assert_eq!(
        lines[0].spans[0].style,
        Style::default().fg(Color::LightCyan)
    );
    assert_eq!(
        lines[0].spans[1].style,
        Style::default().fg(Color::Black).bg(Color::LightCyan)
    );
    assert_eq!(
        lines[2].spans[0].style,
        Style::default().fg(Color::LightCyan)
    );
}

/// 窄终端：描述截到宽度 − 3 列（`…` 结尾）；不到 4 列时只画边线。类型截到 20 列。
#[test]
fn narrow_terminals_truncate_the_label() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    view(&mut app, "general-purpose", DESCRIPTION);
    let top = |app: &App, width: usize| plain(&lines(app, width)[0]);
    assert_eq!(top(&app, 20), " F58 differing du… ─");
    assert_eq!(top(&app, 4), " … ─");
    assert_eq!(top(&app, 3), "───");
    assert_eq!(top(&app, 3).width(), 3);
    for width in [20, 33, 34, 35, 50] {
        assert_eq!(top(&app, width).width(), width, "{width}");
    }
    // 正好放得下：左边没有 `─`；少 1 列就截断。
    assert_eq!(top(&app, 33), format!(" {DESCRIPTION} ─"));
    assert_eq!(top(&app, 32), " F58 differing duplicate valu… ─");
    // 类型很长：占位文字里截到 20 列；类型为空时写 `agent`。
    view(&mut app, "a-very-long-agent-type-name", DESCRIPTION);
    assert_eq!(
        plain(&lines(&app, 120)[1]),
        "❯ Viewing @a-very-long-agent-t… (read-only) · esc to return"
    );
    view(&mut app, "", DESCRIPTION);
    assert!(plain(&lines(&app, 120)[1]).starts_with("❯ Viewing @agent (read-only)"));
}

/// 描述为空：没有标签，只画子代理色的边线；换行与连续空白压成一个空格。
#[test]
fn an_empty_description_leaves_a_plain_colored_rule() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    view(&mut app, "Explore", "  ");
    let lines_now = lines(&app, 30);
    assert_eq!(
        spans(&lines_now[0]),
        [("─".repeat(30), Style::default().fg(CYAN))]
    );
    view(&mut app, "Explore", "two\n  lines");
    assert!(plain(&lines(&app, 30)[0]).ends_with(" two lines ─"));
}

/// bash 模式的颜色让位；提示符照旧。
#[test]
fn the_subagent_color_wins_over_bash_mode() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    app.composer.mode = crate::ui::composer::InputMode::Bash;
    view(&mut app, "Explore", "Find callers");
    let lines = lines(&app, 40);
    assert_eq!(lines[0].spans[0].style, Style::default().fg(CYAN));
    assert_eq!(lines[2].spans[0].style, Style::default().fg(CYAN));
}

/// 回到主会话后边线、会话标题与占位文字恢复原样（都按 `viewing` 现算）。
#[test]
fn returning_to_main_restores_the_box() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    let before = lines(&app, 80);
    assert!(plain(&before[0]).ends_with(" Fix lint ─"));
    assert!(
        before[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
    view(&mut app, "Explore", "Find callers");
    assert_ne!(lines(&app, 80), before);
    app.viewing = None;
    assert_eq!(lines(&app, 80), before);
}
