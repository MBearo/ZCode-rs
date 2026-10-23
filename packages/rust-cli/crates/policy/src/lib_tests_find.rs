//! find 的路径检查（shell-permissions.md
//! §4.5.1 第 2 条、D16）：第一个选项之前的词、带路径的谓词（`-newer`、`-samefile`、`-path` 等与 `-newerXY`）的值、`--`
//! 之后的词是路径；`-H` / `-L` / `-P` 不算选项。bfs 允许路径写在表达式后面，这条规则不校验它们（用户决定 2026-10-22，
//! 保留）。

use super::*;

fn search_blocked(path: &str) -> String {
    format!(
        "find in '{path}' was blocked. For security, ZCode may only search files in the allowed working directories for this session: '/w'."
    )
}

#[tokio::test]
async fn the_extracted_find_paths_are_checked() {
    let gate = PolicyGate::default();
    for (command, path) in [
        ("find /etc -name x", "/etc"),
        ("find -L /etc -name x", "/etc"),
        ("find -H -P /etc", "/etc"),
        ("find . -newer /etc/hosts", "/etc/hosts"),
        ("find . -samefile /etc/hosts", "/etc/hosts"),
        ("find . -path /etc/x", "/etc/x"),
        ("find . -newermt /etc", "/etc"),
        ("find -- /etc", "/etc"),
        ("find -- -name /etc", "/etc"),
    ] {
        let decision = bash(&gate, PermissionMode::Default, command).await;
        assert_eq!(ask_message(&decision), search_blocked(path), "{command}");
    }
}

/// 第一个选项之后的普通词不是路径（`-name` 等谓词的值、bfs 写在表达式后面的起点、bfs 的 `-f PATH`）：不校验。
#[tokio::test]
async fn words_after_the_first_option_are_not_paths() {
    let gate = PolicyGate::default();
    for command in [
        "find . -name '*.rs'",
        "find . -name /etc",
        "find . -regex '/etc/.*' -type f",
        "find -S dfs -regextype findutils-default . -name x",
        "find . -maxdepth 2 -printf '%p\\n'",
        "find . \\( -name a -o -name b \\) -print",
        "find src -mtime -1 -size +1k",
        // bfs 会搜这些位置，路径提取不校验（用户已知晓）。
        "find \\( -name x \\) /etc",
        "find ! -name x /etc",
        "find -name x /etc",
        "find . -f /etc",
        "find -Hf /etc -name x",
    ] {
        assert_eq!(
            bash(&gate, PermissionMode::Default, command).await,
            allow(),
            "{command}"
        );
    }
}
