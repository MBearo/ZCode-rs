//! deny / ask 规则的候选（shell-permissions.md §4.5.6，工作线 F85）：前缀赋值、argv 层的包装命令、命令名的引号、
//! 重定向与多余的空白都挡不住 deny 规则；allow 规则只去掉安全的前缀赋值。文字逐字比较。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::{message, run};

const RM: [(RuleBehavior, &str); 1] = [(RuleBehavior::Deny, "Bash(rm:*)")];
const RM_INSIDE: &str = "ask: rm in '/w/x' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there.";

fn verdict(rules: &[(RuleBehavior, &str)], command: &str) -> String {
    run(PermissionMode::Default, rules, |inputs| {
        message(&check(command, inputs))
    })
}

fn names(command: &str) -> String {
    format!("deny: Permission to use Bash with command {command} has been denied.")
}

fn all_denied(rules: &[(RuleBehavior, &str)], commands: &[&str]) {
    for command in commands {
        assert_eq!(verdict(rules, command), names(command), "{command:?}");
    }
}

/// 修复（工作线 F85）：以前只去掉安全名单里的前缀赋值，`X=1 rm -f x` 不命中 `Bash(rm:*)`，guarded 与 yolo 下执行。
#[test]
fn prefix_assignments_do_not_hide_a_denied_command() {
    all_denied(
        &RM,
        &[
            "X=1 rm -f x",
            "X=1 Y=2 rm -f x",
            "X='a b' rm -f x",
            "X=\"a b\" rm -f x",
            "X=a=b rm -f x",
            "X+=1 rm -f x",
            "X[0]=1 rm -f x",
            "X=a\\ b rm -f x",
            "X=$(echo a) rm -f x",
            "NODE_ENV=x rm -f x",
            "echo hi && X=1 rm -f x",
            "X=1 \\\n rm -f x",
        ],
    );
}

#[test]
fn wrapper_commands_do_not_hide_a_denied_command() {
    all_denied(
        &RM,
        &[
            "env rm -f x",
            "env X=1 rm -f x",
            "env -i rm -f x",
            "env -u HOME rm -f x",
            "env -- rm -f x",
            "env -S 'rm -f x'",
            "env -S'rm -f x'",
            "command rm -f x",
            "command -p rm -f x",
            "command -- rm -f x",
            "builtin rm -f x",
            "noglob rm -f x",
            "nocorrect rm -f x",
            "exec rm -f x",
            "sudo rm -f x",
            "sudo -u root rm -f x",
            "sudo -- rm -f x",
            "sudo -u root -- rm -f x",
            "doas rm -f x",
            "watch rm -f x",
            "watch -n 1 rm -f x",
            "ionice rm -f x",
            "ionice -c 3 rm -f x",
            "setsid rm -f x",
            "taskset 1 rm -f x",
            "flock lk rm -f x",
            "flock -c 'rm -f x' lk",
            "unshare rm -f x",
            "strace rm -f x",
            "chrt 1 rm -f x",
            "script -c 'rm -f x'",
            "nice -10 rm -f x",
        ],
    );
}

#[test]
fn quoting_the_command_name_does_not_hide_it() {
    all_denied(&RM, &["\\rm -f x", "\"rm\" -f x", "'rm' -f x", "r\\m -f x"]);
}

#[test]
fn wrappers_and_assignments_mix_in_any_order() {
    all_denied(
        &RM,
        &[
            "FOO=bar timeout 5 rm -f x",
            "nohup FOO=bar rm -f x",
            "FOO=bar nohup rm -f x",
            "timeout 5 X=1 rm -f x",
            "time X=1 rm -f x",
        ],
    );
}

/// 回归：以前就拒绝的写法。
#[test]
fn safe_wrappers_still_deny() {
    all_denied(
        &RM,
        &[
            "nice rm -f x",
            "nice -n 5 rm -f x",
            "timeout 5 rm -f x",
            "timeout -s KILL 5 rm -f x",
            "nohup rm -f x",
            "time rm -f x",
            "stdbuf -o0 rm -f x",
            "xargs rm -f x",
            "echo hi\nrm -f x",
            "echo hi; rm -f x",
            "rm \\\n -f x",
        ],
    );
    assert_eq!(verdict(&RM, "echo x | xargs rm -f"), names("xargs rm -f"));
}

/// 精确规则比去掉重定向的命令，也看前缀赋值与包装命令；不并空白。
#[test]
fn exact_rules_see_through_redirections_and_wrappers() {
    let rules = [(RuleBehavior::Deny, "Bash(mv a b)")];
    all_denied(
        &rules,
        &[
            "mv a b > out.txt",
            "mv a b 2>/dev/null",
            "X=1 mv a b",
            "sudo mv a b",
            "nohup mv a b",
            "\"mv\" a b",
        ],
    );
    assert!(!verdict(&rules, "mv  a b").starts_with("deny"));
}

/// 前缀与通配规则比较前把连续的空格、制表符并成一个空格；通配的 deny 规则也认 `xargs`。
#[test]
fn prefix_and_wildcard_rules_ignore_extra_blanks() {
    all_denied(
        &[(RuleBehavior::Deny, "Bash(rm -f:*)")],
        &["rm  -f x", "rm\t-f x", "rm -f  x"],
    );
    all_denied(
        &[(RuleBehavior::Deny, "Bash(cp *)")],
        &[
            "cp\tx y",
            "cp  x y",
            "xargs cp x",
            "X=1 cp x y",
            "nohup cp x y",
            "sudo cp x y",
        ],
    );
}

#[test]
fn ask_rules_see_the_same_forms() {
    let rules = [(RuleBehavior::Ask, "Bash(rm:*)")];
    for command in [
        "X=1 rm -f x",
        "X='a b' rm -f x",
        "env X=1 rm -f x",
        "command rm -f x",
        "\"rm\" -f x",
        "\\rm -f x",
        "sudo rm -f x",
        "nohup FOO=bar rm -f x",
        "X=1 \\\n rm -f x",
        "echo hi && X=1 rm -f x",
        "rm  -f x",
    ] {
        assert_eq!(
            verdict(&rules, command),
            "ask: Permission rule 'Bash(rm:*)' from CLI argument requires approval for this Bash command",
            "{command:?}"
        );
    }
}

/// allow 规则只去掉安全的前缀赋值（不放宽）；包装命令、命令名的引号与多余的空白照 deny 去掉。
#[test]
fn allow_rules_strip_only_safe_assignments() {
    let rules = [
        (RuleBehavior::Allow, "Bash(rm:*)"),
        (RuleBehavior::Allow, "Bash(echo *)"),
    ];
    assert_eq!(verdict(&rules, "X=1 rm -f x"), RM_INSIDE);
    assert_eq!(verdict(&rules, "env rm -f x"), RM_INSIDE);
    for command in [
        "NODE_ENV=x rm -f x",
        "command rm -f x",
        "\"rm\" -f x",
        "\\rm -f x",
        "rm  -f x",
        "nohup rm -f x",
        "builtin rm -f x",
        "noglob rm -f x",
        "xargs rm -f x",
        "xargs echo hi",
        "rm -f x 2>&1",
    ] {
        assert_eq!(verdict(&rules, command), "allow", "{command:?}");
    }
}

/// 对照实现也不拒绝的写法：照它不拒绝（不加严）。
#[test]
fn forms_left_to_the_other_checks_are_not_denied() {
    for command in [
        "/bin/rm -f x",
        "./rm -f x",
        "X=1 /bin/rm -f x",
        "xargs -0 rm -f",
        "X=$Y rm -f x",
        "sh -c 'rm -f x'",
        "bash -c \"rm -f x\"",
        "eval rm -f x",
        "command -v rm",
    ] {
        let found = verdict(&RM, command);
        assert!(!found.starts_with("deny"), "{command:?}: {found}");
    }
}
