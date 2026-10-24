//! 复合结构拆开逐段套规则（shell-permissions.md §4.5.4，工作线 F76）：后台 `&`、子 shell、if / while / for、`!`、
//! `[[ … ]]`，以及拆不开的写法的理由。只有一段不放行而且它是询问时整条按它询问。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use super::*;
use crate::test_support::{message, run};

const ECHO: [(RuleBehavior, &str); 1] = [(RuleBehavior::Allow, "Bash(echo:*)")];
const BACKGROUND: &str = "ask: This command uses the `&` background operator, which defers execution past approval-time safety checks. Approve only if you trust it.";
const OPERATORS: &str = "ask: This command uses shell operators that require approval for safety";
const GENERIC: &str = "ask: This command can't be checked before it runs";
const VARIABLE: &str = "ask: A variable in this command can't be checked before it runs";

fn verdict(mode: PermissionMode, entries: &[(RuleBehavior, &str)], command: &str) -> String {
    run(mode, entries, |inputs| message(&check(command, inputs)))
}

fn echo(command: &str) -> String {
    verdict(PermissionMode::Default, &ECHO, command)
}

fn kind(mode: PermissionMode, entries: &[(RuleBehavior, &str)], command: &str) -> Option<AskKind> {
    run(mode, entries, |inputs| match check(command, inputs) {
        Check::Ask { kind, .. } => Some(kind),
        _ => None,
    })
}

fn mkdir(name: &str) -> String {
    format!(
        "ask: mkdir in '/w/{name}' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there."
    )
}

fn too_complex(what: &str) -> String {
    format!("ask: {what} in this command can't be checked before it runs")
}

#[test]
fn conditions_branches_and_loop_bodies_are_checked_one_by_one() {
    for command in [
        "if true; then echo yes; fi",
        "for x in a b; do echo hi; done",
        "for x in a b\ndo\n  echo hi\ndone",
        "while false; do echo x; done",
        "until true; do echo x; done",
        "if false; then echo a; elif true; then echo b; else echo c; fi",
        "! echo hi",
        "[[ -f a.txt ]] && echo yes",
        "if [[ -d src ]]; then echo dir; fi",
    ] {
        assert_eq!(echo(command), "allow", "{command}");
    }
}

#[test]
fn one_part_that_asks_decides_the_whole_command() {
    assert_eq!(echo("if true; then mkdir zz1; fi"), mkdir("zz1"));
    assert_eq!(echo("for x in a; do mkdir zz3; done"), mkdir("zz3"));
    assert_eq!(echo("echo a & mkdir zz4"), mkdir("zz4"));
    // 不是复合结构的命令也一样（以前列出多操作）。
    assert_eq!(echo("true && mkdir zz5"), mkdir("zz5"));
    // 两段要批准时照旧列出。
    assert_eq!(
        echo("if true; then mkdir a; touch b; fi"),
        "ask: This Bash command contains multiple operations. The following parts require approval: mkdir a, touch b"
    );
}

#[test]
fn background_commands_ask_once_when_every_part_is_allowed() {
    assert_eq!(echo("echo a & echo b"), BACKGROUND);
    assert_eq!(echo("echo a &"), BACKGROUND);
    assert_eq!(
        kind(PermissionMode::Default, &ECHO, "echo a & echo b"),
        Some(AskKind::Safety)
    );
    // 只读命令也问；整个工具的 allow 规则下各段没有意见，不加这一问。
    assert_eq!(
        verdict(PermissionMode::Default, &[], "ls & pwd"),
        BACKGROUND
    );
    assert_eq!(
        verdict(
            PermissionMode::Default,
            &[(RuleBehavior::Allow, "Bash")],
            "npm a & npm b"
        ),
        "passthrough: This Bash command contains multiple operations. The following parts require approval: npm a, npm b"
    );
}

#[test]
fn a_subshell_asks_after_deny_rules_and_dangerous_removals() {
    assert_eq!(echo("echo hi && (mkdir zz2)"), OPERATORS);
    assert_eq!(echo("(echo a && echo b)"), OPERATORS);
    assert_eq!(echo("echo a | (cat)"), OPERATORS);
    assert_eq!(echo("if true; then (echo a); fi"), OPERATORS);
    // 前缀与精确的 allow 规则都不放行它。
    assert_eq!(
        verdict(
            PermissionMode::Default,
            &[(RuleBehavior::Allow, "Bash((echo a && echo b))")],
            "(echo a && echo b)"
        ),
        OPERATORS
    );
    // 里面的命令照样拆出来套 deny 规则与危险删除；文字是整条命令（工作线 F80）。
    let deny = [(RuleBehavior::Deny, "Bash(rm:*)")];
    assert_eq!(
        verdict(PermissionMode::Default, &deny, "echo hi && (rm -f x)"),
        "deny: Permission to use Bash with command echo hi && (rm -f x) has been denied."
    );
    assert!(
        verdict(PermissionMode::Default, &[], "(rm -rf ~)")
            .starts_with("ask: Dangerous rm operation detected: '/home/dev'")
    );
    // 性质是其他：bypass 类模式放行。
    assert_eq!(
        kind(PermissionMode::Default, &ECHO, "(echo a)"),
        Some(AskKind::Other)
    );
}

#[test]
fn separators_directly_inside_a_subshell_or_a_body_cannot_be_checked() {
    assert_eq!(echo("(echo a; echo b)"), GENERIC);
    assert_eq!(echo("(echo a;)"), GENERIC);
    assert_eq!(echo("(echo a &)"), GENERIC);
    assert_eq!(echo("for x in a; do echo a & done"), GENERIC);
    assert_eq!(echo("if true; then echo a & fi"), GENERIC);
    // 换行分隔照常。
    assert_eq!(echo("(echo a\necho b)"), OPERATORS);
}

#[test]
fn loop_variables_have_no_known_value() {
    assert_eq!(echo("for x in a b; do echo \"$x\"; done"), VARIABLE);
    assert_eq!(echo("for x in a b; do echo $x; done"), VARIABLE);
    assert_eq!(echo("for x in a b; do echo ${x}; done"), VARIABLE);
    assert_eq!(echo("for x in a; do echo hi; done; echo $x"), VARIABLE);
    // 名字只是前缀时不算。
    assert_eq!(echo("for x in a; do echo $xy; done"), "allow");
    assert_eq!(
        echo("for PATH in /tmp; do echo hi; done"),
        "ask: PATH as loop variable bypasses assignment validation"
    );
    assert_eq!(
        echo("for LD_PRELOAD in a; do echo hi; done"),
        "ask: LD_PRELOAD as loop variable bypasses assignment validation"
    );
}

#[test]
fn loop_words_with_expansions_cannot_be_checked() {
    assert_eq!(
        echo("for x in $(ls); do echo hi; done"),
        "ask: A nested command in this command can't be checked before it runs"
    );
    assert_eq!(echo("for x in $FILES; do echo hi; done"), VARIABLE);
    assert_eq!(
        echo("for x in ${FILES}; do echo hi; done"),
        too_complex("A variable in braces")
    );
    assert_eq!(
        echo("for x in {a,b}; do echo hi; done"),
        too_complex("A brace pattern")
    );
    assert_eq!(echo("for x in *.txt; do echo hi; done"), "allow");
}

#[test]
fn structures_that_cannot_be_split_name_themselves() {
    for (command, what) in [
        ("{ echo a; }", "A group in braces or double parentheses"),
        ("(( 1 + 1 ))", "A group in braces or double parentheses"),
        ("case a in a) echo hi;; esac", "A case statement"),
        ("f() { echo hi; }", "A function definition"),
        ("function f { echo hi; }", "A function definition"),
        (
            "for ((i=0; i<1; i++)); do echo hi; done",
            "A counting for loop",
        ),
        (
            "while read l; do echo \"$l\"; done < a.txt",
            "A while or until loop",
        ),
        ("(echo a) > out.txt", "A group in parentheses"),
        ("if true; then echo a; fi 2>&1", "An if statement"),
        (
            "for x in a; do echo hi; done > out.txt",
            "A for or select loop",
        ),
        ("[[ -f a ]] > out.txt", "A test in brackets"),
    ] {
        assert_eq!(echo(command), too_complex(what), "{command}");
    }
    assert_eq!(
        echo("select x in a; do echo hi; done"),
        "ask: select statement reads stdin into $REPLY; cannot statically model"
    );
    // too-complex 的判决：每一段的 deny 规则（含花括号里的）、危险删除、整条的精确 allow 规则。
    let deny = [(RuleBehavior::Deny, "Bash(rm:*)")];
    assert_eq!(
        verdict(PermissionMode::Default, &deny, "{ rm -f x; }"),
        "deny: Permission to use Bash with command { rm -f x; } has been denied."
    );
    assert!(
        verdict(PermissionMode::Default, &[], "{ rm -rf ~; }")
            .starts_with("ask: Dangerous rm operation detected: '/home/dev'")
    );
    assert_eq!(
        verdict(
            PermissionMode::Default,
            &[(RuleBehavior::Allow, "Bash({ echo a; })")],
            "{ echo a; }"
        ),
        "allow"
    );
    // 不能拆的替换在前时用替换的理由。
    assert_eq!(
        echo("echo $(mkdir x); { echo a; }"),
        "ask: A nested command in this command can't be checked before it runs"
    );
}

#[test]
fn unfinished_structures_still_need_an_exact_rule() {
    for command in ["if true; then echo a", "(echo a", "for x in a; do echo a"] {
        assert_eq!(
            echo(command),
            "passthrough: This command requires approval",
            "{command}"
        );
    }
}

#[test]
fn tests_in_brackets_are_read_only_unless_an_operand_is_suspicious() {
    let none: [(RuleBehavior, &str); 0] = [];
    for command in [
        "[[ -f a.txt ]]",
        "[[ a == b ]]",
        "[[ 1 -eq 2 ]]",
        "[[ -t 1 ]]",
    ] {
        assert_eq!(
            verdict(PermissionMode::Default, &none, command),
            "allow",
            "{command}"
        );
    }
    for command in [
        "[[ -v a[0] ]]",
        "[[ -t x ]]",
        "[[ a[0] -eq 1 ]]",
        "[[ $x == y ]]",
    ] {
        assert_eq!(
            verdict(PermissionMode::Default, &none, command),
            "passthrough: This command requires approval",
            "{command}"
        );
    }
}

/// 工作线 F80（用户决定 2026-10-24“同 Node”）：guarded 下不问 background（F76 按安全检查询问过）；plan 开着时
/// 判决用的模式是 plan，照旧询问。shell operators 是其他（guarded 由判决链放行）。
#[test]
fn guarded_does_not_ask_about_background_commands() {
    for command in ["echo a & echo b", "echo a &", "ls & pwd"] {
        assert_eq!(
            verdict(PermissionMode::Guarded, &ECHO, command),
            "allow",
            "{command}"
        );
    }
    assert_eq!(
        verdict(PermissionMode::Plan, &ECHO, "echo a & echo b"),
        BACKGROUND
    );
    assert_eq!(
        verdict(PermissionMode::AcceptEdits, &ECHO, "echo a & echo b"),
        BACKGROUND
    );
    assert_eq!(
        kind(PermissionMode::Guarded, &ECHO, "(echo a)"),
        Some(AskKind::Other)
    );
}

#[test]
fn subcommand_count_follows_the_split() {
    assert_eq!(subcommand_count("if true; then echo a; echo b; fi"), 3);
    assert_eq!(subcommand_count("echo a & echo b"), 2);
    assert_eq!(subcommand_count("{ echo a; }"), 1);
}
