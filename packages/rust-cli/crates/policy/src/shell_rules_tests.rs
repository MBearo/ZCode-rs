use super::*;

fn hit(content: &str, command: &str) -> bool {
    matches(content, &candidates(command), Mode::Prefix, |_| false)
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
    assert!(!matches(
        "git *",
        &candidates("git log"),
        Mode::Exact,
        |_| false
    ));
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

#[test]
fn compound_commands_do_not_match_prefix_rules() {
    assert!(!matches(
        "ls:*",
        &candidates("ls && rm x"),
        Mode::Prefix,
        |text| text.contains("&&")
    ));
}
