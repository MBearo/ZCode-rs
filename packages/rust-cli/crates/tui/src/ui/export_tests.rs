//! `/export` 的缺省文件名与文字（tui-manage.md §3.1）。
use super::*;

/// 2026-10-18 09:05:07 UTC。
const AT: u64 = 1_792_314_307_000;

#[test]
fn the_default_name_is_the_time_and_a_slug() {
    assert_eq!(timestamp(AT, 0), "2026-10-18-090507");
    assert_eq!(
        default_name(Some("  Fix the README, please!  "), AT, 0),
        "2026-10-18-090507-fix-the-readme-please.txt"
    );
    assert_eq!(
        default_name(Some("a -- b\tc"), AT, 0),
        "2026-10-18-090507-a-b-c.txt"
    );
    // 超过 50 个字符：取前 49 个，省略号被 slug 去掉。
    let long = "word ".repeat(20);
    let name = default_name(Some(&long), AT, 0);
    assert_eq!(
        name.len(),
        "2026-10-18-090507-".len() + 49 + ".txt".len(),
        "{name}"
    );
    assert_eq!(
        default_name(Some("中文 输入"), AT, 0),
        "conversation-2026-10-18-090507.txt"
    );
    assert_eq!(
        default_name(None, AT, 0),
        "conversation-2026-10-18-090507.txt"
    );
}

/// 修复（R2-6）：以前缺省文件名按 UTC（tui-manage.md D9）；现在用本地时间，偏移取入口注入的时钟。
#[test]
fn the_default_name_uses_the_local_time() {
    assert_eq!(timestamp(AT, 8 * 60), "2026-10-18-170507");
    assert_eq!(timestamp(AT, -10 * 60), "2026-10-17-230507");
    assert_eq!(
        default_name(Some("fix it"), AT, 8 * 60),
        "2026-10-18-170507-fix-it.txt"
    );
}

#[test]
fn the_dialog_shows_the_two_methods_and_the_filename_input() {
    let palette = crate::ui::palette::Palette::new(
        crate::ui::palette::ColorLevel::TrueColor,
        Some((0, 0, 0)),
    );
    let text = |view: &ExportView| {
        lines(view, Lang::En, &palette, 100)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let choose = text(&ExportView::Choose { selected: 1 });
    assert!(choose.contains("Export conversation"), "{choose}");
    assert!(choose.contains("Select export method"));
    assert!(
        choose.contains("1. Copy to clipboard   Copy the conversation to your system clipboard")
    );
    assert!(choose.contains("› 2. Save to file"), "{choose}");
    let name = text(&ExportView::Name("chat.txt".to_owned()));
    assert!(name.contains("Enter filename:"));
    assert!(name.contains("> chat.txt█"), "{name}");
    assert!(name.contains("Enter to save · Esc to go back"));
}
