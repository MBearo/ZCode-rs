//! 请求的行（工作线 TK 的阶段 S，docs/specs/rust-cli/tui-kit.md §4.2「权限询问、提问、计划审批、同伴消息」、§4.0「板」）：
//! 旧界面的询问、计划、提问、同伴消息与切到完全访问的确认照 Puffin 的请求画（docs/puffin-tui.md §4.2
//! 「请求」的「预览」「板」，壳里的 `shell/asks/words.rs` 与 `shell/composer.rs`）：
//!
//! - **行下面**（预览）：底色上，第 3 列是 line 色的 `│`，文字从第 5 列起，按回答的折行宽度（`min(宽度 − 2, 99)`）折行；
//! - **板上的行**：头一行与回答（序号 fg3、两个空格、名字 fg2 补齐到最长名字之后再空三格、说明 fg3；选中的序号与名字
//!   brand，说明 fg2），最后是输入行（` › ` 与光标格、灰的占位文字）。板只有一块，由 `pane.rs` 连同栈与草稿行一起画；
//! - **槽**：按键提示在输入行（没有输入行时是草稿行）的右沿（第 `宽度 − 1` 列，不含）；放不下时不画（Puffin `on_row`）。
//!   以前每块请求自己画一块板，替换整个底部区域，放不下的槽落到板下面一行。
//!
//! 只用 kit 的 `Element` 与 `Layout::lay`，所以同样的内容与 Puffin 的壳逐格相同（同画面测试在
//! `crates/cli/tests/same_picture_s4.rs`）。壳的这几个函数对着壳的类型写，旧界面不能依赖它（frontend 不依赖 frontend），
//! 这里是同一规则的重写。
use rust_cli_tui_kit::app::element::{Element, Wrap, measure, single_line, wrap_styled};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::key_hints;
use rust_cli_tui_kit::markdown::render::INDENT;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

/// 正文的折行宽度：`min(宽度 − 2, 99)`（Puffin 的 `consts::MEASURE_MAX` / `MEASURE_MARGIN`）。
pub const MEASURE_MAX: usize = 99;
pub const MEASURE_MARGIN: usize = 2;

/// 把一个元素按 `width` 铺成行。
pub fn lay(element: &Element, width: usize) -> Vec<StyledLine> {
    Layout::lay(element, width)
}

/// `   │ `：预览的轨。
fn rail(theme: &Theme) -> Vec<Span> {
    vec![
        Span::plain(INDENT),
        Span::new("│", theme.line),
        Span::plain(" "),
    ]
}

/// 轨上的一段文字（Puffin `words::rail_line`）：空行只有轨；tab 写成四个空格。
pub fn rail_line(theme: &Theme, text: &str, style: Style, mode: Wrap) -> Element {
    if text.is_empty() {
        return single_line(vec![Span::plain(INDENT), Span::new("│", theme.line)], false);
    }
    rail_wrapped(
        theme,
        vec![Span::new(text.replace('\t', "    "), style)],
        mode,
    )
}

/// 轨上折行的几段（同一规则，段带各自的墨）。
pub fn rail_wrapped(theme: &Theme, spans: Vec<Span>, mode: Wrap) -> Element {
    measure(
        wrap_styled(rail(theme), rail(theme), spans, mode),
        MEASURE_MAX,
        MEASURE_MARGIN,
    )
}

/// 槽的段（键 fg2、说明 fg3、两对之间两个空格；Puffin 的 `key_hints::spans`）。
pub fn slot(theme: &Theme, keys: &[(&str, &str)]) -> Vec<Span> {
    let keys: Vec<(String, String)> = keys
        .iter()
        .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
        .collect();
    key_hints::spans(theme, &keys)
}

/// 同 [`slot`]，键与说明是自己的字符串（请求的槽，Puffin `ask_slot` 的对）。
pub fn slot_pairs(theme: &Theme, keys: &[(String, String)]) -> Vec<Span> {
    key_hints::spans(theme, keys)
}

/// 排好的一块，还没有放上板：`lines` 是预览、（有预览时）空一行、板上的行与输入行。板只有一块，由 `pane.rs` 连同栈与草稿行
/// 一起画（docs/specs/rust-cli/tui-kit.md §4.0「板」）：`board_at` 起的行在板上，之前的在板的上面。输入行有时它代替草稿行，槽
/// 放在它的最后一行；没有输入行时槽在草稿行上。以前每块请求自己画一块板，替换整个底部区域。
#[derive(Default)]
pub struct Laid {
    pub lines: Vec<StyledLine>,
    pub board_at: usize,
    pub input_at: usize,
    /// 输入行的行数（0：没有输入行）。
    pub input_rows: usize,
    pub slot: Vec<Span>,
    /// 回答在真的输入框里打（Puffin `composer` 里请求的回答输入，tui-kit.md §4.0「板」）：有它时草稿行照常画、有光标，
    /// 草稿空着时占位写这一句，槽是请求的键（`slot`）；请求的行在板的顶上，栈在它们下面。没有时（现在的请求都是）草稿行
    /// 不画光标，或由请求自己的输入行代替。
    pub answer: Option<String>,
}

/// 一组元素铺成的行。
pub fn lay_all(elements: &[Element], width: usize) -> Vec<StyledLine> {
    elements.iter().flat_map(|e| lay(e, width)).collect()
}
