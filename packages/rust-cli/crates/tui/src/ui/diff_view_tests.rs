//! `/diff` 的 git 输出解析（tui-manage.md §4.1）。
use super::*;

const PATCH: &str = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n one\n-two\n+2\n+three\ndiff --git a/b.bin b/b.bin\nBinary files a/b.bin and b/b.bin differ";

#[test]
fn numstat_patch_and_untracked_files_make_the_list() {
    let Current::Loaded { files, stats } = parse("2\t1\ta.txt\n-\t-\tb.bin", PATCH, "new.txt\n")
    else {
        panic!("not loaded");
    };
    assert_eq!(stats, (3, 2, 1));
    let names: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["a.txt", "b.bin", "new.txt"]);
    assert_eq!(
        files[0].body,
        ["@@ -1,2 +1,3 @@", " one", "-two", "+2", "+three"]
    );
    assert!(files[1].binary && files[1].body.is_empty());
    assert!(files[2].untracked);
}

#[test]
fn limits_cap_files_lines_and_size() {
    let numstat: String = (0..60).map(|i| format!("1\t0\tf{i}.rs\n")).collect();
    let Current::Loaded { files, stats } = parse(&numstat, "", "") else {
        panic!("not loaded");
    };
    assert_eq!(files.len(), 50);
    assert_eq!(stats, (60, 60, 0));
    let long: String = std::iter::once("diff --git a/x b/x\n@@ -0,0 +1,500 @@\n".to_owned())
        .chain((0..500).map(|i| format!("+line {i}\n")))
        .collect();
    let Current::Loaded { files, .. } = parse("500\t0\tx", &long, "") else {
        panic!("not loaded");
    };
    assert_eq!(files[0].body.len(), 400);
    let huge = format!(
        "diff --git a/x b/x\n@@ -0,0 +1 @@\n+{}\n",
        "y".repeat(1_100_000)
    );
    let Current::Loaded { files, .. } = parse("1\t0\tx", &huge, "") else {
        panic!("not loaded");
    };
    assert!(files[0].body.is_empty());
}

/// 列表照 Puffin 的选择器画（tui-kit.md §4.3）：没有 `›` 指针与选中条，选中的文件名用 `pick::chosen`、其余 fg2，
/// 提示从第 3 列起在层的下面。
#[test]
fn the_list_has_the_picker_look() {
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::testing::selection;

    use crate::ui::pick;
    use crate::ui::probe::{ColorLevel, Colors};
    use crate::view::Transcript;

    let app = App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    );
    let view = DiffView {
        current: parse("2\t1\ta.txt\n-\t-\tb.bin", PATCH, "new.txt\n"),
        ..DiffView::new()
    };
    let (lines, _) = super::super::diff_view_lines::lines(&app, &view, 80, 20, 0);
    let text: Vec<String> = lines
        .iter()
        .map(rust_cli_tui_kit::render::StyledLine::plain)
        .collect();
    assert!(!text.iter().any(|line| line.contains('›')), "{text:#?}");
    let style_of = |name: &str| {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.text == name)
            .map(|span| span.style)
    };
    assert_eq!(style_of("a.txt"), Some(pick::chosen(&app.ink)), "{text:#?}");
    assert_eq!(style_of("b.bin"), Some(app.ink.theme.fg2), "{text:#?}");
    let hint = text
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("the hint");
    assert!(
        hint.starts_with("   ") && !hint.starts_with("    "),
        "{text:#?}"
    );
}
