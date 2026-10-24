//! cd 之后的路径与输入重定向（shell-permissions.md §4.5.7、§4.5.8，工作线 F85）：cd 的目标带进后面各段的路径判断，
//! 路径 deny 规则不因 cd 失效；首个 cd 指向工作目录里时后面各段按它解析；输入重定向的目标也查。文字逐字比较。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::{message, run};

const RULES: [(RuleBehavior, &str); 4] = [
    (RuleBehavior::Deny, "Bash(rm:*)"),
    (RuleBehavior::Deny, "Read(//etc/**)"),
    (RuleBehavior::Deny, "Edit(//etc/**)"),
    (RuleBehavior::Deny, "Read(./secret/**)"),
];
const CD_READ: &str = "reads a file by a relative path after a cd in a compound command; which file that is cannot be resolved statically while a Read() deny rule is configured, so this needs approval.";

fn verdict_in(mode: PermissionMode, rules: &[(RuleBehavior, &str)], command: &str) -> String {
    run(mode, rules, |inputs| message(&check(command, inputs)))
}

fn verdict(command: &str) -> String {
    verdict_in(PermissionMode::Default, &RULES, command)
}

fn names(command: &str) -> String {
    format!("deny: Permission to use Bash with command {command} has been denied.")
}

/// 修复（工作线 F85）：以前后面各段按会话的当前目录解析，`cd /etc && cat hosts` 只问 cd，guarded 与 yolo 下读到
/// `/etc/hosts`。
#[test]
fn a_cd_target_reaches_the_path_deny_rules() {
    for command in [
        "cd /etc && cat hosts",
        "cd /etc; cat hosts",
        "cd /etc || cat hosts",
        "cd /etc && cat ./hosts",
        "cd / && cat etc/hosts",
        "cd /usr && cat ../etc/hosts",
        "pushd /etc && cat hosts",
        "cd -P /etc && cat hosts",
        "X=1 cd /etc && cat hosts",
        "command cd /etc && cat hosts",
        "cd /etc 2>/dev/null && cat hosts",
        "echo hi && cd /etc && cat hosts",
        "cd /etc && echo hi && cat hosts",
        "cd /etc && cat hosts | head -1",
        "cd /etc && cd / && cat hosts",
        "cd /etc && cd - && cat hosts",
        "pushd /etc && popd && cat hosts",
    ] {
        assert_eq!(
            verdict(command),
            "deny: cat from '/etc/hosts' was blocked by a deny rule.",
            "{command:?}"
        );
    }
    assert_eq!(
        verdict("cd /etc && echo hi > hosts"),
        "deny: Output redirection to '/etc/hosts' was blocked by a deny rule."
    );
    assert_eq!(
        verdict("cd /etc && head -1 < hosts"),
        "deny: Input redirection from '/etc/hosts' was blocked by a deny rule."
    );
}

#[test]
fn a_cd_inside_the_workspace_reaches_the_deny_rules_too() {
    for command in [
        "cd secret && cat key.txt",
        "cd secret; cat key.txt",
        "cd sub && cat ../secret/key.txt",
        "cd ./secret && cat key.txt",
    ] {
        assert_eq!(
            verdict(command),
            "deny: cat from '/w/secret/key.txt' was blocked by a deny rule.",
            "{command:?}"
        );
    }
    // 可能的当前目录是保守的集合：回到上级之后仍按 secret 查。
    assert_eq!(
        verdict("cd secret && cd .. && cat a.txt"),
        "deny: cat from '/w/secret/a.txt' was blocked by a deny rule."
    );
}

#[test]
fn an_unresolvable_cd_checks_the_deny_rule_directories() {
    for (command, name, path) in [
        ("cd /e* && cat hosts", "cat", "hosts"),
        ("cd - && cat hosts", "cat", "hosts"),
        ("cd /etc /usr && cat hosts", "cat", "hosts"),
        ("cd /e* && cat ../hosts", "cat", "../hosts"),
        ("cd /e* && cp a b", "cp", "a"),
        ("cd /e* && cat hosts > out.txt", "cat", "hosts"),
        ("cd /e* && head -1 < hosts", "head", "hosts"),
    ] {
        assert_eq!(
            verdict(command),
            format!(
                "deny: {name} reads '{path}' after a cd whose target cannot be resolved, and a deny rule (//etc/**) names that file under a directory it may run in"
            ),
            "{command:?}"
        );
    }
    // 绝对路径与 `.` 不按规则的目录拼。
    assert!(!verdict("cd /e* && cat /tmp/x").starts_with("deny"));
    assert!(!verdict("cd /e* && ls").starts_with("deny"));
}

#[test]
fn a_relative_read_after_a_cd_asks_while_a_read_deny_rule_exists() {
    for (command, name) in [
        ("cd sub && cat b.txt", "cat"),
        ("cd ./sub; cat b.txt", "cat"),
        ("cd sub && ls", "ls"),
    ] {
        assert_eq!(
            verdict(command),
            format!("ask: {name} {CD_READ}"),
            "{command:?}"
        );
    }
    // 没有 Read deny 规则时不问。
    let rm_only = [(RuleBehavior::Deny, "Bash(rm:*)")];
    assert_eq!(
        verdict_in(PermissionMode::Default, &rm_only, "cd sub && cat b.txt"),
        "allow"
    );
}

/// 首个 cd 指向工作目录里（`./x` 或绝对路径）而且只用 `&&` 连接时，后面各段按它解析，不再算“有 cd”。
#[test]
fn a_leading_cd_into_the_workspace_moves_the_later_parts() {
    assert_eq!(verdict("cd ./sub && cat b.txt"), "allow");
    assert_eq!(verdict("cd /w/sub && cat b.txt"), "allow");
    assert_eq!(
        verdict("cd ./sub && touch zz9"),
        "ask: touch in '/w/sub/zz9' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there."
    );
    assert_eq!(
        verdict_in(PermissionMode::AcceptEdits, &RULES, "cd ./sub && touch zz9"),
        "allow"
    );
    assert_eq!(
        verdict("cd ./sub && cat ../secret/key.txt"),
        names("cd ./sub && cat ../secret/key.txt")
    );
    // `sub` 不是 `./sub`：照旧按 cd 加写入询问。
    assert_eq!(
        verdict("cd sub && touch zz9"),
        format!("ask: {}", texts::CD_WITH_WRITE)
    );
}

/// `cd <会话当前目录>` 那一段不算 cd，也不检查。
#[test]
fn a_cd_to_the_current_directory_is_dropped() {
    let inside = "ask: touch in '/w/zz8' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there.";
    assert_eq!(verdict("cd /w && touch zz8"), inside);
    assert_eq!(verdict("echo hi && cd /w && touch zz8"), inside);
}

/// `x/**` 也匹配 `x` 本身；cd 在管道里不影响别的一截。
#[test]
fn a_denied_directory_denies_the_cd_itself() {
    for command in ["cd /etc", "cat hosts && cd /etc"] {
        assert_eq!(verdict(command), names(command), "{command:?}");
    }
    assert_eq!(verdict("cd /etc | cat hosts"), names("cd /etc"));
}

/// 修复（工作线 F85）：以前输入重定向的目标不检查，`cat < /etc/hosts` 放行。
#[test]
fn input_redirections_are_checked() {
    for command in ["cat < /etc/hosts", "head -1 0< /etc/hosts"] {
        assert_eq!(verdict(command), names(command), "{command:?}");
    }
    assert_eq!(
        verdict("echo a | cat < /etc/hosts"),
        names("cat < /etc/hosts")
    );
    assert_eq!(
        verdict("cat < /tmp/x"),
        "ask: Input redirection from '/tmp/x' was blocked. For security, ZCode may only read files in the allowed working directories for this session: '/w'."
    );
    assert_eq!(
        verdict("cat < ~/x"),
        "ask: Input redirection from '/home/dev/x' was blocked. For security, ZCode may only read files in the allowed working directories for this session: '/w'."
    );
    assert_eq!(
        verdict("cat < *.txt"),
        "ask: Input redirection from '*.txt' requires manual approval: the file it names depends on glob expansion or a directory change in the same command."
    );
    assert_eq!(
        verdict("cd sub && cat < b.txt"),
        "ask: Input redirection from 'b.txt' requires manual approval: the file it names depends on glob expansion or a directory change in the same command."
    );
    assert_eq!(
        verdict("cd sub && cat < /etc/hosts"),
        "deny: Input redirection from '/etc/hosts' was blocked by a deny rule."
    );
    for command in ["cat < a.txt", "cat < /dev/null", "cat <& 3"] {
        assert_eq!(verdict(command), "allow", "{command:?}");
    }
}
