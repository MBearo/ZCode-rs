//! 子进程环境的投影（shell-permissions.md §2.3）与 `SHELL` 的唯一来源（修复线 F58，README §4.1）。

use std::collections::BTreeMap;

use crate::LocalEnvironment;
use crate::shell::ShellConfig;

fn snapshot(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn environment(pairs: &[(&str, &str)]) -> LocalEnvironment {
    LocalEnvironment::new(None).with_shell(ShellConfig {
        env: snapshot(pairs).into(),
        ..ShellConfig::default()
    })
}

/// git、pdf、worktree 的子进程继承 `SHELL`，没有缺省值（继承进程环境）。以前没设时写成空串。
#[test]
fn git_children_get_shell_only_when_it_is_set() {
    let unset = environment(&[("PATH", "/usr/bin")]).git_env();
    assert_eq!(unset.get("SHELL"), None, "{unset:?}");
    let set = environment(&[("PATH", "/usr/bin"), ("SHELL", "/bin/zsh")]).git_env();
    assert_eq!(set.get("SHELL").map(String::as_str), Some("/bin/zsh"));
}

/// MCP stdio 服务器同样继承，没有 `/bin/sh` 的缺省值；配置的 `env` 在后。
#[test]
fn mcp_servers_get_shell_only_when_it_is_set() {
    let none = BTreeMap::new();
    let unset = crate::mcp::stdio::child_env(
        &snapshot(&[("PATH", "/usr/bin")]),
        &Default::default(),
        &none,
    );
    assert_eq!(unset.get("SHELL"), None, "{unset:?}");
    let set = crate::mcp::stdio::child_env(
        &snapshot(&[("SHELL", "/bin/zsh")]),
        &Default::default(),
        &none,
    );
    assert_eq!(set.get("SHELL").map(String::as_str), Some("/bin/zsh"));
    let configured = BTreeMap::from([("SHELL".to_owned(), "/bin/fish".to_owned())]);
    let overridden = crate::mcp::stdio::child_env(
        &snapshot(&[("SHELL", "/bin/zsh")]),
        &Default::default(),
        &configured,
    );
    assert_eq!(
        overridden.get("SHELL").map(String::as_str),
        Some("/bin/fish")
    );
}

/// Bash 命令写实际执行的 shell（不变）。
#[test]
fn commands_get_the_shell_that_runs_them() {
    let env = super::project(&snapshot(&[("SHELL", "/bin/zsh")]), Some("/bin/bash"));
    assert_eq!(env.get("SHELL").map(String::as_str), Some("/bin/bash"));
    assert_eq!(env.get("GIT_EDITOR").map(String::as_str), Some("true"));
    assert_eq!(env.get("ZCODE").map(String::as_str), Some("1"));
}
