//! `!` 命令的输出（tui-input.md §2.1「显示」）：收起时最新的 10 行，更早的在上面数出来；^O 时全部。
use crate::ui::cells::tests::ctx;
use crate::ui::lines::plain;
use crate::ui::probe::{ColorLevel, Colors};

use super::*;

fn colors() -> Colors {
    Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)))
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

#[test]
fn folded_output_keeps_the_newest_ten_lines() {
    let colors = colors();
    let lines: Vec<String> = output(&numbered(14), "", 0, &ctx(&colors, false))
        .iter()
        .map(plain)
        .collect();
    assert_eq!(lines.len(), 11, "{lines:#?}");
    assert_eq!(lines[0].trim_end(), "   │ … 4 earlier lines");
    assert_eq!(lines[1].trim_end(), "   │ line 5");
    assert_eq!(lines[10].trim_end(), "   │ line 14");
}

#[test]
fn stderr_follows_stdout_and_expanded_shows_all() {
    let colors = colors();
    let lines: Vec<String> = output(&numbered(10), "oops", 0, &ctx(&colors, false))
        .iter()
        .map(plain)
        .collect();
    assert_eq!(lines[0].trim_end(), "   │ … 1 earlier line");
    assert_eq!(lines.last().unwrap().trim_end(), "   │ oops");
    let all = output(&numbered(14), "", 0, &ctx(&colors, true));
    assert_eq!(all.len(), 14);
}

#[test]
fn long_lines_are_cut_not_wrapped() {
    let colors = colors();
    let long = "x".repeat(200);
    let lines = output(&long, "", 0, &ctx(&colors, false));
    assert_eq!(lines.len(), 1);
}
