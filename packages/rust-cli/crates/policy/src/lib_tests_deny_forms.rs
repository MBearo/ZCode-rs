//! deny 规则在 guarded 与 yolo 下也挡住前缀赋值、包装命令与 cd 之后的路径（shell-permissions.md §4.5.6、§4.5.7，
//! 工作线 F85）：这两种模式除拒绝外放行，以前这些写法在两种模式下直接执行。

use super::*;

const DENIED: [&str; 7] = [
    "X=1 rm -f x",
    "X='a b' rm -f x",
    "env rm -f x",
    "sudo rm -f x",
    "command rm -f x",
    "\\rm -f x",
    "nohup FOO=bar rm -f x",
];

fn gate() -> PolicyGate {
    PolicyGate::new(rules(&[
        (RuleBehavior::Deny, "Bash(rm:*)"),
        (RuleBehavior::Deny, "Read(//etc/**)"),
        (RuleBehavior::Deny, "Read(./secret/**)"),
    ]))
}

#[tokio::test]
async fn guarded_and_yolo_deny_a_prefixed_command() {
    let gate = gate();
    for mode in [PermissionMode::Guarded, PermissionMode::BypassPermissions] {
        for command in DENIED {
            assert_eq!(
                bash(&gate, mode, command).await,
                deny(format!(
                    "Permission to use Bash with command {command} has been denied."
                )),
                "{mode:?} {command:?}"
            );
        }
    }
}

#[tokio::test]
async fn guarded_and_yolo_deny_a_read_after_cd() {
    let gate = gate();
    for mode in [PermissionMode::Guarded, PermissionMode::BypassPermissions] {
        assert_eq!(
            bash(&gate, mode, "cd /etc && cat hosts").await,
            deny("cat from '/etc/hosts' was blocked by a deny rule."),
            "{mode:?}"
        );
        assert_eq!(
            bash(&gate, mode, "cd secret && cat key.txt").await,
            deny("cat from '/w/secret/key.txt' was blocked by a deny rule."),
            "{mode:?}"
        );
        assert_eq!(
            bash(&gate, mode, "cat < /etc/hosts").await,
            deny("Permission to use Bash with command cat < /etc/hosts has been denied."),
            "{mode:?}"
        );
        // cd 之后读相对路径的询问是其他类：这两种模式放行。
        assert_eq!(
            bash(&gate, mode, "cd sub && cat b.txt").await,
            allow(),
            "{mode:?}"
        );
    }
}

/// shell -c 脚本里有被 deny 的命令（§4.5.9，工作线 F85 追加，用户决定 2026-10-24）：guarded 询问，yolo 不问。
#[tokio::test]
async fn a_denied_command_in_a_shell_script_asks_in_guarded_but_not_in_yolo() {
    let gate = gate();
    for command in ["sh -c 'rm -f x'", "bash -lc 'echo hi; rm -f x'"] {
        assert_eq!(
            ask_message(&bash(&gate, PermissionMode::Guarded, command).await),
            "A permission rule denies a command inside this shell -c script, so ZCode could not check it for dangerous removals. Approving runs the whole script, including that command.",
            "{command:?}"
        );
        assert_eq!(
            bash(&gate, PermissionMode::BypassPermissions, command).await,
            allow(),
            "{command:?}"
        );
    }
}
