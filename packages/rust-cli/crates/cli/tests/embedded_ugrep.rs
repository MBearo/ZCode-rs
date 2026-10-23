//! 内嵌的 ugrep 按 argv0 分派的真实进程（docs/specs/rust-cli/embedded-search.md §3、§7，工作线 F53）：把 zcode 链接
//! （Windows 复制）成 `ugrep` 运行，或用 `ARGV0=ugrep`；参数带上 prelude 给 grep 的缺省参数，在固定的小目录里检查 GNU
//! 风格的命令（排序后比较）。
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

const ZCODE: &str = env!("CARGO_BIN_EXE_zcode-rs");
/// prelude 里 grep 函数的缺省参数。
const DEFAULTS: [&str; 10] = [
    "-G",
    "--ignore-files",
    "--hidden",
    "-I",
    "--exclude-dir=.git",
    "--exclude-dir=.svn",
    "--exclude-dir=.hg",
    "--exclude-dir=.bzr",
    "--exclude-dir=.jj",
    "--exclude-dir=.sl",
];

fn alias(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join(format!("{name}.exe"));
        std::fs::copy(ZCODE, &path).unwrap();
        path
    } else {
        let path = dir.join(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(ZCODE, &path).unwrap();
        path
    }
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
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
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
        .map(|line| line.replace('\\', "/"))
        .collect();
    lines.sort();
    lines
}

/// 固定的小目录：`.git` 里、`.gitignore` 忽略的、二进制的文件被跳过；隐藏文件照搜。
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn a() {}\nlet needle = 1;\n").unwrap();
    std::fs::write(root.join("b.txt"), "needle here\nneedles\n").unwrap();
    std::fs::write(root.join(".hidden.txt"), "hidden needle\n").unwrap();
    std::fs::write(root.join("ignored.log"), "needle ignored\n").unwrap();
    std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(root.join(".git/config"), "needle in git\n").unwrap();
    std::fs::write(root.join("blob.bin"), b"needle\x00\x01\x02binary").unwrap();
    dir
}

fn grep(ugrep: &Path, tree: &Path, home: &Path, args: &[&str]) -> Output {
    let all: Vec<&str> = DEFAULTS
        .iter()
        .copied()
        .chain(args.iter().copied())
        .collect();
    run(ugrep, &all, tree, home, &[])
}

#[test]
fn zcode_named_ugrep_is_ugrep_with_the_expected_features() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let ugrep = alias(bin.path(), "ugrep");
    let version = run(&ugrep, &["--version"], home.path(), home.path(), &[]);
    assert!(version.status.success(), "{version:?}");
    let text = String::from_utf8_lossy(&version.stdout);
    assert!(text.starts_with("ugrep 7.8.4 "), "{text}");
    // 特性串（embedded-search.md §7.1）：PCRE2（JIT 视目标而定）与这些解压格式。
    assert!(text.contains("; -P:pcre2"), "{text}");
    assert!(
        text.contains("; -z:zlib,bzip2,zstd,brotli,7z,tar/pax/cpio/zip"),
        "{text}"
    );
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn gnu_style_searches_with_the_prelude_defaults() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let ugrep = alias(bin.path(), "ugrep");
    let tree = fixture();
    let (root, home) = (tree.path(), home.path());
    // `-r`：跳过 .git、.gitignore 里的与二进制文件，隐藏文件照搜。路径没有 `./` 前缀（GNU grep 有，
    // embedded-search.md §8.4）。
    let found = grep(&ugrep, root, home, &["-rn", "needle", "."]);
    assert!(found.status.success(), "{found:?}");
    assert_eq!(
        sorted(&found),
        vec![
            ".hidden.txt:1:hidden needle",
            "b.txt:1:needle here",
            "b.txt:2:needles",
            "src/a.rs:2:let needle = 1;",
        ]
    );
    // `-E` 与 `-w`、`-c`。
    let extended = grep(
        &ugrep,
        root,
        home,
        &["-E", "-w", "-c", "ne+dle|absent", "b.txt"],
    );
    assert_eq!(sorted(&extended), vec!["1"], "{extended:?}");
    // `--include`。
    let included = grep(
        &ugrep,
        root,
        home,
        &["-rl", "--include=*.rs", "needle", "."],
    );
    assert_eq!(sorted(&included), vec!["src/a.rs"], "{included:?}");
    // `-P`（PCRE2）。
    let perl = grep(
        &ugrep,
        root,
        home,
        &["-P", "-o", r"(?<=let )\w+", "src/a.rs"],
    );
    assert_eq!(sorted(&perl), vec!["needle"], "{perl:?}");
    // 没有匹配时退出码 1。
    let none = grep(&ugrep, root, home, &["absent-word", "b.txt"]);
    assert_eq!(none.status.code(), Some(1), "{none:?}");
}

#[test]
fn the_argv0_variable_dispatches_to_ugrep() {
    let home = tempfile::tempdir().unwrap();
    let tree = fixture();
    let output = run(
        Path::new(ZCODE),
        &["-c", "needle", "b.txt"],
        tree.path(),
        home.path(),
        &[("ARGV0", "ugrep")],
    );
    assert_eq!(sorted(&output), vec!["2"], "{output:?}");
}
