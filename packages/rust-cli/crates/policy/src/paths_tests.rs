use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::permission::RuleBehavior::{Allow, Ask, Deny};

use super::*;
use crate::test_support::paths;

#[test]
fn resolution_expands_home_and_normalizes() {
    let context = paths();
    assert_eq!(context.resolve("a/../b/./c"), "/w/b/c");
    assert_eq!(context.resolve("~/x"), "/home/dev/x");
    assert_eq!(context.resolve("/etc//hosts"), "/etc/hosts");
    assert!(context.in_working_dirs("/w"));
    assert!(context.in_working_dirs("/w/sub/file"));
    assert!(!context.in_working_dirs("/www"));
}

/// 修复（R1-9）：以前 policy 自己的规范化丢掉 UNC 开头的一个 `\`、把盘符改成大写；现在用 kernel-api
/// `env_path`（Node `path.win32`），UNC 的 `..` 停在共享名，盘符大小写保留、比较时不分大小写。
#[test]
fn windows_paths_keep_the_unc_share_and_compare_without_case() {
    let context = PathContext {
        cwd: r"C:\Work".to_owned(),
        roots: vec![r"c:\work".to_owned(), r"\\server\share\repo".to_owned()],
        home: Some(r"C:\Users\dev".to_owned()),
        style: PathStyle::Windows,
        ..paths()
    };
    assert_eq!(context.resolve(r"c:\Users\dev\..\x"), r"c:\Users\x");
    assert_eq!(context.resolve(r"\\server\share\a"), r"\\server\share\a");
    assert_eq!(
        context.resolve(r"\\server\share\repo\..\..\..\x"),
        r"\\server\share\x"
    );
    assert_eq!(context.resolve(r"~\.ssh"), r"C:\Users\dev\.ssh");
    assert!(context.in_working_dirs(r"C:\Work\a"));
    assert!(context.in_working_dirs(r"\\SERVER\share\repo\src"));
    assert!(!context.in_working_dirs(r"\\server\share\other"));
}

#[test]
fn path_rules_follow_gitignore_style_anchors() {
    let context = paths();
    let hit = |pattern: &str, path: &str| rule_matches(pattern, path, "/w", &context, Deny);
    assert!(hit("//etc/**", "/etc/hosts"));
    assert!(hit("~/.ssh/**", "/home/dev/.ssh/id_rsa"));
    assert!(hit("/src/**", "/w/src/a/b.rs"));
    assert!(!hit("/src/**", "/w/lib/src/a.rs"));
    assert!(hit("src/**", "/w/src/a.rs"));
    assert!(hit("*.env", "/w/deep/x.env"));
    assert!(hit(".env", "/w/a/.env"));
    assert!(hit("secrets", "/w/secrets/key"));
    assert!(!hit("*.env", "/w/x.envy"));
}

/// 工作线 F57：规则先按根算出相对路径，再按 glob 匹配；模式部分在所有平台都不分大小写
/// （以前区分：deny `Read(.env)` 拦不住 `.ENV`）。根部分在 POSIX 上照原样比。
#[test]
fn path_rule_patterns_ignore_case() {
    let context = paths();
    let hit =
        |pattern: &str, path: &str, behavior| rule_matches(pattern, path, "/w", &context, behavior);
    assert!(hit(".env", "/w/a/.ENV", Deny));
    assert!(hit("*.env", "/w/X.Env", Ask));
    assert!(hit("src/**", "/w/SRC/a.rs", Allow));
    assert!(hit("//etc/**", "/ETC/hosts", Deny));
    // 根（规则的根到目标的前缀）不折叠：大小写不同就不在里面。
    assert!(!hit("/src/**", "/W/src/a.rs", Deny));
    assert!(!hit("~/.ssh/**", "/HOME/dev/.ssh/id_rsa", Deny));
}

fn windows() -> PathContext {
    PathContext {
        cwd: r"C:\Work".to_owned(),
        roots: vec![r"C:\Work".to_owned()],
        home: Some(r"C:\Users\dev".to_owned()),
        style: PathStyle::Windows,
        ..paths()
    }
}

/// 工作线 F57：Windows 上目标、根与 home 换成 Git Bash
/// 写法再比；`//c/x`、`C:/x`、`C:\x` 的根是 `C:\`；`~\x`、`\x` 先把 `\` 换成 `/`。以前 `//c/x/**` 等都不命中。
#[test]
fn windows_path_rules_use_git_bash_spelling() {
    let context = windows();
    let hit = |pattern: &str, path: &str| rule_matches(pattern, path, r"C:\Work", &context, Deny);
    for pattern in [
        "//c/secrets/**",
        "//C/secrets/**",
        "C:/secrets/**",
        r"C:\secrets\**",
        "c:/secrets",
    ] {
        assert!(hit(pattern, r"C:\secrets\a.txt"), "{pattern}");
    }
    assert!(!hit("//d/secrets/**", r"C:\secrets\a.txt"));
    // `//C:/x/**`：根是 `/`、模式是 `C:/x/**`，与 `/c/x/…` 对不上，永远不命中（保留）。
    assert!(!hit("//C:/secrets/**", r"C:\secrets\a.txt"));
    assert!(hit("~/.ssh/**", r"C:\Users\dev\.ssh\id_rsa"));
    assert!(hit(r"~\.ssh\**", r"C:\Users\dev\.ssh\id_rsa"));
    assert!(hit(r"\src\**", r"C:\Work\src\a.rs"));
    assert!(hit("/src/**", r"C:\Work\src\a.rs"));
    assert!(!hit("/src/**", r"C:\Other\src\a.rs"));
    assert!(hit(".env", r"C:\Work\a\.env"));
    assert!(hit("*.env", r"C:\Work\deep\x.env"));
    // `\!`、`\#` 开头的是 gitignore 的转义，不换。
    assert!(!hit(r"\!x", r"C:\Work\!x"));
    // 网络共享：`\\server\share` 写成 `//server/share`。
    let unc = PathContext {
        cwd: r"\\server\share\repo".to_owned(),
        roots: vec![r"\\server\share\repo".to_owned()],
        ..windows()
    };
    assert!(rule_matches(
        ".env",
        r"\\server\share\repo\.env",
        r"\\server\share\repo",
        &unc,
        Deny
    ));
    assert!(rule_matches(
        "/src/**",
        r"\\server\share\repo\src\a",
        r"\\server\share\repo",
        &unc,
        Deny
    ));
}

/// 工作线 F57：Windows 上 deny 与 ask 规则的根部分不分大小写，allow 照原样
/// （放行规则不因大小写多放行）。盘符在 Git Bash 写法里总是小写，所以盘符的大小写不影响 allow。
#[test]
fn windows_deny_and_ask_rules_fold_the_root() {
    let context = windows();
    let hit = |path: &str, behavior| rule_matches("/src/**", path, r"C:\Work", &context, behavior);
    assert!(hit(r"c:\work\SRC\a.rs", Deny));
    assert!(hit(r"c:\work\SRC\a.rs", Ask));
    assert!(!hit(r"c:\work\SRC\a.rs", Allow));
    assert!(hit(r"C:\Work\SRC\a.rs", Allow));
    assert!(hit(r"c:\Work\src\a.rs", Allow));
    // 只有 Windows 折叠根：POSIX 上 deny 也照原样比（见上一个测试）。
    let home =
        |path: &str, behavior| rule_matches("~/.ssh/**", path, r"C:\Work", &context, behavior);
    assert!(home(r"C:\USERS\DEV\.ssh\id_rsa", Deny));
    assert!(!home(r"C:\USERS\DEV\.ssh\id_rsa", Allow));
}
