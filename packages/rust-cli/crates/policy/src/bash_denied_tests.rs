//! 一段被拒绝时的文字（shell-permissions.md §4.5.5，工作线 F80，用户决定 2026-10-24“用整条命令”）：文字里是整条命令
//! （原文，不去首尾空白）；有管道时是那一段（管道符之间，去掉 `>` / `>>` 重定向）；管道里的输出重定向被拒绝时保留
//! 路径那一句。规则与路径的拒绝都一样。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::{message, run};

const RULES: [(RuleBehavior, &str); 4] = [
    (RuleBehavior::Allow, "Bash(echo:*)"),
    (RuleBehavior::Deny, "Bash(rm:*)"),
    (RuleBehavior::Deny, "Read(//etc/**)"),
    (RuleBehavior::Deny, "Edit(//etc/**)"),
];

fn denied(command: &str) -> String {
    run(PermissionMode::Default, &RULES, |inputs| {
        message(&check(command, inputs))
    })
}

fn names(command: &str) -> String {
    format!("deny: Permission to use Bash with command {command} has been denied.")
}

#[test]
fn a_denied_part_names_the_whole_command() {
    for command in [
        "echo hi && rm -f x",
        "echo hi; rm -f x",
        "echo a\nrm -f x",
        "ls && rm -rf b",
        "if true; then rm -f x; fi",
        "for x in a; do rm -f x; done",
        "! rm -f x",
        "echo a & rm -f x",
        "rm -f x > out.txt && echo hi",
        // 有子 shell 时不按管道分段。
        "echo hi && (rm -f x)",
        "echo a && echo b | (rm -f x)",
        // 替换里拆出的命令也一样。
        "echo \"gone: $(rm -f x)\"",
    ] {
        assert_eq!(denied(command), names(command), "{command}");
    }
}

#[test]
fn the_whole_command_keeps_its_surrounding_spaces() {
    for command in [
        "echo hi && rm -f x ",
        "  echo hi && rm -f x",
        "rm -f x ",
        "{ rm -f x; } ",
        "echo $(rm -f x) ",
    ] {
        assert_eq!(denied(command), names(command), "{command:?}");
    }
    // 整条命令命中精确的 deny 规则时去掉首尾空白。
    let exact = [(RuleBehavior::Deny, "Bash(rm -f y)")];
    assert_eq!(
        run(PermissionMode::Default, &exact, |inputs| message(&check(
            " rm -f y ",
            inputs
        ))),
        names("rm -f y")
    );
}

#[test]
fn a_denied_part_of_a_pipeline_names_its_segment() {
    for (command, segment) in [
        ("echo a | rm -f x", "rm -f x"),
        ("echo a |& rm -f x", "rm -f x"),
        ("echo a | rm -f x | cat", "rm -f x"),
        ("echo a && rm -f x | cat", "echo a && rm -f x"),
        ("echo a | rm -f x && echo b", "rm -f x && echo b"),
        ("cat a.txt | head -1 && rm -f x", "head -1 && rm -f x"),
        ("echo a & rm -f x | cat", "echo a & rm -f x"),
        ("echo a && rm -f x | rm -f y", "echo a && rm -f x"),
        ("echo \"a $(rm -f x)\" | cat", "echo \"a $(rm -f x)\""),
        ("echo a | X=$(rm -f x) cat", "X=$(rm -f x) cat"),
        ("echo a |   rm -f x   ", "rm -f x"),
        ("echo a |\n rm -f x", "rm -f x"),
        ("echo a | rm  -f   x", "rm  -f   x"),
        // 结构里面的管道符也分段（原文照切）。
        ("if true; then echo a | rm -f x; fi", "rm -f x; fi"),
        ("while true; do echo a | rm -f x; done", "rm -f x; done"),
    ] {
        assert_eq!(denied(command), names(segment), "{command:?}");
    }
}

#[test]
fn output_redirections_are_dropped_from_a_segment() {
    for (command, segment) in [
        ("echo a | rm -f x > out.txt", "rm -f x"),
        ("echo a | rm  -f   x>out.txt", "rm -f x"),
        ("echo a | rm -f x 2> err.txt", "rm -f x"),
        ("echo a | rm -f x >> log.txt", "rm -f x"),
        ("echo a | rm -f x 2>&1", "rm -f x 2>&1"),
        ("echo a | rm -f x > /etc/x", "rm -f x"),
    ] {
        assert_eq!(denied(command), names(segment), "{command:?}");
    }
}

#[test]
fn path_denials_name_the_command_too() {
    assert_eq!(denied("cat /etc/hosts"), names("cat /etc/hosts"));
    assert_eq!(
        denied("echo hi && cat /etc/hosts"),
        names("echo hi && cat /etc/hosts")
    );
    assert_eq!(denied("echo a > /etc/x"), names("echo a > /etc/x"));
    assert_eq!(
        denied("echo a > /etc/x && echo b"),
        names("echo a > /etc/x && echo b")
    );
    assert_eq!(
        denied("echo a | cat /etc/hosts > out.txt"),
        names("cat /etc/hosts")
    );
}

#[test]
fn a_denied_output_redirection_in_a_pipeline_keeps_the_path_text() {
    for command in ["echo a | cat > /etc/x", "echo a | cat 2>/etc/x"] {
        assert_eq!(
            denied(command),
            "deny: Output redirection to '/etc/x' was blocked by a deny rule.",
            "{command}"
        );
    }
}
