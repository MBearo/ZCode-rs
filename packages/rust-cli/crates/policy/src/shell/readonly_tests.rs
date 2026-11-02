//! 只读判定的代表样例（完整的对照表在 apps/zcode-cli-rust 的 Node 夹具里，未复制）。

use super::*;

#[test]
fn tables_are_loaded() {
    assert!(tables::tables().commands.len() >= 60);
}

#[test]
fn read_only_and_not() {
    for command in [
        "echo hello",
        "ls -la",
        "cat a.txt | head -1",
        "git status",
        "git log --oneline -5",
        "pwd",
        "wc -l a.txt",
        "grep -rn foo .",
    ] {
        assert!(is_read_only(command, false), "{command}");
    }
    for command in [
        "touch x",
        "rm -rf x",
        "echo hi > out.txt",
        "git push",
        "npm install",
        "sed -i s/a/b/ f",
        "find . -delete",
        "echo $(rm x)",
    ] {
        assert!(!is_read_only(command, false), "{command}");
    }
    assert!(!is_read_only("git status", true), "untrusted git config");
}

/// 工作线 F53 阶段 0（shell-permissions.md §4.5.1 第 1 条）：find 换成 bfs 之后，bfs 的 `-rm` 等同 `-delete`；
/// find 的只读判定按写操作表、bfs 标志组与带值谓词。
#[test]
fn bfs_actions_and_flag_groups_follow_the_find_rules() {
    for command in [
        "find . -rm",
        "find . -name '*.log' -rm",
        "find . -type f -o -rm",
        // `-f` 的值仍查写操作表。
        "find -f -rm",
        // `/^-[dsx]+f$/`：BSD find 的 getopt 写法（`-s -f PATH`），不算只读。
        "find -sf /tmp",
        "find -xf . -name x",
        "find . -fprint out.txt",
        "find . -files0-from list",
        // 同 `-delete`：任何位置出现 `-rm` 都按写操作（Node `hasKnownBashWriteOption`，保守）。
        "find . -name -rm",
    ] {
        assert!(!is_read_only(command, false), "{command}");
    }
    for command in [
        "find . -name '*.rs'",
        "find -f . -name x",
        "find -Hf . -type f",
        "find -L . -maxdepth 2",
        // 带值谓词的值跳过。
        "find . -newermt 2026-01-01",
        "find . -path '*/-delete'",
    ] {
        assert!(is_read_only(command, false), "{command}");
    }
}
