use super::*;

#[test]
fn templates_have_their_placeholders() {
    assert!(prompt().ends_with("## Input\n\n{{input}}"));
    assert!(autonomous_fixed().contains("just the interval `{{interval}}`"));
    assert!(autonomous_fixed().ends_with("## Autonomous-loop instructions (for the immediate execution and every fire)\n\n{{preamble}}"));
    assert!(autonomous_dynamic().contains("the literal string `<<autonomous-loop-dynamic>>`"));
    assert!(file_dynamic().contains("`{{path}}`") && file_dynamic().ends_with("{{content}}"));
    assert!(tick_fixed().starts_with("# Autonomous loop tick\n\n"));
    assert!(preamble().starts_with("# Autonomous loop check\n") && preamble().ends_with(".\n"));
}

#[test]
fn loop_files_are_trimmed_and_truncated() {
    assert_eq!(loop_file("/p", " \n "), None);
    assert_eq!(
        loop_file("/p", "\n- task\n"),
        Some(LoopFile {
            path: "/p".to_owned(),
            content: "- task".to_owned()
        })
    );
    let long = format!("{}\n{}", "a".repeat(24_990), "b".repeat(100));
    let cut = loop_file("/p", &long).unwrap().content;
    assert_eq!(
        cut,
        format!(
            "{}\n\n> WARNING: loop.md was truncated to 25000 bytes. Keep the task list concise.",
            "a".repeat(24_990)
        )
    );
}

/// 先项目、再 home；拼法由调用方给（执行环境的 `PathStyle::join`，F58）。
#[test]
fn loop_file_paths_are_the_project_then_home() {
    let join = |base: &str, parts: &[&str]| format!("{base}|{}", parts.join("|"));
    assert_eq!(
        loop_file_paths("/Users/dev/repo", Some("/Users/dev"), join),
        [
            "/Users/dev/repo|.zcode|loop.md",
            "/Users/dev|.zcode|loop.md"
        ]
    );
    assert_eq!(
        loop_file_paths("C:\\repo", None, join),
        ["C:\\repo|.zcode|loop.md"]
    );
}
