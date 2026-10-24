//! shell -c 脚本（shell-permissions.md §4.5.9，工作线 F85 追加，用户决定 2026-10-24）：脚本里有被 deny 的 rm 一类命令、
//! 检查不了而又跑 rm、或里面是危险删除时，按危险删除一类询问（guarded 也问，yolo 不问）。文字逐字比较。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::{message, run};

const RULES: [(RuleBehavior, &str); 4] = [
    (RuleBehavior::Deny, "Bash(rm:*)"),
    (RuleBehavior::Deny, "Read(//etc/**)"),
    (RuleBehavior::Deny, "Edit(//etc/**)"),
    (RuleBehavior::Deny, "Bash(mv a b)"),
];
const DENIED_INSIDE: &str = "ask: A permission rule denies a command inside this shell -c script, so ZCode could not check it for dangerous removals. Approving runs the whole script, including that command.";
const UNCHECKED: &str = "ask: This command passes a shell -c script that runs rm, and ZCode could not check the script for dangerous removals. Approve only if you have read the script.";
const APPROVAL: &str = "passthrough: This command requires approval";

fn verdict_in(mode: PermissionMode, rules: &[(RuleBehavior, &str)], command: &str) -> String {
    run(mode, rules, |inputs| message(&check(command, inputs)))
}

fn verdict(command: &str) -> String {
    verdict_in(PermissionMode::Default, &RULES, command)
}

/// 修复（工作线 F85 追加）：以前脚本里的命令看不到，`sh -c 'rm -f x'` 只是“需要批准”，guarded 下执行。
#[test]
fn a_denied_command_inside_a_shell_script_asks() {
    for command in [
        "sh -c 'rm -f x'",
        "bash -c \"rm -f x\"",
        "zsh -c 'rm -f x'",
        "dash -c 'rm -f x'",
        "sh -c 'echo hi; rm -f x'",
        "sh -xc 'rm -f x'",
        "bash -lc 'rm -f x'",
        "bash --norc -c 'rm -f x'",
        "env sh -c 'rm -f x'",
        "sudo sh -c 'rm -f x'",
        "X=1 sh -c 'rm -f x'",
        "sh -c 'rm -f x' arg0",
        "sh -c 'rm -f \"$1\"' _ x",
        "sh -c 'rm -rf /'",
        "echo hi && sh -c 'rm -f x'",
        "sh -c 'X=1 rm -f x'",
        "busybox sh -c 'rm -f x'",
        "/bin/sh -c 'rm -f x'",
        "sh -c 'cd /etc && rm -f hosts'",
        "sh -c 'rm -f x' && echo done",
        "sh -c 'sudo rm -f x'",
        "sh -c 'echo {a,b}; rm -f x'",
        "sh -c \"sh -c 'rm -f x'\"",
    ] {
        assert_eq!(verdict(command), DENIED_INSIDE, "{command:?}");
    }
}

#[test]
fn a_script_that_cannot_be_checked_and_runs_rm_asks() {
    for command in ["bash --foo -c 'rm -f x'", "sh -c 'alias x=y; rm -f x'"] {
        assert_eq!(verdict(command), UNCHECKED, "{command:?}");
    }
}

/// 脚本里是危险删除时按它询问。
#[test]
fn a_dangerous_removal_inside_a_script_asks_with_its_own_text() {
    assert_eq!(
        verdict_in(PermissionMode::Default, &[], "sh -c 'rm -rf /'"),
        "ask: Dangerous rm operation detected: '/'\n\nThis command would remove a critical system directory. This requires explicit approval and cannot be auto-allowed by permission rules."
    );
}

/// 范围照对照实现：脚本里没有 rm 一类的词、不是 shell、没有 deny 也不危险时不问。
#[test]
fn scripts_outside_the_scope_stay_as_they_were() {
    for command in [
        "sh -c 'cat /etc/zz-none'",
        "sh -c 'mv a b'",
        "sh -c 'rmdir zz'",
        "fish -c 'rm -f x'",
        "sh -c 'echo hi'",
    ] {
        assert_eq!(verdict(command), APPROVAL, "{command:?}");
    }
    assert_eq!(
        verdict_in(PermissionMode::Default, &[], "sh -c 'rm -f x'"),
        APPROVAL
    );
}

/// guarded 照对照实现询问（危险删除一类）；yolo 不问。
#[test]
fn guarded_asks_and_yolo_does_not() {
    let command = "sh -c 'rm -f x'";
    assert_eq!(
        verdict_in(PermissionMode::Guarded, &RULES, command),
        DENIED_INSIDE
    );
    let found = run(PermissionMode::Guarded, &RULES, |inputs| {
        check(command, inputs)
    });
    assert!(matches!(
        found,
        Check::Ask {
            kind: AskKind::Immune,
            ..
        }
    ));
}
