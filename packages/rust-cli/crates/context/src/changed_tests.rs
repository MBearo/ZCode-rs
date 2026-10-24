//! 文件被改动的提醒文字。

use rust_cli_kernel_api::tool::ChangedFile;

use super::render;

const NOTE: &str = "changed on disk since you last read it. That's usually deliberate, so take it as the current state rather than reverting it; if the change looks wrong, say so rather than undoing it yourself \u{2014} otherwise no need to call it out.";

#[test]
fn a_change_with_lines_shows_them() {
    let (text, data) = render(&ChangedFile {
        path: "/w/a.txt".to_owned(),
        snippet: "1\ta\n2\tB".to_owned(),
    });
    assert_eq!(
        text,
        format!(
            "Note: /w/a.txt {NOTE} Here are the relevant changes (shown with line numbers):\n1\ta\n2\tB"
        )
    );
    assert_eq!(data["filename"], "/w/a.txt");
}

#[test]
fn a_change_without_lines_points_to_read() {
    let (text, _) = render(&ChangedFile {
        path: "/w/.env".to_owned(),
        snippet: String::new(),
    });
    assert_eq!(
        text,
        format!(
            "Note: /w/.env {NOTE} The changes are not shown here; use Read if you need the current content."
        )
    );
}

#[test]
fn the_path_is_escaped() {
    let (text, _) = render(&ChangedFile {
        path: "/w/<a>\u{7}\u{2028}.txt".to_owned(),
        snippet: String::new(),
    });
    assert!(
        text.starts_with("Note: /w/&lt;a&gt;&#7;&#8232;.txt changed"),
        "{text}"
    );
}
