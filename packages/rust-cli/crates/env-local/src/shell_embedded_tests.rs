//! Bash 工具命令前的 prelude 真实地跑（docs/specs/rust-cli/embedded-search.md §8，工作线 F53）：bash 与 zsh 下
//! `find` / `grep` / `rg` 转给本进程的二进制（这里是记下参数的假二进制），带上缺省参数；grep 的退回列表、二进制
//! 不可执行时、系统已有 rg 时用系统的命令（临时 PATH 里的假命令）；hook 命令没有 prelude 与 `ZCODE_EXECPATH`。
//! 真实的分派（argv0 → bfs / ugrep / rg）在 cli 的 `tests/embedded_bash.rs`。
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rust_cli_kernel_api::env_shell::{CwdScope, HookIo};

use super::embedded::EmbeddedSearch;
use super::tests::{Fixture, collect, context, fixture, request};
use super::*;

/// 可执行的小脚本。
#[expect(clippy::disallowed_methods, reason = "测试准备假的命令")]
fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Setup {
    fixture: Fixture,
}

/// `shell` 跑 Bash 工具命令；`executable` 是本进程二进制的位置（不存在时函数退回系统命令）；系统目录里有假的 find 与
/// grep（`rg` 只在 `with_rg` 时有）。
fn setup(shell: &str, executable: Option<&str>, find_and_grep: bool, with_rg: bool) -> Setup {
    let probe = fixture(Vec::new());
    let base = probe.root.parent().unwrap().to_path_buf();
    let system = base.join("system");
    for name in ["find", "grep"] {
        script(&system.join(name), &format!(r#"echo "system {name}: $*""#));
    }
    if with_rg {
        script(&system.join("rg"), r#"echo "system rg: $*""#);
    }
    let binary = base.join("bin/zcode");
    script(
        &binary,
        r#"echo "zcode[${ZCODE_EXECPATH:-unset}${ARGV0:+ ARGV0=$ARGV0}]: $*""#,
    );
    let mut fixture = probe;
    let mut config = fixture.shells.config.clone();
    config.bash = Some(PathBuf::from(shell));
    config.env = vec![(
        "PATH".to_owned(),
        format!("{}:/usr/bin:/bin", system.display()),
    )]
    .into();
    config.embedded = Some(EmbeddedSearch {
        executable: executable.map_or(binary, |name| base.join(name)),
        find_and_grep,
    });
    fixture.shells = Shells::new(config);
    Setup { fixture }
}

async fn output(setup: &Setup, operation: &str, command: &str) -> String {
    let start = request(command, &setup.fixture.root, CwdScope::Pinned);
    setup
        .fixture
        .shells
        .start(context(operation), start)
        .await
        .unwrap();
    collect(&setup.fixture, operation).await.output
}

const GREP: &str = "-G --ignore-files --hidden -I --exclude-dir=.git --exclude-dir=.svn --exclude-dir=.hg --exclude-dir=.bzr --exclude-dir=.jj --exclude-dir=.sl";

#[tokio::test]
async fn find_grep_and_rg_go_to_the_binary_with_the_default_arguments() {
    for shell in ["/bin/bash", "/bin/zsh"] {
        let setup = setup(shell, None, true, false);
        let binary = setup.fixture.root.parent().unwrap().join("bin/zcode");
        let tag = format!("zcode[{}]", binary.display());
        assert_eq!(
            output(&setup, "op1", "find . -name 'x y'").await,
            format!("{tag}: -S dfs -regextype findutils-default . -name x y\n"),
            "{shell}"
        );
        assert_eq!(
            output(&setup, "op2", "grep -rn needle").await,
            format!("{tag}: {GREP} -rn needle\n"),
            "{shell}"
        );
        // 系统没有 rg：函数转给二进制，不带缺省参数。
        assert_eq!(
            output(&setup, "op3", "rg -l needle").await,
            format!("{tag}: -l needle\n"),
            "{shell}"
        );
        // 用户写 `command find` 时照常是系统的。
        assert_eq!(
            output(&setup, "op4", "command find .").await,
            "system find: .\n",
            "{shell}"
        );
    }
}

/// 退回列表：这些写法交给系统 grep。
#[tokio::test]
async fn the_grep_fallback_list_uses_the_system_grep() {
    for shell in ["/bin/bash", "/bin/zsh"] {
        let setup = setup(shell, None, true, false);
        for (index, args) in [
            "-z foo",
            "-Z foo",
            "-rZ foo",
            "-iz foo",
            "--null foo",
            "--null-data foo",
            "--filter=x foo",
            "--pager foo",
            "--view foo",
            "--format-open=x foo",
            "--config foo",
            "---x foo",
            "-@ foo",
            "--save-config foo",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(
                output(&setup, &format!("op{index}"), &format!("grep {args}")).await,
                format!("system grep: {args}\n"),
                "{shell}: {args}"
            );
        }
        // `--zmax` 之类以 `--` 开头的长选项不在列表里（`-[!-]*[Zz]*` 要求第二个字符不是 `-`）。
        let tag = format!(
            "zcode[{}]",
            setup
                .fixture
                .root
                .parent()
                .unwrap()
                .join("bin/zcode")
                .display()
        );
        assert_eq!(
            output(&setup, "opz", "grep --zmax=2 foo").await,
            format!("{tag}: {GREP} --zmax=2 foo\n")
        );
    }
}

/// 二进制不可执行（被删掉、换了位置）：函数退回系统命令（`command find "$@"`）。
#[tokio::test]
async fn a_missing_binary_falls_back_to_the_system_commands() {
    for shell in ["/bin/bash", "/bin/zsh"] {
        let setup = setup(shell, Some("missing/zcode"), true, false);
        assert_eq!(
            output(&setup, "op1", "find . -name x").await,
            "system find: . -name x\n"
        );
        assert_eq!(
            output(&setup, "op2", "grep -n x f").await,
            "system grep: -n x f\n"
        );
        // 没有系统 rg、也没有二进制：`command rg` 找不到命令（退出码 127）。
        let missing = output(&setup, "op3", "rg x; echo $?").await;
        assert!(missing.ends_with("127\n"), "{shell}: {missing}");
    }
}

/// 系统已有 rg：不定义 rg 函数（`if ! hash rg …`）；启动参数点名 Glob / Grep：没有 find / grep。
#[tokio::test]
async fn system_rg_and_search_tools_keep_the_system_commands() {
    let setup = setup("/bin/bash", None, true, true);
    assert_eq!(output(&setup, "op1", "rg -l x").await, "system rg: -l x\n");
    let setup = setup_without_find_and_grep();
    assert_eq!(
        output(&setup, "op2", "find . -name x").await,
        "system find: . -name x\n"
    );
    assert_eq!(
        output(&setup, "op3", "grep x f").await,
        "system grep: x f\n"
    );
    let tag = format!(
        "zcode[{}]",
        setup
            .fixture
            .root
            .parent()
            .unwrap()
            .join("bin/zcode")
            .display()
    );
    assert_eq!(output(&setup, "op4", "rg x").await, format!("{tag}: x\n"));
}

fn setup_without_find_and_grep() -> Setup {
    setup("/bin/bash", None, false, false)
}

/// hook 命令不经包装：没有 prelude，也没有 `ZCODE_EXECPATH`。
#[tokio::test]
async fn hooks_get_neither_the_prelude_nor_the_variable() {
    let setup = setup("/bin/bash", None, true, false);
    let mut start = request(
        "echo ${ZCODE_EXECPATH:-unset}; find . -name x",
        &setup.fixture.root,
        CwdScope::Pinned,
    );
    start.hook = Some(HookIo {
        stdin: String::new(),
        env: BTreeMap::new(),
    });
    setup
        .fixture
        .shells
        .start(context("op1"), start)
        .await
        .unwrap();
    let output = collect(&setup.fixture, "op1").await.output;
    // hook 的 shell 是 /bin/bash、PATH 同样来自快照：系统 find 是临时目录里的假命令。
    assert_eq!(output, "unset\nsystem find: . -name x\n");
}
