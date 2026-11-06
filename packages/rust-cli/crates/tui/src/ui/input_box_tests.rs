//! 输入框照 Puffin 画（docs/specs/rust-cli/tui-kit.md §4.2「输入框」、§4.3）：板、`›`、光标格、占位文字、命令名与参数提示、
//! 粘贴的占位，以及板上面那一行（会话标题、`History k/n`、查看子代理时的描述，tui-panels.md §1.6）。
use rust_cli_kernel_api::ids::AgentId;
use rust_cli_kernel_api::panels::SubagentStatus;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Style, StyledLine, attr};

use super::*;
use crate::ui::agents::Viewing;
use crate::ui::probe::{ColorLevel, Colors, Rgb};
use crate::view::Transcript;

const DARK: Rgb = (0, 0, 0);
const LIGHT: Rgb = (255, 255, 255);
const DESCRIPTION: &str = "F58 differing duplicate values";

fn transcript() -> Transcript {
    Transcript::new(
        selection(),
        PermissionMode::Default,
        crate::view::test_workspace(),
    )
}

/// 主会话有标题 `Fix lint` 的 App。前景取与背景相反的黑白（真彩时是 rich 的主题，16 色是 plain，tui-kit.md §3.4）。
fn app(level: ColorLevel, background: Rgb, lang: Lang) -> App {
    let foreground = if background == DARK { LIGHT } else { DARK };
    let mut app = App::new(
        Colors::new(level, Some(background)).with_foreground(Some(foreground)),
        lang,
        transcript(),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    app.view.apply(
        SequencedEvent {
            seq: 1,
            revision: 0,
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

/// 板（上边、输入行、下边）排成的行。
fn board_lines(app: &App, width: usize) -> Vec<StyledLine> {
    let input = input_rows(app, width, 3, Vec::new(), true, None);
    Layout::lay(&board(app, input.rows), width)
}

/// 板上面那一行（没有计数）。
fn caption_line(app: &App, width: usize) -> StyledLine {
    app.title_row(width, std::time::Instant::now(), title(app), false)
        .map(|row| row.line)
        .unwrap_or_default()
}

fn plain(line: &StyledLine) -> String {
    line.plain().trim_end().to_owned()
}

/// 某段文字所在的那一段的样式。
fn style_of(line: &StyledLine, text: &str) -> Style {
    line.spans
        .iter()
        .find(|s| s.text == text)
        .unwrap_or_else(|| panic!("{text:?} in {line:?}"))
        .style
}

/// rich：板是 `panel` 的块，上下边是 `▄` / `▀`；plain：上下两条 `line` 色的细线。输入行是 brand 粗体的 `›`、带
/// `attr::CARET` 的光标格与 fg3 的占位文字（Puffin `composer` 的空草稿）。
#[test]
fn an_empty_draft_is_puffins_slab_with_the_caret_and_the_placeholder() {
    let rich = app(ColorLevel::TrueColor, DARK, Lang::En);
    assert!(rich.ink.theme.rich);
    let lines = board_lines(&rich, 60);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].plain(), "▄".repeat(60));
    assert_eq!(lines[2].plain(), "▀".repeat(60));
    assert_eq!(plain(&lines[1]), format!(" ›  {}", placeholder(&rich)));
    let theme = &rich.ink.theme;
    assert_eq!(style_of(&lines[1], "›").fg_color(), theme.brand);
    assert!(style_of(&lines[1], "›").has_attr(attr::BOLD));
    let caret = lines[1]
        .spans
        .iter()
        .find(|s| s.style.has_attr(attr::CARET));
    assert_eq!(caret.map(|s| s.text.as_ref()), Some(" "));
    assert_eq!(
        style_of(&lines[1], &placeholder(&rich)).fg_color(),
        theme.fg3.fg_color()
    );

    let flat = app(ColorLevel::Ansi16, DARK, Lang::En);
    assert!(!flat.ink.theme.rich);
    let lines = board_lines(&flat, 60);
    assert_eq!(lines[0].plain(), "─".repeat(60));
    assert_eq!(lines[2].plain(), "─".repeat(60));
    assert_eq!(lines[0].spans[0].style, flat.ink.theme.line);
}

/// 草稿：文字从第 3 列起，光标格是它下面的字（行末是一个空格），续行缩进 3 列。
#[test]
fn the_caret_cell_is_the_character_under_the_cursor() {
    let mut app = app(ColorLevel::Ansi16, DARK, Lang::En);
    app.composer.area.set_text("hello");
    let row = &board_lines(&app, 40)[1];
    assert_eq!(plain(row), " › hello");
    assert_eq!(row.spans.last().unwrap().text, " ");
    assert!(row.spans.last().unwrap().style.has_attr(attr::CARET));
    app.composer.area.set_cursor(1);
    let row = &board_lines(&app, 40)[1];
    let caret = row.spans.iter().find(|s| s.style.has_attr(attr::CARET));
    assert_eq!(caret.map(|s| s.text.as_ref()), Some("e"));
    app.composer.area.set_text("one\ntwo");
    let lines = board_lines(&app, 40);
    assert_eq!(plain(&lines[1]), " › one");
    assert_eq!(plain(&lines[2]), "   two");
}

/// 开头的命令名是 brand；只输入了 `/名字 ` 时，参数提示是 fg3 斜体，第一个字在光标格里（Puffin「草稿」的幽灵提示）。
#[test]
fn a_command_name_is_brand_and_its_argument_hint_is_a_ghost() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    app.composer.area.set_text("/theme ");
    let row = &board_lines(&app, 60)[1];
    // Puffin `consts::hint` 的词。
    assert_eq!(plain(row), " › /theme name");
    assert_eq!(style_of(row, "/theme").fg_color(), app.ink.theme.brand);
    let ghost = style_of(row, "n");
    assert!(ghost.has_attr(attr::CARET | attr::REVERSE | attr::ITALIC));
    assert!(style_of(row, "ame").has_attr(attr::ITALIC));
    // 不认识的名字照常是正文的墨。
    app.composer.area.set_text("/nope ");
    let row = &board_lines(&app, 60)[1];
    assert!(
        row.spans
            .iter()
            .all(|s| s.style.fg_color() != app.ink.theme.brand || s.text == "›")
    );
}

/// 板上面那一行：会话标题右对齐、fg3，最后一格在第 `宽度 − 2` 列；浏览输入历史时是 `History k/n`。
#[test]
fn the_caption_row_names_the_session_or_the_history_position() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    let line = caption_line(&app, 40);
    // 右沿是 kit 的 `edge`（宽度 − 1，不含），与槽对齐。
    assert_eq!(line.plain(), format!("{}Fix lint", " ".repeat(31)));
    assert_eq!(style_of(&line, "Fix lint"), app.ink.theme.fg3);
    for text in ["first", "second"] {
        app.composer.area.set_text(text);
        app.composer.discard();
    }
    assert!(app.composer.history_prev());
    let line = caption_line(&app, 40).plain();
    assert!(line.ends_with("History 1/2"), "{line}");
}

/// 查看子代理（tui-panels.md §1.6，tui-kit.md §4.3）：`›` 是子代理色，板上面那一行是子代理色的描述（取代会话标题），
/// 板的底色带一点子代理色，plain 时板的两条线是子代理色；占位文字写 `@类型` 与只读。回到主会话后恢复。
#[test]
fn viewing_a_subagent_paints_the_chevron_and_the_caption() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    let before = (caption_line(&app, 80), board_lines(&app, 80));
    view(&mut app, "general-purpose", DESCRIPTION);
    let lines = board_lines(&app, 80);
    assert_eq!(
        plain(&lines[1]),
        " ›  Viewing @general-purpose (read-only) · esc to return"
    );
    assert_eq!(style_of(&lines[1], "›").fg_color(), app.ink.theme.agent);
    let caption = caption_line(&app, 80);
    assert_eq!(
        caption.plain(),
        format!(
            "{}{DESCRIPTION}",
            " ".repeat(79 - string_width(DESCRIPTION))
        )
    );
    assert_eq!(style_of(&caption, DESCRIPTION), app.ink.subagent_ink());
    // 板（连 `▄` `▀` 边）换成带子代理色的底，输入框还是一个长方形。
    let tint = app.ink.subagent_ground();
    assert_ne!(tint, app.ink.theme.panel);
    assert_eq!(ground(&app), tint);
    assert!(lines.iter().all(|l| {
        l.spans
            .iter()
            .any(|s| s.style.bg_color() == tint.unwrap() || s.style.fg_color() == tint.unwrap())
    }));
    // 中文与很长的类型。
    let mut zh = self::app(ColorLevel::TrueColor, DARK, Lang::Zh);
    view(&mut zh, "Explore", "找调用方");
    assert_eq!(
        plain(&board_lines(&zh, 60)[1]),
        " ›  正在查看 @Explore（只读）· esc 返回"
    );
    view(&mut app, "a-very-long-agent-type-name", "  ");
    assert_eq!(
        plain(&board_lines(&app, 120)[1]),
        " ›  Viewing @a-very-long-agent-t… (read-only) · esc to return"
    );
    // 描述为空：没有标签。
    assert_eq!(caption_line(&app, 80).plain().trim(), "");
    app.viewing = None;
    assert_eq!((caption_line(&app, 80), board_lines(&app, 80)), before);
}

/// 16 色：板的两条线是子代理色；它压过 bash 模式。
#[test]
fn plain_rules_take_the_subagent_color_even_in_bash_mode() {
    let mut app = app(ColorLevel::Ansi16, DARK, Lang::En);
    app.composer.mode = crate::ui::composer::InputMode::Bash;
    let bash = board_lines(&app, 40);
    assert_eq!(style_of(&bash[1], "!").fg_color(), app.ink.theme.code);
    view(&mut app, "Explore", "Find callers");
    let lines = board_lines(&app, 40);
    assert_eq!(lines[0].spans[0].style, Style::fg(app.ink.theme.agent));
    assert_eq!(lines[2].spans[0].style, Style::fg(app.ink.theme.agent));
    assert_eq!(style_of(&lines[1], "›").fg_color(), app.ink.theme.agent);
}

/// 窄终端：板上面那一行截到第 3 列与右沿之间，结尾 `…`。
#[test]
fn narrow_terminals_truncate_the_caption() {
    let mut app = app(ColorLevel::TrueColor, DARK, Lang::En);
    view(&mut app, "general-purpose", DESCRIPTION);
    let line = caption_line(&app, 20).plain();
    assert_eq!(line, "   F58 differing d…");
    assert_eq!(string_width(&line), 19);
}

/// 请求的回答在输入框里打时（`Laid::answer`）：草稿空着的输入行写请求的一句、有光标格（tui-kit.md §4.0「板」）。
#[test]
fn an_answer_in_the_composer_takes_the_placeholder() {
    let app = app(ColorLevel::TrueColor, DARK, Lang::En);
    let input = input_rows(
        &app,
        60,
        3,
        Vec::new(),
        true,
        Some("Tell ZCode what to do instead"),
    );
    let line = Layout::lay(&input.rows[0], 60).remove(0);
    assert_eq!(plain(&line), " ›  Tell ZCode what to do instead");
    assert!(
        line.spans
            .iter()
            .any(|span| span.style.attrs() & attr::CARET != 0),
        "{line:?}"
    );
}
