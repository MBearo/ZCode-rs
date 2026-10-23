//! 内嵌的 bfs 按 argv0 分派的真实进程（docs/specs/rust-cli/embedded-search.md §3、§6，工作线 F53）：把 zcode 链接成
//! `bfs` 运行，或用 `ARGV0=bfs`；参数带上 prelude 给 find 的缺省参数（`-S dfs -regextype findutils-default`），在固定的
//! 小目录里检查 GNU 风格的命令（排序后比较）。只在 Unix 上（Windows 没有 bfs，§6.4）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    reason = "测试在临时目录里建文件、链接二进制并读回结果"
)]
#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

const ZCODE: &str = env!("CARGO_BIN_EXE_zcode-rs");
/// prelude 里 find 函数的缺省参数。
const DEFAULTS: [&str; 4] = ["-S", "dfs", "-regextype", "findutils-default"];

fn link(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::os::unix::fs::symlink(ZCODE, &path).unwrap();
    path
}

fn run(program: &Path, args: &[&str], cwd: &Path, home: &Path, extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ZCODE_DATA_BASE_DIR", home)
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn sorted(output: &Output) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

/// 固定的小目录：`a.ts`、`b.tsx`、`src/c.rs`、三天前改过的 `old.txt`。
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    for file in ["a.ts", "b.tsx", "src/c.rs", "old.txt"] {
        std::fs::write(root.join(file), "x\n").unwrap();
    }
    let old = SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(root.join("old.txt"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    dir
}

fn find(bfs: &Path, tree: &Path, home: &Path, args: &[&str]) -> Vec<String> {
    let all: Vec<&str> = DEFAULTS
        .iter()
        .copied()
        .chain(args.iter().copied())
        .collect();
    let output = run(bfs, &all, tree, home, &[]);
    assert!(output.status.success(), "{args:?}: {output:?}");
    sorted(&output)
}

#[test]
fn zcode_named_bfs_is_bfs_with_oniguruma() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let bfs = link(bin.path(), "bfs");
    let version = run(&bfs, &["--version"], home.path(), home.path(), &[]);
    assert!(version.status.success(), "{version:?}");
    let text = String::from_utf8_lossy(&version.stdout);
    assert!(text.starts_with("bfs 4.1.4\n"), "{text}");
    assert!(
        text.contains("CONFFLAGS := --enable-release --with-oniguruma --without-libselinux --without-libacl --without-liburing --without-libcap"),
        "{text}"
    );
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);

    let tree = fixture();
    let (root, home) = (tree.path(), home.path());
    assert_eq!(
        find(
            &bfs,
            root,
            home,
            &[".", "-name", "*.ts", "-o", "-name", "*.tsx"]
        ),
        vec!["./a.ts", "./b.tsx"]
    );
    assert_eq!(
        find(&bfs, root, home, &[".", "-type", "f", "-path", "./src/*"]),
        vec!["./src/c.rs"]
    );
    assert_eq!(
        find(&bfs, root, home, &[".", "-type", "f", "-mtime", "+1"]),
        vec!["./old.txt"]
    );
    // findutils-default 的正则（Oniguruma）。分支按左侧优先：`ts` 先匹配上，`b.tsx` 整体不匹配（GNU find 两个都匹配；
    // bfs 相同，embedded-search.md §8.4）。
    assert_eq!(
        find(&bfs, root, home, &[".", "-regex", r".*\.\(ts\|tsx\)"]),
        vec!["./a.ts"]
    );
    assert_eq!(
        find(&bfs, root, home, &[".", "-regex", r".*\.\(tsx\|ts\)"]),
        vec!["./a.ts", "./b.tsx"]
    );
    // bfs 的起点可以写在表达式后面（shell-permissions.md §4.5.1）。
    assert_eq!(
        find(&bfs, root, home, &["-name", "c.rs", "src"]),
        vec!["src/c.rs"]
    );
}

#[test]
fn the_argv0_variable_dispatches_to_bfs() {
    let home = tempfile::tempdir().unwrap();
    let tree = fixture();
    let output = run(
        Path::new(ZCODE),
        &[".", "-name", "c.rs"],
        tree.path(),
        home.path(),
        &[("ARGV0", "bfs")],
    );
    assert_eq!(sorted(&output), vec!["./src/c.rs"], "{output:?}");
    // `-exec` 起的子进程看不到 ARGV0。
    let output = run(
        Path::new(ZCODE),
        &[
            ".",
            "-name",
            "c.rs",
            "-exec",
            "/bin/sh",
            "-c",
            "echo ${ARGV0:-unset}",
            ";",
        ],
        tree.path(),
        home.path(),
        &[("ARGV0", "bfs")],
    );
    assert_eq!(sorted(&output), vec!["unset"], "{output:?}");
}

/// SIGPIPE 按缺省处理（与系统的 find 相同）：读的一方提前关掉管道时 bfs 被信号结束，不报错。
#[test]
fn a_closed_pipe_ends_bfs_quietly() {
    let home = tempfile::tempdir().unwrap();
    let tree = tempfile::tempdir().unwrap();
    // 输出要比管道的缓冲区（64 KiB）大，bfs 才会在 head 退出之后还在写：500 行、每行补到 500 字节。
    for index in 0..500 {
        std::fs::write(tree.path().join(format!("f{index}")), "").unwrap();
    }
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "ARGV0=bfs {ZCODE:?} . -printf '%-500p\\n' | head -1 | tr -d ' '"
        ))
        .current_dir(tree.path())
        .env_clear()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), ".\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}
