//! Bash 命令前的 prelude 加上真实的 zcode（docs/specs/rust-cli/embedded-search.md §8，工作线 F53）：bash 与 zsh 下
//! `find` / `grep` / `rg` 经 argv0 分派到内嵌的 bfs / ugrep / rg（版本行、ugrep 的输出形状），系统的 find / grep 不参与。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    reason = "测试在临时目录里建文件并读回结果"
)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use rust_cli_env_local::{EmbeddedSearch, LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvPath, OperationContext, Origin, StartCommand, WorkspaceRef,
};
use rust_cli_kernel_api::env_shell::CwdScope;
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ShellTaskId,
};
use rust_cli_kernel_api::machine::Commands;

const ZCODE: &str = env!("CARGO_BIN_EXE_zcode-rs");

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    env: LocalEnvironment,
}

fn harness(shell: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("work");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(base.join("home")).unwrap();
    std::fs::write(root.join("src/a.rs"), "let needle = 1;\n").unwrap();
    std::fs::write(root.join("b.txt"), "needle here\n").unwrap();
    std::fs::write(root.join(".git/config"), "needle in git\n").unwrap();
    let shell = ShellConfig {
        scratch: base.join("scratch"),
        ledger: base.join("ledger"),
        bash: Some(PathBuf::from(shell)),
        bwrap: None,
        // 没有 rg 的 PATH：rg 函数转给内嵌的 rg。
        env: vec![
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            (
                "HOME".to_owned(),
                base.join("home").to_string_lossy().into_owned(),
            ),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ]
        .into(),
        embedded: Some(EmbeddedSearch {
            executable: PathBuf::from(ZCODE),
            find_and_grep: true,
        }),
    };
    let env = LocalEnvironment::new(Some(base.join("home"))).with_shell(shell);
    Harness {
        _dir: dir,
        root,
        env,
    }
}

async fn bash(harness: &Harness, index: usize, command: &str) -> String {
    let operation = OperationId::new(format!("op{index}"));
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(harness.root.to_string_lossy()),
    };
    let context = OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: operation.clone(),
        origin: Origin::Agent {
            session: SessionId::new("s1"),
            run: RunId::new("r1"),
        },
    };
    let request = StartCommand {
        command: command.to_owned(),
        cwd: EnvPath::new(harness.root.to_string_lossy()),
        timeout_ms: Some(30_000),
        task: ShellTaskId::new(format!("b{index}")),
        shell: Default::default(),
        cwd_scope: CwdScope::Pinned,
        background: None,
        sandbox: None,
        hook: None,
        on_timeout: None,
        socket: None,
        env: Default::default(),
    };
    let mut events = harness
        .env
        .start_command(&workspace, context, request)
        .await
        .unwrap();
    let mut output = Vec::new();
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => output.extend(data),
            CommandEvent::Finished { .. } => break,
            CommandEvent::Started { .. }
            | CommandEvent::Backgrounded
            | CommandEvent::BackgroundedBy { .. } => {}
        }
    }
    String::from_utf8(output).unwrap()
}

#[tokio::test]
async fn find_grep_and_rg_run_the_embedded_tools() {
    for shell in ["/bin/bash", "/bin/zsh"] {
        if !Path::new(shell).exists() {
            continue;
        }
        let harness = harness(shell);
        let versions = bash(
            &harness,
            1,
            "find --version | head -1; grep --version | head -1; rg --version | head -1",
        )
        .await;
        let lines: Vec<&str> = versions.lines().collect();
        assert_eq!(lines.len(), 3, "{shell}: {versions}");
        assert_eq!(lines[0], "bfs 4.1.4", "{shell}");
        assert!(
            lines[1].starts_with("ugrep 7.8.4 "),
            "{shell}: {}",
            lines[1]
        );
        assert_eq!(lines[2], "ripgrep 15.2.0", "{shell}");
        // bfs 的缺省参数（`-S dfs -regextype findutils-default`）与 GNU 风格的写法。
        assert_eq!(
            bash(&harness, 2, "find . -name '*.rs' -o -name '*.txt' | sort").await,
            "./b.txt\n./src/a.rs\n",
            "{shell}"
        );
        // ugrep 的缺省参数：跳过 .git，路径没有 `./`（GNU grep 会给 `./b.txt` 并搜 .git）。
        assert_eq!(
            bash(&harness, 3, "grep -rn needle . | sort").await,
            "b.txt:1:needle here\nsrc/a.rs:1:let needle = 1;\n",
            "{shell}"
        );
        assert_eq!(
            bash(&harness, 4, "rg -l needle | sort").await,
            "b.txt\nsrc/a.rs\n",
            "{shell}"
        );
    }
}
