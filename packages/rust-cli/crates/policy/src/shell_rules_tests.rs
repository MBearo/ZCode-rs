use rust_cli_kernel_api::permission::RuleBehavior;

use super::*;

fn hit_as(behavior: RuleBehavior, content: &str, command: &str) -> bool {
    let subject = Subject::text(command);
    matches(
        content,
        &candidates(subject, Mode::Prefix, behavior),
        Mode::Prefix,
        behavior,
    )
}

fn hit(content: &str, command: &str) -> bool {
    hit_as(RuleBehavior::Allow, content, command)
}

#[test]
fn prefix_exact_and_wildcard_rules() {
    assert!(hit("git status:*", "git status"));
    assert!(hit("git status:*", "git status --short"));
    assert!(!hit("git status:*", "git statusx"));
    assert!(hit("git status:*", "xargs git status"));
    assert!(hit("npm run test", "npm run test"));
    assert!(!hit("npm run test", "npm run test -- x"));
    assert!(hit("git *", "git"));
    assert!(hit("git *", "git log -1"));
    assert!(hit("docker * --rm", "docker run --rm"));
    assert!(!hit(r"echo \*", "echo hi"));
    // 只有转义星号时不是通配，按精确命令比较。
    assert!(hit(r"echo \*", r"echo \*"));
    assert!(!hit(r"echo \*", "echo *"));
    let exact = candidates(Subject::text("git log"), Mode::Exact, RuleBehavior::Allow);
    assert!(!matches("git *", &exact, Mode::Exact, RuleBehavior::Allow));
}

#[test]
fn safe_wrappers_and_env_vars_are_stripped() {
    assert_eq!(
        strip_safe_wrappers("NODE_ENV=test timeout 10 npm test"),
        "npm test"
    );
    assert_eq!(strip_safe_wrappers("nice -n 5 nohup make"), "make");
    assert_eq!(
        strip_safe_wrappers("SECRET=1 npm test"),
        "SECRET=1 npm test"
    );
    assert!(hit("npm test:*", "RUST_LOG=debug npm test"));
}

/// 规则匹配前去掉的变量与只读判定是同一张表（39 个；修复线 F58）。以前这里用另一张
/// `SAFE_ENV_VARS`（32 个），少了下面 7 个：`CI=1 npm test` 命中不了 `Bash(npm test)`。
#[test]
fn rule_matching_strips_every_safe_variable() {
    for name in [
        "COLUMNS",
        "LINES",
        "CLICOLOR",
        "CLICOLOR_FORCE",
        "CI",
        "DEBIAN_FRONTEND",
        "GIT_TERMINAL_PROMPT",
    ] {
        assert_eq!(
            strip_safe_wrappers(&format!("{name}=1 npm test")),
            "npm test",
            "{name}"
        );
    }
    assert!(hit("npm test", "CI=1 npm test"));
    assert_eq!(crate::safe_env::SAFE_ENV_VARS.len(), 39);
}

/// 工作线 F85（§4.5.6）：deny 与 ask 规则去掉全部前缀赋值，allow 规则只去安全的。
#[test]
fn deny_rules_strip_every_assignment_but_allow_rules_do_not() {
    assert!(hit_as(RuleBehavior::Deny, "rm:*", "X=1 rm -f x"));
    assert!(hit_as(RuleBehavior::Ask, "rm:*", "X='a b' rm -f x"));
    assert!(!hit_as(RuleBehavior::Allow, "rm:*", "X=1 rm -f x"));
    assert!(!hit_as(RuleBehavior::Deny, "rm:*", "X=$Y rm -f x"));
}

/// 前缀与通配规则并空白；通配的 allow 规则只在模式以 `*` 结尾时认 `xargs`，deny 规则都认。
#[test]
fn blanks_and_xargs_in_prefix_and_wildcard_rules() {
    assert!(hit("rm -f:*", "rm  -f x"));
    assert!(hit("rm -f:*", "rm\t-f x"));
    assert!(hit("cp *", "cp\tx y"));
    assert!(hit("echo *", "xargs echo hi"));
    assert!(!hit("docker * --rm", "xargs docker run --rm"));
    assert!(hit_as(
        RuleBehavior::Deny,
        "docker * --rm",
        "xargs docker run --rm"
    ));
    assert!(wildcard_matches("src/**/a.rs", "src/a.rs"));
    assert!(wildcard_matches("src/**/a.rs", "src/x/y/a.rs"));
}

#[test]
fn exact_rules_compare_without_redirections() {
    let subject = Subject {
        text: "mv a b > out.txt",
        plain: Some("mv a b"),
        argv: None,
        assigned: false,
    };
    for behavior in [RuleBehavior::Deny, RuleBehavior::Allow] {
        let found = candidates(subject, Mode::Exact, behavior);
        assert!(
            matches("mv a b", &found, Mode::Exact, behavior),
            "{behavior:?}"
        );
    }
}
