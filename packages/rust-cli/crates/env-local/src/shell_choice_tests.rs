//! Bash 工具用找到的 shell（docs/specs/rust-cli/shell-permissions.md §2.1、§2.5，工作线 F44）：zsh 也能跑、
//! 每条命令前按 shell 关掉扩展 glob、hook 仍用 `/bin/bash`、找不到 shell 时的原文。
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rust_cli_kernel_api::env::{EnvError, StartCommand};
use rust_cli_kernel_api::env_shell::{CwdScope, HookIo};

use super::tests::{Fixture, collect, context, fixture, request};
use super::*;

/// 同 `tests::fixture`，换掉 Bash 工具用的 shell。
fn with_shell(shell: Option<&str>) -> Fixture {
    let mut fixture = fixture(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
    let mut config = fixture.shells.config.clone();
    config.bash = shell.map(PathBuf::from);
    fixture.shells = Shells::new(config);
    fixture
}

async fn output(fixture: &Fixture, operation: &str, start: StartCommand) -> String {
    fixture
        .shells
        .start(context(operation), start)
        .await
        .unwrap();
    collect(fixture, operation).await.output
}

#[test]
fn the_extglob_line_follows_the_shell_name() {
    let cwd = Some(Path::new("/t/b1.cwd"));
    assert_eq!(
        supervise::script("echo 'a'", cwd, false, Path::new("/bin/bash"), None),
        r"shopt -u extglob 2>/dev/null || true && eval 'echo '\''a'\''' < /dev/null && pwd -P >| '/t/b1.cwd'"
    );
    assert_eq!(
        supervise::script("ls", None, false, Path::new("/usr/local/bin/zsh"), None),
        "setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true && eval 'ls' < /dev/null"
    );
    // 名字里没有 bash / zsh：不加。
    assert_eq!(
        supervise::script("ls", None, false, Path::new("/opt/shell/mysh"), None),
        "eval 'ls' < /dev/null"
    );
    // hook 命令不经 bashProvider：不加，stdin 也不接 /dev/null。
    assert_eq!(
        supervise::script("ls", None, true, Path::new("/bin/bash"), None),
        "eval 'ls'"
    );
    // 内嵌搜索工具的 prelude（F53，embedded-search.md §8.2）单独写在最前面，后面照旧。
    assert_eq!(
        supervise::script(
            "ls",
            Some(Path::new("/t/b2.cwd")),
            false,
            Path::new("/bin/zsh"),
            Some("function find {\n  :\n}")
        ),
        "function find {\n  :\n}\nsetopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true && eval 'ls' < /dev/null && pwd -P >| '/t/b2.cwd'"
    );
}

#[tokio::test]
async fn commands_run_in_zsh_with_glob_qualifiers_off() {
    let fixture = with_shell(Some("/bin/zsh"));
    let probe = r#"echo "${ZSH_VERSION:+zsh}${BASH_VERSION:+bash} $SHELL"; [[ -o bareglobqual ]] && echo qualifiers || echo plain; [[ -o extendedglob ]] && echo extended || echo basic"#;
    let start = request(probe, &fixture.root, CwdScope::Pinned);
    assert_eq!(
        output(&fixture, "op1", start).await,
        "zsh /bin/zsh\nplain\nbasic\n"
    );
    // cwd 文件照常写回（`pwd -P >|` 在 zsh 里同样可用）。
    let roots = vec![EnvPath::new(fixture.root.to_string_lossy())];
    let start = request(
        "mkdir -p sub && cd sub",
        &fixture.root,
        CwdScope::Session { roots },
    );
    output(&fixture, "op2", start).await;
    let cwd = fixture
        .shells
        .shared
        .state()
        .sessions
        .get(&SessionId::new("s1"))
        .and_then(|shell| shell.cwd.clone());
    assert_eq!(cwd, Some(fixture.root.join("sub")));
}

#[tokio::test]
async fn hooks_keep_bin_bash_when_the_tool_shell_is_zsh() {
    let fixture = with_shell(Some("/bin/zsh"));
    let mut start = request(
        r#"echo "${BASH_VERSION:+bash}${ZSH_VERSION:+zsh} $SHELL""#,
        &fixture.root,
        CwdScope::Pinned,
    );
    start.hook = Some(HookIo {
        stdin: "{}\n".to_owned(),
        env: BTreeMap::new(),
    });
    assert_eq!(output(&fixture, "op1", start).await, "bash /bin/bash\n");
}

/// 缺陷修复（F44）：以前 Unix 上固定 `/bin/bash`，没有它时命令启动失败（`No such file or directory`），也没有
/// 说明。现在入口找不到 shell 时 `bash` 是 None，命令返回固定的原文。
#[tokio::test]
async fn without_a_shell_commands_fail_with_the_fixed_text() {
    let fixture = with_shell(None);
    let start = request("echo hi", &fixture.root, CwdScope::Pinned);
    let error = fixture
        .shells
        .start(context("op1"), start)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        EnvError::Io {
            message: "No suitable shell found. ZCode requires a Posix shell environment. Please ensure you have a valid shell installed and the SHELL environment variable set.".to_owned(),
        }
    );
    // hook 命令不受影响（仍用 /bin/bash）。
    let mut hook = request("echo hook", &fixture.root, CwdScope::Pinned);
    hook.hook = Some(HookIo {
        stdin: String::new(),
        env: BTreeMap::new(),
    });
    assert_eq!(output(&fixture, "op2", hook).await, "hook\n");
}
