//! transcript 的第一个条目（docs/specs/rust-cli/tui.md §7.3，用户决定的 Colophon；docs/specs/rust-cli/tui-kit.md §4.4）：欢迎块
//! （名字与版本——有新版本时行尾接 `vX is out · zcode update`——、文件夹，然后是有话要说时的第 4、5 行，或恢复的会话的标题）。
//! 入参是朴素的数据：版本、写短的文件夹、两行的段。
use crate::app::element::{Element, beside, br, single_line, stack, text};
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

/// 欢迎块的文字从这一列起（从 0 数）。
pub const TEXT_COL: usize = 2;

/// 刚接好或刚更新过时的第 4 行：`✓ 说的话`。
pub fn done_row(theme: &Theme, says: String) -> Vec<Span> {
    vec![
        Span::new("✓", Style::fg(theme.ok)),
        Span::new(" ", Style::DEFAULT),
        Span::new(says, theme.fg2),
    ]
}

/// 欢迎块：一行空行，名字与版本（`newer` 是更新的版本时行尾接提示，行宽不够时先被截掉）、文件夹（保留结尾），一行空行；
/// 第 4、5 行有内容时接在下面，再空一行。
pub fn block(
    theme: &Theme,
    version: &str,
    folder: &str,
    row4: &[Span],
    row5: &[Span],
    newer: Option<&str>,
) -> Element {
    block_said(theme, version, folder, row4, row5, newer, is_out)
}

/// Puffin 的更新提示：`   X is out · `（后面接 brand 的 `zcode update`）。
pub fn is_out(version: &str) -> String {
    format!("   {version} is out · ")
}

/// [`block`]，更新提示的话是 `is_out` 的（别的界面语言）；命令照旧是 `zcode update`。
pub fn block_said(
    theme: &Theme,
    version: &str,
    folder: &str,
    row4: &[Span],
    row5: &[Span],
    newer: Option<&str>,
    is_out: fn(&str) -> String,
) -> Element {
    let mut lines = name_rows(theme, version, folder, newer, is_out);
    if !row4.is_empty() {
        lines.push(br());
        lines.push(single_line(row4.to_vec(), false));
    }
    if !row5.is_empty() {
        lines.push(single_line(row5.to_vec(), false));
    }
    framed(lines)
}

/// 恢复的会话的欢迎块（tui.md §7.3）：文件夹下面紧接粗体的标题与 fg3 的 `started …`（`started` 是写好的时刻）。
pub fn resumed_said(
    theme: &Theme,
    version: &str,
    folder: &str,
    title: &str,
    started: &str,
    newer: Option<&str>,
    is_out: fn(&str) -> String,
) -> Element {
    let mut lines = name_rows(theme, version, folder, newer, is_out);
    lines.push(single_line(
        vec![
            Span::new(title.to_owned(), Style::BOLD),
            Span::new("  ", Style::DEFAULT),
            Span::new(format!("started {started}"), theme.fg3),
        ],
        false,
    ));
    framed(lines)
}

/// 名字与版本一行、文件夹一行。每行都是单行、按宽度截断：比终端宽的行会被裁掉，长 cwd 保留结尾。
fn name_rows(
    theme: &Theme,
    version: &str,
    folder: &str,
    newer: Option<&str>,
    is_out: fn(&str) -> String,
) -> Vec<Element> {
    // plain 主题里 brand 只是 ANSI 蓝，再加粗才看得出来。
    let brand = if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::fg(theme.brand).with_attr(attr::BOLD)
    };
    let mut version = vec![
        Span::new("Z", Style::fg(theme.brand).with_attr(attr::BOLD)),
        Span::new("Code", Style::BOLD),
        Span::new(format!("  {version}"), theme.fg3),
    ];
    if let Some(v) = newer {
        version.push(Span::new(is_out(v), theme.fg2));
        version.push(Span::new("zcode update", brand));
    }
    vec![
        single_line(version, false),
        single_line(vec![Span::new(folder.to_owned(), theme.fg2)], true),
    ]
}

/// 上下各一行空行，文字从 [`TEXT_COL`] 起。
fn framed(lines: Vec<Element>) -> Element {
    stack([br(), beside(text(" "), TEXT_COL, stack(lines)), br()])
}
