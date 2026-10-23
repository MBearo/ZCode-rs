//! 命令替换与进程替换（shell-permissions.md §4.5.2）：前缀 / 通配的 allow 规则盖不过替换里的命令。

use rust_cli_kernel_api::permission::{PermissionMode, RuleBehavior};

use crate::bash::check;
use crate::test_support::{message, run};

const NESTED: &str = "A nested command in this command can't be checked before it runs";
const AS_FILE: &str = "A command used as a file in this command can't be checked before it runs";
const QUOTED: &str = "Quoted text in this command can't be checked before it runs";

fn verdict(entries: &[(RuleBehavior, &str)], command: &str) -> String {
    run(PermissionMode::Default, entries, |inputs| {
        message(&check(command, inputs))
    })
}

fn echo_allowed(command: &str) -> String {
    verdict(&[(RuleBehavior::Allow, "Bash(echo:*)")], command)
}

#[test]
fn prefix_and_wildcard_rules_do_not_cover_a_bare_command_substitution() {
    for command in [
        "echo $(mkdir zz1)",
        "echo `mkdir zz2`",
        "echo x$(mkdir zz3)y",
        "$(which echo) hi",
        "echo hi > $(mkdir zz4)",
    ] {
        assert_eq!(echo_allowed(command), format!("ask: {NESTED}"), "{command}");
    }
    assert_eq!(
        verdict(
            &[(RuleBehavior::Allow, "Bash(echo *)")],
            "echo $(mkdir zz1)"
        ),
        format!("ask: {NESTED}")
    );
}

#[test]
fn process_substitution_is_never_split() {
    assert_eq!(echo_allowed("echo <(mkdir zz7)"), format!("ask: {AS_FILE}"));
    assert_eq!(echo_allowed("echo \"a\" >(cat)"), format!("ask: {AS_FILE}"));
}

#[test]
fn commands_hidden_in_expansions_are_too_complex() {
    for command in [
        "echo ${x:-$(mkdir zz9)}",
        "echo $(( $(mkdir zz10) ))",
        "echo {a,$(mkdir zz11)}",
        "echo ${ mkdir zz12; }",
    ] {
        assert_eq!(echo_allowed(command), format!("ask: {NESTED}"), "{command}");
    }
    // 没有命令的展开不受影响。
    assert_eq!(echo_allowed("echo ${x:-y} $((1 + 2))"), "allow");
}

#[test]
fn a_quoted_substitution_is_checked_as_its_own_subcommand() {
    assert_eq!(
        echo_allowed("echo \"made: $(mkdir zz6)\""),
        "ask: This Bash command contains multiple operations. The following part requires approval: mkdir zz6"
    );
    assert_eq!(
        echo_allowed("echo \"before `mkdir zz5` after\""),
        "ask: This Bash command contains multiple operations. The following part requires approval: mkdir zz5"
    );
    // 替换里的命令是只读的：两边都放行。
    assert_eq!(
        echo_allowed("echo \"status: $(git status --short)\""),
        "allow"
    );
    // 嵌套：每一层都要放行。
    assert_eq!(
        echo_allowed("echo \"a $(echo \"b $(mkdir zz13)\")\""),
        "ask: This Bash command contains multiple operations. The following part requires approval: mkdir zz13"
    );
}

#[test]
fn a_string_that_is_almost_only_a_substitution_is_too_complex() {
    for command in [
        "echo \"$(mkdir zz6)\"",
        "echo \"$(git status --short)\"",
        "echo \"x`date`\"",
        "X=\"$(mkdir zz8)\" echo hi",
    ] {
        assert_eq!(echo_allowed(command), format!("ask: {QUOTED}"), "{command}");
    }
}

#[test]
fn an_assignment_value_is_split_only_when_it_is_the_whole_substitution() {
    // 外层命令带不安全的赋值前缀，前缀规则也不匹配它：两个都要批准。
    assert_eq!(
        echo_allowed("X=$(mkdir zz8) echo hi"),
        "ask: This Bash command contains multiple operations. The following parts require approval: mkdir zz8, X=$(mkdir zz8) echo hi"
    );
    assert_eq!(
        echo_allowed("X=a$(mkdir zz8) echo hi"),
        format!("ask: {NESTED}")
    );
}

#[test]
fn deny_rules_reach_commands_inside_substitutions() {
    let rules = [
        (RuleBehavior::Allow, "Bash(echo:*)"),
        (RuleBehavior::Deny, "Bash(rm:*)"),
    ];
    assert_eq!(
        verdict(&rules, "echo $(rm -rf x)"),
        "deny: Permission to use Bash with command echo $(rm -rf x) has been denied."
    );
    assert_eq!(
        verdict(&rules, "echo \"$(rm -rf x)\""),
        "deny: Permission to use Bash with command echo \"$(rm -rf x)\" has been denied."
    );
    assert_eq!(
        verdict(&rules, "echo \"gone: $(rm -rf x)\""),
        "deny: Permission to use Bash with command rm -rf x has been denied."
    );
}

#[test]
fn only_an_exact_rule_covers_a_too_complex_command() {
    assert_eq!(
        verdict(
            &[(RuleBehavior::Allow, "Bash(echo $(date))")],
            "echo $(date)"
        ),
        "allow"
    );
    // 整条命令的前缀 ask 规则：询问的文字是通用的那一句。
    assert_eq!(
        verdict(&[(RuleBehavior::Ask, "Bash(echo:*)")], "echo $(date)"),
        "ask: ZCode requested permissions to use Bash, but you haven't granted it yet."
    );
}

#[test]
fn prefix_rules_do_not_cover_a_command_the_parser_cannot_split() {
    // 子 shell、后台等解析不了的结构：整条命令只认精确规则。
    assert_eq!(
        echo_allowed("echo hi; (mkdir zz14)"),
        "passthrough: This command requires approval"
    );
    assert_eq!(
        verdict(
            &[(RuleBehavior::Allow, "Bash(echo hi; (mkdir zz14))")],
            "echo hi; (mkdir zz14)"
        ),
        "allow"
    );
}
